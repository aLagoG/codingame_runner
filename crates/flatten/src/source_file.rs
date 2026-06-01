use miette::NamedSource;
use std::{
    cell::RefCell,
    fmt, fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};
use tracing::{debug, warn};

use crate::error::{FlattenError, ResolveErr, Result};
use crate::scanner::{ModDecl, scan_external_mods};

/// Maximum allowed module-nesting depth. Real-world crates rarely go past
/// ~10; this cap exists so a symlink loop or accidentally pathological tree
/// can't blow the stack via the recursive parser. Bump if a legitimate use
/// case is ever hit.
const MAX_DEPTH: usize = 128;

/// One cfg-skipped module: declaration that was reachable in the
/// scanner but whose target file was missing on the current build
/// target. Used to surface a consolidated summary at flatten time
/// since downstream cargo-build's "cannot find type X" errors don't
/// mention the cfg-skip that caused them.
#[derive(Debug, Clone)]
pub struct SkippedMod {
    /// The skipped mod's name (e.g. `backend`).
    pub mod_name: String,
    /// The file that contained the `mod NAME;` declaration.
    pub declared_in: PathBuf,
    /// Reason: missing file, inner-attr cfg-False, etc.
    pub reason: String,
}

thread_local! {
    static SKIPPED_MODS: RefCell<Vec<SkippedMod>> = const { RefCell::new(Vec::new()) };
}

