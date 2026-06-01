#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_ast_pretty;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use rustc_ast::visit::{self, Visitor};
use rustc_driver::Compilation;
use rustc_middle::ty::TyCtxt;
use rustc_session::config::{self, ExternEntry, ExternLocation, Externs, Input};
use rustc_session::utils::CanonicalizedPath;
use rustc_span::def_id::LOCAL_CRATE;
use rustc_span::{ExpnKind, FileName, MacroKind, Span};

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

struct CliArgs {
    input: String,
    externs: BTreeMap<String, BTreeSet<PathBuf>>,
    rewrite: bool,
}

fn parse_args() -> CliArgs {
    let mut input: Option<String> = None;
    let mut externs: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    let mut rewrite = false;
    let mut iter = std::env::args().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--extern" => {
                let val = iter.next().expect("--extern requires NAME=PATH");
                let (name, path) = val.split_once('=').expect("--extern NAME=PATH");
                externs
                    .entry(name.to_string())
                    .or_default()
                    .insert(PathBuf::from(path));
            }
            "--rewrite" => rewrite = true,
            _ if input.is_none() => input = Some(a),
            other => panic!("unexpected arg: {other}"),
        }
    }
    CliArgs {
        input: input.expect(
            "usage: cargo-flatten-expand <file.rs> [--extern NAME=PATH ...] [--rewrite]",
        ),
        externs,
        rewrite,
    }
}

fn build_externs(map: BTreeMap<String, BTreeSet<PathBuf>>) -> Externs {
    let mut out: BTreeMap<String, ExternEntry> = BTreeMap::new();
    for (name, paths) in map {
        let canon: BTreeSet<CanonicalizedPath> =
            paths.into_iter().map(CanonicalizedPath::new).collect();
        let mut entry = ExternEntry {
            location: ExternLocation::ExactPaths(canon),
            is_private_dep: false,
            add_prelude: true,
            nounused_dep: false,
            force: false,
        };
        // Suppress unused-crate warnings since we're poking at hygiene, not building.
        entry.nounused_dep = true;
        out.insert(name, entry);
    }
    Externs::new(out)
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if is_wrapper_invocation(&argv) {
        wrapper_main(&argv);
    } else {
        standalone_main();
    }
}

/// Cargo invokes `RUSTC_WRAPPER cargo-flatten-expand <real-rustc-path> <rustc-args...>`.
/// Standalone invocation is `cargo-flatten-expand <file.rs> [--rewrite] [--extern ...]`.
/// We disambiguate by extension: `.rs` → standalone, anything else (including
/// the rustc binary path or `-vV` probes) → wrapper. The wrapper also handles
/// non-`--crate-name` cargo probes via a transparent pass-through.
fn is_wrapper_invocation(argv: &[String]) -> bool {
    if argv.len() < 2 {
        return false;
    }
    !argv[1].ends_with(".rs")
}

/// Wrapper-mode entry: cargo handed us a rustc invocation. We may capture
/// the post-expansion rewrite for the crate being compiled if it's in our
/// target list, then always pass through to the real rustc so cargo's build
/// proceeds normally.
fn wrapper_main(argv: &[String]) {
    let rustc_path = &argv[1];
    let rustc_args: Vec<String> = argv[1..].to_vec();

    let crate_name = extract_arg_value(&rustc_args, "--crate-name");
    let targets: BTreeSet<String> = std::env::var("CARGO_FLATTEN_EXPAND_TARGETS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let output_dir = std::env::var("CARGO_FLATTEN_EXPAND_OUTPUT")
        .ok()
        .map(PathBuf::from);

    let should_capture = match (&crate_name, &output_dir) {
        (Some(name), Some(_)) => targets.contains(name),
        _ => false,
    };

    if should_capture {
        let crate_name = crate_name.expect("checked above");
        let output_dir = output_dir.expect("checked above");
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // Fallback: assume the input .rs file lives under <manifest>/src/.
                let input = rustc_args
                    .iter()
                    .find(|a| a.ends_with(".rs"))
                    .map(PathBuf::from);
                input
                    .as_ref()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
            });
        let target_dir = output_dir.join(&crate_name);
        let _ = std::fs::create_dir_all(&target_dir);
        let proc_macro_crates: BTreeSet<String> =
            std::env::var("CARGO_FLATTEN_EXPAND_PROC_MACROS")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        let mut callbacks = CaptureCallbacks {
            crate_name: crate_name.clone(),
            target_dir,
            manifest_dir,
            proc_macro_crates,
            pre_info: PreExpansionInfo {
                attr_to_item_span: BTreeMap::new(),
                helper_attr_strips: Vec::new(),
            },
        };
        // run_compiler does the real compile. Cargo's expected codegen outputs
        // (rlib, dep-info, etc.) are produced as a side effect — no subprocess
        // pass-through needed in this branch.
        rustc_driver::run_compiler(&rustc_args, &mut callbacks);
        return;
    }

    // Pass-through: spawn real rustc with the original args.
    let status = std::process::Command::new(rustc_path)
        .args(&argv[2..])
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn `{rustc_path}`: {e}"));
    std::process::exit(status.code().unwrap_or(1));
}

struct CaptureCallbacks {
    crate_name: String,
    /// Per-crate output directory: $CARGO_FLATTEN_EXPAND_OUTPUT/<crate_name>/
    target_dir: PathBuf,
    /// Cargo's CARGO_MANIFEST_DIR — used to compute file paths relative to
    /// the dep's source root.
    manifest_dir: PathBuf,
    /// Authoritative set of proc-macro crate names. Only expansions from
    /// these crates get inlined; only `use FOO::...;` items where FOO is
    /// in this set get stripped.
    proc_macro_crates: BTreeSet<String>,
    /// Pre-expansion AST info: attr→item-span map for Attr proc-macros,
    /// plus helper-attr strip ranges for items with non-stdlib derives.
    /// Built in `after_crate_root_parsing` since the post-expansion AST has
    /// already had host items consumed by Attr macros.
    pre_info: PreExpansionInfo,
}

impl rustc_driver::Callbacks for CaptureCallbacks {
    fn after_crate_root_parsing(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        krate: &mut rustc_ast::Crate,
    ) -> Compilation {
        self.pre_info = collect_pre_expansion_info(krate);
        Compilation::Continue
    }

    fn after_expansion<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        let (_resolver, expanded) = &*tcx.resolver_for_lowering().borrow();
        let edits = collect_edits(
            tcx,
            expanded,
            &self.proc_macro_crates,
            &self.pre_info,
        );

        // Iterate every LOCAL_CRATE source file under the dep's manifest_dir
        // and dump a rewritten copy to target_dir, preserving relative paths.
        let sm = tcx.sess.source_map();
        let files: Vec<_> = sm.files().iter().cloned().collect();
        for sf in files {
            if sf.cnum != LOCAL_CRATE {
                continue;
            }
            let real = match &sf.name {
                FileName::Real(r) => r.local_path().map(|p| p.to_path_buf()),
                _ => None,
            };
            let Some(path) = real else {
                continue;
            };
            // Source-map paths are relative to cargo's CWD (= manifest_dir)
            // for the local crate. Absolute paths can show up for files
            // included via #[path = "/abs/..."]; reject those.
            let rel: PathBuf = if path.is_absolute() {
                let Ok(stripped) = path.strip_prefix(&self.manifest_dir) else {
                    continue;
                };
                stripped.to_path_buf()
            } else {
                path.clone()
            };
            let Some(src_arc) = sf.src.clone() else {
                continue;
            };
            let original = src_arc.as_str();
            let empty = FileEdits::default();
            let plan = edits.get(&sf.start_pos.0).unwrap_or(&empty);
            let rewritten = apply_edits(original, plan);

            let out_path = self.target_dir.join(rel);
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&out_path, &rewritten) {
                eprintln!(
                    "cargo-flatten-expand: failed to write `{}`: {}",
                    out_path.display(),
                    e
                );
            }
        }
        // The wrapper only sees files rustc actually loaded for the
        // current target. Target-conditional source (getrandom's
        // `use_file.rs` on non-Linux, libc's per-arch mods, etc.) won't
        // be in the source map. Copy any remaining `.rs` files under
        // `<manifest>/src/` verbatim so cargo-flatten's mod scanner can
        // resolve cfg-True branches that didn't match the wrapper's
        // build target.
        copy_unloaded_src_files(&self.manifest_dir, &self.target_dir);

        eprintln!(
            "cargo-flatten-expand: captured `{}` -> {}",
            self.crate_name,
            self.target_dir.display()
        );
        Compilation::Continue
    }
}