/// Drain and return all cfg-skipped mod entries recorded since the
/// last call. Called at the end of `vendor_package` / `parse_target`
/// so the CLI can include a summary in stderr / output banner.
pub fn drain_skipped_mods() -> Vec<SkippedMod> {
    SKIPPED_MODS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Strip the raw-identifier prefix `r#` from a mod name to get the
/// filesystem-safe name. Rust's mod-file resolution uses the
/// unescaped identifier: `mod r#ref;` looks for `ref.rs`, not
/// `r#ref.rs` (zerocopy uses this for its `mod r#ref;`). Apply
/// everywhere we construct a `.rs` / `mod.rs` path from a mod name.
pub(crate) fn mod_filename(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn record_skipped_mod(mod_name: &str, declared_in: &Path, reason: &str) {
    SKIPPED_MODS.with(|v| {
        v.borrow_mut().push(SkippedMod {
            mod_name: mod_name.to_string(),
            declared_in: declared_in.to_path_buf(),
            reason: reason.to_string(),
        });
    });
}

/// A pre-processing hook applied to each file's raw source text before mod
/// scanning. Returned string must still parse as valid Rust.
pub type SourceRewrite<'a> = &'a (dyn Fn(&str) -> Result<String> + Sync);

/// Optional hooks that customize how a source file is processed before
/// `mod NAME;` scanning happens. Used by the vendoring pipeline to apply
/// `crate::` rewriting and `extern crate` stripping before mod resolution.
#[derive(Default)]
pub struct ParseOptions<'a> {
    /// Pre-process each file's raw source text. The returned string is then
    /// scanned for `mod NAME;` declarations, so any byte-shifting rewrites
    /// must produce valid Rust whose `mod` items still parse.
    pub rewrite_source: Option<SourceRewrite<'a>>,
    /// The dep's `OUT_DIR` (build-script output directory). When present,
    /// `include!(concat!(env!("OUT_DIR"), "/file.rs"))` calls in the
    /// dep's source are resolved against this directory, splicing the
    /// generated file's contents inline. Required for crates like
    /// thiserror, anyhow, num-bigint that include build-script-generated
    /// source.
    pub out_dir: Option<PathBuf>,
}

#[derive(Debug)]
enum SpliceKind {
    /// `mod NAME;` whose target file resolved — splice in inlined source.
    External(SourceFile, String /* display_path */),
    /// `mod NAME;` whose target file is missing AND the declaration carries
    /// a `#[cfg(...)]` (so the missing file is presumed inactive on this
    /// build target). Replace the trailing `;` with `{ /* skipped */ }`
    /// so the flat output is self-contained — leaving `mod NAME;` would
    /// make downstream cargo-build try to load the file from disk.
    SkippedCfg(String /* mod name */),
    /// `mod NAME;` with multiple `#[cfg_attr(PRED, path = "...")]`
    /// candidates — emit one `#[cfg(PRED)] mod NAME { contents }`
    /// block per existing candidate file. The user's compile picks
    /// one based on their target. socket2's `#[cfg_attr(unix, path =
    /// "sys/unix.rs")] #[cfg_attr(windows, path = "sys/windows.rs")]
    /// mod sys;` is the canonical case. The `span_start/end` of the
    /// containing ExternalModule covers the entire `[#[cfg_attr(_,
    /// path = ...)]]* mod NAME;` sequence — replaced wholesale by
    /// the multi-cfg-mod splice.
    MultiCfg {
        mod_name: String,
        /// Visibility prefix from the original `mod NAME;` (e.g.
        /// `"pub(crate)"` for rustix's `pub(crate) mod c;`).
        /// Applied to each cfg-gated mod block so the multi-splice
        /// preserves the original visibility.
        vis: String,
        candidates: Vec<(Option<proc_macro2::TokenStream>, SourceFile, String /* display */)>,
    },
}

#[derive(Debug)]
struct ExternalModule {
    span_start: usize,
    span_end: usize,
    kind: SpliceKind,
}

#[derive(Debug)]
pub struct SourceFile {
    contents: String,
    external_modules: Vec<ExternalModule>,
}

/// Summary of one source file in a flattened tree, suitable for rendering as
/// a directory-tree-style overview.
#[derive(Debug, Clone)]
pub struct ModuleTreeNode {
    pub display_path: String,
    pub bytes: usize,
    pub lines: usize,
    pub children: Vec<ModuleTreeNode>,
}

impl ModuleTreeNode {
    pub fn total_bytes(&self) -> usize {
        self.bytes + self.children.iter().map(|c| c.total_bytes()).sum::<usize>()
    }

    pub fn total_files(&self) -> usize {
        1 + self.children.iter().map(|c| c.total_files()).sum::<usize>()
    }
}

/// Whether a `.rs` file uses *mod-rs* or *non-mod-rs* file resolution rules.
/// See <https://doc.rust-lang.org/reference/items/modules.html>.
#[derive(Copy, Clone, Debug)]
enum FileKind {
    /// `lib.rs`, `main.rs`, or `mod.rs` — submodules are searched relative to
    /// this file's own containing directory.
    ModRsOrRoot,
    /// Any other `.rs` file (e.g. `bar.rs`) — submodules are searched in a
    /// directory named after the file (`bar/`).
    NonModRs,
}

impl FileKind {
    fn from_file(path: &Path) -> Self {
        match path.file_name().and_then(|n| n.to_str()) {
            Some("lib.rs" | "main.rs" | "mod.rs") => Self::ModRsOrRoot,
            _ => Self::NonModRs,
        }
    }

    fn submod_search_dir(self, file_path: &Path) -> PathBuf {
        let containing_dir = file_path.parent().unwrap_or(Path::new(""));
        match self {
            Self::ModRsOrRoot => containing_dir.to_path_buf(),
            Self::NonModRs => containing_dir.join(file_path.file_stem().unwrap()),
        }
    }
}

impl SourceFile {
    /// Wrap a fully-resolved source string. Used by the expander pipeline
    /// after it has already produced a single flat .rs blob — no further
    /// `mod NAME;` resolution is performed.
    pub fn from_string(contents: String) -> Self {
        SourceFile {
            contents,
            external_modules: Vec::new(),
        }
    }

    /// Parse a Rust source file and recursively inline every `mod NAME;`.
    /// Source-separator markers reference paths relative to `file_path`'s
    /// containing directory; for richer display paths use [`from_file_with_root`].
    pub fn from_file(file_path: impl AsRef<Path>) -> Result<Self> {
        let file_path = file_path.as_ref();
        let root = file_path.parent().unwrap_or(Path::new("")).to_path_buf();
        Self::from_file_with_root_inner(file_path, &root, 0, &ParseOptions::default())
    }

    /// Like [`from_file`], but display paths in source-separator markers are
    /// rendered relative to `crate_root` so they're readable from the crate's
    /// perspective (e.g. `src/scanner.rs` rather than the absolute path).
    pub fn from_file_with_root(
        file_path: impl AsRef<Path>,
        crate_root: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::from_file_with_root_inner(
            file_path.as_ref(),
            crate_root.as_ref(),
            0,
            &ParseOptions::default(),
        )
    }

    /// Like [`from_file_with_root`], but applies [`ParseOptions`] to every
    /// source file in the tree (rewrite hook applied before mod scanning).
    pub fn from_file_with_options(
        file_path: impl AsRef<Path>,
        crate_root: impl AsRef<Path>,
        options: &ParseOptions,
    ) -> Result<Self> {
        Self::from_file_with_root_inner(file_path.as_ref(), crate_root.as_ref(), 0, options)
    }

    fn from_file_with_root_inner(
        file_path: &Path,
        crate_root: &Path,
        depth: usize,
        options: &ParseOptions,
    ) -> Result<Self> {
        if depth > MAX_DEPTH {
            return Err(FlattenError::other(format!(
                "Module nesting depth exceeded {MAX_DEPTH} at `{}` — \
                 cycle, symlink loop, or pathological mod tree?",
                file_path.display()
            )));
        }
        let raw = fs::read_to_string(file_path).map_err(|e| FlattenError::Io {
            context: format!("Failed reading `{}`", file_path.display()),
            source: e,
        })?;
        // Resolve `include!()` and `include_str!()` macros against the
        // current file's directory FIRST, so the rewrite closure sees the
        // combined text and can rewrite `crate::Foo` paths inside the
        // included content the same way as in the outer file. (Otherwise
        // build-script-generated content like thiserror's
        // `OUT_DIR/private.rs` keeps unrewritten `crate::Foo` paths and
        // fails to resolve once wrapped in `pub mod thiserror`.)
        let containing_dir = file_path
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        let raw = expand_include_macros(&raw, &containing_dir, options.out_dir.as_deref(), 0)?;
        // Inline `mod NAME;` declarations that appear INSIDE macro
        // invocations (tokio's `cfg_io_driver! { mod io; … }` etc.).
        // syn's AST walk doesn't see inside macro tokens, so the
        // standard mod scanner would leave these as-is and downstream
        // cargo-build would fail to read the file (the flat output
        // is single-file). For each such declaration whose target file
        // can be located, splice `mod NAME { /* file contents */ }` in
        // place of the trailing `;`.
        let raw = inline_mods_inside_macros(
            &raw,
            &containing_dir,
            FileKind::from_file(file_path),
            file_path,
        );
        let src = if let Some(rewrite) = options.rewrite_source {
            rewrite(&raw)?
        } else {
            raw
        };

        let display_name = display_for(file_path, crate_root);
        let decls = scan_external_mods(&src).map_err(|e| FlattenError::ParseError {
            src: NamedSource::new(display_name.clone(), src.clone()),
            span: e.span().byte_range().into(),
            message: e.to_string(),
        })?;

        let kind = FileKind::from_file(file_path);
        let submod_search_dir = kind.submod_search_dir(file_path);

        let mut external_modules = Vec::new();
        for decl in decls {
            // Multi-cfg-mod: when a `mod NAME;` has any
            // `#[cfg_attr(PRED, path = "P")]` candidates, emit one
            // cfg-gated `mod NAME { contents }` block per existing
            // candidate file so the user's compile picks one based
            // on their target. This is the cornerstone of cross-
            // target portability — vendor on macOS, run on Linux.
            //
            // Two shapes:
            //   - 2+ cfg_attr-paths (socket2's `mod sys;` with
            //     unix/windows): emit each cfg-gated.
            //   - 1 cfg_attr-path (rustix's `#[cfg_attr(windows,
            //     path = "winsock_c.rs")] mod c;`): emit the
            //     PRED-gated candidate AND, if a standard-resolution
            //     path exists, emit it under `#[cfg(not(PRED))]`
            //     as the fallback. Without this, vendoring on macOS
            //     would always pick the windows-only file.
            let multi: Vec<(Option<proc_macro2::TokenStream>, PathBuf)> =
                resolve_path_attr_candidates(&decl, &containing_dir, &submod_search_dir);
            let cfg_attr_count = decl
                .path_attrs
                .iter()
                .filter(|(p, _)| p.is_some())
                .count();
            if cfg_attr_count >= 1
                && multi.len() >= 1
                && let Some(attrs_range) = decl.path_attrs_range.clone()
            {
                let mut candidates: Vec<(
                    Option<proc_macro2::TokenStream>,
                    SourceFile,
                    String,
                )> = Vec::new();
                for (pred, target) in &multi {
                    let display_path = display_for(target, crate_root);
                    let source = SourceFile::from_file_with_root_inner(
                        target,
                        crate_root,
                        depth + 1,
                        options,
                    )?;
                    candidates.push((pred.clone(), source, display_path));
                }
                // Add a `#[cfg(not(any(all_cfg_attr_preds)))]`
                // fallback if the standard-resolution path
                // exists. This handles the common "exception
                // for Windows, default for everything else"
                // pattern used by rustix and others.
                let has_unconditional_path = decl
                    .path_attrs
                    .iter()
                    .any(|(p, _)| p.is_none());
                if !has_unconditional_path {
                    let fallback_path: Option<PathBuf> = {
                        let mut search = submod_search_dir.to_path_buf();
                        for component in &decl.inline_path {
                            search.push(component);
                        }
                        let fs_name = mod_filename(&decl.name);
                        let foo_rs = search.join(format!("{fs_name}.rs"));
                        let foo_mod = search.join(fs_name).join("mod.rs");
                        if foo_rs.is_file() {
                            Some(foo_rs)
                        } else if foo_mod.is_file() {
                            Some(foo_mod)
                        } else {
                            None
                        }
                    };
                    if let Some(fallback) = fallback_path {
                        // Skip if it's the same file as one of the
                        // cfg_attr candidates (avoid duplicate mod
                        // bodies).
                        let already_listed = multi
                            .iter()
                            .any(|(_, p)| p == &fallback);
                        if !already_listed {
                            // Build `not(any(pred1, pred2, ...))`
                            // from the cfg_attr predicates.
                            let preds: Vec<String> = decl
                                .path_attrs
                                .iter()
                                .filter_map(|(p, _)| {
                                    p.as_ref().map(|t| t.to_string())
                                })
                                .collect();
                            let neg_pred_str = if preds.len() == 1 {
                                format!("not({})", preds[0])
                            } else {
                                format!("not(any({}))", preds.join(", "))
                            };
                            let neg_pred: proc_macro2::TokenStream =
                                neg_pred_str.parse().unwrap_or_default();
                            let display_path = display_for(&fallback, crate_root);
                            let source = SourceFile::from_file_with_root_inner(
                                &fallback,
                                crate_root,
                                depth + 1,
                                options,
                            )?;
                            candidates.push((Some(neg_pred), source, display_path));
                        }
                    }
                }
                external_modules.push(ExternalModule {
                    span_start: attrs_range.start,
                    span_end: decl.semi_range.end,
                    kind: SpliceKind::MultiCfg {
                        mod_name: decl.name.clone(),
                        vis: decl.vis.clone(),
                        candidates,
                    },
                });
                continue;
            }

            let resolved = match resolve_mod(&decl, &containing_dir, &submod_search_dir) {
                Ok(p) => p,
                // cfg-gated mods whose file is missing — leave the source
                // line intact and warn. Other resolution failures (ambiguity,
                // missing #[path] target) are real errors regardless of cfg.
                Err(ResolveErr::NotFound { .. }) if decl.has_cfg => {
                    warn!(
                        "Skipping cfg-gated `mod {}` in `{}`",
                        decl.name,
                        file_path.display()
                    );
                    record_skipped_mod(
                        &decl.name,
                        file_path,
                        "cfg-gated declaration whose target file is missing on this build target",
                    );
                    // Replace the trailing `;` with an empty body so the
                    // flat output stays self-contained; otherwise downstream
                    // cargo-build would try to load the missing file from
                    // disk and fail with "couldn't read src/foo/mod.rs".
                    external_modules.push(ExternalModule {
                        span_start: decl.semi_range.start,
                        span_end: decl.semi_range.end,
                        kind: SpliceKind::SkippedCfg(decl.name.clone()),
                    });
                    continue;
                }
                Err(e) => {
                    return Err(resolve_err_to_flatten(e, &decl, &display_name, &src));
                }
            };

            // Inner-attr cfg gate: previously, when a mod's target
            // file was wrapped in `#![cfg(target_os = "windows")]`
            // (windows-sys' `mod Wdk;` is the canonical case), we
            // skipped inlining on non-Windows. That made the flat
            // output host-specific.
            //
            // For cross-target portability we now ALWAYS inline. The
            // file's existing `#![cfg(target_os = ...)]` inner attr
            // becomes the inner attr of the spliced `mod NAME { ...
            // }` block — gates the contents at user compile time.
            // Vendor on macOS, run on Linux: the cfg flows through
            // and rustc evaluates correctly.

            debug!("Inlining mod `{}` from `{}`", decl.name, resolved.display());

            let display_path = display_for(&resolved, crate_root);

            external_modules.push(ExternalModule {
                span_start: decl.semi_range.start,
                span_end: decl.semi_range.end,
                kind: SpliceKind::External(
                    SourceFile::from_file_with_root_inner(
                        &resolved,
                        crate_root,
                        depth + 1,
                        options,
                    )?,
                    display_path,
                ),
            });
        }

        Ok(SourceFile {
            contents: src,
            external_modules,
        })
    }

    /// Walk the inlined module tree and return a [`ModuleTreeNode`] suitable
    /// for printing a directory-style summary. `root_display` is used as the
    /// label for this node (children carry their own display paths).
    pub fn tree(&self, root_display: impl Into<String>) -> ModuleTreeNode {
        ModuleTreeNode {
            display_path: root_display.into(),
            bytes: self.contents.len(),
            lines: self.contents.lines().count(),
            children: self
                .external_modules
                .iter()
                .flat_map(|m| -> Box<dyn Iterator<Item = ModuleTreeNode>> {
                    match &m.kind {
                        SpliceKind::External(source, display_path) => {
                            Box::new(std::iter::once(
                                source.tree(display_path.clone()),
                            ))
                        }
                        SpliceKind::SkippedCfg(_) => Box::new(std::iter::empty()),
                        SpliceKind::MultiCfg { candidates, .. } => Box::new(
                            candidates
                                .iter()
                                .map(|(_, s, d)| s.tree(d.clone()))
                                .collect::<Vec<_>>()
                                .into_iter(),
                        ),
                    }
                })
                .collect(),
        }
    }

    pub fn to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| FlattenError::Io {
                context: format!("Failed to open `{}` for writing", path.display()),
                source: e,
            })?;
        let mut writer = BufWriter::new(file);
        self.write_to(&mut writer).map_err(|e| FlattenError::Io {
            context: "Failed writing flattened output".to_string(),
            source: e,
        })?;
        writer.flush().map_err(|e| FlattenError::Io {
            context: "Failed flushing output".to_string(),
            source: e,
        })?;
        Ok(())
    }

    /// Stream the flattened source. Single source of truth — both `to_file`
    /// and `Display` go through this.
    fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let mut cursor = 0;
        for module in self.external_modules.iter() {
            w.write_all(&self.contents.as_bytes()[cursor..module.span_start])?;
            match &module.kind {
                SpliceKind::External(source, display_path) => {
                    writeln!(w, " {{ // === {display_path} ===")?;
                    source.write_to(w)?;
                    writeln!(w, "\n}} // === end {display_path} ===")?;
                }
                SpliceKind::SkippedCfg(name) => {
                    writeln!(
                        w,
                        " {{ /* cfg-skipped: source for `mod {name}` not present \
                         on this build target */ }}"
                    )?;
                }
                SpliceKind::MultiCfg { mod_name, vis, candidates } => {
                    // Replace `[#[cfg_attr(_, path = ...)]]* (pub)?
                    // mod NAME;` with one `#[cfg(PRED)] (pub)? mod
                    // NAME { contents }` block per candidate. The
                    // user's compile picks one based on their
                    // target. The cfg predicates flow through
                    // verbatim so this works cross-target (vendor
                    // on macOS, run on Linux). Visibility is
                    // preserved so e.g. rustix's `pub(crate) mod
                    // c;` still emits `pub(crate)` mods.
                    let vis_prefix = if vis.is_empty() {
                        String::new()
                    } else {
                        format!("{vis} ")
                    };
                    for (pred, source, display_path) in candidates {
                        writeln!(w)?;
                        if let Some(pred_tokens) = pred {
                            writeln!(w, "#[cfg({})]", pred_tokens)?;
                        }
                        writeln!(
                            w,
                            "{vis_prefix}mod {mod_name} {{ // === {display_path} ===",
                        )?;
                        source.write_to(w)?;
                        writeln!(w, "\n}} // === end {display_path} ===")?;
                    }
                }
            }
            cursor = module.span_end;
        }
        w.write_all(&self.contents.as_bytes()[cursor..])
    }
}

impl fmt::Display for SourceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Adapter<'a, 'b> {
            inner: &'a mut fmt::Formatter<'b>,
            err: Option<fmt::Error>,
        }
        impl io::Write for Adapter<'_, '_> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let s = std::str::from_utf8(buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                match self.inner.write_str(s) {
                    Ok(()) => Ok(buf.len()),
                    Err(e) => {
                        self.err = Some(e);
                        Err(io::Error::other("fmt error"))
                    }
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut adapter = Adapter { inner: f, err: None };
        match self.write_to(&mut adapter) {
            Ok(()) => Ok(()),
            Err(_) => Err(adapter.err.unwrap_or(fmt::Error)),
        }
    }
}

/// Compute the base directory for resolving #[path] / cfg_attr-path
/// declarations on a mod. Per the Rust Reference (§items.mod.outlined.path):
/// - top-level (non-inline) `#[path]` is relative to the file's
///   containing directory;
/// - `#[path]` on a mod nested inside inline `mod` blocks is relative
///   to the file's *submod search dir* + each inline mod name as a
///   directory component.
fn path_attr_base(
    decl: &ModDecl,
    containing_dir: &Path,
    submod_search_dir: &Path,
) -> PathBuf {
    if decl.inline_path.is_empty() {
        containing_dir.to_path_buf()
    } else {
        let mut p = submod_search_dir.to_path_buf();
        for c in &decl.inline_path {
            p.push(c);
        }
        p
    }
}

/// Resolve every existing candidate file for a mod with `#[path]` /
/// `#[cfg_attr(PRED, path = "...")]` attributes. Returns each
/// (cfg_pred, resolved_path) pair, in attribute order, for files
/// that actually exist on disk. Used by the multi-cfg-mod splice
/// path so the flat output emits ALL platform variants for
/// cross-target portability.
fn resolve_path_attr_candidates(
    decl: &ModDecl,
    containing_dir: &Path,
    submod_search_dir: &Path,
) -> Vec<(Option<proc_macro2::TokenStream>, PathBuf)> {
    let base = path_attr_base(decl, containing_dir, submod_search_dir);
    decl.path_attrs
        .iter()
        .filter_map(|(pred, rel)| {
            let resolved = base.join(rel);
            resolved.is_file().then(|| (pred.clone(), resolved))
        })
        .collect()
}