fn copy_unloaded_src_files(manifest_dir: &Path, target_dir: &Path) {
    // Copy unloaded .rs files under src/ (target-conditional source not
    // hit by the wrapper's compile target).
    let src_root = manifest_dir.join("src");
    if src_root.is_dir() {
        let target_src_root = target_dir.join("src");
        walk_and_copy_missing(&src_root, &src_root, &target_src_root, /*rs_only=*/ true);
    }
    // Also copy non-source resources at the crate root (README.md,
    // LICENSE, CHANGELOG.md, etc.) so `include_str!("../README.md")`
    // calls in src/lib.rs resolve against the dump dir. We don't recurse
    // into other top-level dirs (target/, .git/, examples/, …) — only
    // single top-level files.
    if let Ok(entries) = std::fs::read_dir(manifest_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // Skip Cargo.toml/Cargo.lock — vendored output doesn't need
            // them and keeping them out reduces chance of name collisions
            // if downstream cargo introspects the dir.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "Cargo.toml" || name == "Cargo.lock" {
                continue;
            }
            let dest = target_dir.join(name);
            if dest.exists() {
                continue;
            }
            let _ = std::fs::copy(&path, &dest);
        }
    }
}

fn walk_and_copy_missing(
    dir: &Path,
    src_root: &Path,
    target_src_root: &Path,
    rs_only: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_and_copy_missing(&path, src_root, target_src_root, rs_only);
            continue;
        }
        if rs_only && path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(src_root) else {
            continue;
        };
        let dest = target_src_root.join(rel);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(&path, &dest);
    }
}

fn extract_arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some(rest.to_string());
        }
    }
    None
}

fn standalone_main() {
    let args = parse_args();
    let original_src = std::fs::read_to_string(&args.input).expect("read file");
    let rewrite_mode = args.rewrite;
    let extern_names: BTreeSet<String> = args.externs.keys().cloned().collect();

    let config = rustc_interface::Config {
        opts: config::Options {
            edition: rustc_span::edition::Edition::Edition2021,
            externs: build_externs(args.externs),
            ..Default::default()
        },
        crate_cfg: Vec::new(),
        crate_check_cfg: Vec::new(),
        input: Input::Str {
            name: FileName::Custom("input.rs".to_string()),
            input: original_src.clone(),
        },
        output_dir: None,
        output_file: None,
        ice_file: None,
        file_loader: None,
        lint_caps: Default::default(),
        psess_created: None,
        track_state: None,
        register_lints: None,
        override_queries: None,
        extra_symbols: Vec::new(),
        make_codegen_backend: None,
        using_internal_features: &USING_INTERNAL_FEATURES,
    };

    rustc_interface::run_compiler(config, |compiler| {
        let krate = rustc_interface::passes::parse(&compiler.sess);
        let pre_info = collect_pre_expansion_info(&krate);

        rustc_interface::create_and_enter_global_ctxt(compiler, krate, |tcx| {
            let (_resolver, expanded) = &*tcx.resolver_for_lowering().borrow();

            if rewrite_mode {
                let rewritten = rewrite_source(
                    tcx,
                    &original_src,
                    expanded,
                    &extern_names,
                    &pre_info,
                );
                print!("{rewritten}");
            } else {
                println!(
                    "Post-expansion: {} top-level items",
                    expanded.items.len()
                );
                let mut v = ExpnReporter {
                    tcx,
                    seen: std::collections::HashSet::new(),
                };
                visit::walk_crate(&mut v, expanded);
                println!("Distinct expansion contexts: {}", v.seen.len());
            }
        });
    });
}

/// Per-file edit plan. Three edit kinds, all keyed by file-local byte
/// offsets:
///   - `strips`: ranges to delete (used for derive paths within
///     `#[derive(...)]` and for `use proc_macro_crate::...;` items).
///   - `replacements`: in-place replacements for Bang/Attr proc-macro
///     invocations whose call_site is in user source (root context).
///   - `appends`: derive-generated impls, joined onto the end of the file.
#[derive(Default, Debug)]
struct FileEdits {
    strips: Vec<(usize, usize)>,
    replacements: Vec<(usize, usize, String)>,
    appends: Vec<String>,
}

/// Walk the post-expansion AST and produce per-file edit plans. Files are
/// keyed by their `start_pos` in the SourceMap.
///
/// `proc_macro_crates` is authoritative: only expansions originating from a
/// crate in this set get inlined, and only `use FOO::...;` items whose first
/// segment is in this set get stripped. We can't auto-derive from "any
/// non-stdlib expansion" because a regular runtime crate (simba, num_traits,
/// etc.) may define `macro_rules!` macros — those would be misclassified and
/// cause valid `use crate_name::...;` lines to be incorrectly stripped.
struct WalkCtx<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    sm: &'a rustc_span::source_map::SourceMap,
    proc_macro_crates: &'a BTreeSet<String>,
    attr_to_item_span: &'a BTreeMap<u32, (u32, u32)>,
    tainted_names: &'a HashSet<String>,
}

fn collect_edits(
    tcx: TyCtxt<'_>,
    expanded: &rustc_ast::Crate,
    proc_macro_crates: &BTreeSet<String>,
    pre_info: &PreExpansionInfo,
) -> BTreeMap<u32, FileEdits> {
    let sm = tcx.sess.source_map();
    let attr_to_item_span = &pre_info.attr_to_item_span;
    let mut edits: BTreeMap<u32, FileEdits> = BTreeMap::new();
    let mut seen_strip: HashSet<(u32, u32)> = HashSet::new();

    // Helper-attribute strips: items with non-stdlib derives may carry
    // proc-macro helper attrs (#[serde(...)], etc.) that are stranded once
    // the owning derive is inlined. record_strip handles file lookup.
    for (lo, hi) in &pre_info.helper_attr_strips {
        let span_lo = rustc_span::BytePos(*lo);
        let span_hi = rustc_span::BytePos(*hi);
        let span = rustc_span::Span::new(
            span_lo,
            span_hi,
            rustc_span::SyntaxContext::root(),
            None,
        );
        record_strip(sm, span, &mut edits, &mut seen_strip);
    }

    // Multiple expanded items can share a single Bang/Attr call_site. Group
    // their pretty-printed text by (call_site_lo, call_site_hi) so we emit
    // one `replacements` entry per call rather than overlapping ones.
    let mut bang_groups: BTreeMap<(u32, u32), (u32, String)> = BTreeMap::new();

    // Detect "tainted" macro_rules — defs whose body references a proc-
    // macro crate (e.g. simba's `complex_trait_methods!` body invokes
    // `paste::item!`). Has to run on the POST-expansion AST because
    // `after_crate_root_parsing` only sees the crate root file, not
    // submodule contents.
    let mut tainted_names: HashSet<String> = HashSet::new();
    scan_for_tainted_macros(&expanded.items, proc_macro_crates, &mut tainted_names);

    let ctx = WalkCtx {
        tcx,
        sm,
        proc_macro_crates,
        attr_to_item_span,
        tainted_names: &tainted_names,
    };
    walk_expanded_items(
        &ctx,
        &expanded.items,
        &mut edits,
        &mut seen_strip,
        &mut bang_groups,
    );

    // Fold the grouped Bang/Attr replacements into FileEdits.
    for ((lo, hi), (file_start, text)) in bang_groups {
        let local_lo = (lo - file_start) as usize;
        let local_hi = (hi - file_start) as usize;
        edits
            .entry(file_start)
            .or_default()
            .replacements
            .push((local_lo, local_hi, text));
    }

    edits
}