fn resolve_mod(
    decl: &ModDecl,
    containing_dir: &Path,
    submod_search_dir: &Path,
) -> std::result::Result<PathBuf, ResolveErr> {
    if !decl.path_attrs.is_empty() {
        let base = path_attr_base(decl, containing_dir, submod_search_dir);
        let mut last_resolved = base.join(&decl.path_attrs[0].1);
        for (_, rel) in &decl.path_attrs {
            let resolved = base.join(rel);
            if resolved.is_file() {
                return Ok(resolved);
            }
            last_resolved = resolved;
        }
        return Err(ResolveErr::PathAttrMissing {
            name: decl.name.clone(),
            rel: decl
                .path_attrs
                .iter()
                .map(|(_, p)| p.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            resolved: last_resolved.display().to_string(),
        });
    }

    let mut search = submod_search_dir.to_path_buf();
    for component in &decl.inline_path {
        search.push(component);
    }
    let fs_name = mod_filename(&decl.name);
    let foo_rs = search.join(format!("{fs_name}.rs"));
    let foo_mod = search.join(fs_name).join("mod.rs");
    match (foo_rs.is_file(), foo_mod.is_file()) {
        (true, true) => Err(ResolveErr::Ambiguous {
            name: decl.name.clone(),
            foo_rs: foo_rs.display().to_string(),
            foo_mod: foo_mod.display().to_string(),
        }),
        (true, false) => Ok(foo_rs),
        (false, true) => Ok(foo_mod),
        (false, false) => Err(ResolveErr::NotFound {
            name: decl.name.clone(),
            search_dir: search.display().to_string(),
        }),
    }
}

fn resolve_err_to_flatten(
    err: ResolveErr,
    decl: &ModDecl,
    src_name: &str,
    src: &str,
) -> FlattenError {
    let span = decl.item_range.clone().into();
    let named = NamedSource::new(src_name, src.to_string());
    match err {
        ResolveErr::NotFound { name, search_dir } => FlattenError::ModNotFound {
            name,
            search_dir,
            src: named,
            span,
            // Real edit-distance suggestions land with the typo-suggestions
            // roadmap item (needs strsim). Don't fake it from "first .rs in
            // dir" — that produces nonsense like "did you mean `lib`?".
            help: None,
        },
        ResolveErr::Ambiguous {
            name,
            foo_rs,
            foo_mod,
        } => FlattenError::AmbiguousMod {
            name,
            foo_rs,
            foo_mod,
            src: named,
            span,
        },
        ResolveErr::PathAttrMissing {
            name,
            rel,
            resolved,
        } => FlattenError::PathAttrMissing {
            name,
            rel,
            resolved,
            src: named,
            span,
        },
    }
}

fn display_for(file_path: &Path, crate_root: &Path) -> String {
    file_path
        .strip_prefix(crate_root)
        .unwrap_or(file_path)
        .display()
        .to_string()
}

/// Resolve `include!("path")` and `include_str!("path")` macro calls in
/// `src` against `dir`. Recursive — a file brought in via `include!()`
/// is itself expanded (relative to its own dir) before being spliced.
///
/// Also resolves `include!(concat!(env!("OUT_DIR"), "/file.rs"))` when
/// `out_dir` is provided. This is the conventional pattern for crates
/// (thiserror, anyhow, num-bigint, …) that generate source via a build
/// script. The resolved file is read from the actual on-disk OUT_DIR
/// captured by the wrapper.
///
/// `include!()` substitutes the file's contents (parsed as Rust);
/// `include_str!()` substitutes a Rust string literal containing the
/// file's contents. Both can appear at item OR expression position.
///
/// Files whose source isn't valid Rust are skipped (returns `src`
/// unchanged); the downstream parser will produce a better diagnostic.
///
/// `depth` is incremented on every recursive expansion of an
/// included file's `include!()` calls. Bounded at [`MAX_DEPTH`] to
/// prevent stack overflow from cyclic includes (`a.rs` includes `b.rs`
/// includes `a.rs`, or any longer cycle). Errors with a structured
/// diagnostic when the limit is exceeded; the offending file path is
/// included to help users debug the cycle.
fn expand_include_macros(
    src: &str,
    dir: &Path,
    out_dir: Option<&Path>,
    depth: usize,
) -> Result<String> {
    use std::ops::Range;
    use syn::spanned::Spanned;
    use syn::visit::Visit;
    if depth > MAX_DEPTH {
        return Err(FlattenError::other(format!(
            "include!() nesting depth exceeded {MAX_DEPTH} (under `{}`) — \
             cycle in include chain?",
            dir.display()
        )));
    }
    let Ok(file) = syn::parse_file(src) else {
        return Ok(src.to_string());
    };

    struct V<'a> {
        dir: &'a Path,
        out_dir: Option<&'a Path>,
        depth: usize,
        edits: Vec<(Range<usize>, String)>,
        err: Option<FlattenError>,
    }
    impl V<'_> {
        fn try_handle(&mut self, mac: &syn::Macro, span: Range<usize>) {
            if self.err.is_some() {
                return;
            }
            let is_include = mac.path.is_ident("include");
            let is_include_str = mac.path.is_ident("include_str");
            if !is_include && !is_include_str {
                return;
            }
            // Two arg shapes:
            //   1. `include!("path/literal")` — the simple case.
            //   2. `include!(concat!(env!("OUT_DIR"), "/file.rs"))` —
            //      the conventional pattern for build-script-generated
            //      source. Only resolvable when `out_dir` is set.
            let resolved_path = if let Ok(lit) =
                syn::parse2::<syn::LitStr>(mac.tokens.clone())
            {
                self.dir.join(lit.value())
            } else if let Some(out_dir) = self.out_dir
                && let Some(rel) = parse_concat_out_dir(&mac.tokens)
            {
                let trimmed = rel.trim_start_matches('/');
                out_dir.join(trimmed)
            } else {
                // include_bytes!, or args we can't parse — leave alone.
                return;
            };
            let contents = match fs::read_to_string(&resolved_path) {
                Ok(s) => s,
                Err(e) => {
                    self.err = Some(FlattenError::Io {
                        context: format!(
                            "Failed reading `{}` for {} macro",
                            resolved_path.display(),
                            if is_include { "include!" } else { "include_str!" }
                        ),
                        source: e,
                    });
                    return;
                }
            };
            let replacement = if is_include {
                let inner_dir = resolved_path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_path_buf();
                match expand_include_macros(
                    &contents,
                    &inner_dir,
                    self.out_dir,
                    self.depth + 1,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        self.err = Some(e);
                        return;
                    }
                }
            } else {
                escape_as_str_literal(&contents)
            };
            self.edits.push((span, replacement));
        }
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            self.try_handle(&im.mac, im.span().byte_range());
            // Macro args are opaque to syn; recurse via tokens to
            // catch include!()/include_str!() inside macro
            // invocations like serde's `crate_root! { include!(
            // concat!(env!("OUT_DIR"), "/private.rs")) }`.
            walk_tokens_for_includes(&im.mac.tokens, self);
        }
        fn visit_expr_macro(&mut self, em: &'ast syn::ExprMacro) {
            self.try_handle(&em.mac, em.span().byte_range());
            walk_tokens_for_includes(&em.mac.tokens, self);
        }
    }

    /// Walk a proc_macro2 token stream looking for `include!(...)`
    /// or `include_str!(...)` invocations nested inside other macro
    /// args. Calls `try_handle` on each one. Recurses into nested
    /// Group tokens (Brace, Paren — but NOT Bracket, which is an
    /// attribute body that shouldn't contain include).
    fn walk_tokens_for_includes(ts: &proc_macro2::TokenStream, v: &mut V<'_>) {
        use proc_macro2::TokenTree;
        let toks: Vec<TokenTree> = ts.clone().into_iter().collect();
        let mut i = 0;
        while i < toks.len() {
            // Detect `include` Ident, `!` Punct, `( ... )` Group.
            if let Some(TokenTree::Ident(id)) = toks.get(i)
                && (id == "include" || id == "include_str")
                && let Some(TokenTree::Punct(p)) = toks.get(i + 1)
                && p.as_char() == '!'
                && let Some(TokenTree::Group(args)) = toks.get(i + 2)
                && args.delimiter() == proc_macro2::Delimiter::Parenthesis
            {
                let id_span = id.span().byte_range();
                let args_span = args.span().byte_range();
                // Consume trailing `;` if present — at item position
                // include! has a statement-terminating semicolon
                // that's part of the syntactic shape. Without
                // consuming it, the inlined content leaves a stray
                // `;` at item position → "macro expansion ignores
                // `;` and any tokens following" error. (serde's
                // `crate_root! { include!(concat!(env!("OUT_DIR"),
                // "/private.rs")); }` is the canonical case.)
                let span = if let Some(TokenTree::Punct(semi)) = toks.get(i + 3)
                    && semi.as_char() == ';'
                {
                    let semi_span = semi.span().byte_range();
                    id_span.start..semi_span.end
                } else {
                    id_span.start..args_span.end
                };
                // Build a syn::Macro from the tokens to reuse
                // try_handle's path-resolution logic.
                if let Ok(path) = syn::parse_str::<syn::Path>(&id.to_string()) {
                    let mac = syn::Macro {
                        path,
                        bang_token: syn::Token![!](proc_macro2::Span::call_site()),
                        delimiter: syn::MacroDelimiter::Paren(syn::token::Paren::default()),
                        tokens: args.stream(),
                    };
                    v.try_handle(&mac, span);
                }
                i += 3;
                continue;
            }
            // Recurse into nested groups (but skip bracket = attr).
            if let Some(TokenTree::Group(g)) = toks.get(i)
                && g.delimiter() != proc_macro2::Delimiter::Bracket
            {
                walk_tokens_for_includes(&g.stream(), v);
            }
            i += 1;
        }
    }

    let mut v = V {
        dir,
        out_dir,
        depth,
        edits: Vec::new(),
        err: None,
    };
    v.visit_file(&file);
    if let Some(err) = v.err {
        return Err(err);
    }
    Ok(crate::edits::apply_simple_edits(src, v.edits))
}

/// Walk the source's proc_macro2 token stream looking for `mod IDENT ;`
/// triplets nested INSIDE macro invocation delim-args (`cfg_io_driver! {
/// mod io; ... }` shape). For each one whose target file can be
/// located, splice `mod IDENT { /* file contents */ }` over the trailing
/// `;` so downstream cargo-build doesn't try to load the file from disk.
///
/// The standard syn-based mod scanner only sees `mod NAME;` items at the
/// AST item-position. Anything inside a macro invocation's delim-args is
/// opaque to syn (it's just tokens). Without this pass, vendored deps
/// like tokio (which uses `cfg_io_driver!`, `cfg_io_util!`,
/// `cfg_signal!` etc. extensively) leave `mod io;` declarations in the
/// flat output that downstream cargo-build can't satisfy.
///
/// Conservative: only fires INSIDE macro tokens (won't double-process
/// the top-level mod scanner's items). Only splices when a file
/// actually resolves on disk; missing files are left alone (the
/// downstream compile error will still mention the offending mod, just
/// not flag what we should have done).
fn inline_mods_inside_macros(
    src: &str,
    containing_dir: &Path,
    kind: FileKind,
    file_path: &Path,
) -> String {
    inline_mods_inside_macros_inner(src, containing_dir, kind, file_path, false)
}