fn walk_expanded_items(
    ctx: &WalkCtx,
    items: &[Box<rustc_ast::Item>],
    edits: &mut BTreeMap<u32, FileEdits>,
    seen_strip: &mut HashSet<(u32, u32)>,
    bang_groups: &mut BTreeMap<(u32, u32), (u32, String)>,
) {
    use rustc_ast::visit::{self, Visitor};

    struct Walker<'a, 'tcx> {
        ctx: &'a WalkCtx<'a, 'tcx>,
        edits: &'a mut BTreeMap<u32, FileEdits>,
        seen_strip: &'a mut HashSet<(u32, u32)>,
        bang_groups: &'a mut BTreeMap<(u32, u32), (u32, String)>,
    }

    impl<'ast> Visitor<'ast> for Walker<'_, '_> {
        fn visit_item(&mut self, item: &'ast rustc_ast::Item) {
            process_one_item(
                self.ctx,
                item,
                self.edits,
                self.seen_strip,
                self.bang_groups,
            );
            // Recurse only when this item is at root context AND its
            // body wasn't transformed wholesale by an Attr proc-macro.
            //
            // The Attr-transformed case (`#[tokio::main] async fn main`,
            // `#[async_trait] impl FooFor for Bar`): rustc parses the
            // outer item with the attribute, the proc-macro consumes
            // the attribute and emits a NEW token stream that re-parses
            // into an item at the SAME source span as the original.
            // The outer item ends up at root ctxt (the attr macro
            // preserved the span), but its body contains nodes at
            // non-root ctxt that came from the macro.
            //
            // If we recurse, visit_expr fires per inner non-root child
            // and each child gets emit_direct'd separately into
            // bang_groups at the SAME host-item span — they concat
            // into a fragmented, invalid replacement. Detect this by
            // scanning the item's body for non-root child spans coming
            // from a proc-macro, and if found, emit the item's full
            // pretty-print at the host span and skip recursion.
            if item.span.ctxt().is_root() {
                if let Some(host_span) =
                    detect_attr_macro_transformation(self.ctx, item)
                {
                    emit_attr_transformed_item(
                        self.ctx,
                        item,
                        host_span,
                        self.bang_groups,
                    );
                } else {
                    visit::walk_item(self, item);
                }
            }
        }

        fn visit_assoc_item(
            &mut self,
            item: &'ast rustc_ast::AssocItem,
            ctxt: rustc_ast::visit::AssocCtxt,
        ) {
            process_assoc_item(
                self.ctx,
                item,
                self.edits,
                self.seen_strip,
                self.bang_groups,
            );
            if item.span.ctxt().is_root() {
                visit::walk_assoc_item(self, item, ctxt);
            }
        }

        fn visit_expr(&mut self, expr: &'ast rustc_ast::Expr) {
            // Expression-position macros (e.g. `nalgebra::vector![1,2,3]`,
            // `paste::paste!{...}` in expression position) appear as
            // Exprs whose span ctxt points to the macro expansion. Same
            // treatment as items: process via process_expanded_node,
            // recurse only at root context.
            let cctxt = expr.span.ctxt();
            if !cctxt.is_root() {
                process_expanded_node(
                    self.ctx,
                    cctxt,
                    expr.span,
                    || rustc_ast_pretty::pprust::expr_to_string(expr),
                    self.edits,
                    self.seen_strip,
                    self.bang_groups,
                );
            } else {
                visit::walk_expr(self, expr);
            }
        }
    }

    let mut walker = Walker {
        ctx,
        edits,
        seen_strip,
        bang_groups,
    };
    for item in items {
        walker.visit_item(item);
    }
}

/// Walk the item's body for any node at non-root ctxt whose expansion
/// chain leads back to a proc-macro Attr expansion of THIS item. If
/// found, returns the host span (= the item's full source span)
/// suitable for replacing in `bang_groups`. Returns None for items
/// whose body wasn't transformed by an Attr macro (everything else
/// — derive-augmented structs, plain user code, even items containing
/// inner Bang invocations — should follow the normal walk path).
///
/// Detection: an item that was transformed by an Attr macro will have
/// child stmts/exprs whose `span.ctxt().outer_expn()` resolves to an
/// `ExpnData` with `kind = Macro(Attr, _)` AND `call_site.lo()` equal
/// to the lo of one of this item's pre-expansion attributes (or any
/// byte inside the item's pre-expansion span — Attr macros consume
/// their attribute, so the post-expansion item's `attrs` no longer
/// shows the trigger; we have to look up via `attr_to_item_span`).
fn detect_attr_macro_transformation(
    ctx: &WalkCtx,
    item: &rustc_ast::Item,
) -> Option<rustc_span::Span> {
    use rustc_ast::visit::{self as v, Visitor};
    if !item.span.ctxt().is_root() {
        return None;
    }
    // Look for any non-root descendant whose outer expansion is an
    // Attr macro from a known proc-macro crate AND whose call_site
    // lands inside this item's span (so we know the attr was on this
    // item, not on an inner macro_rules invocation). If found, return
    // the host span.
    struct Scanner<'a, 'tcx> {
        ctx: &'a WalkCtx<'a, 'tcx>,
        item_span: rustc_span::Span,
        found_host: Option<rustc_span::Span>,
    }
    impl<'a, 'tcx> Scanner<'a, 'tcx> {
        fn check_ctxt(&mut self, span: rustc_span::Span) {
            if self.found_host.is_some() {
                return;
            }
            let cctxt = span.ctxt();
            if cctxt.is_root() {
                return;
            }
            let data = cctxt.outer_expn().expn_data();
            if !matches!(
                data.kind,
                rustc_span::ExpnKind::Macro(rustc_span::MacroKind::Attr, _)
            ) {
                return;
            }
            // Was the attr from a known proc-macro crate?
            let pm = data
                .macro_def_id
                .map(|id| {
                    self.ctx
                        .proc_macro_crates
                        .contains(&self.ctx.tcx.crate_name(id.krate).to_string())
                })
                .unwrap_or(false);
            if !pm {
                return;
            }
            let cs = data.call_site;
            if cs.is_dummy() {
                return;
            }
            // The Attr macro's call_site is its attribute's source
            // span — which sits BEFORE the item in the source
            // (`#[attr]\n item`). Pre-expansion attr_to_item_span maps
            // attr.lo → (attr.lo, item.hi) — use it to recover the
            // host item's full source range. If the attr isn't in our
            // pre-expansion map (item lives in a submodule that wasn't
            // walked pre-expansion), fall back to "cs ends just before
            // self.item_span starts within a few bytes of whitespace"
            // — the conservative rule that covers most cases.
            let host = if let Some((lo, hi)) =
                self.ctx.attr_to_item_span.get(&cs.lo().0).copied()
            {
                rustc_span::Span::new(
                    rustc_span::BytePos(lo),
                    rustc_span::BytePos(hi),
                    rustc_span::SyntaxContext::root(),
                    None,
                )
            } else {
                let item_lo = self.item_span.lo().0;
                let cs_hi = cs.hi().0;
                if cs_hi > item_lo || (item_lo - cs_hi) > 32 {
                    // Attr isn't immediately before this item — skip.
                    return;
                }
                rustc_span::Span::new(
                    cs.lo(),
                    self.item_span.hi(),
                    rustc_span::SyntaxContext::root(),
                    None,
                )
            };
            self.found_host = Some(host);
        }
    }
    impl<'ast, 'a, 'tcx> Visitor<'ast> for Scanner<'a, 'tcx> {
        fn visit_expr(&mut self, expr: &'ast rustc_ast::Expr) {
            self.check_ctxt(expr.span);
            if self.found_host.is_none() {
                v::walk_expr(self, expr);
            }
        }
        fn visit_stmt(&mut self, stmt: &'ast rustc_ast::Stmt) {
            self.check_ctxt(stmt.span);
            if self.found_host.is_none() {
                v::walk_stmt(self, stmt);
            }
        }
        fn visit_item(&mut self, item: &'ast rustc_ast::Item) {
            self.check_ctxt(item.span);
            if self.found_host.is_none() {
                v::walk_item(self, item);
            }
        }
        fn visit_assoc_item(
            &mut self,
            assoc: &'ast rustc_ast::AssocItem,
            ctxt: rustc_ast::visit::AssocCtxt,
        ) {
            self.check_ctxt(assoc.span);
            // Recurse into the assoc item's body. async_trait-style
            // Attr macros emit non-root code INSIDE the assoc fn's
            // body (the type-inference `if let __ret = None::<...>`
            // pattern), not on the assoc item itself. Without
            // recursing here, we miss those and the per-expr
            // emit_direct fragments the impl block.
            if self.found_host.is_none() {
                v::walk_assoc_item(self, assoc, ctxt);
            }
        }
    }
    // The item span doesn't include leading attributes — for an
    // `#[attr] item` shape, attribute byte positions are before
    // `item.span.lo()`. Compute the OUTER span (covering attrs + item)
    // so the cs-inside-item check matches Attr macros whose call_site
    // is at the attribute (always before the item).
    let outer_lo = item
        .attrs
        .iter()
        .map(|a| a.span.lo())
        .min()
        .unwrap_or(item.span.lo())
        .min(item.span.lo());
    let outer_hi = item.span.hi();
    let outer_span = rustc_span::Span::new(
        outer_lo,
        outer_hi,
        item.span.ctxt(),
        item.span.parent(),
    );
    let mut scanner = Scanner {
        ctx,
        item_span: outer_span,
        found_host: None,
    };
    v::walk_item(&mut scanner, item);
    scanner.found_host
}