/// Internal worker. `force_in_macro_args` is set TRUE when called
/// recursively on content about to be spliced into a macro invocation
/// — in that context, ALL `mod NAME;` declarations need inlining
/// (including those at top-level of the file we're processing) since
/// they'll end up inside the outer macro after the splice.
fn inline_mods_inside_macros_inner(
    src: &str,
    containing_dir: &Path,
    kind: FileKind,
    file_path: &Path,
    force_in_macro_args: bool,
) -> String {
    use proc_macro2::{Spacing, TokenTree};
    use std::ops::Range;
    use std::str::FromStr;
    let Ok(stream) = proc_macro2::TokenStream::from_str(src) else {
        return src.to_string();
    };
    let submod_search_dir = kind.submod_search_dir(file_path);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();

    /// Build the splice text for a `mod NAME;` declaration that has
    /// multiple `#[cfg_attr(PRED, path = "P")]` candidates. Emits one
    /// `#[cfg(PRED)] mod NAME { contents }` block per existing
    /// candidate file. Plain `#[path]` (no PRED) emits an unguarded
    /// `mod NAME { contents }`.
    ///
    /// Recursively processes each candidate's contents so any
    /// nested `mod NAME;` declarations get resolved too.
    fn build_multi_cfg_mod_splice(
        mod_name: &str,
        candidates: &[(Option<proc_macro2::TokenStream>, PathBuf)],
    ) -> String {
        let mut out = String::new();
        for (pred, target) in candidates {
            let Ok(contents) = fs::read_to_string(target) else {
                continue;
            };
            let target_dir = target
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf();
            let target_kind = FileKind::from_file(target);
            let processed = inline_mods_inside_macros_inner(
                &contents,
                &target_dir,
                target_kind,
                target,
                true,
            );
            if let Some(pred_tokens) = pred {
                out.push_str(&format!("\n#[cfg({})]\n", pred_tokens));
            } else {
                out.push('\n');
            }
            out.push_str(&format!(
                "mod {mod_name} {{\n// === inlined-from-macro: {} ===\n{}\n// === end {} ===\n}}\n",
                target.display(),
                processed.trim_end(),
                target.display()
            ));
        }
        out
    }

    /// Scan from the END of the slice up to `before` looking for
    /// `#[cfg_attr(PRED, path = "PATH")]` and `#[path = "PATH"]`
    /// attributes immediately preceding the `mod` keyword. Return:
    /// - each candidate path paired with its cfg PRED (None for
    ///   plain `#[path]`), in attribute order;
    /// - the byte offset where the FIRST attribute starts (so the
    ///   caller can replace the entire `[#[cfg_attr(...)]]* mod NAME;`
    ///   sequence with a multi-cfg-mod splice).
    fn collect_path_attrs_before(
        toks: &[TokenTree],
        before: usize,
    ) -> (Vec<(Option<proc_macro2::TokenStream>, String)>, Option<usize>) {
        let mut paths: Vec<(Option<proc_macro2::TokenStream>, String)> = Vec::new();
        let mut first_attr_start: Option<usize> = None;
        // Walk backward in pairs of (Punct(#), Group([attr...])).
        let mut i = before;
        while i >= 2 {
            let hi = i - 1;
            let lo = i - 2;
            let is_attr = matches!(&toks[lo], TokenTree::Punct(p) if p.as_char() == '#')
                && matches!(&toks[hi], TokenTree::Group(g)
                    if g.delimiter() == proc_macro2::Delimiter::Bracket);
            if !is_attr {
                break;
            }
            if let TokenTree::Group(g) = &toks[hi]
                && let TokenTree::Punct(pound) = &toks[lo]
                && let Some(entry) = extract_path_from_attr(&g.stream())
            {
                paths.insert(0, entry);
                first_attr_start = Some(pound.span().byte_range().start);
            }
            i = lo;
        }
        (paths, first_attr_start)
    }

    /// Extract `(cfg_pred, path)` from a single attribute's args.
    /// `cfg_pred` is None for plain `#[path = "P"]`, `Some(tokens)`
    /// for `#[cfg_attr(PRED, path = "P")]`. Caller can use PRED to
    /// filter platform-specific candidates. Returns None for any
    /// other shape.
    fn extract_path_from_attr(
        stream: &proc_macro2::TokenStream,
    ) -> Option<(Option<proc_macro2::TokenStream>, String)> {
        let toks: Vec<TokenTree> = stream.clone().into_iter().collect();
        // `path = "P"` — 3 tokens: Ident("path"), Punct('='), Literal("P")
        if toks.len() == 3
            && let TokenTree::Ident(id) = &toks[0]
            && id == "path"
            && let TokenTree::Punct(p) = &toks[1]
            && p.as_char() == '='
            && let TokenTree::Literal(lit) = &toks[2]
        {
            let raw = lit.to_string();
            return raw
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .map(|p| (None, p.to_string()));
        }
        // `cfg_attr(...)` — Ident("cfg_attr") + Group(parens content)
        if toks.len() == 2
            && let TokenTree::Ident(id) = &toks[0]
            && id == "cfg_attr"
            && let TokenTree::Group(g) = &toks[1]
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            // Inside parens: PRED, path = "P". Split at the first
            // top-level comma to separate PRED from the path attr.
            let stream = g.stream();
            let mut pred = proc_macro2::TokenStream::new();
            let mut after_comma: Vec<TokenTree> = Vec::new();
            let mut seen_comma = false;
            for tt in stream {
                if !seen_comma
                    && matches!(&tt, TokenTree::Punct(p) if p.as_char() == ',')
                {
                    seen_comma = true;
                    continue;
                }
                if seen_comma {
                    after_comma.push(tt);
                } else {
                    pred.extend(std::iter::once(tt));
                }
            }
            // Walk after_comma for `path = "P"` shape (allowing
            // additional commas after, though not common).
            for j in 0..after_comma.len().saturating_sub(2) {
                if let TokenTree::Ident(id) = &after_comma[j]
                    && id == "path"
                    && let Some(TokenTree::Punct(p)) = after_comma.get(j + 1)
                    && p.as_char() == '='
                    && let Some(TokenTree::Literal(lit)) = after_comma.get(j + 2)
                {
                    let raw = lit.to_string();
                    return raw
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .map(|p| (Some(pred), p.to_string()));
                }
            }
        }
        None
    }

    fn walk(
        stream: proc_macro2::TokenStream,
        in_macro_args: bool,
        containing_dir: &Path,
        submod_search_dir: &Path,
        edits: &mut Vec<(Range<usize>, String)>,
    ) {
        let toks: Vec<TokenTree> = stream.into_iter().collect();
        let mut i = 0;
        while i < toks.len() {
            // Detect `macro_rules ! IDENT GROUP` — a macro_rules
            // DEFINITION whose body contains `mod NAME;` declarations
            // that need inlining. libc's `prelude!()` macro expands
            // `mod types;` at every call site, looking for a file we
            // don't have in the flat output. serde's `crate_root!{}`
            // similarly has `mod private;` whose contents define
            // `Result` etc. that the build-script-generated
            // `__private228` re-exports.
            //
            // Surgical: ONLY recurse into the body Group of each `=>`
            // arm (skipping matchers, which can contain `$NAME:tt`-
            // style pattern syntax we mustn't treat as file content).
            let is_macro_rules_def = matches!(
                toks.get(i),
                Some(TokenTree::Ident(id)) if id == "macro_rules"
            ) && matches!(
                toks.get(i + 1),
                Some(TokenTree::Punct(p)) if p.as_char() == '!'
            ) && matches!(toks.get(i + 2), Some(TokenTree::Ident(_)))
                && matches!(toks.get(i + 3), Some(TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Brace);
            if is_macro_rules_def {
                if let Some(TokenTree::Group(body)) = toks.get(i + 3) {
                    let arm_toks: Vec<TokenTree> =
                        body.stream().into_iter().collect();
                    walk_macro_rules_arms(
                        &arm_toks,
                        containing_dir,
                        submod_search_dir,
                        edits,
                    );
                }
                i += 4;
                continue;
            }
            // Detect `IDENT ! GROUP` (macro invocation). When seen at
            // the OUTER level (not already in_macro_args), recurse
            // into the GROUP with in_macro_args=true. Recursion needs
            // to handle nested macro calls too.
            let is_invocation = matches!(toks.get(i), Some(TokenTree::Ident(_)))
                && matches!(
                    toks.get(i + 1),
                    Some(TokenTree::Punct(p)) if p.as_char() == '!' && p.spacing() == Spacing::Alone
                )
                && matches!(toks.get(i + 2), Some(TokenTree::Group(_)));
            if is_invocation {
                let macro_name = match &toks[i] {
                    TokenTree::Ident(id) => id.to_string(),
                    _ => unreachable!(),
                };
                // Skip macros that are handled by other passes:
                //   - cfg_if! is expanded by `crate::cfg::expand_cfg_if`
                //     after rewrite_source runs. Inlining mod
                //     declarations inside a cfg_if! body would put
                //     full-file content inside a `$item` slot the
                //     macro can't accept.
                if !matches!(macro_name.as_str(), "cfg_if") {
                    if let Some(TokenTree::Group(g)) = toks.get(i + 2) {
                        walk(
                            g.stream(),
                            true,
                            containing_dir,
                            submod_search_dir,
                            edits,
                        );
                    }
                }
                i += 3;
                continue;
            }
            // Path-style `IDENT :: IDENT ! GROUP` (e.g. `tokio::pin!`).
            // Handle a single :: (more general would parse arbitrary
            // path segments).
            let is_qualified_invocation = matches!(toks.get(i), Some(TokenTree::Ident(_)))
                && matches!(
                    toks.get(i + 1),
                    Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint
                )
                && matches!(
                    toks.get(i + 2),
                    Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Alone
                )
                && matches!(toks.get(i + 3), Some(TokenTree::Ident(_)))
                && matches!(
                    toks.get(i + 4),
                    Some(TokenTree::Punct(p)) if p.as_char() == '!' && p.spacing() == Spacing::Alone
                )
                && matches!(toks.get(i + 5), Some(TokenTree::Group(_)));
            if is_qualified_invocation {
                let last_seg = match &toks[i + 3] {
                    TokenTree::Ident(id) => id.to_string(),
                    _ => unreachable!(),
                };
                if !matches!(last_seg.as_str(), "cfg_if") {
                    if let Some(TokenTree::Group(g)) = toks.get(i + 5) {
                        walk(
                            g.stream(),
                            true,
                            containing_dir,
                            submod_search_dir,
                            edits,
                        );
                    }
                }
                i += 6;
                continue;
            }
            // INSIDE a macro args group: look for `mod IDENT ;` triplets.
            if in_macro_args
                && let Some(TokenTree::Ident(id)) = toks.get(i)
                && id == "mod"
                && let Some(TokenTree::Ident(name)) = toks.get(i + 1)
                && let Some(TokenTree::Punct(p)) = toks.get(i + 2)
                && p.as_char() == ';'
            {
                let mod_name = name.to_string();
                // Look for `#[path = "P"]` / `#[cfg_attr(PRED, path =
                // "P")]` attributes immediately preceding the `mod`
                // keyword.
                let (path_candidates, first_attr_start) =
                    collect_path_attrs_before(&toks, i);

                // Cross-target portability: when multiple cfg_attr-
                // paths list per-platform alternatives (mio's
                // `mod selector;` with epoll/kqueue/poll, socket2's
                // `mod sys;` with sys/unix.rs and sys/windows.rs),
                // emit ALL existing candidates as separate
                // cfg-gated `mod NAME { contents_i }` blocks. The
                // user's compile picks one based on their target.
                // The compiler-set cfg predicates flow through
                // unchanged so vendoring on macOS produces output
                // that compiles on Linux too.
                let multi_candidates: Vec<(Option<proc_macro2::TokenStream>, PathBuf)> =
                    path_candidates
                        .iter()
                        .filter_map(|(pred, p)| {
                            let candidate = containing_dir.join(p);
                            candidate.is_file().then(|| (pred.clone(), candidate))
                        })
                        .collect();

                // Only emit a multi-cfg-mod splice when there's an
                // ACTUAL cfg_attr-path (not just plain `#[path]`).
                // tokio's pattern of two separate `#[path = "X.rs"]
                // #[cfg(unix)] mod imp;` declarations (one for unix,
                // one for windows) has only plain #[path] attrs;
                // each declaration's #[cfg(...)] sits BETWEEN the
                // path attr and `mod`. Wholesale-replacing the span
                // would wipe the cfg attr too. For these single-
                // path-attr cases, fall through to the legacy
                // single-file splice that just replaces the `;`.
                let has_cfg_attr_path = path_candidates
                    .iter()
                    .any(|(p, _)| p.is_some());
                if has_cfg_attr_path
                    && !multi_candidates.is_empty()
                    && let Some(start) = first_attr_start
                {
                    let semi_end = p.span().byte_range().end;
                    let splice = build_multi_cfg_mod_splice(
                        &mod_name,
                        &multi_candidates,
                    );
                    edits.push((start..semi_end, splice));
                    i += 3;
                    continue;
                }

                // Single `#[path]` (no cfg_attr alternatives) or no
                // path attr at all: fall back to standard resolution
                // and inline the single file.
                let mut resolved: Option<PathBuf> = if !path_candidates.is_empty() {
                    multi_candidates.into_iter().next().map(|(_, p)| p)
                } else {
                    None
                };
                if resolved.is_none() {
                    // No path attr (or none resolved): try standard
                    // resolution. NAME.rs or NAME/mod.rs in submod
                    // search dir, with a fall back to containing_dir.
                    // Use unescaped name for filesystem lookup —
                    // `mod r#ref;` resolves to `ref.rs`.
                    let fs_name = mod_filename(&mod_name);
                    let candidate_a = submod_search_dir.join(format!("{fs_name}.rs"));
                    let candidate_b = submod_search_dir.join(fs_name).join("mod.rs");
                    resolved = if candidate_a.is_file() {
                        Some(candidate_a)
                    } else if candidate_b.is_file() {
                        Some(candidate_b)
                    } else {
                        let alt = containing_dir.join(format!("{fs_name}.rs"));
                        if alt.is_file() {
                            Some(alt)
                        } else {
                            let alt2 = containing_dir.join(fs_name).join("mod.rs");
                            if alt2.is_file() {
                                Some(alt2)
                            } else {
                                None
                            }
                        }
                    };
                }
                if let Some(target) = resolved
                    && let Ok(contents) = fs::read_to_string(&target)
                {
                    let target_dir = target
                        .parent()
                        .unwrap_or(Path::new(""))
                        .to_path_buf();
                    let target_kind = FileKind::from_file(&target);
                    let processed = inline_mods_inside_macros_inner(
                        &contents,
                        &target_dir,
                        target_kind,
                        &target,
                        true,
                    );
                    let semi_range = p.span().byte_range();
                    let splice = format!(
                        " {{\n// === inlined-from-macro: {} ===\n{}\n// === end {} ===\n}}",
                        target.display(),
                        processed.trim_end(),
                        target.display()
                    );
                    edits.push((semi_range, splice));
                    i += 3;
                    continue;
                }
                // File not found — leave the `mod NAME;` alone.
                i += 3;
                continue;
            }
            // Recurse into ANY Group (delim block) so nested macros
            // and nested groups within macro args are walked.
            if let Some(TokenTree::Group(g)) = toks.get(i) {
                walk(
                    g.stream(),
                    in_macro_args,
                    containing_dir,
                    submod_search_dir,
                    edits,
                );
            }
            i += 1;
        }
    }

    /// Walk a macro_rules body (a sequence of `($matcher) => { $body }`
    /// arms separated by `;`). For each arm, recurse INTO the body
    /// Group with `in_macro_args=true` so the existing `mod NAME;`
    /// triplet detector inlines from file. Skips matcher Groups so
    /// pattern syntax (`$NAME:tt` etc.) doesn't get mistaken for
    /// content. SKIPS arms whose body contains build-script-generated
    /// content (heuristic: `pub mod __private<N>` items, the marker
    /// serde's build script writes into OUT_DIR/private.rs); inlining
    /// the sibling `mod NAME;` source there would conflict with the
    /// already-inlined build-script output and break re-export
    /// resolution.
    fn walk_macro_rules_arms(
        toks: &[TokenTree],
        containing_dir: &Path,
        submod_search_dir: &Path,
        edits: &mut Vec<(Range<usize>, String)>,
    ) {
        let mut j = 0;
        while j < toks.len() {
            if let TokenTree::Group(_matcher) = &toks[j] {
                let arrow_lo = j + 1;
                if matches!(toks.get(arrow_lo), Some(TokenTree::Punct(p)) if p.as_char() == '=')
                    && matches!(toks.get(arrow_lo + 1), Some(TokenTree::Punct(p)) if p.as_char() == '>')
                    && let Some(TokenTree::Group(body)) = toks.get(arrow_lo + 2)
                    && body.delimiter() == proc_macro2::Delimiter::Brace
                {
                    if !arm_has_build_script_marker(&body.stream()) {
                        walk(
                            body.stream(),
                            true,
                            containing_dir,
                            submod_search_dir,
                            edits,
                        );
                    }
                    j = arrow_lo + 3;
                    continue;
                }
            }
            j += 1;
        }
    }

    /// True if the macro_rules arm body contains a build-script-
    /// generated marker — heuristic: `pub mod __private*` (any mod
    /// whose name starts with `__private`). serde's build script
    /// writes `pub mod __private<patch_version> { pub use
    /// crate::private::*; }` into OUT_DIR/private.rs, which
    /// `--expand-deep` then inlines into the source verbatim. When
    /// such a marker is present, the macro_rules body has already
    /// "consumed" its `mod private;` declaration via the build-
    /// script mechanism; our sibling-file inline would duplicate
    /// content and break re-export resolution.
    fn arm_has_build_script_marker(stream: &proc_macro2::TokenStream) -> bool {
        let toks: Vec<TokenTree> = stream.clone().into_iter().collect();
        for w in toks.windows(3) {
            // Pattern: `pub mod __private...`
            if matches!(&w[0], TokenTree::Ident(id) if id == "pub")
                && matches!(&w[1], TokenTree::Ident(id) if id == "mod")
                && matches!(&w[2], TokenTree::Ident(id) if id.to_string().starts_with("__private"))
            {
                return true;
            }
        }
        for tt in &toks {
            if let TokenTree::Group(g) = tt
                && arm_has_build_script_marker(&g.stream())
            {
                return true;
            }
        }
        false
    }

    walk(
        stream,
        force_in_macro_args,
        containing_dir,
        &submod_search_dir,
        &mut edits,
    );

    crate::edits::apply_simple_edits(src, edits)
}

/// Inspect a source string's inner attributes (`#![cfg(...)]` at the
/// file root) and return true if any `#![cfg(...)]` predicate
/// statically EXCLUDES this file on the current build target. We only
/// know the few predicates that can be evaluated locally — primarily
/// `target_os = "X"` for X != current OS, `target_family = "Y"` for
/// Y != current family. Anything we can't decide returns false (the
/// safe default — let the file be inlined and let downstream cargo-
/// build do the real cfg evaluation).
///
/// Cheap pre-filter: skip the syn parse if the source doesn't start
/// with `#![` or contain `cfg`.
pub(crate) fn file_has_inactive_inner_cfg(src: &str) -> bool {
    if !src.contains("#![") || !src.contains("cfg") {
        return false;
    }
    let Ok(file) = syn::parse_file(src) else {
        return false;
    };
    for attr in &file.attrs {
        if !matches!(attr.style, syn::AttrStyle::Inner(_)) {
            continue;
        }
        if !attr.path().is_ident("cfg") {
            continue;
        }
        let syn::Meta::List(list) = &attr.meta else {
            continue;
        };
        if cfg_predicate_known_false(&list.tokens) {
            return true;
        }
    }
    false
}

/// Extract the FIRST `#![cfg(...)]` inner-attribute predicate from
/// a source file as a Rust source-code string (e.g. `target_os =
/// "windows"`, `windows`, `all(unix, target_pointer_width = "64")`).
/// Returns None if the file has no `#![cfg(...)]` inner attribute.
/// Used by the assembler to cfg-gate sibling-import injections that
/// reference deps with such attributes — the dep's body evaporates
/// at user compile time on non-matching targets, and an unguarded
/// `use crate::DEP;` would fail.
pub(crate) fn file_inner_cfg_predicate(src: &str) -> Option<String> {
    if !src.contains("#![") || !src.contains("cfg") {
        return None;
    }
    let file = syn::parse_file(src).ok()?;
    for attr in &file.attrs {
        if !matches!(attr.style, syn::AttrStyle::Inner(_)) {
            continue;
        }
        if !attr.path().is_ident("cfg") {
            continue;
        }
        let syn::Meta::List(list) = &attr.meta else {
            continue;
        };
        return Some(list.tokens.to_string());
    }
    None
}

/// Statically evaluate a cfg-predicate token stream. Returns true iff
/// we can prove it's false on the current build target. Conservative:
/// returns false for anything we can't decide.
pub(crate) fn cfg_predicate_known_false(tokens: &proc_macro2::TokenStream) -> bool {
    use proc_macro2::TokenTree;
    let toks: Vec<TokenTree> = tokens.clone().into_iter().collect();
    // Bare ident: `unix`, `windows`, `wasm32`, etc. (target_family /
    // target_arch shorthands). crossterm_winapi's `#![cfg(windows)]`
    // is the canonical case where this matters at vendor-skip time.
    if toks.len() == 1 {
        if let TokenTree::Ident(id) = &toks[0] {
            return cfg_bare_ident_known_false(&id.to_string());
        }
    }
    // Single key=value: `target_os = "windows"`, `target_family = "unix"`, etc.
    if toks.len() == 3 {
        if let (TokenTree::Ident(k), TokenTree::Punct(p), TokenTree::Literal(v)) =
            (&toks[0], &toks[1], &toks[2])
            && p.as_char() == '='
        {
            let key = k.to_string();
            let raw = v.to_string();
            let val = raw.trim_matches('"');
            return cfg_keyvalue_known_false(&key, val);
        }
    }
    // `not(...)`: known-false iff inner is known-true.
    if toks.len() == 2 {
        if let (TokenTree::Ident(name), TokenTree::Group(g)) = (&toks[0], &toks[1])
            && name == "not"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            return cfg_predicate_known_true(&g.stream());
        }
    }
    // `all(...)`: known-false iff any inner cfg is known-false.
    if toks.len() == 2 {
        if let (TokenTree::Ident(name), TokenTree::Group(g)) = (&toks[0], &toks[1])
            && name == "all"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            for arg in split_top_level_commas(&g.stream()) {
                if cfg_predicate_known_false(&arg) {
                    return true;
                }
            }
            return false;
        }
    }
    // `any(...)`: known-false iff EVERY inner cfg is known-false.
    // mio's `mod selector;` lists `any(target_os = "android",
    // target_os = "linux", ...)` candidates this pass needs to
    // recognise as fully-false on macOS so the wrong sibling file
    // isn't picked.
    if toks.len() == 2 {
        if let (TokenTree::Ident(name), TokenTree::Group(g)) = (&toks[0], &toks[1])
            && name == "any"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            let args = split_top_level_commas(&g.stream());
            if args.is_empty() {
                return false;
            }
            return args.iter().all(cfg_predicate_known_false);
        }
    }
    false
}