/// Emit the FULL pretty-print of `item` at `host_span`. Pushes a
/// single bang_groups entry covering the original item's source range,
/// so when apply_edits runs the original `#[attr_macro] item` text is
/// replaced with the post-expansion form in one atomic substitution.
fn emit_attr_transformed_item(
    ctx: &WalkCtx,
    item: &rustc_ast::Item,
    host_span: rustc_span::Span,
    bang_groups: &mut BTreeMap<(u32, u32), (u32, String)>,
) {
    let host_file = ctx.sm.lookup_source_file(host_span.lo());
    let key = (host_span.lo().0, host_span.hi().0);
    let entry = bang_groups
        .entry(key)
        .or_insert_with(|| (host_file.start_pos.0, String::new()));
    if !entry.1.is_empty() {
        entry.1.push('\n');
    }
    entry.1.push_str(&rustc_ast_pretty::pprust::item_to_string(item));
}

fn process_one_item(
    ctx: &WalkCtx,
    item: &rustc_ast::Item,
    edits: &mut BTreeMap<u32, FileEdits>,
    seen_strip: &mut HashSet<(u32, u32)>,
    bang_groups: &mut BTreeMap<(u32, u32), (u32, String)>,
) {
    let cctxt = item.span.ctxt();
    if cctxt.is_root() {
        if let rustc_ast::ItemKind::Use(use_tree) = &item.kind {
            if first_segment_matches(use_tree, ctx.proc_macro_crates)
                && !item.span.is_dummy()
            {
                record_strip(ctx.sm, item.span, edits, seen_strip);
            }
        }
        // Helper-attr stripping at post-expansion catches items inside
        // submodules that the pre-expansion `walk_items_pre_expansion`
        // can't reach (it sees only the crate root file). Scope to
        // Struct/Enum/Union since those are the kinds proc-macro derives
        // attach helper attrs to (`#[serde(...)]`, `#[error(...)]`,
        // `#[arg(...)]`). For these item kinds, by the time we look at
        // the AST the proc-macro derive name has typically been removed
        // from the `#[derive(...)]` list (rustc consumes it), so we
        // can't gate on `item_has_nonstdlib_derive`. Instead always
        // strip non-builtin sibling and field-level attrs on these
        // kinds — over-stripping risk is low because users very rarely
        // attach unknown attributes to struct/enum items that aren't
        // proc-macro helpers.
        if matches!(
            &item.kind,
            rustc_ast::ItemKind::Struct(..)
                | rustc_ast::ItemKind::Enum(..)
                | rustc_ast::ItemKind::Union(..)
        ) {
            let mut all_strips: Vec<(u32, u32)> = collect_helper_attr_strips(&item.attrs);
            collect_field_helper_strips(&item.kind, &mut all_strips);
            for (lo, hi) in all_strips {
                let span = rustc_span::Span::new(
                    rustc_span::BytePos(lo),
                    rustc_span::BytePos(hi),
                    rustc_span::SyntaxContext::root(),
                    None,
                );
                record_strip(ctx.sm, span, edits, seen_strip);
            }
        }
        return;
    }
    process_expanded_node(
        ctx,
        cctxt,
        item.span,
        || rustc_ast_pretty::pprust::item_to_string(item),
        edits,
        seen_strip,
        bang_groups,
    );
}

fn process_assoc_item(
    ctx: &WalkCtx,
    assoc: &rustc_ast::AssocItem,
    edits: &mut BTreeMap<u32, FileEdits>,
    seen_strip: &mut HashSet<(u32, u32)>,
    bang_groups: &mut BTreeMap<(u32, u32), (u32, String)>,
) {
    let cctxt = assoc.span.ctxt();
    if cctxt.is_root() {
        return;
    }
    process_expanded_node(
        ctx,
        cctxt,
        assoc.span,
        || rustc_ast_pretty::pprust::assoc_item_to_string(assoc),
        edits,
        seen_strip,
        bang_groups,
    );
}

fn process_expanded_node(
    ctx: &WalkCtx,
    cctxt: rustc_span::SyntaxContext,
    _node_span: Span,
    pretty: impl FnOnce() -> String,
    edits: &mut BTreeMap<u32, FileEdits>,
    seen_strip: &mut HashSet<(u32, u32)>,
    bang_groups: &mut BTreeMap<(u32, u32), (u32, String)>,
) {
    let data = cctxt.outer_expn().expn_data();
    let direct_pm = data
        .macro_def_id
        .map(|id| {
            ctx.proc_macro_crates
                .contains(&ctx.tcx.crate_name(id.krate).to_string())
        })
        .unwrap_or(false);
    let cs = data.call_site;

    if direct_pm && !cs.is_dummy() && cs.ctxt().is_root() {
        emit_direct(
            &data,
            cs,
            pretty,
            ctx.attr_to_item_span,
            ctx.sm,
            edits,
            seen_strip,
            bang_groups,
        );
        return;
    }

    // Tainted-macro_rules case: a macro_rules definition we scanned in the
    // pre-expansion pass has a proc-macro path in its body (paste,
    // profiling-procmacros, etc.). Treat invocations of that macro_rules
    // as if they were Bang proc-macros for inlining purposes — detection
    // is by name-match because paste's transparent hygiene fuses its
    // output into the outer macro_rules' expn ctxt, so a runtime chain
    // walk can't see paste at all.
    if let Some(root_cs) = macro_rules_inline_target(cctxt, ctx.tainted_names) {
        let text = pretty();
        let host_file = ctx.sm.lookup_source_file(root_cs.lo());
        let entry = bang_groups
            .entry((root_cs.lo().0, root_cs.hi().0))
            .or_insert_with(|| (host_file.start_pos.0, String::new()));
        if !entry.1.is_empty() {
            entry.1.push('\n');
        }
        entry.1.push_str(&text);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_direct(
    data: &rustc_span::ExpnData,
    cs: Span,
    pretty: impl FnOnce() -> String,
    attr_to_item_span: &BTreeMap<u32, (u32, u32)>,
    sm: &rustc_span::source_map::SourceMap,
    edits: &mut BTreeMap<u32, FileEdits>,
    seen_strip: &mut HashSet<(u32, u32)>,
    bang_groups: &mut BTreeMap<(u32, u32), (u32, String)>,
) {
    let text = pretty();
    let host_file = sm.lookup_source_file(cs.lo());
    match data.kind {
        ExpnKind::Macro(MacroKind::Derive, _) => {
            record_strip(sm, cs, edits, seen_strip);
            edits
                .entry(host_file.start_pos.0)
                .or_default()
                .appends
                .push(text);
        }
        ExpnKind::Macro(MacroKind::Bang, _) => {
            let entry = bang_groups
                .entry((cs.lo().0, cs.hi().0))
                .or_insert_with(|| (host_file.start_pos.0, String::new()));
            if !entry.1.is_empty() {
                entry.1.push('\n');
            }
            entry.1.push_str(&text);
        }
        ExpnKind::Macro(MacroKind::Attr, _) => {
            // Attribute macros REPLACE their host item, so the
            // replacement edit must cover the FULL `#[attr] item`
            // range — not just `#[attr]`. The pre-expansion AST walker
            // builds attr→item-span mappings for items in the crate
            // root file (`pre_info.attr_to_item_span`); for items in
            // submodules (loaded during expansion), fall back to a
            // text-based scan from cs.hi() that finds the host item's
            // end via brace balancing or trailing `;`. Without this,
            // the original function/struct stays in the source AND the
            // proc-macro's expansion is appended → duplicate item
            // definition errors.
            let target = match attr_to_item_span.get(&cs.lo().0).copied() {
                Some(t) => t,
                None => {
                    let host_start_pos = host_file.start_pos.0;
                    let local_attr_hi = (cs.hi().0 - host_start_pos) as usize;
                    if let Some(src) = host_file.src.as_ref() {
                        if let Some(item_end_local) =
                            find_item_end(src.as_str(), local_attr_hi)
                        {
                            let item_end = host_start_pos + item_end_local as u32;
                            (cs.lo().0, item_end)
                        } else {
                            (cs.lo().0, cs.hi().0)
                        }
                    } else {
                        (cs.lo().0, cs.hi().0)
                    }
                }
            };
            let entry = bang_groups
                .entry(target)
                .or_insert_with(|| (host_file.start_pos.0, String::new()));
            if !entry.1.is_empty() {
                entry.1.push('\n');
            }
            entry.1.push_str(&text);
        }
        _ => {}
    }
}

/// Text-based scan: starting `from` (a byte offset just after the closing
/// `]` of an `#[attr]`), find the end of the item the attr annotates.
/// Returns the byte offset of the byte AFTER the item's terminator (one
/// past the matching `}` for braced items, or one past `;` for unit
/// structs / type aliases / use). Skips block- and line-comments,
/// nested block comments, and the contents of all string- and char-
/// literal forms (`"…"`, `r#"…"#`, `b"…"`, `c"…"`, `b'…'`, `'…'`).
/// Returns None on parse failure.
///
/// LIMITATION: this is a hand-rolled lexer; the long-term plan
/// (REVIEW.md "Larger refactors") is to replace it with a syn parse so
/// we get rustc-grade tokenisation for free.
fn find_item_end(src: &str, from: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = from;
    // Combined depth across all bracket pairs (`{}`, `[]`, `()`). A `;`
    // is only an item terminator when ALL brackets are balanced — e.g.
    // `fn f(x: [u32; 3]) { … }` has a `;` inside `[u32; 3]` at depth 1
    // that must NOT be mistaken for the function-decl terminator.
    let mut depth: i32 = 0;
    // Brace depth tracked separately — once we've seen the opening `{`
    // of the item body and it balances back to zero, that's the end.
    let mut brace_depth: i32 = 0;
    let mut saw_open_brace = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // Nested block comments are valid Rust; track depth.
                let mut comment_depth = 1;
                i += 2;
                while i + 1 < bytes.len() && comment_depth > 0 {
                    if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                        comment_depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        comment_depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            // Raw string: `r"..."`, `r#"..."#`, `r##"..."##`, etc.
            // Optional `b`/`c` prefix: `br#"..."#`, `cr#"..."#`.
            // Detect by walking backward from `r` to confirm it's not
            // mid-identifier, then count the opening `#`s and look for
            // a matching `"` followed by the same count of `#`s.
            b'r' | b'b' | b'c' if is_token_start(bytes, i) => {
                if let Some(next) = parse_raw_string_or_byte_string(bytes, i) {
                    i = next;
                } else {
                    i += 1;
                }
            }
            b'"' => {
                // Plain string literal.
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if i + 1 < bytes.len() => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'\'' => {
                // Char literal OR lifetime token. Differentiate by
                // looking at the second byte: a lifetime is `'<ident-
                // continue>` not followed by `'`; a char literal closes
                // with `'` within ~6 bytes (longest is `'\u{ABCDEF}'`).
                if let Some(next) = parse_char_or_lifetime(bytes, i) {
                    i = next;
                } else {
                    i += 1;
                }
            }
            b'{' => {
                depth += 1;
                brace_depth += 1;
                saw_open_brace = true;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                brace_depth -= 1;
                i += 1;
                if saw_open_brace && brace_depth == 0 {
                    return Some(i);
                }
            }
            b'[' | b'(' => {
                depth += 1;
                i += 1;
            }
            b']' | b')' => {
                depth -= 1;
                i += 1;
            }
            b';' if depth == 0 && !saw_open_brace => {
                return Some(i + 1);
            }
            _ => i += 1,
        }
    }
    None
}

/// True if the byte at position `i` could start a fresh token (i.e. the
/// previous byte is not an ident-continue byte). Used to distinguish
/// `r"…"` (raw string, `r` is a token start) from the `r` inside
/// `for_r"foo"` (which would be invalid Rust anyway, but defensive).
fn is_token_start(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = bytes[i - 1];
    !(prev.is_ascii_alphanumeric() || prev == b'_')
}

/// Try to parse a raw-string or byte-string literal starting at `i`.
/// Recognises: `r"…"`, `r#"…"#`, `r##"…"##`, `br"…"`, `br#"…"#`,
/// `cr"…"`, `cr#"…"#`, `b"…"`, `c"…"`. Returns the byte offset just
/// past the closing quote (and matching `#`s). Returns None if the
/// shape isn't a string literal at this position.
fn parse_raw_string_or_byte_string(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    // Optional `b` or `c` prefix.
    let has_byte_prefix = matches!(bytes.get(i), Some(b'b') | Some(b'c'));
    if has_byte_prefix {
        i += 1;
    }
    // Optional `r` for raw.
    let raw = matches!(bytes.get(i), Some(b'r'));
    if raw {
        i += 1;
    }
    if !has_byte_prefix && !raw {
        return None;
    }
    if raw {
        // Raw string: count opening `#`s, then expect `"`.
        let hash_start = i;
        while bytes.get(i) == Some(&b'#') {
            i += 1;
        }
        let hashes = i - hash_start;
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        i += 1;
        // Walk to closing `"` followed by exactly `hashes` `#`s.
        loop {
            match bytes.get(i)? {
                b'"' => {
                    let mut j = i + 1;
                    let mut close_count = 0;
                    while close_count < hashes && bytes.get(j) == Some(&b'#') {
                        close_count += 1;
                        j += 1;
                    }
                    if close_count == hashes {
                        return Some(j);
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
    }
    // Non-raw byte/c-string: `b"…"` or `c"…"`. Same body shape as
    // a plain `"…"`, with `\` escapes.
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Try to parse a char literal OR lifetime token starting at `'` at
/// position `start`. Returns the byte offset just past the construct.
///
/// Char literal forms: `'a'`, `'\n'`, `'\\'`, `'\''`, `'\x41'`,
/// `'\u{1F600}'`. Lifetime forms: `'a`, `'foo`, `'static`, `'_`. The
/// distinguishing feature is whether a closing `'` appears within the
/// reasonable char-literal length (~10 bytes for the unicode escape).
///
/// Algorithm: peek for an escape (`\`) — if present, must be a char
/// literal, walk to the closing `'`. Otherwise look for a closing `'`
/// at offset 2 (`'a'`) — if present, char literal of length 1.
/// Otherwise it's a lifetime; scan ident-continue bytes until the
/// first non-ident byte, returning that position.
fn parse_char_or_lifetime(bytes: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(bytes.get(start), Some(&b'\''));
    let mut i = start + 1;
    if bytes.get(i) == Some(&b'\\') {
        // Escape — definitely a char literal. Walk to closing `'`,
        // skipping the `\<byte>` pair and any `\u{…}` block.
        i += 1;
        if bytes.get(i) == Some(&b'u') && bytes.get(i + 1) == Some(&b'{') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            if bytes.get(i) == Some(&b'}') {
                i += 1;
            }
        } else if i < bytes.len() {
            i += 1; // single-char escape body (e.g. `n`, `t`, `\\`, `'`, `0`)
        }
        if bytes.get(i) == Some(&b'\'') {
            return Some(i + 1);
        }
        // Malformed; fall through.
        return None;
    }
    // Non-escape: peek for `<char>'` shape.
    if bytes.get(i + 1) == Some(&b'\'') {
        return Some(i + 2);
    }
    // Lifetime: walk ident-continue bytes.
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' {
            i += 1;
        } else {
            break;
        }
    }
    Some(i)
}

/// Decide whether to inline an expansion at its root call_site, when the
/// outer expansion is a `macro_rules!` (not a proc-macro itself) but its
/// body invokes a proc-macro.
///
/// Why we can't use a chain walk: proc-macros like `paste::item!` set
/// their output token spans via `Span::call_site()` (the conventional
/// transparent-hygiene mode). Rustc fuses those tokens into the parent
/// expansion's syntax context — so walking up the chain from an item
/// generated by `paste!` inside `complex_trait_methods!` finds only
/// `complex_trait_methods!`, never paste. Detection has to happen at the
/// macro_rules definition: if its body textually references a known
/// proc-macro crate, we treat invocations of THIS macro_rules as if it
/// were itself a Bang proc-macro for replacement purposes.
///
/// Returns the call_site to use for the byte-edit replacement, or None
/// if this expansion shouldn't be inlined (the macro_rules wasn't tainted
/// by a proc-macro reference, or the call_site isn't at root context, or
/// the chain has no source-level anchor).
fn macro_rules_inline_target(
    ctxt: rustc_span::SyntaxContext,
    tainted_names: &HashSet<String>,
) -> Option<Span> {
    let mut current = ctxt.outer_expn();
    let root_expn = rustc_span::hygiene::ExpnId::root();
    let mut found_at_cs: Option<Span> = None;

    for _ in 0..64 {
        if current == root_expn {
            return found_at_cs;
        }
        let data = current.expn_data();
        let cs = data.call_site;
        let cs_at_root = !cs.is_dummy() && cs.ctxt().is_root();
        if let ExpnKind::Macro(MacroKind::Bang, name) = data.kind {
            if tainted_names.contains(name.as_str()) && cs_at_root {
                // Track the OUTERMOST tainted invocation in the chain.
                // Keep walking — a higher tainted macro that also has a
                // root call_site should win, since replacing higher up
                // captures the broader expansion.
                found_at_cs = Some(cs);
            }
        }
        let next = cs.ctxt().outer_expn();
        if next == current {
            return found_at_cs;
        }
        current = next;
    }
    found_at_cs
}

fn record_strip(
    sm: &rustc_span::source_map::SourceMap,
    span: Span,
    edits: &mut BTreeMap<u32, FileEdits>,
    seen: &mut HashSet<(u32, u32)>,
) {
    let lo = span.lo().0;
    let hi = span.hi().0;
    if !seen.insert((lo, hi)) {
        return;
    }
    let file = sm.lookup_source_file(span.lo());
    let start = file.start_pos.0;
    let local_lo = (lo - start) as usize;
    let local_hi = (hi - start) as usize;
    edits
        .entry(start)
        .or_default()
        .strips
        .push((local_lo, local_hi));
}

/// Apply a `FileEdits` plan to a file's original content. Strips are
/// expanded to swallow surrounding comma+whitespace so `#[derive(...)]`
/// stays well-formed. Strips and replacements are merged into a single
/// sorted edit list (replacements supplying their own text) and applied
/// back-to-front so byte offsets stay valid. Appended items go onto the
/// end behind a marker comment.
fn apply_edits(original: &str, edits: &FileEdits) -> String {
    let bytes = original.as_bytes();

    // Build a unified edit list: (lo, hi, replacement_text).
    let mut all_edits: Vec<(usize, usize, String)> = Vec::new();

    // Strips → expand for trailing/leading comma, replace with "".
    let mut sorted_strips = edits.strips.clone();
    sorted_strips.sort_unstable();
    sorted_strips.dedup();
    for (lo, hi) in sorted_strips {
        // Defensive: skip ranges that don't fit this file's bytes. Helper-
        // attr strips collected from pre-expansion AST may carry spans that
        // belong to a different file or were renormalized by rustc.
        if lo > bytes.len() || hi > bytes.len() || lo > hi {
            continue;
        }
        // Only do separator-swallowing for derive-path strips (e.g. the
        // `Error` token inside `#[derive(Debug, Error)]`). For full
        // attribute strips (`#[error(...)]`, `#[serde(...)]`), the span
        // already covers the entire `#[...]` brackets — swallowing
        // surrounding commas would chew into a sibling field's
        // terminator and break the surrounding struct. Detect by
        // whether the strip starts with `#`.
        let is_full_attr = lo < bytes.len() && bytes[lo] == b'#';
        let (new_lo, new_hi) = if is_full_attr {
            (lo, hi)
        } else {
            swallow_strip_separators(bytes, lo, hi)
        };
        all_edits.push((new_lo, new_hi, String::new()));
    }
    for (lo, hi, text) in &edits.replacements {
        // Skip out-of-range edits defensively — some helper attrs in
        // pre-expansion AST can have spans that land outside the file we
        // dumped (e.g. expansion-introduced attrs). Better to skip than
        // panic mid-rewrite.
        if *lo > bytes.len() || *hi > bytes.len() || lo > hi {
            continue;
        }
        // For item-position Bang/Attr macros, the source typically has a
        // trailing `;` after the call (`make_const!(...);`). The expansion
        // text already terminates the produced item — emitting both yields
        // an extra `;` at item position which is a parse error. If the
        // replacement looks like a complete item (ends with `}` or `;`)
        // and the next source byte is `;`, swallow it.
        let mut end = *hi;
        let last = text.trim_end().chars().last();
        let looks_complete = matches!(last, Some('}') | Some(';'));
        if looks_complete && end < bytes.len() && bytes[end] == b';' {
            end += 1;
        }
        all_edits.push((*lo, end, text.clone()));
    }
    all_edits.sort_by_key(|e| e.0);

    // Resolve overlaps. Two adjacent derive-path strips both try to swallow
    // the comma between them — left as-is, the second strip would be dropped
    // and the source would keep one of the two paths. Merge overlapping
    // strip-only edits into a single contiguous strip; for content overlaps
    // (a strip falling inside a Bang/Attr replacement, or vice versa) keep
    // the first, drop the second.
    let mut deduped: Vec<(usize, usize, String)> = Vec::with_capacity(all_edits.len());
    for (lo, hi, text) in all_edits {
        if let Some(last) = deduped.last_mut() {
            if lo < last.1 {
                let both_strips = last.2.is_empty() && text.is_empty();
                if both_strips {
                    last.1 = last.1.max(hi);
                } else if last.2.is_empty() {
                    // Existing was a strip, new is content — prefer content.
                    last.1 = last.1.max(hi);
                    last.2 = text;
                }
                // else: existing has content, drop new.
                continue;
            }
        }
        deduped.push((lo, hi, text));
    }

    let mut out = original.to_string();
    for (lo, hi, text) in deduped.into_iter().rev() {
        out.replace_range(lo..hi, &text);
    }

    if !edits.appends.is_empty() {
        out.push_str(
            "\n// --- generated by cargo-flatten-expand (third-party proc-macros) ---\n",
        );
        for t in &edits.appends {
            out.push_str(t);
            out.push('\n');
        }
    }
    rewrite_unstable_panic_calls(&out)
}

/// Pretty-printed post-expansion code uses unstable internal calls
/// for `assert!()` / `unimplemented!()` / `unreachable!()` / `panic!()`
/// expansions:
///
///   - `assert!(cond)` → `if !cond { ::core::panicking::panic("…"); }`
///   - `panic!("msg")` → `::std::rt::begin_panic("msg")`
///
/// Both internals are gated behind `#![feature(...)]` flags
/// (`panic_internals` and `libstd_sys_internals` respectively), which
/// the downstream flat output won't have. Rewrite each to the stable
/// `panic!("…")` macro form so the flat file compiles on stable rustc.
///
/// Only handles the single-string-literal form (the most common
/// expansion shape). Multi-arg `panicking::panic_fmt(...)` etc. would
/// need a richer rewriter but rarely show up in third-party crates.
fn rewrite_unstable_panic_calls(src: &str) -> String {
    const NEEDLES: &[&str] = &[
        "::core::panicking::panic(",
        "::std::rt::begin_panic(",
    ];
    if !NEEDLES.iter().any(|n| src.contains(n)) {
        return src.to_string();
    }
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        let matched_needle = NEEDLES
            .iter()
            .find(|n| bytes[i..].starts_with(n.as_bytes()));
        if let Some(needle) = matched_needle {
            let arg_start = i + needle.len();
            // Skip whitespace.
            let mut p = arg_start;
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            if p < bytes.len() && bytes[p] == b'"' {
                // Walk a Rust string literal, tracking escapes.
                let lit_start = p;
                p += 1;
                let mut closed = false;
                while p < bytes.len() {
                    match bytes[p] {
                        b'\\' if p + 1 < bytes.len() => {
                            p += 2;
                        }
                        b'"' => {
                            p += 1;
                            closed = true;
                            break;
                        }
                        _ => p += 1,
                    }
                }
                if closed {
                    let lit_end = p;
                    // Skip trailing whitespace before the closing paren.
                    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                        p += 1;
                    }
                    if p < bytes.len() && bytes[p] == b')' {
                        let lit = &src[lit_start..lit_end];
                        out.push_str("panic!(");
                        out.push_str(lit);
                        out.push(')');
                        i = p + 1;
                        continue;
                    }
                }
            }
            // Pattern didn't match — emit verbatim and resume scanning
            // one len_utf8 ahead.
            let c = src[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let c = src[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Expand a strip range to also swallow a trailing or leading `, ` so the
/// containing `#[derive(...)]` list stays syntactically well-formed after
/// the path token is removed.
fn swallow_strip_separators(bytes: &[u8], lo: usize, hi: usize) -> (usize, usize) {
    let mut new_lo = lo;
    let mut new_hi = hi;
    let mut probe = new_hi;
    while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
        probe += 1;
    }
    if probe < bytes.len() && bytes[probe] == b',' {
        new_hi = probe + 1;
    } else {
        let mut probe = new_lo;
        while probe > 0 && bytes[probe - 1].is_ascii_whitespace() {
            probe -= 1;
        }
        if probe > 0 && bytes[probe - 1] == b',' {
            new_lo = probe - 1;
        }
    }
    (new_lo, new_hi)
}

/// Standalone-mode convenience: rewrite a single in-memory source string.
fn rewrite_source(
    tcx: TyCtxt<'_>,
    original: &str,
    expanded: &rustc_ast::Crate,
    proc_macro_crates: &BTreeSet<String>,
    pre_info: &PreExpansionInfo,
) -> String {
    let sm = tcx.sess.source_map();
    let edits = collect_edits(tcx, expanded, proc_macro_crates, pre_info);
    let file = sm
        .files()
        .iter()
        .find(|f| matches!(&f.name, FileName::Custom(s) if s == "input.rs"))
        .expect("input.rs source file")
        .clone();
    let empty = FileEdits::default();
    let plan = edits.get(&file.start_pos.0).unwrap_or(&empty);
    apply_edits(original, plan)
}

struct ExpnReporter<'tcx> {
    tcx: TyCtxt<'tcx>,
    seen: std::collections::HashSet<(u32, u32)>,
}

impl<'tcx, 'ast> Visitor<'ast> for ExpnReporter<'tcx> {
    fn visit_item(&mut self, item: &'ast rustc_ast::Item) {
        report_span(self.tcx, &mut self.seen, item.span);
        visit::walk_item(self, item);
    }
    fn visit_expr(&mut self, expr: &'ast rustc_ast::Expr) {
        report_span(self.tcx, &mut self.seen, expr.span);
        visit::walk_expr(self, expr);
    }
    fn visit_stmt(&mut self, stmt: &'ast rustc_ast::Stmt) {
        report_span(self.tcx, &mut self.seen, stmt.span);
        visit::walk_stmt(self, stmt);
    }
}

#[derive(Debug)]
enum Origin {
    Stdlib,
    ThirdParty,
    BuiltIn,
    Local,
}

/// Walk the pre-expansion AST and produce two pieces of information that
/// can only be derived from the *original* (un-expanded) source:
///
/// 1. `attr_to_item_span`: maps `#[attr]` span lo → full annotated-item
///    span. Used so Attr proc-macros replace the WHOLE host item rather
///    than just the attribute (whose call_site span only covers `#[name]`).
///
/// 2. `helper_attr_strips`: non-builtin sibling attributes on items that
///    carry at least one non-stdlib `#[derive(...)]`. These are *very
///    likely* proc-macro helper attrs (e.g. `#[serde(rename = "...")]`)
///    that get stranded — and rejected by stable rustc — once their
///    owning derive is stripped. Heuristic: any single-segment attr whose
///    name isn't in the known-builtins set on an item with a non-stdlib
///    derive. Over-stripping risk is small in practice.
struct PreExpansionInfo {
    attr_to_item_span: BTreeMap<u32, (u32, u32)>,
    helper_attr_strips: Vec<(u32, u32)>,
}

fn collect_pre_expansion_info(krate: &rustc_ast::Crate) -> PreExpansionInfo {
    let mut info = PreExpansionInfo {
        attr_to_item_span: BTreeMap::new(),
        helper_attr_strips: Vec::new(),
    };
    walk_items_pre_expansion(&krate.items, &mut info);
    info
}

const STDLIB_DERIVES: &[&str] = &[
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "Hash",
    "Default",
    "Ord",
    "PartialOrd",
];

const BUILTIN_ATTRS: &[&str] = &[
    "derive",
    "cfg",
    "cfg_attr",
    "allow",
    "warn",
    "deny",
    "forbid",
    "expect",
    "must_use",
    "inline",
    "repr",
    "doc",
    "non_exhaustive",
    "automatically_derived",
    "no_mangle",
    "link",
    "link_name",
    "link_section",
    "used",
    "no_main",
    "track_caller",
    "panic_handler",
    "global_allocator",
    "target_feature",
    "thread_local",
    "test",
    "ignore",
    "should_panic",
    "bench",
    "no_std",
    "feature",
    "macro_use",
    "macro_export",
    "path",
    "no_implicit_prelude",
    "prelude_import",
    "rustfmt",
    "clippy",
    "rustc_allow_const_fn_unstable",
    // Marker for the chosen variant of `#[derive(Default)]` on enums.
    // Built into rustc; not a proc-macro helper. Stripping it breaks
    // the auto-generated Default impl.
    "default",
    // Crate-level metadata attributes typically attached to root items.
    "deprecated",
    "stable",
    "unstable",
    // Test/benchmark frameworks.
    "test_case",
];

fn item_has_nonstdlib_derive(attrs: &[rustc_ast::Attribute]) -> bool {
    use rustc_ast::AttrKind;
    for a in attrs {
        if !matches!(a.kind, AttrKind::Normal(_)) {
            continue;
        }
        if !a.has_name(rustc_span::sym::derive) {
            continue;
        }
        let Some(list) = a.meta_item_list() else {
            continue;
        };
        for inner in list {
            if let rustc_ast::MetaItemInner::MetaItem(mi) = inner {
                if let Some(ident) = mi.ident() {
                    if !STDLIB_DERIVES.contains(&ident.name.as_str()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn collect_helper_attr_strips(item_attrs: &[rustc_ast::Attribute]) -> Vec<(u32, u32)> {
    use rustc_ast::AttrKind;
    let mut out = Vec::new();
    for a in item_attrs {
        let AttrKind::Normal(normal) = &a.kind else {
            continue;
        };
        let Some(first) = normal.item.path.segments.first() else {
            continue;
        };
        let name = first.ident.name.as_str();
        if name == "derive" || BUILTIN_ATTRS.contains(&name) {
            continue;
        }
        // Multi-segment paths (e.g. `tool::lint`) are tool attributes —
        // leave them alone.
        if normal.item.path.segments.len() > 1 {
            continue;
        }
        out.push((a.span.lo().0, a.span.hi().0));
    }
    out
}

fn walk_items_pre_expansion(items: &[Box<rustc_ast::Item>], info: &mut PreExpansionInfo) {
    for item in items {
        record_attrs_for_item(item.span.hi().0, &item.attrs, info);
        if item_has_nonstdlib_derive(&item.attrs) {
            info.helper_attr_strips
                .extend(collect_helper_attr_strips(&item.attrs));
            collect_field_helper_strips(&item.kind, &mut info.helper_attr_strips);
        }
        match &item.kind {
            rustc_ast::ItemKind::Mod(_, _, rustc_ast::ModKind::Loaded(inner, _, _)) => {
                walk_items_pre_expansion(inner, info);
            }
            rustc_ast::ItemKind::Impl(imp) => {
                for assoc in &imp.items {
                    record_attrs_for_item(assoc.span.hi().0, &assoc.attrs, info);
                }
            }
            rustc_ast::ItemKind::Trait(t) => {
                for assoc in &t.items {
                    record_attrs_for_item(assoc.span.hi().0, &assoc.attrs, info);
                }
            }
            _ => {}
        }
    }
}

/// Recursively walk items (descending into mods/impls/traits) and collect
/// the names of `macro_rules!` definitions whose bodies textually
/// reference a proc-macro crate. Runs over the POST-expansion AST so
/// macro_rules defs inside submodules (like simba's `mod scalar; mod
/// complex;`) are reachable.
fn scan_for_tainted_macros(
    items: &[Box<rustc_ast::Item>],
    proc_macro_crates: &BTreeSet<String>,
    out: &mut HashSet<String>,
) {
    for item in items {
        if let rustc_ast::ItemKind::MacroDef(name, mac_def) = &item.kind {
            if macro_def_uses_proc_macro(&mac_def.body, proc_macro_crates) {
                out.insert(name.to_string());
            }
        }
        match &item.kind {
            rustc_ast::ItemKind::Mod(_, _, rustc_ast::ModKind::Loaded(inner, _, _)) => {
                scan_for_tainted_macros(inner, proc_macro_crates, out);
            }
            // macro_rules! defs inside impl/trait bodies are vanishingly
            // rare; skipped to keep the walker shallow.
            _ => {}
        }
    }
}

/// Walk a macro_rules definition's token tree and check whether any path
/// segment is a proc-macro crate name. This catches patterns like
/// `paste::item! { ... }` or `quote::quote! { ... }` inside the body.
fn macro_def_uses_proc_macro(
    body: &rustc_ast::DelimArgs,
    proc_macro_crates: &BTreeSet<String>,
) -> bool {
    use rustc_ast::tokenstream::{TokenStream, TokenTree};
    fn scan(stream: &TokenStream, pm: &BTreeSet<String>) -> bool {
        for tt in stream.iter() {
            match tt {
                TokenTree::Token(tok, _) => {
                    if let Some((name, _is_raw)) = tok.ident() {
                        if pm.contains(name.name.as_str()) {
                            return true;
                        }
                    }
                }
                TokenTree::Delimited(_, _, _, inner) => {
                    if scan(inner, pm) {
                        return true;
                    }
                }
            }
        }
        false
    }
    scan(&body.tokens, proc_macro_crates)
}

fn record_attrs_for_item(
    item_hi: u32,
    attrs: &[rustc_ast::Attribute],
    info: &mut PreExpansionInfo,
) {
    for attr in attrs {
        if matches!(&attr.kind, rustc_ast::AttrKind::Normal(_)) {
            info.attr_to_item_span
                .insert(attr.span.lo().0, (attr.span.lo().0, item_hi));
        }
    }
}

fn collect_field_helper_strips(
    kind: &rustc_ast::ItemKind,
    strips: &mut Vec<(u32, u32)>,
) {
    use rustc_ast::ItemKind;
    match kind {
        ItemKind::Struct(_, _, data) | ItemKind::Union(_, _, data) => {
            walk_variant_data_helpers(data, strips);
        }
        ItemKind::Enum(_, _, en) => {
            for v in &en.variants {
                strips.extend(collect_helper_attr_strips(&v.attrs));
                walk_variant_data_helpers(&v.data, strips);
            }
        }
        _ => {}
    }
}

fn walk_variant_data_helpers(data: &rustc_ast::VariantData, strips: &mut Vec<(u32, u32)>) {
    use rustc_ast::VariantData;
    let fields = match data {
        VariantData::Struct { fields, .. } => fields,
        VariantData::Tuple(fields, _) => fields,
        VariantData::Unit(_) => return,
    };
    for f in fields {
        strips.extend(collect_helper_attr_strips(&f.attrs));
    }
}

fn first_segment_matches(
    use_tree: &rustc_ast::UseTree,
    crate_names: &BTreeSet<String>,
) -> bool {
    let Some(first) = use_tree.prefix.segments.first() else {
        return false;
    };
    crate_names.contains(first.ident.name.as_str())
}

fn classify_origin(tcx: TyCtxt<'_>, data: &rustc_span::ExpnData) -> (Origin, String) {
    let Some(def_id) = data.macro_def_id else {
        return (Origin::BuiltIn, "<no def_id>".to_string());
    };
    let krate = def_id.krate;
    let name = tcx.crate_name(krate);
    let name_str = name.to_string();
    let origin = if krate == rustc_span::def_id::LOCAL_CRATE {
        Origin::Local
    } else if matches!(name_str.as_str(), "core" | "std" | "alloc" | "proc_macro") {
        Origin::Stdlib
    } else {
        Origin::ThirdParty
    };
    (origin, name_str)
}

fn report_span(
    tcx: TyCtxt<'_>,
    seen: &mut std::collections::HashSet<(u32, u32)>,
    span: Span,
) {
    let ctxt = span.ctxt();
    if ctxt.is_root() {
        return;
    }
    let outer = ctxt.outer_expn();
    let id = (outer.krate.as_u32(), outer.local_id.as_u32());
    if !seen.insert(id) {
        return;
    }
    let data = outer.expn_data();
    let kind_label = match &data.kind {
        ExpnKind::Root => "Root".to_string(),
        ExpnKind::Macro(MacroKind::Bang, name) => format!("Bang `{name}!()`"),
        ExpnKind::Macro(MacroKind::Attr, name) => format!("Attr `#[{name}]`"),
        ExpnKind::Macro(MacroKind::Derive, name) => format!("Derive `{name}`"),
        ExpnKind::AstPass(p) => format!("AstPass {p:?}"),
        ExpnKind::Desugaring(d) => format!("Desugaring {d:?}"),
    };
    let (origin, krate_name) = classify_origin(tcx, &data);
    let call_site = span_to_string(tcx, data.call_site);
    println!(
        "  expansion {id:?}: {kind_label} [origin={origin:?} krate={krate_name}] @ {call_site}"
    );
}

fn span_to_string(tcx: TyCtxt<'_>, span: Span) -> String {
    if span.is_dummy() {
        return "<dummy>".to_string();
    }
    let sm = tcx.sess.source_map();
    let lo = sm.lookup_char_pos(span.lo());
    let hi = sm.lookup_char_pos(span.hi());
    format!(
        "{}:{}:{}-{}:{}",
        format_filename(&lo.file.name),
        lo.line,
        lo.col.0 + 1,
        hi.line,
        hi.col.0 + 1,
    )
}

fn format_filename(name: &FileName) -> String {
    match name {
        FileName::Real(rfn) => rfn
            .local_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("{rfn:?}")),
        FileName::Custom(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

static USING_INTERNAL_FEATURES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

#[cfg(test)]
mod find_item_end_tests {
    use super::find_item_end;

    /// Helper: pass `from` as the byte just after the simulated
    /// `#[attr]`. The host item starts at `from`. Returns whether
    /// `find_item_end` found the expected end byte.
    fn check(src: &str, from: usize, expected_end: usize) {
        match find_item_end(src, from) {
            Some(actual) => assert_eq!(
                actual, expected_end,
                "src starting at {from}: expected end {expected_end}, got {actual}\n\
                 captured: {:?}",
                &src[from..actual.min(src.len())]
            ),
            None => panic!(
                "find_item_end returned None; expected {expected_end}\n\
                 src after `from`: {:?}",
                &src[from..]
            ),
        }
    }

    #[test]
    fn simple_braced_fn() {
        let src = "fn f() { let _ = 1; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn unit_struct() {
        let src = "pub struct Foo;\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn type_alias() {
        let src = "pub type T = u32;\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_array_in_signature() {
        // `[u32; 3]` contains a `;` at depth 1; must not be the terminator.
        let src = "fn f(a: [u32; 3]) -> u8 { a[0] as u8 }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_brace_in_char_literal() {
        // The `}` inside the char literal must NOT be mistaken for the
        // function-body close. This was REVIEW A1.
        let src = "fn f() { let c = '}'; let _ = 1; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_quote_in_char_literal() {
        let src = "fn f() { let c = '\"'; let _ = 1; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_unicode_escape_char() {
        let src = "fn f() { let c = '\\u{007D}'; let _ = 1; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_lifetime() {
        // `'a` is a lifetime, NOT a char literal. Must not consume the
        // following `>` as part of a literal.
        let src = "fn f<'a>(x: &'a u32) -> u32 { *x }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_static_lifetime() {
        let src = "fn f() -> &'static str { \"x\" }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_raw_string_containing_braces() {
        // `r#"…"#` containing `}` and `;` must not break the scanner.
        // This was REVIEW A2.
        let src = "fn f() { let s = r#\"contains } and ;\"#; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_raw_string_containing_inner_quotes() {
        // The harder case: raw string contains `"` followed by `}`.
        // A naive `"…"` walker would close at the inner `"`.
        let src = "fn f() { let s = r#\"x\"; }\"#; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_raw_string_multi_hash() {
        let src = "fn f() { let s = r##\"with \"# inside\"##; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_byte_string() {
        let src = "fn f() { let b = b\"hello\"; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_byte_string_containing_brace() {
        let src = "fn f() { let b = b\"contains } in bytes\"; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_c_string() {
        let src = "fn f() { let c = c\"hello\"; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_byte_char_literal() {
        let src = "fn f() { let b = b'}'; }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_nested_block_comment() {
        let src = "fn f() { /* outer /* inner */ still in comment */ }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn fn_with_line_comment_having_brace() {
        let src = "fn f() { // closing } in comment\n }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }

    #[test]
    fn struct_with_brace_in_string_field() {
        let src = "struct S { name: &'static str }\nrest";
        check(src, 0, src.find("\nrest").unwrap());
    }
}