/// Mirror of `cfg_predicate_known_false` for the positive case.
/// Used by `not(...)` evaluation.
fn cfg_predicate_known_true(tokens: &proc_macro2::TokenStream) -> bool {
    use proc_macro2::TokenTree;
    let toks: Vec<TokenTree> = tokens.clone().into_iter().collect();
    if toks.len() == 1 {
        if let TokenTree::Ident(id) = &toks[0] {
            return cfg_bare_ident_known_true(&id.to_string());
        }
    }
    if toks.len() == 3 {
        if let (TokenTree::Ident(k), TokenTree::Punct(p), TokenTree::Literal(v)) =
            (&toks[0], &toks[1], &toks[2])
            && p.as_char() == '='
        {
            let key = k.to_string();
            let raw = v.to_string();
            let val = raw.trim_matches('"');
            return cfg_keyvalue_known_true(&key, val);
        }
    }
    if toks.len() == 2 {
        if let (TokenTree::Ident(name), TokenTree::Group(g)) = (&toks[0], &toks[1])
            && name == "not"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            return cfg_predicate_known_false(&g.stream());
        }
    }
    if toks.len() == 2 {
        if let (TokenTree::Ident(name), TokenTree::Group(g)) = (&toks[0], &toks[1])
            && name == "all"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            let args = split_top_level_commas(&g.stream());
            if args.is_empty() {
                return true;
            }
            return args.iter().all(cfg_predicate_known_true);
        }
    }
    if toks.len() == 2 {
        if let (TokenTree::Ident(name), TokenTree::Group(g)) = (&toks[0], &toks[1])
            && name == "any"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            for arg in split_top_level_commas(&g.stream()) {
                if cfg_predicate_known_true(&arg) {
                    return true;
                }
            }
            return false;
        }
    }
    false
}

/// Bare cfg idents that we recognise as platform shorthands. Only
/// `unix` / `windows` (target_family) and `wasm32` (target_arch) are
/// handled — others (`debug_assertions`, `test`, `proc_macro`,
/// build-script-emitted `--cfg=NAME`) we don't decide. Conservative:
/// return false on unknown so the predicate stays "unknown" and the
/// caller doesn't act.
fn cfg_bare_ident_known_false(name: &str) -> bool {
    match name {
        "unix" => std::env::consts::FAMILY != "unix",
        "windows" => std::env::consts::FAMILY != "windows",
        "wasm32" => std::env::consts::ARCH != "wasm32",
        _ => false,
    }
}

fn cfg_bare_ident_known_true(name: &str) -> bool {
    match name {
        "unix" => std::env::consts::FAMILY == "unix",
        "windows" => std::env::consts::FAMILY == "windows",
        "wasm32" => std::env::consts::ARCH == "wasm32",
        _ => false,
    }
}

/// Split a token stream on top-level commas. Used to enumerate the
/// arguments of `all(...)` / `any(...)` cfg predicates.
fn split_top_level_commas(
    stream: &proc_macro2::TokenStream,
) -> Vec<proc_macro2::TokenStream> {
    use proc_macro2::TokenTree;
    let mut out = Vec::new();
    let mut current = proc_macro2::TokenStream::new();
    let mut current_empty = true;
    for tt in stream.clone() {
        match tt {
            TokenTree::Punct(p) if p.as_char() == ',' => {
                if !current_empty {
                    out.push(std::mem::take(&mut current));
                    current_empty = true;
                }
            }
            other => {
                current.extend(std::iter::once(other));
                current_empty = false;
            }
        }
    }
    if !current_empty {
        out.push(current);
    }
    out
}

/// `key=val` is known-false on the current build target iff we know
/// the actual target value differs.
fn cfg_keyvalue_known_false(key: &str, val: &str) -> bool {
    match key {
        "target_os" => val != std::env::consts::OS,
        "target_family" => val != std::env::consts::FAMILY,
        "target_arch" => val != std::env::consts::ARCH,
        "target_pointer_width" => match val.parse::<u32>() {
            Ok(w) => w as usize != usize::BITS as usize,
            Err(_) => false,
        },
        "target_endian" => {
            (val == "little" && cfg!(target_endian = "big"))
                || (val == "big" && cfg!(target_endian = "little"))
        }
        _ => false,
    }
}

fn cfg_keyvalue_known_true(key: &str, val: &str) -> bool {
    match key {
        "target_os" => val == std::env::consts::OS,
        "target_family" => val == std::env::consts::FAMILY,
        "target_arch" => val == std::env::consts::ARCH,
        "target_pointer_width" => match val.parse::<u32>() {
            Ok(w) => w as usize == usize::BITS as usize,
            Err(_) => false,
        },
        "target_endian" => {
            (val == "little" && cfg!(target_endian = "little"))
                || (val == "big" && cfg!(target_endian = "big"))
        }
        _ => false,
    }
}

/// Recognize the `concat!(env!("OUT_DIR"), "/path/to/file.rs")` token
/// shape and return the concatenated trailing literals as the relative
/// path. Handles both the 2-arg form (`concat!(env!("OUT_DIR"),
/// "file.rs")`) and the 3+-arg form (`concat!(env!("OUT_DIR"), "/",
/// "file.rs")`) — the latter is common when devs split the slash from
/// the filename. Returns None if the tokens don't match.
fn parse_concat_out_dir(tokens: &proc_macro2::TokenStream) -> Option<String> {
    use proc_macro2::TokenTree;
    let outer: Vec<TokenTree> = tokens.clone().into_iter().collect();
    // Expect: `concat ! ( … )`
    let TokenTree::Ident(ident) = outer.first()? else {
        return None;
    };
    if ident != "concat" {
        return None;
    }
    let TokenTree::Punct(p) = outer.get(1)? else {
        return None;
    };
    if p.as_char() != '!' {
        return None;
    }
    let TokenTree::Group(g) = outer.get(2)? else {
        return None;
    };
    if g.delimiter() != proc_macro2::Delimiter::Parenthesis {
        return None;
    }
    let inner: Vec<TokenTree> = g.stream().into_iter().collect();
    // Expect: `env ! ( "OUT_DIR" ) , "lit" [ , "lit" ]*`.
    if inner.len() < 5 {
        return None;
    }
    let TokenTree::Ident(env_id) = &inner[0] else {
        return None;
    };
    if env_id != "env" {
        return None;
    }
    let TokenTree::Punct(env_bang) = &inner[1] else {
        return None;
    };
    if env_bang.as_char() != '!' {
        return None;
    }
    let TokenTree::Group(env_args) = &inner[2] else {
        return None;
    };
    if env_args.delimiter() != proc_macro2::Delimiter::Parenthesis {
        return None;
    }
    // env!()'s arg is a string literal; only OUT_DIR is supported here.
    let env_arg = env_args.stream().to_string();
    if env_arg.trim() != "\"OUT_DIR\"" {
        return None;
    }
    // Walk the rest as `, "lit" , "lit" , ...` and concatenate the
    // string literals' contents. Non-string-literal segments abort.
    let mut joined = String::new();
    let mut idx = 3;
    while idx < inner.len() {
        let TokenTree::Punct(comma) = &inner[idx] else {
            return None;
        };
        if comma.as_char() != ',' {
            return None;
        }
        let TokenTree::Literal(lit) = inner.get(idx + 1)? else {
            return None;
        };
        let raw = lit.to_string();
        let trimmed = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"'))?;
        joined.push_str(trimmed);
        idx += 2;
    }
    if joined.is_empty() {
        return None;
    }
    Some(joined)
}

/// Encode `s` as a Rust string literal: wrap in `"…"`, escape `\`, `"`,
/// and ASCII-control characters. Non-control chars (including non-ASCII)
/// pass through verbatim — Rust string literals are valid UTF-8.
fn escape_as_str_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proc_macro2::TokenStream;
    use std::str::FromStr;

    fn parse(s: &str) -> Option<String> {
        let ts = TokenStream::from_str(s).expect("lex");
        parse_concat_out_dir(&ts)
    }

    #[test]
    fn concat_out_dir_two_arg() {
        assert_eq!(
            parse(r#"concat!(env!("OUT_DIR"), "/file.rs")"#),
            Some("/file.rs".to_string()),
        );
    }

    #[test]
    fn concat_out_dir_three_arg() {
        assert_eq!(
            parse(r#"concat!(env!("OUT_DIR"), "/", "file.rs")"#),
            Some("/file.rs".to_string()),
        );
    }

    #[test]
    fn concat_out_dir_many_args() {
        assert_eq!(
            parse(r#"concat!(env!("OUT_DIR"), "/", "sub", "/", "file.rs")"#),
            Some("/sub/file.rs".to_string()),
        );
    }

    #[test]
    fn concat_out_dir_rejects_non_string_segment() {
        assert!(parse(r#"concat!(env!("OUT_DIR"), "/", FOO)"#).is_none());
    }

    #[test]
    fn concat_out_dir_rejects_non_out_dir_env() {
        assert!(parse(r#"concat!(env!("HOME"), "/file.rs")"#).is_none());
    }
}
