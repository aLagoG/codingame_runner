//! Dependency vendoring — read from cargo's on-disk source cache and inline
//! vendorable deps into the flat output. See `VENDORING.md` for the full
//! design and `EXTERNAL.md` / `EXPAND.md` for the `--external*` and
//! `--expand*` extensions.
//!
//! Two public entry points:
//!
//! - [`report`] — read-only classification of the dep graph as
//!   vendorable / unvendorable / warns. Drives `--vendor-report`.
//! - [`vendor_package`] — full pipeline: classify, BFS the graph
//!   (cutting `--external` deps), optionally pre-expand proc-macros
//!   via `flatten_expand` (`--expand` / `--expand-deep`),
//!   rewrite each vendored dep's source for inlining, and emit a
//!   [`VendoredPackage`] with the user's flattened source plus per-dep
//!   blocks ready to wrap in `pub mod NAME { ... }`.
//!
//! The cfg expression evaluator and `cfg_if!` expander used by
//! vendoring live in [`crate::cfg`].

use cargo_metadata::{DependencyKind, MetadataCommand, Package, PackageId, TargetKind};
use proc_macro2::TokenTree;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::error::{FlattenError, Result};
use crate::source_file::{ParseOptions, SourceFile};
use crate::{PackageType, TargetSelector, parse_target};

// ---------------------------------------------------------------------------
// Phase V0 — classification and reporting
// ---------------------------------------------------------------------------

/// Outcome of inspecting one transitive dep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Could be inlined into the flat output today.
    Vendorable,
    /// Could be vendored but with caveats (e.g. needs nightly).
    Warn(Vec<String>),
    /// Cannot be vendored — listed reasons describe why.
    Unvendorable(Vec<String>),
}

impl Classification {
    pub fn is_vendorable(&self) -> bool {
        matches!(self, Self::Vendorable | Self::Warn(_))
    }
}

/// One transitive dep, classified.
#[derive(Debug, Clone)]
pub struct DepEntry {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub classification: Classification,
    /// Resolved feature set (from `cargo metadata`'s resolver). Used by V2's
    /// cfg-feature evaluation pass.
    pub features: HashSet<String>,
    /// Edition declared in this dep's manifest.
    pub edition: cargo_metadata::Edition,
    /// Names of this dep's normal-kind direct dependencies, taken from
    /// the resolver's edge list. Used by the BFS-with-cut-points algorithm
    /// in `vendor_package` (see EXTERNAL.md).
    pub normal_deps: Vec<String>,
    /// Cfgs emitted by the dep's `build.rs` (`cargo:rustc-cfg=NAME`
    /// directives). Captured by spawning `cargo check
    /// --message-format=json` on the user crate and parsing the
    /// `BuildScriptExecuted` messages. The cfg evaluator treats these
    /// as additional True predicates alongside the resolved feature
    /// set. Empty for deps without a build script.
    pub build_cfgs: Vec<String>,
    /// The dep's build-script `OUT_DIR` (the actual on-disk path), for
    /// resolving `include!(concat!(env!("OUT_DIR"), "/foo.rs"))`-style
    /// macros. Captured from the `out_dir` field of cargo's
    /// `build-script-executed` JSON message. None for deps without a
    /// build script.
    pub out_dir: Option<PathBuf>,
}

/// Why a dependency ended up in the `external` list of a [`VendoredPackage`].
/// Lets the banner explain to the user what they need to do for each one.
#[derive(Debug, Clone)]
pub enum ExternalReason {
    /// Manifest-level unvendorable: proc-macro / build script / native lib.
    Unvendorable(Vec<String>),
    /// Source-level dealbreaker discovered while attempting to vendor.
    VendorFailed(String),
    /// User passed `--external NAME` for this crate.
    UserExcluded,
    /// Cut from the dep walk by a user-excluded ancestor, but still
    /// referenced by at least one vendored dep — user must list it in
    /// their Cargo.toml so the flat file's `use NAME::...` resolves.
    /// `because` names the vendored crates that reference it.
    Required { because: Vec<String> },
}

/// Output of [`report`].
#[derive(Debug, Clone)]
pub struct VendorReport {
    pub root_name: String,
    pub root_version: String,
    /// Sorted by crate name.
    pub deps: Vec<DepEntry>,
}

impl VendorReport {
    pub fn vendorable_count(&self) -> usize {
        self.deps
            .iter()
            .filter(|d| d.classification.is_vendorable())
            .count()
    }

    pub fn unvendorable_count(&self) -> usize {
        self.deps.len() - self.vendorable_count()
    }
}

/// Build a vendor report for the crate rooted at `crate_root`.
pub fn report(crate_root: impl AsRef<Path>) -> Result<VendorReport> {
    let crate_root = crate_root.as_ref();
    let manifest_path = crate_root.join("Cargo.toml");
    if !manifest_path.is_file() {
        return Err(FlattenError::other(format!(
            "No Cargo.toml at `{}` — vendoring requires a manifest",
            manifest_path.display()
        )));
    }

    let metadata = MetadataCommand::new()
        .manifest_path(&manifest_path)
        .exec()
        .map_err(|e| FlattenError::other(format!("cargo metadata failed: {e}")))?;

    // Run `cargo check --message-format=json` to capture build-script
    // outputs (`cargo:rustc-cfg=...` lines AND OUT_DIR for include!()
    // resolution). Lets us evaluate cfgs like
    // `#[cfg(error_generic_member_access)]` (thiserror) or
    // `#[cfg(has_total_cmp)]` (num-traits), and inline build-script-
    // generated source via `include!(concat!(env!("OUT_DIR"), …))`.
    let build_outputs = collect_build_script_outputs(&manifest_path).unwrap_or_default();

    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or_else(|| FlattenError::other("cargo metadata produced no resolve graph"))?;
    let root = resolve
        .root
        .as_ref()
        .ok_or_else(|| FlattenError::other("cargo metadata produced no resolve root"))?;

    let mut reachable: HashSet<PackageId> = HashSet::new();
    let mut stack: Vec<PackageId> = vec![root.clone()];
    while let Some(id) = stack.pop() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        let Some(node) = resolve.nodes.iter().find(|n| n.id == id) else {
            continue;
        };
        for dep in &node.deps {
            let is_normal = dep
                .dep_kinds
                .iter()
                .any(|k| k.kind == DependencyKind::Normal);
            if is_normal {
                stack.push(dep.pkg.clone());
            }
        }
    }
    reachable.remove(root);

    let mut entries: Vec<DepEntry> = reachable
        .into_iter()
        .filter_map(|id| {
            let pkg = metadata.packages.iter().find(|p| p.id == id.clone())?;
            let node = resolve.nodes.iter().find(|n| n.id == id);
            let features: HashSet<String> = node
                .map(|n| n.features.iter().map(|f| f.to_string()).collect())
                .unwrap_or_default();
            let normal_deps: Vec<String> = node
                .map(|n| {
                    n.deps
                        .iter()
                        .filter(|d| d.dep_kinds.iter().any(|k| k.kind == DependencyKind::Normal))
                        .filter_map(|d| {
                            metadata
                                .packages
                                .iter()
                                .find(|p| p.id == d.pkg)
                                .map(|p| p.name.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let bso = build_outputs.get(&pkg.id.to_string()).cloned();
            let classification = classify(pkg, bso.as_ref());
            let bso = bso.unwrap_or_default();
            Some(DepEntry {
                name: pkg.name.to_string(),
                version: pkg.version.to_string(),
                manifest_path: PathBuf::from(pkg.manifest_path.as_std_path()),
                classification,
                features,
                edition: pkg.edition,
                normal_deps,
                build_cfgs: bso.cfgs,
                out_dir: bso.out_dir,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));

    let root_pkg = metadata
        .packages
        .iter()
        .find(|p| p.id == *root)
        .ok_or_else(|| FlattenError::other("resolve root not in packages list"))?;

    Ok(VendorReport {
        root_name: root_pkg.name.to_string(),
        root_version: root_pkg.version.to_string(),
        deps: entries,
    })
}

/// Classify one package per the rules in VENDORING.md (manifest-level only).
///
/// Decide whether `pkg` can be vendored, given what we observed when
/// running its build script (if any) via `cargo check`.
///
/// Holistic build-script policy (per the spec at
/// https://doc.rust-lang.org/cargo/reference/build-scripts.html):
///
/// **Safe to vendor** — build script emits ONLY:
///   - `cargo::rustc-cfg=…` (we capture + replay via cfg evaluator)
///   - `cargo::rustc-check-cfg=…` (lint-only, harmless)
///   - `cargo::rerun-if-*` (cargo-side, no compile effect)
///   - `cargo::warning=…` (cosmetic)
///   - OUT_DIR contents that the source `include!()`s (we inline)
///
/// **Hard block** — build script emits ANY of:
///   - `cargo::rustc-link-lib=…` / `rustc-link-search=…` /
///     `rustc-link-arg=…` / `rustc-flags=…` — needs a native lib at
///     the user's link time, which the flat output can't replicate.
///   - `cargo::rustc-env=…` — affects `env!()` expansions; not yet
///     supported (roadmap: capture + rewrite source `env!()` calls).
///
/// **`links = "foo"` Cargo.toml key**: vendorable IFF the build
/// script emitted no `rustc-link-*` directives. Some crates declare
/// `links` purely for cargo's uniqueness-claim purpose (rayon-core's
/// "I'm the only rayon-core" pattern); those don't actually link
/// anything and are safe to vendor.
///
/// `build_outputs` is `None` when we couldn't run the build script
/// (e.g., `cargo check` failed); in that case we conservatively
/// fall back to "has build script" being a hard block.
pub fn classify(pkg: &Package, build_outputs: Option<&BuildScriptOutput>) -> Classification {
    let mut blockers = Vec::new();

    if is_proc_macro(pkg) {
        blockers.push("proc-macro".to_string());
    }
    if has_build_script(pkg) {
        match build_outputs {
            None => {
                // No captured build-script output — conservative.
                blockers.push("has build script".to_string());
            }
            Some(bso) => {
                // Holistic check: any link-affecting directive blocks.
                if !bso.linked_libs.is_empty() {
                    blockers.push(format!(
                        "build script links native lib(s): {}",
                        bso.linked_libs.join(", ")
                    ));
                }
                if !bso.linked_args.is_empty() {
                    blockers.push("build script emits linker args".to_string());
                }
                if !bso.set_envs.is_empty() {
                    // rustc-env affects env!() expansions in source.
                    // Until we rewrite env!() calls at vendor time
                    // (roadmap), can't replay safely.
                    blockers.push(format!(
                        "build script sets compile-time env: {}",
                        bso.set_envs
                            .iter()
                            .map(|kv| kv.split('=').next().unwrap_or(kv))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                // linked_paths alone is harmless without linked_libs
                // (no -l means no link), but in practice non-empty
                // linked_paths almost always pairs with linked_libs.
                // If only linked_paths is set, it's noise from
                // overly-aggressive build scripts; allow.
            }
        }
    }
    // `links = "foo"` Cargo.toml key — hard block ONLY if the build
    // script also emits `rustc-link-lib`. Otherwise treat as cargo-
    // side uniqueness claim that has no compile-time effect.
    if let Some(lib) = &pkg.links {
        let actually_links = build_outputs
            .map(|bso| !bso.linked_libs.is_empty() || !bso.linked_args.is_empty())
            .unwrap_or(true);
        if actually_links {
            blockers.push(format!("links native lib `{lib}`"));
        }
    }

    if !blockers.is_empty() {
        return Classification::Unvendorable(blockers);
    }
    Classification::Vendorable
}


/// Names of every proc-macro crate in the dep graph (i.e. crates the
/// classifier marked as `Unvendorable("proc-macro")`). Used by the
/// `--expand` paths to populate `auto_externals` so the BFS cuts those
/// crates and they don't trigger strict-mode vendoring blockers.
fn proc_macro_dep_names(report: &VendorReport) -> HashSet<String> {
    report
        .deps
        .iter()
        .filter(|d| {
            matches!(
                &d.classification,
                Classification::Unvendorable(reasons)
                    if reasons.iter().any(|r| r == "proc-macro")
            )
        })
        .map(|d| d.name.clone())
        .collect()
}

fn is_proc_macro(pkg: &Package) -> bool {
    pkg.targets
        .iter()
        .any(|t| t.kind.iter().any(|k| matches!(k, TargetKind::ProcMacro)))
}

fn has_build_script(pkg: &Package) -> bool {
    let custom_build_target = pkg
        .targets
        .iter()
        .any(|t| t.kind.iter().any(|k| matches!(k, TargetKind::CustomBuild)));
    if custom_build_target {
        return true;
    }
    let manifest_dir = pkg.manifest_path.parent();
    manifest_dir
        .map(|d| d.join("build.rs").is_file())
        .unwrap_or(false)
}

/// Spawn `cargo check --message-format=json` on the user crate and
/// parse `BuildScriptExecuted` messages, returning a map from package
/// id to the list of `cargo:rustc-cfg=NAME` directives the build
/// script emitted. Failures are non-fatal — just return empty.
///
/// This runs the user's actual build pipeline (which is slow on first
/// invocation), but cargo caches the build script results, so
/// subsequent invocations are fast.
/// Per-package output of one `cargo check` build-script run.
///
/// Captured from cargo's `build-script-executed` JSON message. All
/// fields populated by the build script via `cargo::*` directives.
/// Used by [`classify`] to decide whether a build-script-bearing
/// dep can be vendored: vendorable iff the build script's effects
/// are limited to "safe" outputs (cfgs and OUT_DIR include) — any
/// link-related effect (link_libs, link_paths, link_args) means the
/// dep needs the user's link-time environment to provide a native
/// library, which we can't replicate in the flat output.
#[derive(Default, Debug, Clone)]
pub struct BuildScriptOutput {
    /// `cargo::rustc-cfg=…` directives. Captured + replayed by our
    /// cfg evaluator.
    pub cfgs: Vec<String>,
    /// The build script's `OUT_DIR`, where any
    /// `include!(concat!(env!("OUT_DIR"), "/foo.rs"))` content lives.
    pub out_dir: Option<PathBuf>,
    /// `cargo::rustc-link-lib=…` directives — names of native libs
    /// the build script tells the linker to link in. Non-empty
    /// means "this dep wants to link a native library at user's
    /// link time". Hard block for vendoring.
    pub linked_libs: Vec<String>,
    /// `cargo::rustc-link-search=…` paths.
    pub linked_paths: Vec<String>,
    /// `cargo::rustc-link-arg=…` and friends — raw flags forwarded
    /// to the linker. Treated as link-related: hard block.
    pub linked_args: Vec<String>,
    /// `cargo::rustc-env=…` (`KEY=VALUE` strings). Affects
    /// `env!()` / `option_env!()` expansions in the dep's source.
    /// Currently treated as a soft block until the env! source
    /// rewrite is implemented (roadmap item).
    pub set_envs: Vec<String>,
}

fn collect_build_script_outputs(
    manifest_path: &Path,
) -> Option<HashMap<String, BuildScriptOutput>> {
    let output = std::process::Command::new("cargo")
        .arg("check")
        .arg("--message-format=json")
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut by_pkg: HashMap<String, BuildScriptOutput> = HashMap::new();
    for line in std::str::from_utf8(&output.stdout).ok()?.lines() {
        // Lazy JSON parsing: just look for the keys we care about.
        // The full message-format=json schema is stable per cargo's
        // docs but we don't need every field.
        if !line.contains("\"reason\":\"build-script-executed\"") {
            continue;
        }
        let Some(pkg_id) = json_extract_string(line, "\"package_id\":\"") else {
            continue;
        };
        let mut cfgs = json_extract_string_array(line, "\"cfgs\":[");
        // Filter out cfgs that gate unstable rustc features. When the
        // build script probes a nightly toolchain (flatten itself
        // requires nightly for --expand-deep), it may detect support
        // for nightly-only features and emit cfgs that flip on code
        // requiring `#![feature(...)]` to compile. The flat output gets
        // compiled by downstream stable rustc, where those gates fail.
        // Hard-coded denylist of well-known offenders.
        cfgs.retain(|c| !is_unstable_feature_cfg(c));
        let out_dir = json_extract_string(line, "\"out_dir\":\"").map(PathBuf::from);
        let linked_libs = json_extract_string_array(line, "\"linked_libs\":[");
        let linked_paths = json_extract_string_array(line, "\"linked_paths\":[");
        // cargo's JSON uses `linked_libs` and `linked_paths` for
        // `rustc-link-lib` and `rustc-link-search` respectively; the
        // various `rustc-link-arg*` directives flow through cargo to
        // rustc but the JSON only exposes them via an "env" stream
        // for some shapes. Capture both `linked_args` (where present)
        // and `env` for completeness.
        let linked_args = json_extract_string_array(line, "\"linked_args\":[");
        let set_envs = json_extract_string_array(line, "\"env\":[");
        by_pkg.insert(
            pkg_id,
            BuildScriptOutput {
                cfgs,
                out_dir,
                linked_libs,
                linked_paths,
                linked_args,
                set_envs,
            },
        );
    }
    Some(by_pkg)
}

/// Cfg names known to be set by build scripts as "I detected the compiler
/// supports unstable feature X" and that gate code requiring
/// `#![feature(X)]` to compile. The flat output won't have those feature
/// attributes, so propagating these as True-evaluated cfgs would inline
/// code that downstream stable rustc rejects.
fn is_unstable_feature_cfg(cfg: &str) -> bool {
    matches!(
        cfg,
        "error_generic_member_access"
            | "thiserror_nightly_testing"
            | "tokio_unstable"
            | "rustc_attrs"
            | "specialization"
    )
}

/// Extract a JSON string value following a known key prefix. Returns
/// the string contents (without the surrounding quotes). Returns None
/// if the key isn't found.
fn json_extract_string(line: &str, key_prefix: &str) -> Option<String> {
    let start = line.find(key_prefix)? + key_prefix.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract a JSON array of strings following a known key prefix.
/// Returns the array elements (without surrounding quotes). Returns
/// an empty Vec if the array is empty or malformed.
fn json_extract_string_array(line: &str, key_prefix: &str) -> Vec<String> {
    let Some(start) = line.find(key_prefix) else {
        return Vec::new();
    };
    let after = &line[start + key_prefix.len()..];
    let Some(end) = after.find(']') else {
        return Vec::new();
    };
    let inner = &after[..end];
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    let mut cur = String::new();
    let mut in_str = false;
    let mut escape = false;
    for c in chars.by_ref() {
        if escape {
            cur.push(c);
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            if in_str {
                out.push(std::mem::take(&mut cur));
                in_str = false;
            } else {
                in_str = true;
            }
            continue;
        }
        if in_str {
            cur.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Phase V5 — proc-macro expansion via flatten_expand subprocess
// ---------------------------------------------------------------------------

/// Spawn `cargo build --message-format=json` and parse `compiler-artifact`
/// messages to discover the dylib path of every proc-macro dep that gets
/// built for the user crate. Returns a map from crate name to dylib path.
/// Failures are non-fatal — return empty.
fn collect_proc_macro_dylibs(manifest_path: &Path) -> Option<HashMap<String, PathBuf>> {
    let output = std::process::Command::new("cargo")
        .arg("build")
        .arg("--message-format=json")
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()
        .ok()?;
    if !output.status.success() {
        tracing::warn!(
            "cargo build (for proc-macro discovery) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    for line in std::str::from_utf8(&output.stdout).ok()?.lines() {
        if !line.contains("\"reason\":\"compiler-artifact\"") {
            continue;
        }
        // Only take artifacts whose target.kind contains "proc-macro".
        if !line.contains("\"proc-macro\"") {
            continue;
        }
        let Some(name) = json_extract_string(line, "\"name\":\"") else {
            continue;
        };
        // The `filenames` array holds compilation outputs; pick the dylib.
        for path in json_extract_string_array(line, "\"filenames\":[") {
            if path.ends_with(".dylib") || path.ends_with(".so") || path.ends_with(".dll") {
                out.insert(name.clone(), PathBuf::from(path));
                break;
            }
        }
    }
    Some(out)
}

/// Result of running `flatten_expand` over the user crate's flattened
/// source: the rewritten source plus the set of proc-macro crate names that
/// were expanded inline (and thus no longer need to be Cargo deps).
pub struct ExpandResult {
    pub rewritten_source: String,
    pub consumed_proc_macros: HashSet<String>,
}

/// Run the flatten_expand subprocess on a fully-flattened source blob.
///
/// `expander_path` is the path to the `flatten_expand` binary.
/// `dylibs` is the proc-macro crate name → dylib path map from
/// `collect_proc_macro_dylibs`. The expander sees ALL of them as `--extern`
/// and decides which ones the source actually exercises.
pub fn run_expander(
    source: &str,
    dylibs: &HashMap<String, PathBuf>,
    expander_path: &Path,
) -> Result<ExpandResult> {
    use std::process::Stdio;

    // Write source to a temp file. The expander takes a path argument.
    let tmp_dir = std::env::temp_dir();
    let tmp_in = tmp_dir.join(format!("flatten_expand-input-{}.rs", std::process::id()));
    std::fs::write(&tmp_in, source).map_err(|e| FlattenError::Io {
        context: format!("Failed to write expander input to `{}`", tmp_in.display()),
        source: e,
    })?;

    let mut cmd = std::process::Command::new(expander_path);
    cmd.arg(&tmp_in).arg("--rewrite");
    for (name, path) in dylibs {
        cmd.arg("--extern")
            .arg(format!("{name}={}", path.display()));
    }
    // Set DYLD_LIBRARY_PATH (macOS) / LD_LIBRARY_PATH (Linux) to the nightly
    // sysroot's lib dir so the expander binary can locate librustc_driver.
    if let Some(sysroot_lib) = nightly_sysroot_lib_dir() {
        let key = if cfg!(target_os = "macos") {
            "DYLD_LIBRARY_PATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        let existing = std::env::var(key).unwrap_or_default();
        let combined = if existing.is_empty() {
            sysroot_lib.display().to_string()
        } else {
            format!("{}:{}", sysroot_lib.display(), existing)
        };
        cmd.env(key, combined);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = cmd.output().map_err(|e| FlattenError::Io {
        context: format!(
            "Failed to spawn flatten_expand at `{}`",
            expander_path.display()
        ),
        source: e,
    })?;
    let _ = std::fs::remove_file(&tmp_in);
    if !output.status.success() {
        return Err(FlattenError::other(format!(
            "flatten_expand failed (exit {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let rewritten = String::from_utf8(output.stdout)
        .map_err(|e| FlattenError::other(format!("expander emitted non-UTF8 output: {e}")))?;

    // Conservative: assume every proc-macro dylib we passed via --extern
    // could have been consumed. The assembler downstream only
    // externalises crates that are actually in the dep graph, so this
    // doesn't over-strip in practice.
    let consumed: HashSet<String> = dylibs.keys().cloned().collect();
    Ok(ExpandResult {
        rewritten_source: rewritten,
        consumed_proc_macros: consumed,
    })
}

/// Run `cargo build` with `RUSTC_WRAPPER=<expander_path>` so the expander
/// captures every target crate's post-expansion source, dumping per-file
/// rewrites to `dump_dir/<crate_name>/<rel_path>`.
fn run_wrapper_expand(
    crate_root: &Path,
    targets: &BTreeSet<String>,
    proc_macro_crates: &BTreeSet<String>,
    dump_dir: &Path,
    expander_path: &Path,
) -> Result<()> {
    let manifest_path = find_manifest_path(crate_root).ok_or_else(|| {
        FlattenError::other(format!(
            "Could not find Cargo.toml under `{}`",
            crate_root.display()
        ))
    })?;

    // Normalize dashes → underscores for both env vars: cargo metadata uses
    // package names ("nalgebra-macros") but Rust crate idents use underscores
    // ("nalgebra_macros"). The wrapper compares against `tcx.crate_name`
    // (underscore form) and against AST `use` path segments (also underscore).
    let normalize = |s: &String| s.replace('-', "_");
    let targets_csv = targets.iter().map(normalize).collect::<Vec<_>>().join(",");
    let pm_csv = proc_macro_crates
        .iter()
        .map(normalize)
        .collect::<Vec<_>>()
        .join(",");
    // Use a private target dir colocated with the dump so cargo's own
    // build cache doesn't skip rustc invocations. Without this, a
    // previously-built crate would have its rlibs cached and cargo would
    // never call rustc again — meaning our wrapper never sees the
    // compilation, the dump dir stays empty, and `vendor_one_dep` falls
    // back to reading the unrewritten cargo cache for every dep.
    let private_target = dump_dir.join("__target");
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--target-dir")
        .arg(&private_target)
        .env("RUSTC_WRAPPER", expander_path)
        .env("FLATTEN_EXPAND_TARGETS", targets_csv)
        .env("FLATTEN_EXPAND_PROC_MACROS", pm_csv)
        .env("FLATTEN_EXPAND_OUTPUT", dump_dir);
    if let Some(sysroot_lib) = nightly_sysroot_lib_dir() {
        let key = if cfg!(target_os = "macos") {
            "DYLD_LIBRARY_PATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        let existing = std::env::var(key).unwrap_or_default();
        let combined = if existing.is_empty() {
            sysroot_lib.display().to_string()
        } else {
            format!("{}:{}", sysroot_lib.display(), existing)
        };
        cmd.env(key, combined);
    }

    let output = cmd.output().map_err(|e| FlattenError::Io {
        context: "Failed to spawn `cargo build` for --expand-deep".to_string(),
        source: e,
    })?;
    if !output.status.success() {
        return Err(FlattenError::other(format!(
            "cargo build (--expand-deep) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Rewrite absolute paths whose first segment names a vendored sibling
/// crate so they go through the synthetic crate root. `::clap_builder::Foo`
/// (typically emitted by proc-macro expansions like `#[derive(Parser)]`)
/// would not resolve in the flat output because `clap_builder` isn't in
/// the extern prelude — but `crate::clap_builder::Foo` does, since
/// vendored deps live there. Operates only on the user crate's source;
/// vendored deps are handled by the existing rewrite chain.
pub fn rewrite_absolute_sibling_paths(src: &str, siblings: &HashSet<String>) -> String {
    use syn::spanned::Spanned;
    use syn::visit::Visit;

    let Ok(file) = syn::parse_file(src) else {
        return src.to_string();
    };

    struct V<'a> {
        siblings: &'a HashSet<String>,
        edits: Vec<(Range<usize>, String)>,
        seen: HashSet<(usize, usize)>,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path.leading_colon.is_some()
                && let Some(first) = path.segments.first()
            {
                let name = first.ident.to_string();
                if self.siblings.contains(&name) {
                    // Replace the entire `::FIRST` range with `crate::FIRST`.
                    // We anchor on the first segment's ident span (proc-
                    // macro2's PathSep span semantics aren't reliable
                    // across token positions) and back up 2 bytes to cover
                    // the `::` prefix.
                    let ident_span = first.ident.span().byte_range();
                    if ident_span.start >= 2 {
                        let lo = ident_span.start - 2;
                        let hi = ident_span.end;
                        let key = (lo, hi);
                        if self.seen.insert(key) {
                            self.edits.push((lo..hi, format!("crate::{name}")));
                        }
                    }
                }
            }
            syn::visit::visit_path(self, path);
        }
        fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
            // Proc-macros (`#[derive(FromPrimitive)]` from num_derive,
            // `#[derive(thiserror::Error)]`, …) emit
            // `extern crate FOO as ALIAS;` blocks inside their generated
            // const-block scope to bring FOO into scope. In the flat
            // output FOO is a vendored sibling under `crate::FOO`, not
            // an externally-resolvable crate — rewrite to
            // `use crate::FOO as ALIAS;` (semantically equivalent in
            // both 2018+ and 2015 editions for the const-block context).
            let name = item.ident.to_string();
            if !self.siblings.contains(&name) {
                return;
            }
            let span = item.span().byte_range();
            let key = (span.start, span.end);
            if !self.seen.insert(key) {
                return;
            }
            let alias = item
                .rename
                .as_ref()
                .map(|(_, id)| format!(" as {id}"))
                .unwrap_or_default();
            let replacement = format!("use crate::{name}{alias};");
            self.edits.push((span, replacement));
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            // `use ::FOO::Bar` carries the leading `::` on the ItemUse,
            // not on a Path. Detect it separately and inspect the first
            // UseTree segment to find FOO.
            if item.leading_colon.is_some() {
                let first_ident = match &item.tree {
                    syn::UseTree::Path(p) => Some((p.ident.to_string(), p.ident.span())),
                    syn::UseTree::Name(n) => Some((n.ident.to_string(), n.ident.span())),
                    syn::UseTree::Rename(r) => Some((r.ident.to_string(), r.ident.span())),
                    _ => None,
                };
                if let Some((name, ident_span)) = first_ident
                    && self.siblings.contains(&name)
                {
                    let ident_range = ident_span.byte_range();
                    if ident_range.start >= 2 {
                        let lo = ident_range.start - 2;
                        let hi = ident_range.end;
                        let key = (lo, hi);
                        if self.seen.insert(key) {
                            self.edits.push((lo..hi, format!("crate::{name}")));
                        }
                    }
                }
            }
            syn::visit::visit_item_use(self, item);
        }
    }

    let mut v = V {
        siblings,
        edits: Vec::new(),
        seen: HashSet::new(),
    };
    v.visit_file(&file);
    crate::edits::apply_simple_edits(src, v.edits)
}

/// Final post-processing: drop `pub use crate::SIBLING::{...};` entries
/// that name items absent from the vendored SIBLING. Without this, when
/// the wrapper inlines a proc-macro from FOO_MACROS and strips
/// `pub use FOO_MACROS::NAME;` from FOO's source, any downstream
/// `pub use crate::FOO::NAME;` still references the now-deleted re-
/// export. The flat output is unchanged for siblings whose explicit
/// exports remain intact.
///
/// `siblings_names` maps each vendored sibling crate ident → the set
/// of top-level identifiers it exposes. Built by walking each vendored
/// dep's syn-AST root for `pub fn`/`pub struct`/`pub enum`/`pub mod`/
/// `pub use` etc. items.
///
/// One-stop assembly-time scrub. Walks the assembled flat output,
/// computes the per-sibling export set and the union of inlined-
/// proc-macro names, and runs [`scrub_unresolvable_sibling_reexports`]
/// against them. Returns the scrubbed body as bytes.
///
/// Runs unconditionally when `pkg.auto_inlined_proc_macros` is non-empty,
/// regardless of whether `--expand` was set — auto-externalisation can
/// happen even in plain `--vendor` mode (e.g. proc-macro deps detected
/// via `Classification::Unvendorable`), and downstream
/// `pub use crate::FOO::macro_name;` re-exports from those crates need
/// to be scrubbed in either case.
///
/// IO failures reading proc-macro source files are logged at warn level
/// and do not abort the scrub — the result is conservatively
/// less-aggressive scrubbing rather than an outright failure.
pub fn scrub_assembled(body: Vec<u8>, pkg: &VendoredPackage) -> Vec<u8> {
    let body_str = match String::from_utf8(body) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("scrub_assembled: assembled output is not UTF-8; skipping scrub ({e})");
            return e.into_bytes();
        }
    };
    // Dedupe duplicate `#[macro_export] macro_rules! NAME` definitions
    // across deps. The strip pass keeps macro_export on built-in-attr-
    // named macros (`warn`, `info`, …) to dodge E0659; when log AND
    // tracing are both vendored, both keep `macro_export warn` and
    // collide at user crate root with E0428. Strip macro_export from
    // every occurrence after the first (alphabetical-by-source-position).
    let body_str = dedupe_macro_export_collisions(&body_str);
    if pkg.auto_inlined_proc_macros.is_empty() {
        return body_str.into_bytes();
    }

    let mut siblings_names: HashMap<String, HashSet<String>> = HashMap::new();
    for d in &pkg.vendored {
        let ident = to_ident(&d.name);
        let names = collect_top_level_exports(&d.source.to_string(), &ident);
        siblings_names.insert(ident, names);
    }

    let mut inlined_macro_names: HashSet<String> = HashSet::new();
    for (name, manifest_path) in &pkg.auto_inlined_proc_macros {
        let Some(manifest_dir) = manifest_path.parent() else {
            tracing::warn!(
                "scrub_assembled: proc-macro `{}` has no manifest parent; skipping",
                name
            );
            continue;
        };
        let lib = manifest_dir.join("src/lib.rs");
        match std::fs::read_to_string(&lib) {
            Ok(src) => {
                inlined_macro_names.extend(collect_proc_macro_export_names(&src));
            }
            Err(e) => {
                tracing::warn!(
                    "scrub_assembled: couldn't read `{}` for `{}`: {e} \
                     (re-export scrub will be incomplete)",
                    lib.display(),
                    name
                );
            }
        }
    }

    scrub_unresolvable_sibling_reexports(&body_str, &siblings_names, &inlined_macro_names)
        .into_bytes()
}

/// Strip `#[macro_export]` from every `macro_rules! NAME` after the
/// first occurrence of that NAME in the assembled source. Multiple
/// deps defining the same builtin-attr-named macro (`log` and
/// `tracing` both define `warn!`) would otherwise lift to the user
/// crate root and collide (E0428). The first occurrence wins —
/// callers reach the kept-at-root one via the existing
/// `collect_builtin_attr_macro_call_rewrites` rewrite. Subsequent
/// occurrences stay locally scoped to their dep's mod, accessible
/// only via `crate::DEP::NAME!()`.
fn dedupe_macro_export_collisions(src: &str) -> String {
    use std::collections::HashSet;
    use std::str::FromStr;
    let Ok(stream) = proc_macro2::TokenStream::from_str(src) else {
        return src.to_string();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    fn walk(
        toks: &[proc_macro2::TokenTree],
        seen: &mut HashSet<String>,
        edits: &mut Vec<(std::ops::Range<usize>, String)>,
    ) {
        use proc_macro2::TokenTree;
        let mut i = 0;
        while i < toks.len() {
            // Recurse into Brace groups (mod bodies) to cover nested
            // macro_rules definitions.
            if let TokenTree::Group(g) = &toks[i]
                && g.delimiter() == proc_macro2::Delimiter::Brace
            {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                walk(&inner, seen, edits);
            }
            // Pattern: `# [ macro_export ] macro_rules ! NAME { ... }`
            // (with possibly more `# [ ... ]` attrs interleaved).
            let TokenTree::Punct(p) = &toks[i] else {
                i += 1;
                continue;
            };
            if p.as_char() != '#' {
                i += 1;
                continue;
            }
            let Some(TokenTree::Group(bracket)) = toks.get(i + 1) else {
                i += 1;
                continue;
            };
            if bracket.delimiter() != proc_macro2::Delimiter::Bracket {
                i += 1;
                continue;
            }
            let inner_attr: Vec<TokenTree> = bracket.stream().into_iter().collect();
            let Some(TokenTree::Ident(attr_name)) = inner_attr.first() else {
                i += 1;
                continue;
            };
            if attr_name != "macro_export" {
                i += 1;
                continue;
            }
            // Look ahead for `macro_rules ! NAME` — possibly skipping
            // additional `# [ ... ]` attrs between this attr and the
            // macro_rules.
            let mut k = i + 2;
            while let Some(TokenTree::Punct(p2)) = toks.get(k)
                && p2.as_char() == '#'
                && matches!(
                    toks.get(k + 1),
                    Some(TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Bracket
                )
            {
                k += 2;
            }
            let Some(TokenTree::Ident(kw)) = toks.get(k) else {
                i += 1;
                continue;
            };
            if kw != "macro_rules" {
                i += 1;
                continue;
            }
            let Some(TokenTree::Punct(bang)) = toks.get(k + 1) else {
                i += 1;
                continue;
            };
            if bang.as_char() != '!' {
                i += 1;
                continue;
            }
            let Some(TokenTree::Ident(name)) = toks.get(k + 2) else {
                i += 1;
                continue;
            };
            let n = name.to_string();
            if seen.contains(&n) {
                // Strip THIS occurrence's `#[macro_export]`. Span
                // covers the `#` punct through the end of the bracket
                // group.
                let pound_start = p.span().byte_range().start;
                let bracket_end = bracket.span().byte_range().end;
                edits.push((pound_start..bracket_end, String::new()));
            } else {
                seen.insert(n);
            }
            i += 1;
        }
    }
    let toks: Vec<proc_macro2::TokenTree> = stream.into_iter().collect();
    walk(&toks, &mut seen, &mut edits);
    if edits.is_empty() {
        return src.to_string();
    }
    crate::edits::apply_simple_edits(src, edits)
}

/// `inlined_macro_names` is the union over every auto-externalized
/// proc-macro crate of macro names it would have exported. Used to
/// gate scrubbing — we only drop a name if it (a) isn't in the
/// sibling's known exports AND (b) IS one of these inlined macro
/// names. Without (b), wildcard-using siblings (`pub use base::*;`)
/// would lose every name we couldn't statically resolve.
pub fn scrub_unresolvable_sibling_reexports(
    src: &str,
    siblings_names: &HashMap<String, HashSet<String>>,
    inlined_macro_names: &HashSet<String>,
) -> String {
    use std::ops::Range;
    use syn::spanned::Spanned;
    use syn::visit::Visit;

    let Ok(file) = syn::parse_file(src) else {
        return src.to_string();
    };
    struct V<'a> {
        siblings: &'a HashMap<String, HashSet<String>>,
        inlined: &'a HashSet<String>,
        edits: Vec<(Range<usize>, String)>,
    }
    impl<'a> V<'a> {
        fn should_scrub(&self, name: &str, known: &HashSet<String>) -> bool {
            // Drop only if (a) not in sibling's known exports AND
            // (b) is a name we know we inlined out of a proc-macro
            // crate. (b) is what makes wildcards safe — we won't
            // scrub `Allocator` from `pub use crate::nalgebra::{...,
            // Allocator, ...};` because Allocator isn't a known
            // inlined macro name (even though we can't directly see
            // it in nalgebra's known names due to `pub use base::*`).
            !known.contains(name) && self.inlined.contains(name)
        }
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            // Match shape: `pub use crate::SIBLING::{...};`
            // (or `pub use crate::SIBLING::NAME;`).
            let syn::UseTree::Path(crate_path) = &item.tree else {
                return;
            };
            if crate_path.ident != "crate" {
                return;
            }
            let syn::UseTree::Path(sib_path) = crate_path.tree.as_ref() else {
                return;
            };
            let sibling = sib_path.ident.to_string();
            let Some(known) = self.siblings.get(&sibling) else {
                return;
            };
            // Collect each leaf in the use tree below SIBLING. Filter
            // those NOT present in `known`.
            let leaf = sib_path.tree.as_ref();
            let (kept, total) = match leaf {
                syn::UseTree::Group(group) => {
                    let mut kept: Vec<String> = Vec::new();
                    for it in &group.items {
                        let name = match it {
                            syn::UseTree::Name(n) => n.ident.to_string(),
                            syn::UseTree::Rename(r) => r.ident.to_string(),
                            syn::UseTree::Path(p) => p.ident.to_string(),
                            syn::UseTree::Glob(_) => "*".to_string(),
                            syn::UseTree::Group(_) => continue,
                        };
                        if name == "*" {
                            kept.push(quote_use_tree(it));
                            continue;
                        }
                        if !self.should_scrub(&name, known) {
                            kept.push(quote_use_tree(it));
                        }
                    }
                    (kept, group.items.len())
                }
                syn::UseTree::Name(n) => {
                    if !self.should_scrub(&n.ident.to_string(), known) {
                        return;
                    }
                    (Vec::new(), 1)
                }
                syn::UseTree::Rename(r) => {
                    if !self.should_scrub(&r.ident.to_string(), known) {
                        return;
                    }
                    (Vec::new(), 1)
                }
                _ => return,
            };
            if kept.len() == total {
                return;
            }
            let item_span = item.span().byte_range();
            let prefix = format!("crate::{sibling}");
            let vis = match &item.vis {
                syn::Visibility::Public(_) => "pub ",
                syn::Visibility::Restricted(r) => {
                    if r.path.is_ident("crate") {
                        "pub(crate) "
                    } else {
                        "pub "
                    }
                }
                syn::Visibility::Inherited => "",
            };
            let replacement = if kept.is_empty() {
                String::new()
            } else if kept.len() == 1 {
                format!("{vis}use {prefix}::{};", kept[0])
            } else {
                format!("{vis}use {prefix}::{{{}}};", kept.join(", "))
            };
            self.edits.push((item_span, replacement));
        }
    }
    let mut v = V {
        siblings: siblings_names,
        inlined: inlined_macro_names,
        edits: Vec::new(),
    };
    v.visit_file(&file);
    crate::edits::apply_simple_edits(src, v.edits)
}

/// Walk a proc-macro crate's `src/lib.rs` for `#[proc_macro]`,
/// `#[proc_macro_derive(NAME, ...)]`, and `#[proc_macro_attribute]`
/// items. Returns the public macro names exposed by the crate (the
/// derive's NAME, or the function ident otherwise — proc-macro
/// derives expose a different name from the function they're
/// attached to).
pub fn collect_proc_macro_export_names(src: &str) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    let Ok(file) = syn::parse_file(src) else {
        return out;
    };
    for item in &file.items {
        let syn::Item::Fn(f) = item else {
            continue;
        };
        let mut is_pm = false;
        let mut derive_name: Option<String> = None;
        for attr in &f.attrs {
            let path = attr.path();
            if path.is_ident("proc_macro") || path.is_ident("proc_macro_attribute") {
                is_pm = true;
                break;
            }
            if path.is_ident("proc_macro_derive") {
                use syn::Token;
                use syn::punctuated::Punctuated;
                if let Ok(items) =
                    attr.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated)
                    && let Some(first) = items.first()
                {
                    if let Some(id) = first.path().get_ident() {
                        derive_name = Some(id.to_string());
                    } else if let Some(seg) = first.path().segments.first() {
                        derive_name = Some(seg.ident.to_string());
                    }
                }
                is_pm = true;
                break;
            }
        }
        if !is_pm {
            continue;
        }
        if let Some(d) = derive_name {
            out.insert(d);
        } else {
            out.insert(f.sig.ident.to_string());
        }
    }
    out
}

/// Walk top-level items of a vendored crate's source and build a set of
/// all the identifiers it exposes — function/type/etc. names AND the
/// final ident of each `pub use ...::NAME` re-export, including names
/// reached through `pub use FOO::*;` for any local `pub mod FOO {...}`
/// in the same source. Used for final `pub use crate::SIBLING::NAME;`
/// validation.
pub fn collect_top_level_exports(src: &str, self_ident: &str) -> HashSet<String> {
    let Ok(file) = syn::parse_file(src) else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();
    collect_exports_from_items(&file.items, &mut out, &mut visiting, self_ident);
    out
}

fn collect_exports_from_items(
    items: &[syn::Item],
    out: &mut HashSet<String>,
    visiting: &mut HashSet<String>,
    self_ident: &str,
) {
    fn add_use_leaf(t: &syn::UseTree, out: &mut HashSet<String>) {
        match t {
            syn::UseTree::Name(n) => {
                out.insert(n.ident.to_string());
            }
            syn::UseTree::Rename(r) => {
                out.insert(r.rename.to_string());
            }
            syn::UseTree::Path(p) => add_use_leaf(&p.tree, out),
            syn::UseTree::Group(g) => {
                for it in &g.items {
                    add_use_leaf(it, out);
                }
            }
            // Wildcards are handled by the second pass below: we walk
            // `pub use FOO::*;` by descending into `mod FOO {...}` in
            // the same source. Names reached only through external
            // wildcards (`pub use ::ext_crate::*`) won't be in our set,
            // but that's fine — the scrubber gates on "is in inlined
            // macro names" so over-stripping is bounded to those.
            syn::UseTree::Glob(_) => {}
        }
    }
    // First pass: gather direct items.
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                out.insert(f.sig.ident.to_string());
            }
            syn::Item::Struct(s) => {
                out.insert(s.ident.to_string());
            }
            syn::Item::Enum(e) => {
                out.insert(e.ident.to_string());
            }
            syn::Item::Trait(t) => {
                out.insert(t.ident.to_string());
            }
            syn::Item::TraitAlias(t) => {
                out.insert(t.ident.to_string());
            }
            syn::Item::Type(t) => {
                out.insert(t.ident.to_string());
            }
            syn::Item::Const(c) => {
                out.insert(c.ident.to_string());
            }
            syn::Item::Static(s) => {
                out.insert(s.ident.to_string());
            }
            syn::Item::Mod(m) => {
                out.insert(m.ident.to_string());
            }
            syn::Item::Macro(m) => {
                if let Some(ident) = &m.ident {
                    out.insert(ident.to_string());
                }
            }
            syn::Item::Use(u) => {
                add_use_leaf(&u.tree, out);
            }
            syn::Item::ExternCrate(ec) => {
                let name = ec
                    .rename
                    .as_ref()
                    .map(|(_, id)| id.to_string())
                    .unwrap_or_else(|| ec.ident.to_string());
                out.insert(name);
            }
            _ => {}
        }
    }
    // Second pass: walk every `pub use ...` item recursively to find
    // wildcards (`...::*;`). For each wildcard's resolvable in-source
    // module target, descend into the target's items and collect those
    // names too. This handles nested re-exports like
    // `pub use crate::{marker_traits::*, ops::*};` that typenum-style
    // crates use to flatten their public surface.
    fn walk_for_wildcards(
        tree: &syn::UseTree,
        crate_root: &[syn::Item],
        path_so_far: &mut Vec<String>,
        out: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        self_ident: &str,
    ) {
        match tree {
            syn::UseTree::Path(p) => {
                let seg = p.ident.to_string();
                if path_so_far.is_empty() && (seg == "self" || seg == "super") {
                    return;
                }
                if seg == "crate" {
                    // `crate::...` rebases to the root. After vendoring,
                    // intra-crate paths get rewritten to `crate::SELF::...`,
                    // so the next segment may be the dep's own ident — skip
                    // it transparently if so.
                    let saved = std::mem::take(path_so_far);
                    let mut next_tree = p.tree.as_ref();
                    if let syn::UseTree::Path(p2) = next_tree
                        && p2.ident == self_ident
                    {
                        next_tree = p2.tree.as_ref();
                    }
                    walk_for_wildcards(
                        next_tree,
                        crate_root,
                        path_so_far,
                        out,
                        visiting,
                        self_ident,
                    );
                    *path_so_far = saved;
                    return;
                }
                path_so_far.push(seg);
                walk_for_wildcards(&p.tree, crate_root, path_so_far, out, visiting, self_ident);
                path_so_far.pop();
            }
            syn::UseTree::Group(g) => {
                for it in &g.items {
                    walk_for_wildcards(it, crate_root, path_so_far, out, visiting, self_ident);
                }
            }
            syn::UseTree::Glob(_) => {
                if path_so_far.is_empty() {
                    return;
                }
                let mut current_items = crate_root;
                for seg in path_so_far.iter() {
                    let next = current_items.iter().find_map(|it| {
                        if let syn::Item::Mod(m) = it
                            && m.ident == seg.as_str()
                        {
                            return m.content.as_ref().map(|(_, items)| items.as_slice());
                        }
                        None
                    });
                    let Some(child) = next else {
                        return;
                    };
                    current_items = child;
                }
                let key = path_so_far.join("::");
                if !visiting.insert(key.clone()) {
                    return;
                }
                collect_exports_from_items(current_items, out, visiting, self_ident);
                visiting.remove(&key);
            }
            syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {}
        }
    }
    for item in items {
        let syn::Item::Use(u) = item else {
            continue;
        };
        let mut path: Vec<String> = Vec::new();
        walk_for_wildcards(&u.tree, items, &mut path, out, visiting, self_ident);
    }
}

/// Pre-syn-parse rewrite: edition-2015 type aliases that use bare-
/// trait-object syntax (`type Action = Fn(...) + Send + Sync;`) fail
/// `syn::parse_file` outright — syn 2.x requires `dyn` for trait
/// objects per the 2018+ syntax. signal-hook-registry's `Action`
/// alias is the canonical case.
///
/// The fix injects `dyn ` after the `=` of any `type IDENT = ...;`
/// whose body contains a top-level `+` and doesn't already start
/// with `dyn`/`impl`. proc_macro2 lexes the bad source fine, so we
/// can do this at the token level before syn sees it.
pub fn rewrite_bare_trait_object_aliases(src: &str) -> String {
    use proc_macro2::TokenTree;
    use std::str::FromStr;
    let Ok(stream) = proc_macro2::TokenStream::from_str(src) else {
        return src.to_string();
    };
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    fn walk(toks: &[TokenTree], edits: &mut Vec<(std::ops::Range<usize>, String)>) {
        let mut i = 0;
        while i < toks.len() {
            // Recurse into braces (mod bodies, impl bodies). Skip
            // brackets (attribute bodies) and parens (trait-arg lists).
            if let TokenTree::Group(g) = &toks[i]
                && g.delimiter() == proc_macro2::Delimiter::Brace
            {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                walk(&inner, edits);
            }
            // Pattern: `type IDENT [<...>] = BODY ;`
            let TokenTree::Ident(kw) = &toks[i] else {
                i += 1;
                continue;
            };
            if kw != "type" {
                i += 1;
                continue;
            }
            // Skip `type` and the alias ident; also any `<...>` generics.
            let mut j = i + 1;
            if !matches!(toks.get(j), Some(TokenTree::Ident(_))) {
                i += 1;
                continue;
            }
            j += 1;
            // Optional generics: `<T, U>`. Walk balanced `<...>`.
            if matches!(toks.get(j), Some(TokenTree::Punct(p)) if p.as_char() == '<') {
                let mut depth = 1usize;
                j += 1;
                while j < toks.len() && depth > 0 {
                    if let TokenTree::Punct(p) = &toks[j] {
                        if p.as_char() == '<' {
                            depth += 1;
                        } else if p.as_char() == '>' {
                            depth -= 1;
                        }
                    }
                    j += 1;
                }
            }
            // Expect `=`.
            let Some(TokenTree::Punct(eq)) = toks.get(j) else {
                i = j;
                continue;
            };
            if eq.as_char() != '=' {
                i = j;
                continue;
            }
            j += 1;
            // First body token must be Fn/FnMut/FnOnce — that's the
            // unambiguous bare-trait-object pattern that breaks syn 2.x
            // outright. Other shapes (`Trait + Send`, `Box<T+Send>`,
            // `Iter<Item=u8>+Send`) either parse fine in syn or need
            // depth-aware token walking we don't do here. Narrow scope
            // intentionally: signal-hook-registry's `Action = Fn(...)
            // + Send + Sync` is the case we need to fix; other bare-
            // trait-objects can be added later case by case.
            let Some(TokenTree::Ident(first_id)) = toks.get(j) else {
                i = j;
                continue;
            };
            let first = first_id.to_string();
            if !matches!(first.as_str(), "Fn" | "FnMut" | "FnOnce") {
                i = j;
                continue;
            }
            // Walk to the next `;` and confirm a top-level `+` is
            // present. Naive scan (no nesting tracked) is OK here:
            // Fn-traits don't take nested generics with `+` bounds —
            // their args are in `(...)` which proc_macro2 represents
            // as a single Group token, so the `+`s we see are all at
            // body level.
            let mut k = j;
            let mut found_plus = false;
            let mut hit_semi = false;
            while k < toks.len() {
                if let TokenTree::Punct(p) = &toks[k] {
                    if p.as_char() == ';' {
                        hit_semi = true;
                        break;
                    }
                    if p.as_char() == '+' {
                        found_plus = true;
                    }
                }
                k += 1;
            }
            if !hit_semi || !found_plus {
                i = j;
                continue;
            }
            let injection_at = first_id.span().byte_range().start;
            edits.push((injection_at..injection_at, "dyn ".to_string()));
            i = k + 1;
        }
    }
    let toks: Vec<TokenTree> = stream.into_iter().collect();
    walk(&toks, &mut edits);
    if edits.is_empty() {
        return src.to_string();
    }
    crate::edits::apply_simple_edits(src, edits)
}

/// Pre-syn-parse rewrite: `$IDENT:pat` followed by `|` is a hard
/// error in 2021+ (`pat` fragment specifier no longer permits `|` as
/// the next token without `pat_param`). Macros that worked under the
/// old semantics (tokio's `select!`, several DSL crates) won't compile
/// once vendored under a 2021+ outer crate.
///
/// The compiler suggests `$IDENT:pat_param` — same matcher in 2021+
/// editions and a strict subset semantically. This pass walks tokens
/// looking for the exact failing shape (`$ IDENT : pat <ws>* |`) and
/// rewrites the `pat` ident to `pat_param`. Other `:pat` uses (where
/// the next token is `=>`, `,`, `=`, `if`, `if let`, `in`) parse fine
/// and stay untouched.
pub fn rewrite_pat_followed_by_pipe(src: &str) -> String {
    use proc_macro2::TokenTree;
    use std::str::FromStr;
    let Ok(stream) = proc_macro2::TokenStream::from_str(src) else {
        return src.to_string();
    };
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    fn walk(toks: &[TokenTree], edits: &mut Vec<(std::ops::Range<usize>, String)>) {
        for i in 0..toks.len() {
            // Recurse into every Group (macro_rules bodies live inside
            // Brace groups, but matchers also nest inside Paren groups
            // for `$($p:pat)|+` repetitions).
            if let TokenTree::Group(g) = &toks[i] {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                walk(&inner, edits);
            }
            // Pattern: `$ IDENT : pat <next>` where <next> is Punct('|').
            let TokenTree::Punct(dollar) = &toks[i] else {
                continue;
            };
            if dollar.as_char() != '$' {
                continue;
            }
            if !matches!(toks.get(i + 1), Some(TokenTree::Ident(_))) {
                continue;
            }
            let Some(TokenTree::Punct(colon)) = toks.get(i + 2) else {
                continue;
            };
            if colon.as_char() != ':' {
                continue;
            }
            let Some(TokenTree::Ident(frag)) = toks.get(i + 3) else {
                continue;
            };
            if frag != "pat" {
                continue;
            }
            // The next token (in this group OR the closing of a
            // wrapping group like `$($p:pat)|+`) is what determines
            // whether we need to rewrite. Inside a `(... )|+` repetition,
            // `$p:pat` is the LAST token of the inner group; the `|` is
            // outside, so we can't see it from here. Rewrite anyway when
            // it's the last token in the inner group — repetition
            // separators are the most common shape we want to fix.
            let next = toks.get(i + 4);
            let needs_rewrite = match next {
                None => true, // last in inner group → likely a `|+` separator outside
                Some(TokenTree::Punct(p)) if p.as_char() == '|' => true,
                _ => false,
            };
            if !needs_rewrite {
                continue;
            }
            let span = frag.span().byte_range();
            edits.push((span, "pat_param".to_string()));
        }
    }
    let toks: Vec<TokenTree> = stream.into_iter().collect();
    walk(&toks, &mut edits);
    if edits.is_empty() {
        return src.to_string();
    }
    crate::edits::apply_simple_edits(src, edits)
}

/// Rewrite `try!(EXPR)` → `(EXPR)?` in a source string. Used when
/// vendoring an edition-2015 dep that uses the now-unusable
/// `try!()` macro: edition 2018 reserved `try` as a keyword, so the
/// macro can't be called from any outer 2018+ context. The flat
/// output's outer crate is typically 2021, so any vendored 2015 dep
/// using `try!()` would fail to compile.
///
/// Done as a syn-AST visit (visit `ExprMacro` whose path is `try` or
/// `r#try`) so we get correct token boundaries and ignore matches
/// inside string literals/comments. The replacement byte text is
/// `(<args>)?` which preserves the EXPR's source-level form
/// (including comments and whitespace).
pub fn rewrite_try_macro(src: &str) -> String {
    use std::ops::Range;
    use syn::spanned::Spanned;
    use syn::visit::Visit;
    let Ok(file) = syn::parse_file(src) else {
        return src.to_string();
    };
    struct V<'a> {
        src: &'a str,
        edits: Vec<(Range<usize>, String)>,
    }
    impl V<'_> {
        fn try_handle(&mut self, mac: &syn::Macro, span: Range<usize>) {
            // `try!` or `r#try!` (latter unlikely but allowed).
            if !mac.path.is_ident("try") && !mac.path.is_ident("r#try") {
                return;
            }
            // mac.tokens.span() covers the bytes between `(` and `)`.
            let inner_span = mac.tokens.span().byte_range();
            if inner_span.start >= self.src.len() || inner_span.end > self.src.len() {
                return;
            }
            let inner = &self.src[inner_span.clone()];
            // `(EXPR)?` — wrap to preserve precedence; trim args of any
            // trailing whitespace that came inside the macro tokens.
            let replacement = format!("({})?", inner.trim());
            self.edits.push((span, replacement));
        }
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_macro(&mut self, em: &'ast syn::ExprMacro) {
            self.try_handle(&em.mac, em.span().byte_range());
        }
        // Stmt-position `try!()` is a Stmt::Macro whose span INCLUDES the
        // trailing `;`. Use the inner mac.span() (just `try!(...)`) so
        // our replacement doesn't swallow the statement terminator.
        fn visit_stmt_macro(&mut self, sm: &'ast syn::StmtMacro) {
            self.try_handle(&sm.mac, sm.mac.span().byte_range());
        }
    }
    let mut v = V {
        src,
        edits: Vec::new(),
    };
    v.visit_file(&file);
    crate::edits::apply_simple_edits(src, v.edits)
}

fn quote_use_tree(t: &syn::UseTree) -> String {
    match t {
        syn::UseTree::Name(n) => n.ident.to_string(),
        syn::UseTree::Rename(r) => format!("{} as {}", r.ident, r.rename),
        syn::UseTree::Path(p) => format!("{}::{}", p.ident, quote_use_tree(&p.tree)),
        syn::UseTree::Glob(_) => "*".to_string(),
        syn::UseTree::Group(g) => {
            let parts: Vec<String> = g.items.iter().map(quote_use_tree).collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Self-cleaning scratch directory under `std::env::temp_dir()`. Used for
/// the per-process dump location passed to the `RUSTC_WRAPPER` expander.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(prefix: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).map_err(|e| FlattenError::Io {
            context: format!("Failed to create scratch dir `{}`", path.display()),
            source: e,
        })?;
        Ok(Self { path })
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if std::env::var_os("FLATTEN_KEEP_SCRATCH").is_none() {
            let _ = std::fs::remove_dir_all(&self.path);
        } else {
            tracing::info!("kept scratch: {}", self.path.display());
        }
    }
}

fn nightly_sysroot_lib_dir() -> Option<PathBuf> {
    let out = std::process::Command::new("rustc")
        .arg("+nightly")
        .arg("--print=sysroot")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    Some(PathBuf::from(s).join("lib"))
}

fn find_manifest_path(crate_root: &Path) -> Option<PathBuf> {
    let mut cur = Some(crate_root);
    while let Some(dir) = cur {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// Look up the flatten_expand binary in this order:
///   1. FLATTEN_EXPAND env var (explicit override)
///   2. expand/target/<profile>/flatten_expand relative to
///      flatten's compile-time CARGO_MANIFEST_DIR — picking
///      <profile> to match THIS binary's build profile first
///      (release if `cargo build --release`, debug otherwise), with
///      a fallback to the other profile (so dev workflows that
///      rebuild only one half don't break).
///   3. PATH lookup
fn locate_expander_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FLATTEN_EXPAND") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    // Pick the profile matching this binary's build first, then fall
    // back to the other. `debug_assertions` is on for debug builds and
    // off for release builds by default — proxy for "which target/
    // subdir did cargo just write me to". Mismatched profiles still
    // work, just with the slight perf surprise that one half is debug
    // and the other release.
    let profile_order: &[&str] = if cfg!(debug_assertions) {
        &["debug", "release"]
    } else {
        &["release", "debug"]
    };
    let expand_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("expand/target");
    for profile in profile_order {
        let candidate = expand_dir.join(profile).join("flatten_expand");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // PATH lookup.
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("flatten_expand");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

impl fmt::Display for VendorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} v{}", self.root_name, self.root_version)?;
        writeln!(f)?;

        let vend: Vec<&DepEntry> = self
            .deps
            .iter()
            .filter(|d| d.classification.is_vendorable())
            .collect();
        let unvend: Vec<&DepEntry> = self
            .deps
            .iter()
            .filter(|d| !d.classification.is_vendorable())
            .collect();

        writeln!(f, "Vendorable ({}):", vend.len())?;
        if vend.is_empty() {
            writeln!(f, "  (none)")?;
        } else {
            for d in &vend {
                writeln!(f, "  {} {}", d.name, d.version)?;
                if let Classification::Warn(warns) = &d.classification {
                    for w in warns {
                        writeln!(f, "    warn: {w}")?;
                    }
                }
            }
        }
        writeln!(f)?;

        writeln!(f, "Unvendorable ({}):", unvend.len())?;
        if unvend.is_empty() {
            writeln!(f, "  (none)")?;
        } else {
            for d in &unvend {
                let Classification::Unvendorable(reasons) = &d.classification else {
                    continue;
                };
                writeln!(f, "  {} {} — {}", d.name, d.version, reasons.join(", "))?;
            }
        }
        writeln!(f)?;

        writeln!(
            f,
            "Summary: {}/{} vendorable, {} unvendorable",
            vend.len(),
            self.deps.len(),
            unvend.len()
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase V1 — actual vendoring with rewrites
// ---------------------------------------------------------------------------

/// Configuration for [`vendor_package`].
#[derive(Debug, Clone)]
pub struct VendorOptions {
    /// Refuse on any dep that can't be vendored OR isn't covered by
    /// `--external`. `--vendor-allow-external` will land in V4 to flip
    /// this default.
    pub strict: bool,
    /// Crate names the user explicitly wants to keep external (don't
    /// vendor; user provides via Cargo.toml). The BFS treats these as
    /// cut points.
    pub external: HashSet<String>,
    /// Crate names that came from a curated source (`--external-preset`,
    /// `--external-file`) — also treated as cut points but unmatched
    /// names DO NOT warn (the curated source intentionally lists more
    /// names than any single project will use).
    pub external_silent: HashSet<String>,
    /// If true, transitive deps of `external` crates that the user can
    /// only reach THROUGH an external are also externalised. Deps that
    /// have a direct (non-external) path from the user stay vendored —
    /// avoids needlessly externalising shared deps like `bitflags` that
    /// happen to also be pulled by an external (`signal-hook`). See
    /// `external_deep_aggressive` for the legacy naive-closure mode.
    pub external_deep: bool,
    /// Legacy aggressive mode for `external_deep`: every transitive dep
    /// of every external becomes external, even when reachable from
    /// vendored crates via non-external paths. Vendored crates that
    /// directly reference a now-cut transitive are auto-promoted to a
    /// "Required" external (the user must list them in their
    /// Cargo.toml). See EXTERNAL.md. Implies `external_deep`.
    pub external_deep_aggressive: bool,
    /// If true, run the user crate's source through `flatten_expand`
    /// (a separate nightly binary built from `expand/`) which inlines
    /// third-party proc-macro expansions and strips them as deps. Stdlib
    /// macros (println!, derive(Debug), etc.) are left as-is. Each
    /// proc-macro crate consumed by this expansion is automatically
    /// added to the externalized set.
    pub expand: bool,
    /// If true, extend `expand` to every vendored dep — not just the user
    /// crate. Switches to a `RUSTC_WRAPPER`-based capture so cargo handles
    /// each crate's compilation context (extern flags, edition, features,
    /// build-script cfgs). The expanded sources replace the cargo-cache
    /// originals when each dep is vendored.
    pub expand_deep: bool,
}

impl Default for VendorOptions {
    fn default() -> Self {
        Self {
            strict: true,
            external: HashSet::new(),
            external_silent: HashSet::new(),
            external_deep: false,
            external_deep_aggressive: false,
            expand: false,
            expand_deep: false,
        }
    }
}

/// One vendored dependency, ready to be wrapped in `mod <name> { ... }`.
#[derive(Debug)]
pub struct VendoredDep {
    pub name: String,
    pub version: String,
    pub source: SourceFile,
    /// Names of this dep's normal-kind direct dependencies (from cargo
    /// metadata). Used by the assembler to emit `use crate::D;` for each
    /// vendored neighbour, since Rust 2018+ won't resolve a path expression
    /// like `D::foo()` from inside `mod V { ... }` to the sibling `mod D`
    /// without an explicit `use`.
    pub normal_deps: Vec<String>,
    /// Macros that originally had `#[macro_export]` and need to be
    /// re-exported at the vendored mod's root so callers can reach them
    /// as `<crate_name>::<name>!()`. Each entry is a ready-to-write
    /// statement (cfg-prefixed if the macro lives behind `#[cfg(...)]`
    /// ancestor mods), e.g.
    /// `#[cfg(target_arch = "x86_64")] pub(crate) use x86_64::dispatch;`.
    pub macro_exports: Vec<String>,
    /// Standard-library crates the vendored crate explicitly imported
    /// via `extern crate alloc;` (or `core` / `std`). The assembler
    /// hoists these to the user crate's root because `extern crate`
    /// only adds to the crate-root extern prelude, not the immediate
    /// mod's scope, and our wrapping `mod <name> { ... }` would
    /// otherwise scope the import to one mod level.
    pub extern_std_libs: Vec<String>,
    /// The dep's lib.rs inner-attribute cfg predicate, if any
    /// (`#![cfg(target_os = "windows")]` style). Captured so the
    /// assembler can cfg-gate sibling-import injections that
    /// reference this dep — without that, on a non-matching host
    /// every other dep's `use crate::SIBLING;` injection would fail
    /// (the SIBLING mod's body evaporates at user compile time).
    pub inner_cfg: Option<String>,
}

/// One dep that ended up external (not vendored) in the final output,
/// paired with the reason. Used by the banner to tell the user what
/// they need to do for each.
#[derive(Debug, Clone)]
pub struct ExternalDep {
    pub name: String,
    pub version: String,
    pub reason: ExternalReason,
    /// Path to the dep's `Cargo.toml`. Captured from cargo-metadata so
    /// downstream tooling (e.g. the proc-macro export-name scan) can
    /// open the dep's source without re-running `cargo metadata`.
    pub manifest_path: Option<PathBuf>,
}

/// Result of vendoring a package: the user's flattened source plus all
/// vendored deps + the list of deps that need to come from elsewhere.
#[derive(Debug)]
pub struct VendoredPackage {
    pub kind: PackageType,
    pub crate_name: String,
    pub crate_version: String,
    pub target_name: String,
    pub user_source: SourceFile,
    pub vendored: Vec<VendoredDep>,
    /// Proc-macro deps that `--expand` / `--expand-deep` automatically
    /// removed from vendoring (their macros got inlined). Kept for
    /// downstream tooling that needs to know which crates' export names
    /// no longer reach the flat output (e.g. to scrub
    /// `pub use crate::FOO::MACRO;` re-exports referencing them).
    /// Each entry is `(crate_name, manifest_path)`.
    pub auto_inlined_proc_macros: Vec<(String, PathBuf)>,
    /// Deps that ended up external (one of: unvendorable, user-excluded,
    /// or required-orphan-of-an-exclusion). In strict mode, an
    /// `Unvendorable` or `VendorFailed` entry causes
    /// [`vendor_package`] to error before returning. `UserExcluded` and
    /// `Required` entries are always allowed.
    pub external: Vec<ExternalDep>,
    /// Highest Rust edition seen across the user crate and all vendored
    /// deps. The flat output should be compiled with at least this edition
    /// (rustc defaults to 2015 unless told otherwise).
    pub max_edition: cargo_metadata::Edition,
}

/// Vendor a package's normal-kind transitive deps into the flat output.
///
/// Algorithm (see `EXTERNAL.md` for the full design):
///
/// 1. **BFS with cut points**: walk the dep graph from the user's crate.
///    Stop at any dep named in `options.external`. Everything not reached
///    is dropped (orphaned-by-exclusion).
///
/// 2. **Vendor the reachable set**: for each reached dep, attempt to
///    inline its source. Refuse (or fall through to external) on the
///    usual unvendorable conditions.
///
/// 3. **Orphan resolution**: scan vendored deps' graph edges for
///    references back into the orphaned-by-exclusion set. Each such
///    reference becomes either:
///      - `Required` external (default) — user must list in Cargo.toml
///      - vendored (if `vendor_extras` and the orphan is vendorable) —
///        the BFS continues from it, possibly pulling in further deps
pub fn vendor_package(
    crate_root: impl AsRef<Path>,
    selector: &TargetSelector,
    options: &VendorOptions,
) -> Result<VendoredPackage> {
    let crate_root = crate_root.as_ref();
    let mut user_pkg = parse_target(crate_root, selector)?;
    let mut report = report(crate_root)?;
    let user_edition = user_edition(crate_root).unwrap_or(cargo_metadata::Edition::E2015);
    let mut max_edition = user_edition;

    // Same-package lib auto-vendoring: when the user picked a bin from a
    // package that also has a `[lib]` target, the bin's
    // `use <self_pkg>::X;` would otherwise dangle in the flat output
    // (cargo metadata treats bin+lib as one package, so the lib isn't a
    // dep). Synthesize a `DepEntry` for the lib and feed it into the
    // normal vendoring pipeline — it then ends up as a sibling
    // `pub mod <self_lib>` in the bundle and the bin's top-level
    // `use <self_lib>::X;` resolves to it via crate-root scope.
    // Tagged so a future polish pass can label the self-lib distinctly
    // in the bundle header ("self-lib" vs ordinary vendored dep). Today
    // it just rides through the same render path.
    let _self_lib_name: Option<String> = if matches!(user_pkg.kind, PackageType::Bin) {
        crate::parse_self_lib(crate_root)?.map(|lib| {
            let crate_name = lib.crate_name;
            report.deps.push(DepEntry {
                name: crate_name.clone(),
                version: report.root_version.clone(),
                manifest_path: crate_root.join("Cargo.toml"),
                classification: Classification::Vendorable,
                features: HashSet::new(),
                edition: user_edition,
                // Empty — the lib's actual deps are already in
                // `report.deps` via the shared package dep table, so
                // the existing BFS picks them up naturally.
                normal_deps: Vec::new(),
                build_cfgs: Vec::new(),
                out_dir: None,
            });
            crate_name
        })
    } else {
        None
    };

    // Shallow `--expand` runs immediately on the user crate further
    // below (after auto_externals is seeded with proc-macro names).
    // Deep `--expand-deep` defers actual expansion until after the BFS
    // picks `to_vendor`, so the wrapper knows which crates to capture.
    let mut auto_externals: HashSet<String> = HashSet::new();
    let mut expand_dump_dir: Option<ScratchDir> = None;

    // For both --expand and --expand-deep: every proc-macro in the dep graph
    // is going to be inlined (in the user crate, or in vendored deps too),
    // so we always pre-populate `auto_externals` + cut them from the BFS.
    if options.expand {
        auto_externals.extend(proc_macro_dep_names(&report));
    }

    // --expand (shallow): run the user's source through flatten_expand
    // to inline third-party proc-macro expansions. The expanded source
    // replaces user_pkg.source; the proc-macro deps were already auto-
    // externalized above so the BFS cuts them out cleanly.
    if options.expand && !options.expand_deep {
        // Cargo metadata gives us a manifest path on the user crate; reuse
        // collect_proc_macro_dylibs which spawns `cargo build --message-format=json`.
        let manifest_path = find_manifest_path(crate_root).ok_or_else(|| {
            FlattenError::other(format!(
                "Could not find Cargo.toml under `{}`",
                crate_root.display()
            ))
        })?;
        let dylibs = collect_proc_macro_dylibs(&manifest_path).unwrap_or_default();
        if dylibs.is_empty() && !auto_externals.is_empty() {
            tracing::warn!(
                "--expand: no proc-macro dylibs discovered, but {} proc-macro deps in graph; \
                 expansion may be incomplete",
                auto_externals.len()
            );
        }

        let expander = locate_expander_binary().ok_or_else(|| {
            FlattenError::other(
                "--expand: could not locate flatten_expand binary. Set \
                 FLATTEN_EXPAND to its path."
                    .to_string(),
            )
        })?;
        let flat_source = user_pkg.source.to_string();
        let result = run_expander(&flat_source, &dylibs, &expander)?;
        user_pkg.source = SourceFile::from_string(result.rewritten_source);
    }

    // Index by name for quick lookup. Multi-version warnings are surfaced
    // separately during the vendor attempt.
    let dep_by_name: HashMap<&str, &DepEntry> =
        report.deps.iter().map(|d| (d.name.as_str(), d)).collect();

    // Validate user-named externals: warn on names that aren't actually
    // in the dep graph (probably typos / packagename mismatch). Names
    // sourced from a curated preset / file (`external_silent`) skip
    // this check — those lists intentionally cover more crates than
    // any single project will actually use.
    for name in &options.external {
        if !dep_by_name.contains_key(name.as_str()) {
            tracing::warn!(
                "--external `{}` doesn't match any transitive dep of `{}`; ignored",
                name,
                report.root_name
            );
        }
        if name == &user_pkg.crate_name {
            tracing::warn!("--external `{}` is the user crate itself; ignored", name);
        }
    }

    // Effective cut set merges user-named + curated-source names. Used
    // by all cut-point / sibling-skip logic below.
    let effective_external: HashSet<String> = options
        .external
        .iter()
        .chain(options.external_silent.iter())
        .cloned()
        .collect();

    // Compute the *expanded* external set. Without --external-deep, this
    // is just the effective cut set. With --external-deep, it also
    // includes deps that the user can ONLY reach through an externalized
    // crate (e.g., signal-hook → bitflags makes bitflags external too,
    // but ONLY if the user has no other path to bitflags).
    //
    // Pre-fix the expansion was naive transitive closure: any dep of
    // any external became external. That externalized deps the user
    // ALSO reached directly (e.g., ratatui → bitflags), pushing the
    // burden of declaring them in the user's Cargo.toml even though
    // we could have vendored them. The refined logic computes vendor
    // candidates first (BFS from user with ONLY user-marked + auto-
    // externals as cuts) and only externalizes external-deep
    // candidates that aren't vendor-reachable.
    let mut expanded_externals: HashSet<String> = effective_external.clone();
    for name in &auto_externals {
        expanded_externals.insert(name.clone());
    }
    if options.external_deep || options.external_deep_aggressive {
        if options.external_deep_aggressive {
            // Legacy mode: naive transitive closure. Every transitive
            // dep of every external becomes external, even when
            // reachable from vendored crates via non-external paths.
            let mut queue: Vec<String> = effective_external.iter().cloned().collect();
            while let Some(cur) = queue.pop() {
                let Some(dep) = dep_by_name.get(cur.as_str()) else {
                    continue;
                };
                for sub in &dep.normal_deps {
                    if expanded_externals.insert(sub.clone()) {
                        queue.push(sub.clone());
                    }
                }
            }
        } else {
            // Refined mode: only externalise deep candidates the user
            // can't reach directly. Preserves vendoring for deps
            // shared between an external (signal-hook) AND a vendored
            // crate (ratatui), instead of pushing the dep to the
            // user's Cargo.toml.
            let (vendor_candidates, _) = bfs_from_user(&report, &expanded_externals);
            let vendor_set: HashSet<String> = vendor_candidates.into_iter().collect();
            let mut queue: Vec<String> = effective_external.iter().cloned().collect();
            let mut deep_candidates: HashSet<String> = HashSet::new();
            while let Some(cur) = queue.pop() {
                let Some(dep) = dep_by_name.get(cur.as_str()) else {
                    continue;
                };
                for sub in &dep.normal_deps {
                    if deep_candidates.insert(sub.clone()) {
                        queue.push(sub.clone());
                    }
                }
            }
            for cand in deep_candidates {
                if !vendor_set.contains(&cand) {
                    expanded_externals.insert(cand);
                }
            }
        }
    }

    // BFS from user's direct deps with the expanded set as cut points.
    let (to_vendor, _) = bfs_from_user(&report, &expanded_externals);

    // --expand-deep: now that we know `to_vendor`, run cargo build under our
    // RUSTC_WRAPPER so the expander captures both the user crate and every
    // vendored dep in cargo's real compilation context. Each dep's dump dir
    // overrides the cargo-cache source in vendor_one_dep below.
    if options.expand_deep {
        let dump = ScratchDir::new("flatten_expand")?;
        let mut targets: BTreeSet<String> = to_vendor.iter().cloned().collect();
        targets.insert(user_pkg.crate_name.clone());
        // The expander uses this to scope its rewriting: only `use FOO::...;`
        // and only expansions whose `macro_def_id.krate` is in this set get
        // touched. Anything else is left alone.
        let pm_set: BTreeSet<String> = auto_externals.iter().cloned().collect();

        let expander = locate_expander_binary().ok_or_else(|| {
            FlattenError::other(
                "--expand-deep: could not locate flatten_expand binary. Build it via \
                 `cargo build --manifest-path expand/Cargo.toml`."
                    .to_string(),
            )
        })?;
        run_wrapper_expand(crate_root, &targets, &pm_set, dump.path(), &expander)?;

        // Replace user_pkg.source with the wrapper-dumped version. Try
        // src/main.rs then src/lib.rs depending on the crate kind.
        let user_dump_dir = dump.path().join(&user_pkg.crate_name);
        for ef in ["src/main.rs", "src/lib.rs"] {
            let p = user_dump_dir.join(ef);
            if p.is_file() {
                let content = std::fs::read_to_string(&p).map_err(|e| FlattenError::Io {
                    context: format!("Failed reading dumped user source `{}`", p.display()),
                    source: e,
                })?;
                user_pkg.source = SourceFile::from_string(content);
                break;
            }
        }

        expand_dump_dir = Some(dump);
    }

    let mut external: Vec<ExternalDep> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();

    // Phase 2: actually vendor the `to_vendor` set. Apply the existing
    // unvendorable / collision / multi-version checks here.
    let user_top_mods = collect_top_level_mod_names(&user_pkg.source);
    let mut version_map: HashMap<String, String> = HashMap::new();
    let mut vendored: Vec<VendoredDep> = Vec::new();
    let mut to_vendor_sorted: Vec<&DepEntry> = to_vendor
        .iter()
        .filter_map(|n| dep_by_name.get(n.as_str()).copied())
        .collect();
    // Chain on version for stable ordering when multiple versions of
    // the same crate end up in `to_vendor` (the multi-version blocker
    // path). Without the secondary key, the message text varies across
    // runs because `sort_by` isn't stable across equal keys per the
    // platform's sort impl details (REVIEW B4).
    to_vendor_sorted.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));

    // Capture each dep's lib.rs `#![cfg(...)]` inner attribute (if
    // any) so the assembler can cfg-gate sibling-import injections
    // that reference deps which only compile on certain targets.
    // crossterm_winapi (Windows-only via `#![cfg(windows)]`) is the
    // canonical case — pre-fix we skipped vendoring it entirely on
    // non-Windows; now we vendor it and gate the injections so
    // vendor-on-macOS-run-on-Windows works.
    let inner_cfgs: HashMap<String, String> = to_vendor_sorted
        .iter()
        .filter_map(|dep| {
            let manifest_dir = dep.manifest_path.parent()?;
            let lib_path = manifest_dir.join("src/lib.rs");
            let src = std::fs::read_to_string(&lib_path).ok()?;
            crate::source_file::file_inner_cfg_predicate(&src)
                .map(|pred| (to_ident(&dep.name), pred))
        })
        .collect();

    // Set of sibling vendored mod names (Rust idents, not package names).
    // Used by the rewriter to convert `use SIBLING::...` into
    // `use crate::SIBLING::...` — required because Rust 2018+ `use` paths
    // are absolute (extern prelude or crate root) and don't auto-walk to
    // sibling mods.
    let sibling_idents: HashSet<String> = to_vendor.iter().map(|n| to_ident(n)).collect();

    // After --expand / --expand-deep, the user source can carry absolute
    // paths from proc-macro expansions like `::clap_builder::Foo` — those
    // don't resolve in the flat output (no extern prelude). Rewrite them
    // to `crate::clap_builder::Foo` now that we know the final sibling set.
    if options.expand {
        let rewritten =
            rewrite_absolute_sibling_paths(&user_pkg.source.to_string(), &sibling_idents);
        user_pkg.source = SourceFile::from_string(rewritten);
    }

    for dep in to_vendor_sorted {
        // Manifest-level Unvendorable.
        if let Classification::Unvendorable(reasons) = &dep.classification {
            blockers.push(format!(
                "  {} v{} — {}",
                dep.name,
                dep.version,
                reasons.join(", ")
            ));
            external.push(ExternalDep {
                name: dep.name.clone(),
                version: dep.version.clone(),
                reason: ExternalReason::Unvendorable(reasons.clone()),
                manifest_path: Some(dep.manifest_path.clone()),
            });
            continue;
        }

        // Collision with user's mod.
        if user_top_mods.contains(&dep.name) {
            blockers.push(format!(
                "  {} v{} — name collision: your crate already declares `mod {}` at the crate root",
                dep.name, dep.version, dep.name
            ));
            external.push(ExternalDep {
                name: dep.name.clone(),
                version: dep.version.clone(),
                reason: ExternalReason::VendorFailed(format!(
                    "name collision with user's `mod {}`",
                    dep.name
                )),
                manifest_path: Some(dep.manifest_path.clone()),
            });
            continue;
        }

        // Multi-version.
        if let Some(existing) = version_map.get(&dep.name) {
            blockers.push(format!(
                "  {} — multiple versions resolved ({} and {}); v1 cannot disambiguate",
                dep.name, existing, dep.version
            ));
            external.push(ExternalDep {
                name: dep.name.clone(),
                version: dep.version.clone(),
                reason: ExternalReason::VendorFailed(format!(
                    "multiple versions in dep graph ({existing} and {})",
                    dep.version
                )),
                manifest_path: Some(dep.manifest_path.clone()),
            });
            continue;
        }

        let mut siblings_for_dep = sibling_idents.clone();
        siblings_for_dep.remove(&to_ident(&dep.name));
        let dep_override = expand_dump_dir
            .as_ref()
            .map(|d| d.path().join(&dep.name))
            .filter(|p| p.join("src/lib.rs").is_file());
        match vendor_one_dep(
            dep,
            &siblings_for_dep,
            dep_override.as_deref(),
            &inner_cfgs,
            &auto_externals,
        ) {
            Ok(mut vd) => {
                if dep.edition > max_edition {
                    max_edition = dep.edition;
                }
                // With --expand / --expand-deep, proc-macro expansions
                // inlined into this dep's source can carry absolute paths
                // like `::thiserror::Foo` or `::log::Foo` that target the
                // extern prelude — those don't resolve in the flat output.
                // Rewrite them to `crate::thiserror::Foo` etc. (`sibling_idents`
                // is the full vendored set, including the dep itself, in
                // case its proc-macros emit absolute paths back to it).
                if options.expand {
                    let rewritten =
                        rewrite_absolute_sibling_paths(&vd.source.to_string(), &sibling_idents);
                    vd.source = SourceFile::from_string(rewritten);
                }
                version_map.insert(vd.name.clone(), vd.version.clone());
                vendored.push(vd);
            }
            Err(reason) => {
                blockers.push(format!("  {} v{} — {}", dep.name, dep.version, reason));
                external.push(ExternalDep {
                    name: dep.name.clone(),
                    version: dep.version.clone(),
                    reason: ExternalReason::VendorFailed(reason),
                    manifest_path: Some(dep.manifest_path.clone()),
                });
            }
        }
    }

    // Add user-externals to the external list (only those actually in the
    // resolved graph; unknown names already triggered a warning earlier).
    // Curated-source names (`external_silent`) are also surfaced in the
    // report so the user sees what was cut, but unmatched ones are
    // dropped silently above.
    let mut user_explicit_present: Vec<String> = effective_external
        .iter()
        .filter(|n| dep_by_name.contains_key(n.as_str()))
        .cloned()
        .collect();
    user_explicit_present.sort();
    for name in user_explicit_present {
        let Some(dep) = dep_by_name.get(name.as_str()) else {
            continue;
        };
        external.push(ExternalDep {
            name: dep.name.clone(),
            version: dep.version.clone(),
            reason: ExternalReason::UserExcluded,
            manifest_path: Some(dep.manifest_path.clone()),
        });
    }

    // With --external-deep, vendored crates may reference deps in
    // expanded_externals that the user didn't explicitly name. Promote
    // those to the "Required" list — the user might not realise their
    // Cargo.toml needs them too.
    let mut required: HashMap<String, Vec<String>> = HashMap::new();
    for vd in &vendored {
        let Some(dep) = dep_by_name.get(vd.name.as_str()) else {
            continue;
        };
        for sub in &dep.normal_deps {
            if to_vendor.contains(sub) {
                continue; // sibling vendored mod will provide it
            }
            if effective_external.contains(sub) {
                continue; // user explicitly named it (or via preset); they know
            }
            if expanded_externals.contains(sub) {
                required
                    .entry(sub.clone())
                    .or_default()
                    .push(vd.name.clone());
            }
        }
    }
    let mut required_sorted: Vec<(String, Vec<String>)> = required.into_iter().collect();
    required_sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, mut because) in required_sorted {
        let Some(dep) = dep_by_name.get(name.as_str()) else {
            continue;
        };
        because.sort();
        because.dedup();
        external.push(ExternalDep {
            name: dep.name.clone(),
            version: dep.version.clone(),
            reason: ExternalReason::Required { because },
            manifest_path: Some(dep.manifest_path.clone()),
        });
    }

    if options.strict && !blockers.is_empty() {
        return Err(FlattenError::other(format!(
            "Cannot vendor {} dep(s):\n{}",
            blockers.len(),
            blockers.join("\n")
        )));
    }

    let mut auto_inlined_proc_macros: Vec<(String, PathBuf)> = Vec::new();
    if options.expand {
        for dep in &report.deps {
            let is_pm = matches!(
                &dep.classification,
                Classification::Unvendorable(reasons) if reasons.iter().any(|r| r == "proc-macro")
            );
            if is_pm && auto_externals.contains(&dep.name) {
                auto_inlined_proc_macros.push((dep.name.clone(), dep.manifest_path.clone()));
            }
        }
    }
    Ok(VendoredPackage {
        kind: user_pkg.kind,
        crate_name: user_pkg.crate_name,
        crate_version: report.root_version,
        target_name: user_pkg.target_name,
        user_source: user_pkg.source,
        vendored,
        external,
        max_edition,
        auto_inlined_proc_macros,
    })
}

/// BFS the dep graph from the user's root, treating any name in
/// `external` as a cut point (don't follow its edges). Returns the set
/// of dep names to consider for vendoring, plus the user-externals that
/// are actually present in the graph.
fn bfs_from_user(
    report: &VendorReport,
    external: &HashSet<String>,
) -> (HashSet<String>, Vec<String>) {
    let dep_by_name: HashMap<&str, &DepEntry> =
        report.deps.iter().map(|d| (d.name.as_str(), d)).collect();

    // Find the user's direct deps: everything in the report that's not
    // reachable only via another dep. Approximation: any dep that no
    // OTHER dep lists as a direct dep is one of the user's direct deps.
    // For a tree-rooted graph this is correct; for general graphs it's
    // a fine approximation since cargo metadata's resolver fills the
    // edges in faithfully.
    let mut user_direct: HashSet<String> = report.deps.iter().map(|d| d.name.clone()).collect();
    for dep in &report.deps {
        for sub in &dep.normal_deps {
            user_direct.remove(sub);
        }
    }

    // BFS from the user_direct set, with externals as cut points.
    let mut needed: HashSet<String> = HashSet::new();
    let mut user_externals_present: Vec<String> = Vec::new();
    let mut queue: Vec<String> = user_direct.into_iter().collect();
    while let Some(cur) = queue.pop() {
        if external.contains(&cur) {
            if !user_externals_present.contains(&cur) {
                user_externals_present.push(cur);
            }
            continue;
        }
        if !needed.insert(cur.clone()) {
            continue;
        }
        let Some(dep) = dep_by_name.get(cur.as_str()) else {
            continue;
        };
        for sub in &dep.normal_deps {
            queue.push(sub.clone());
        }
    }
    (needed, user_externals_present)
}

/// Find the byte offset in a vendored mod's source where it's safe to
/// inject `use crate::D;` lines: AFTER any leading inner attributes (incl.
/// `//!` doc comments, which lex as `#![doc = "..."]`). Returns 0 if there
/// are no inner attrs or if the source can't be parsed.
///
/// This matters for crates whose lib.rs starts with `//!` documentation
/// — naively prepending a `use` would put it before the inner doc, which
/// is rejected by rustc with E0753 "expected outer doc comment".
pub fn safe_inject_point(src: &str) -> usize {
    let Ok(file) = syn::parse_file(src) else {
        return 0;
    };
    let last_inner_attr_end = file
        .attrs
        .iter()
        .filter(|a| matches!(a.style, syn::AttrStyle::Inner(_)))
        .filter_map(|a| {
            let end = a.bracket_token.span.close().byte_range().end;
            (end > 0).then_some(end)
        })
        .max();
    let Some(end) = last_inner_attr_end else {
        return 0;
    };
    // Round up to the next line boundary so the injected `use` lands on
    // its own line, not appended to the same line as the `]`.
    let bytes = src.as_bytes();
    let mut p = end;
    while p < bytes.len() && bytes[p] != b'\n' {
        p += 1;
    }
    if p < bytes.len() {
        p += 1;
    }
    p
}

fn user_edition(crate_root: &Path) -> Option<cargo_metadata::Edition> {
    let manifest_path = crate_root.join("Cargo.toml");
    let metadata = MetadataCommand::new()
        .manifest_path(&manifest_path)
        .no_deps()
        .exec()
        .ok()?;
    metadata.packages.first().map(|p| p.edition)
}

fn vendor_one_dep(
    entry: &DepEntry,
    siblings: &HashSet<String>,
    override_root: Option<&Path>,
    sibling_inner_cfgs: &HashMap<String, String>,
    inlined_proc_macros: &HashSet<String>,
) -> std::result::Result<VendoredDep, String> {
    let cargo_manifest_dir = entry
        .manifest_path
        .parent()
        .ok_or_else(|| "manifest has no parent directory".to_string())?;
    // When --expand-deep dumps a rewritten copy of this dep's source tree
    // into `override_root`, read from there instead of the cargo cache. The
    // dump preserves the same `src/...` layout so `lib_path` resolution and
    // mod-tree scanning work unchanged.
    let manifest_dir: &Path = override_root.unwrap_or(cargo_manifest_dir);

    // TODO: support custom `[lib].path` from the dep's Cargo.toml.
    // Today we only handle deps using the default `src/lib.rs`.
    let lib_path = manifest_dir.join("src/lib.rs");
    if !lib_path.is_file() {
        return Err(format!(
            "no src/lib.rs at `{}` (custom [lib].path not yet supported)",
            manifest_dir.display()
        ));
    }

    let crate_name = to_ident(&entry.name);
    // Merge captured build-script `rustc-cfg` outputs into the
    // features set. The cfg evaluator looks up `CfgExpr::Bare(name)`
    // in this set — so e.g. `#[cfg(has_total_cmp)]` evaluates True
    // when num-traits' build script emitted `rustc-cfg=has_total_cmp`.
    let mut features = entry.features.clone();
    for cfg in &entry.build_cfgs {
        features.insert(cfg.clone());
    }
    let siblings = siblings.clone();
    // For edition-2015 deps, `use FOO::Bar;` (no path prefix) resolves
    // relative to the crate root. In 2018+ it's absolute (extern
    // prelude only). For 2015 deps we pre-scan lib.rs for items at
    // the crate root and rewrite bare `use NAME::*;` → `use crate::DEPNAME::NAME::*;`
    // wherever NAME is one of those crate-root items. (`approx`'s
    // `use AbsDiffEq;` and `ena`'s `use undo_log;` are the canonical
    // examples.)
    let is_edition_2015 = matches!(entry.edition, cargo_metadata::Edition::E2015);
    let crate_root_items = if is_edition_2015 {
        collect_crate_root_item_names(&lib_path)
    } else {
        HashSet::new()
    };
    // `extern crate FOO as BAR;` (typically at lib.rs root) puts BAR
    // into the extern prelude, accessible as `BAR::Foo` from any
    // submodule. Replacing with `use FOO as BAR;` only scopes BAR to
    // the file it's in. Pre-scan lib.rs for these aliases so the path
    // rewriter can substitute `BAR::Foo` → `FOO::Foo` everywhere.
    let aliases = collect_extern_crate_aliases(&lib_path, &features).unwrap_or_default();
    // `#[macro_use] extern crate FOO;` at lib.rs root makes FOO's
    // macros callable as bare `foo!()` from every submodule. After
    // we replace the declaration with `use FOO::*;`, that import only
    // scopes to lib.rs itself. Pre-scan and inject `use FOO::*;` at
    // the top of every file so submods see the macros too.
    let macro_use_paths = collect_macro_use_externals(&lib_path, &siblings, &features);
    // Sibling vendored crates need to be in lexical scope at every
    // call site too, otherwise proc-macro expansions that produce
    // bare `nalgebra::SVector` (like nalgebra-macros' `vector!`) fail
    // at call sites inside other vendored crates (rapier2d uses
    // nalgebra-macros' `vector!()` from many submods).
    let sibling_paths: Vec<String> = {
        let mut v: Vec<String> = siblings.iter().map(|n| format!("crate::{n}")).collect();
        v.sort();
        v
    };
    let mut all_inject_paths = macro_use_paths.clone();
    for s in &sibling_paths {
        if !all_inject_paths.contains(s) {
            all_inject_paths.push(s.clone());
        }
    }
    let extern_std_libs_seen: std::sync::Mutex<HashSet<String>> =
        std::sync::Mutex::new(HashSet::new());
    // Pre-scan every .rs file under this dep's `src/` (recursively) for
    // `macro_rules!` definitions whose matcher accepts a flat item
    // list (`($($i:item)*)` etc.). Tokio's `cfg_io_driver!`,
    // `cfg_not_wasip1!`, etc. all match this shape; their bodies
    // emit `#[cfg(...)] $item` per element. Knowing the set lets
    // the cfg-attr rewriter recurse into invocations of these macros
    // — the cfgs in their args sit in item-position and need baking
    // against vendor-time features. Without this, those cfgs stay
    // verbatim and evaporate at user compile time (the user crate
    // doesn't enable the dep's `feature = "net"` etc.).
    //
    // Whole-dep scan because macro definitions and invocations can
    // live in different files; the per-file rewrite closure can't
    // see the definitions on its own.
    let macro_scan = scan_dep_for_item_list_macros(manifest_dir);
    let rewrite = |src: &str| -> Result<String> {
        for lib in collect_extern_std_libs(src) {
            extern_std_libs_seen.lock().unwrap().insert(lib);
        }
        // Edition-2015 deps in a vendored `pub mod foo { ... }` block
        // get compiled at the OUTER user crate's edition. `try!()` was
        // a built-in macro in 2015 but became unusable in 2018+ — `try`
        // is a reserved keyword. Rewrite `try!(EXPR)` → `(EXPR)?` so
        // the 2015 dep compiles under any outer edition.
        let try_owned;
        let src: &str = if is_edition_2015 && src.contains("try!(") {
            try_owned = rewrite_try_macro(src);
            &try_owned
        } else {
            src
        };
        // Also pre-syn rewrite bare-trait-object type aliases for any
        // dep — `type T = Trait + Send + Sync;` shape fails syn 2.x
        // outright. signal-hook-registry surfaces this; the rewriter
        // is conservative (only fires on top-level `+` after `=`).
        let dyn_owned;
        let src: &str = if src.contains(" + ") && src.contains("type ") {
            dyn_owned = rewrite_bare_trait_object_aliases(src);
            &dyn_owned
        } else {
            src
        };
        // Pre-syn rewrite `:pat` → `:pat_param` when followed by `|`
        // (or last-in-group inside a repetition). The 2021+ `pat`
        // matcher rejects this shape; tokio's select! and several DSL
        // macros use it. Strict subset semantics — safe to widen
        // unconditionally.
        let pat_owned;
        let src: &str = if src.contains(":pat") {
            pat_owned = rewrite_pat_followed_by_pipe(src);
            &pat_owned
        } else {
            src
        };
        let rewritten = rewrite_for_vendoring(
            src,
            &crate_name,
            &features,
            &siblings,
            &aliases,
            &crate_root_items,
            &macro_scan.item_list_macros,
            &macro_scan.paired_skip_bake,
            &macro_scan.ident_pair_matcher_macros,
            &macro_scan.macro_export_names,
            inlined_proc_macros,
        )?;
        Ok(inject_imports(
            &rewritten,
            &macro_use_paths,
            &sibling_paths,
            &macro_scan.item_list_macros,
            sibling_inner_cfgs,
        ))
    };
    let opts = ParseOptions {
        rewrite_source: Some(&rewrite),
        out_dir: entry.out_dir.clone(),
    };
    let source = SourceFile::from_file_with_options(&lib_path, manifest_dir, &opts)
        .map_err(|e| format!("{e}"))?;

    let rendered = source.to_string();
    let macro_exports = collect_macro_export_paths(&rendered);
    let mut extern_std_libs: Vec<String> = extern_std_libs_seen
        .into_inner()
        .unwrap()
        .into_iter()
        .collect();
    extern_std_libs.sort();

    // Capture this dep's own lib.rs `#![cfg(...)]` predicate so the
    // assembler can use it elsewhere (currently informational, also
    // used downstream by the dep-list display).
    let inner_cfg = sibling_inner_cfgs.get(&crate_name).cloned();

    Ok(VendoredDep {
        name: entry.name.clone(),
        version: entry.version.clone(),
        source,
        normal_deps: entry.normal_deps.clone(),
        macro_exports,
        extern_std_libs,
        inner_cfg,
    })
}

/// Walk the assembled vendored source and produce a ready-to-write
/// `pub(crate) use ...;` statement for every macro that was originally
/// `#[macro_export]`. Detected by the marker pattern emitted by
/// [`collect_macro_export_rewrites`]: a `macro_rules!` item immediately
/// followed by `pub(crate) use NAME;` at the same scope.
///
/// If any ancestor mod carries `#[cfg(...)]`, the cfg predicates get
/// `all(...)`-joined and prepended to the lifted re-export so the lift
/// only fires under configurations where the source mod is actually
/// active. Without this, `#[cfg(target_arch = "x86_64")] mod x86_64`
/// containing a macro_export'd `dispatch` would generate a lift
/// `pub(crate) use x86_64::dispatch;` that fails on non-x86 builds.
fn collect_macro_export_paths(src: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let mut found: Vec<String> = Vec::new();
    walk_for_macro_exports(&file.items, &mut Vec::new(), &mut Vec::new(), &mut found);
    found
}

fn walk_for_macro_exports(
    items: &[syn::Item],
    path: &mut Vec<String>,
    cfg_stack: &mut Vec<String>,
    found: &mut Vec<String>,
) {
    for window in items.windows(2) {
        // Detect the marker emitted by `collect_macro_export_rewrites`:
        // a `macro_rules!` followed by `pub(crate) use NAME;` at the
        // same scope, with NAME matching the macro's ident.
        if let (syn::Item::Macro(m), syn::Item::Use(u)) = (&window[0], &window[1])
            && m.mac.path.is_ident("macro_rules")
            && let Some(name) = &m.ident
            && let syn::Visibility::Restricted(r) = &u.vis
            && r.in_token.is_none()
            && r.path.is_ident("crate")
            && let syn::UseTree::Name(use_name) = &u.tree
            && &use_name.ident == name
        {
            // When the macro lives at the dep's root (no parent mods),
            // the rewriter already emits `pub(crate) use NAME;` at the
            // root, so a lift would just duplicate the binding.
            if path.is_empty() {
                continue;
            }
            let mut full = path.clone();
            full.push(name.to_string());
            let path_str = full.join("::");
            // Combine the parent-mod cfg context with any `#[cfg(...)]`
            // on the macro_rules item itself. `collect_macro_export_rewrites`
            // carries those cfgs onto the inner `pub(crate) use NAME;`,
            // so the lifted re-export must match — otherwise an
            // unconditional lift can reference a name that's only
            // conditionally in scope (allocator_api2's
            // `#[cfg(feature="alloc")] macro_rules! unsize_box` is the
            // canonical case).
            let mut all_cfgs: Vec<String> = cfg_stack.clone();
            all_cfgs.extend(mod_cfg_predicates(&m.attrs));
            let stmt = if all_cfgs.is_empty() {
                format!("pub(crate) use {path_str};")
            } else if all_cfgs.len() == 1 {
                format!("#[cfg({})] pub(crate) use {path_str};", all_cfgs[0])
            } else {
                format!(
                    "#[cfg(all({}))] pub(crate) use {path_str};",
                    all_cfgs.join(", ")
                )
            };
            found.push(stmt);
        }
    }
    for item in items {
        if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            let pushed = mod_cfg_predicates(&m.attrs);
            for p in &pushed {
                cfg_stack.push(p.clone());
            }
            path.push(m.ident.to_string());
            walk_for_macro_exports(inner, path, cfg_stack, found);
            path.pop();
            for _ in &pushed {
                cfg_stack.pop();
            }
        }
    }
}

/// Extract `#[cfg(...)]` predicates from a mod's attribute list as
/// already-stringified token streams (one per cfg attr).
fn mod_cfg_predicates(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|a| {
            if !a.path().is_ident("cfg") {
                return None;
            }
            let syn::Meta::List(list) = &a.meta else {
                return None;
            };
            Some(list.tokens.to_string())
        })
        .collect()
}

// Cfg expression evaluator (`CfgExpr`, `eval_cfg`, `simplify_cfg_expr`,
// `parse_cfg_expr`, `format_cfg_expr`) and the `cfg_if!` expander
// (`expand_cfg_if`) live in `crate::cfg`. Re-exported here for backwards
// compatibility with external callers that imported them from `vendor`.
pub use crate::cfg::expand_cfg_if;
use crate::cfg::{
    CfgEval, cfg_expr_references_feature, eval_cfg, format_cfg_expr, parse_cfg_expr,
    parse_one_cfg_expr, simplify_cfg_expr, split_top_level_commas,
};

/// Apply vendoring rewrites to one source file:
///   - `#[cfg(feature = …)]` evaluated against `features`; gates resolved
///   - `#[cfg_attr(feature = …, ATTRS)]` evaluated; pred replaced with all()/any()
///   - `crate::*` paths → `crate::<crate_name>::*` (skipping `pub(crate)`)
///   - `extern crate …;` items removed
///   - `$crate` inside `macro_rules!` bodies rewritten to
///     `$crate::<crate_name>` (handled in `collect_dollar_crate_rewrites`)
///   - `#[macro_export]` macros stripped of the attribute and lifted via
///     a synthesised `pub(crate) use NAME;` (handled in
///     `collect_macro_export_rewrites`)
///
/// Returns Err only on bare `$crate` references that aren't followed
/// by `::` — those resolve to the source crate's root, which the
/// rewrite to `crate::<crate_name>` would semantically change.
#[allow(clippy::too_many_arguments)]
fn rewrite_for_vendoring(
    src: &str,
    crate_name: &str,
    features: &HashSet<String>,
    siblings: &HashSet<String>,
    aliases: &HashMap<String, String>,
    crate_root_items: &HashSet<String>,
    extra_item_list_macros: &HashSet<String>,
    paired_skip_bake: &HashSet<String>,
    ident_pair_matcher_macros: &HashSet<String>,
    macro_export_names: &HashSet<String>,
    inlined_proc_macros: &HashSet<String>,
) -> Result<String> {
    // Expand `cfg_if! { ... }` invocations to plain `#[cfg(...)]` items
    // BEFORE running any other pass. The scanner that runs after this
    // closure walks the syn AST looking for `mod NAME;` declarations,
    // and syn doesn't see inside macro bodies. Without this expansion,
    // `mod imp;` decls inside `cfg_if! {}` go uninlined, and rustc
    // can't find the file in the user's tree.
    let expanded = expand_cfg_if(src);
    let src = expanded.as_str();
    let file = syn::parse_file(src).map_err(|e| {
        // Carry the failing line:col into the error — `expected ;`
        // from syn alone is hard to debug once we've expanded macros
        // and run pre-syn rewrites, since the failure usually sits
        // hundreds of lines deep.
        let start = e.span().start();
        FlattenError::other(format!(
            "syn parse failed in vendored source at line {}:{} — {e}",
            start.line, start.column,
        ))
    })?;

    // Phase 1: figure out which whole items get deleted. Includes
    // cfg-False items, stdlib aliases (`pub use X as core;`), and
    // `extern crate` items that get fully stripped (no replacement).
    // Subsequent passes skip anything inside these ranges so we never
    // emit overlapping edits.
    let mut deleted: Vec<Range<usize>> = Vec::new();
    walk_items_for_deletion(&file.items, features, &mut deleted);
    collect_stdlib_alias_deletions(&file, &mut deleted);
    collect_extern_crate_strip_deletions(&file, siblings, &mut deleted);
    // The builtin-attr macro call rewrites overlap (subsume) the
    // sibling-path rewrites — both pass want to edit the leading
    // `SIBLING` ident. Pre-emit the macro-call rewrites and add
    // their spans to `deleted` so other passes skip.
    let mut early_edits: Vec<(Range<usize>, String)> = Vec::new();
    collect_builtin_attr_macro_call_rewrites(&file, siblings, &deleted, &mut early_edits);
    for (range, _) in &early_edits {
        deleted.push(range.clone());
    }

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    collect_cfg_attr_rewrites(&file, features, &deleted, &mut edits);
    collect_macro_body_cfg_rewrites(
        &file,
        features,
        extra_item_list_macros,
        paired_skip_bake,
        &deleted,
        &mut edits,
    );
    collect_dollar_crate_rewrites(&file, crate_name, siblings, &deleted, &mut edits)?;
    collect_macro_export_rewrites(&file, features, &deleted, &mut edits);
    collect_wrapper_macro_export_rewrites(&file, &deleted, &mut edits);
    // Cross-file companion to `walk_macro_export_rewrites`'s in-file
    // `pub use NAME;` demoter: catches `pub use NAME;` and
    // `pub use NAME as ALIAS;` re-exports of macros defined in OTHER
    // files in the same dep. anyhow's `lib.rs` has
    // `pub use anyhow as format_err;` re-exporting the macro defined
    // in `macros.rs`; without this pass, the re-export becomes E0364
    // (pub use of a pub(crate) item) once we strip `#[macro_export]`
    // and lift the macro as `pub(crate) use macros::anyhow`.
    collect_cross_file_macro_export_demotions(&file, macro_export_names, &deleted, &mut edits);
    // Edition-2024 default-binding-mode rule: when the scrutinee of a
    // match is a `&` or `&mut` reference, a non-reference arm pattern
    // implicitly borrows; combining that with a `mut FIELD` binding is
    // rejected. Pre-2024 source like anyhow's `Chain::next_back` does
    // `match &mut self.state { Linked { mut next } => … }` and now
    // needs `&mut Linked { mut next }`. Add the explicit reference
    // prefix here so vendored deps stay compatible across editions.
    collect_implicit_borrow_match_rewrites(&file, &deleted, &mut edits);
    // Add the builtin-attr macro call rewrites collected in phase 1.
    // They were collected early so their spans could be added to
    // `deleted`; the actual edits go in here.
    edits.extend(early_edits);
    collect_crate_path_rewrites(&file, crate_name, &deleted, &mut edits);
    collect_sibling_use_rewrites(&file, siblings, aliases, &deleted, &mut edits);
    collect_edition_2015_bare_path_rewrites(
        &file,
        crate_name,
        crate_root_items,
        siblings,
        aliases,
        &deleted,
        &mut edits,
    );
    collect_macro_invocation_token_rewrites(
        &file,
        crate_name,
        siblings,
        aliases,
        ident_pair_matcher_macros,
        &deleted,
        &mut edits,
    );
    collect_macro_doc_attr_removals(&file, &deleted, &mut edits);
    collect_lint_attr_removals(&file, &deleted, &mut edits);
    collect_mod_visibility_bumps(&file, &deleted, &mut edits);
    collect_extern_crate_reexport_downgrades(&file, &deleted, &mut edits);
    collect_extern_crate_removals(&file, siblings, inlined_proc_macros, &deleted, &mut edits);

    // Add deletion edits last; they don't overlap each other or the
    // skip-checked edits above.
    for r in deleted {
        edits.push((r, String::new()));
    }

    if edits.is_empty() {
        return Ok(src.to_string());
    }
    // Defensive dedup: when two passes both decide to strip the same
    // attribute (e.g. cfg + extern_crate both targeting `#[cfg(...)]`),
    // applying the duplicate edit causes the second `replace_range` to
    // over-delete adjacent bytes — the first replace already shortened
    // the string under it. Dedupe identical (range, replacement) pairs.
    edits.sort_by(|a, b| {
        a.0.start
            .cmp(&b.0.start)
            .then(a.0.end.cmp(&b.0.end))
            .then(a.1.cmp(&b.1))
    });
    edits.dedup();
    // When two edits target the same exact range with different
    // replacements, the deletion-list mechanism (which converts every
    // entry in `deleted` to an empty replacement) can collide with
    // a real replacement edit. Prefer the non-empty replacement.
    let mut i = 0;
    while i + 1 < edits.len() {
        if edits[i].0 == edits[i + 1].0 {
            if edits[i].1.is_empty() && !edits[i + 1].1.is_empty() {
                edits.remove(i);
            } else if !edits[i].1.is_empty() && edits[i + 1].1.is_empty() {
                edits.remove(i + 1);
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    let mut out = src.to_string();
    for (range, replacement) in edits.iter().rev() {
        out.replace_range(range.clone(), replacement);
    }
    Ok(out)
}

fn is_in_deleted(span: &Range<usize>, deleted: &[Range<usize>]) -> bool {
    deleted
        .iter()
        .any(|r| r.start <= span.start && span.end <= r.end)
}

fn collect_top_level_mod_names(source: &SourceFile) -> HashSet<String> {
    // Parse the user's flattened source to find top-level mod NAME items.
    // Use the rendered string (Display) so inlined mods are visible too.
    let rendered = source.to_string();
    let Ok(file) = syn::parse_file(&rendered) else {
        return HashSet::new();
    };
    file.items
        .iter()
        .filter_map(|item| {
            if let syn::Item::Mod(m) = item {
                Some(m.ident.to_string())
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// AST passes for the rewriter and the dealbreaker checks
// ---------------------------------------------------------------------------

/// Per-attribute rewriter for surviving (non-deleted) cfg / cfg_attr attrs.
/// True cfgs get stripped; False cfgs in non-item positions get force-off
/// (only reached for fields/variants/exprs since item-level Falses were
/// resolved into deletions). cfg_attr handled likewise.
fn collect_cfg_attr_rewrites(
    file: &syn::File,
    features: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        features: &'a HashSet<String>,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'a> V<'a> {
        fn process(&mut self, attr: &syn::Attribute) {
            let Some(span) = attr_byte_range(attr) else {
                return;
            };
            if is_in_deleted(&span, self.deleted) {
                return;
            }
            // Preserve inner-vs-outer attribute style: `#![...]` must
            // stay `#![...]`. Stripping the `!` would silently turn an
            // inner attribute into an outer one — at best a parse error
            // a few tokens later, at worst a semantic change.
            let bang = match attr.style {
                syn::AttrStyle::Outer => "",
                syn::AttrStyle::Inner(_) => "!",
            };
            if attr.path().is_ident("cfg") {
                let syn::Meta::List(list) = &attr.meta else {
                    return;
                };
                let expr = parse_cfg_expr(&list.tokens);
                match eval_cfg(&expr, self.features) {
                    CfgEval::True => self.edits.push((span, String::new())),
                    CfgEval::False => {
                        self.edits.push((span, format!("#{bang}[cfg(any())]")));
                    }
                    CfgEval::Unknown => {
                        // Partial eval: simplify `cfg(all(True, X))` → `cfg(X)`
                        // (and similar for any/not). Without this,
                        // `cfg(all(feature = "alloc", not(no_global_oom_handling)))`
                        // — which we know `feature = "alloc"` is True
                        // for — survives verbatim, and at user compile
                        // time `feature = "alloc"` evaluates False
                        // (the user's downstream Cargo.toml doesn't
                        // know about the dep's features), gating out
                        // items the dep needed.
                        let simplified = simplify_cfg_expr(&expr, self.features);
                        if let Some(s) = simplified
                            && s != format_cfg_expr(&expr)
                        {
                            self.edits.push((span, format!("#{bang}[cfg({s})]")));
                        }
                    }
                }
                return;
            }
            if attr.path().is_ident("cfg_attr") {
                let syn::Meta::List(list) = &attr.meta else {
                    return;
                };
                let segments = split_top_level_commas(&list.tokens);
                if segments.is_empty() {
                    return;
                }
                let pred = parse_one_cfg_expr(&segments[0]);
                match eval_cfg(&pred, self.features) {
                    CfgEval::True => {
                        let attrs_str = segments[1..]
                            .iter()
                            .map(|seg| {
                                seg.iter()
                                    .cloned()
                                    .collect::<proc_macro2::TokenStream>()
                                    .to_string()
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.edits
                            .push((span, format!("#{bang}[cfg_attr(all(), {attrs_str})]")));
                    }
                    CfgEval::False => self.edits.push((span, String::new())),
                    CfgEval::Unknown => {}
                }
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            self.process(attr);
        }
    }
    let mut v = V {
        features,
        edits,
        deleted,
    };
    v.visit_file(file);
}

/// Evaluate `#[cfg(feature = …)]` attributes that appear inside
/// `macro_rules!` bodies (or any non-`macro_rules!` macro invocation
/// tokens) and rewrite True/False outcomes the same way
/// [`collect_cfg_attr_rewrites`] does for AST-visible attrs. Without
/// this, a macro whose body says `#[cfg(not(feature = "std"))] impl Foo
/// for $ty` would expand to a real `cfg(not(feature = "std"))` at the
/// user's call site — and `feature = "std"` there refers to the user's
/// crate, not the original dep's. That can re-enable code paths that
/// reference items we deleted (e.g. a trait gated by the same cfg).
fn collect_macro_body_cfg_rewrites(
    file: &syn::File,
    features: &HashSet<String>,
    extra_item_list_macros: &HashSet<String>,
    paired_skip_bake: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        features: &'a HashSet<String>,
        /// Reserved for the future paired-macro skip-bake pass.
        /// `paired_skip_bake` from `scan_dep_for_item_list_macros`
        /// flows here (still computed at scan time so the field is
        /// at least read and validated). `skip_feature_bake` would
        /// be flipped per-macro_rules to suppress baking of cfgs that
        /// reference Cargo features. The naive
        /// always-skip-bake-for-paired approach over-gates feature-
        /// only macros (cfg_io_driver, cfg_net, etc.) — see ROADMAP.
        #[allow(dead_code)]
        paired_skip_bake: &'a HashSet<String>,
        #[allow(dead_code)]
        skip_feature_bake: bool,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'a> V<'a> {
        /// Detect bare `cfg(EXPR)` token sequences in macro argument
        /// positions and bake them like attribute-form cfgs. Either
        /// crate's `impl_specific_ref_and_mut!(::std::path::Path,
        /// cfg(feature = "std"), doc = "...")` is the canonical
        /// case: the macro accepts `$($attr:meta)*` and emits
        /// `#[$attr]` per arg, so the bare `cfg(...)` arg becomes a
        /// `#[cfg(...)]` on the emitted impl. At user compile time
        /// `feature = "std"` is False (the user has no either Cargo
        /// features) and the impl gets gated out — leaving only the
        /// generic `AsRef<Target>` impl which can't handle unsized
        /// targets, producing the `[u8] cannot be known at
        /// compilation time` errors. Baking at vendor time
        /// `cfg(feature = "std")` → `cfg(all())` keeps the impl
        /// always present.
        ///
        /// Skip if preceded by `#` or `:` (already inside an attr or
        /// a path), or by `if`/`else` (cfg_if!-style DSL).
        fn maybe_bake_bare_cfg(&mut self, toks: &[TokenTree], i: usize) -> bool {
            let TokenTree::Ident(id) = &toks[i] else {
                return false;
            };
            if id != "cfg" {
                return false;
            }
            let Some(TokenTree::Group(args)) = toks.get(i + 1) else {
                return false;
            };
            if args.delimiter() != proc_macro2::Delimiter::Parenthesis {
                return false;
            }
            // Skip when preceded by `#` (it's `#[cfg(...)]`, handled
            // by the attr-form branch below) or `:` (`crate::cfg`
            // would only show up after `:` in a path, but defensive).
            if let Some(prev) = toks.get(i.wrapping_sub(1)) {
                if let TokenTree::Punct(p) = prev
                    && matches!(p.as_char(), '#' | ':')
                {
                    return false;
                }
                // Skip cfg_if-style: `if cfg(...)` or `else if
                // cfg(...)`. Either keyword in prev → DSL syntax.
                if let TokenTree::Ident(p) = prev
                    && (p == "if" || p == "else")
                {
                    return false;
                }
            }
            let id_span = id.span().byte_range();
            let args_span = args.span().byte_range();
            let span = id_span.start..args_span.end;
            if span.end <= span.start || is_in_deleted(&span, self.deleted) {
                return false;
            }
            let expr = parse_cfg_expr(&args.stream());
            // Only bake when the expression references a Cargo
            // feature — bare `cfg(unix)` etc. would still evaluate
            // correctly at user time without baking, so leave them
            // alone to minimise blast radius. The motivating case
            // (either's `cfg(feature = "std")`) always references a
            // feature.
            if !cfg_expr_references_feature(&expr) {
                return false;
            }
            match eval_cfg(&expr, self.features) {
                CfgEval::True => {
                    self.edits.push((span, "cfg(all())".to_string()));
                    true
                }
                CfgEval::False => {
                    self.edits.push((span, "cfg(any())".to_string()));
                    true
                }
                CfgEval::Unknown => false,
            }
        }
        fn process(&mut self, ts: &proc_macro2::TokenStream) {
            let toks: Vec<TokenTree> = ts.clone().into_iter().collect();
            for i in 0..toks.len() {
                // Bare-arg form: `cfg(EXPR)` not preceded by `#`
                // (i.e., used as a macro argument that the macro
                // body re-emits as `#[$attr]`).
                if self.maybe_bake_bare_cfg(&toks, i) {
                    continue;
                }
                if let TokenTree::Punct(p) = &toks[i]
                    && p.as_char() == '#'
                {
                    // Inner attr (`#![...]`) has a `!` token between
                    // `#` and the bracket group; outer attr (`#[...]`)
                    // doesn't. Detect and preserve the style.
                    let (bang, bracket_idx) = match toks.get(i + 1) {
                        Some(TokenTree::Punct(b)) if b.as_char() == '!' => ("!", i + 2),
                        _ => ("", i + 1),
                    };
                    let Some(TokenTree::Group(bracket)) = toks.get(bracket_idx) else {
                        continue;
                    };
                    if bracket.delimiter() != proc_macro2::Delimiter::Bracket {
                        continue;
                    }
                    let inner: Vec<TokenTree> = bracket.stream().into_iter().collect();
                    if let (Some(TokenTree::Ident(id)), Some(TokenTree::Group(args))) =
                        (inner.first(), inner.get(1))
                        && args.delimiter() == proc_macro2::Delimiter::Parenthesis
                    {
                        let attr_name = id.to_string();
                        let pound_span = p.span().byte_range();
                        let bracket_span = bracket.span().byte_range();
                        let attr_span = pound_span.start..bracket_span.end;
                        if attr_span.end > attr_span.start
                            && !is_in_deleted(&attr_span, self.deleted)
                        {
                            // Inside macro_rules! bodies, NEVER strip
                            // an attribute outright — that can leave an
                            // empty `$(  )?` repetition which doesn't
                            // parse. Always preserve the attribute's
                            // syntactic shape with an always-true /
                            // always-false replacement.
                            if attr_name == "cfg" {
                                let expr = parse_cfg_expr(&args.stream());
                                let refs_feat =
                                    self.skip_feature_bake && cfg_expr_references_feature(&expr);
                                match eval_cfg(&expr, self.features) {
                                    CfgEval::True if refs_feat => {}
                                    CfgEval::True => {
                                        self.edits
                                            .push((attr_span, format!("#{bang}[cfg(all())]")));
                                    }
                                    CfgEval::False if refs_feat => {}
                                    CfgEval::False => {
                                        self.edits.push((attr_span, format!("#{bang}[cfg(any())]")))
                                    }
                                    CfgEval::Unknown => {
                                        // Partial eval: substitute
                                        // feature predicates with
                                        // `true` / `false` literals
                                        // so the user's downstream
                                        // compile (with no features)
                                        // sees the consistent answer.
                                        // Critical for tokio's
                                        // `cfg_not_signal_internal!`
                                        // body — without baking the
                                        // feature predicates here,
                                        // the negation evaluates
                                        // True at user time and
                                        // duplicates the positive
                                        // pair member's items.
                                        //
                                        // ONLY fire when the cfg
                                        // actually mentions a Cargo
                                        // feature — otherwise we
                                        // could mangle macro_rules
                                        // bodies that contain raw
                                        // `$($m,)*` template tokens
                                        // (nalgebra has these in
                                        // some `make_simd!` /
                                        // `paste!`-friendly cfg
                                        // attrs that simplify can't
                                        // round-trip).
                                        if !cfg_expr_references_feature(&expr) {
                                            return;
                                        }
                                        let simplified = simplify_cfg_expr(&expr, self.features);
                                        if let Some(s) = simplified
                                            && s != format_cfg_expr(&expr)
                                        {
                                            self.edits
                                                .push((attr_span, format!("#{bang}[cfg({s})]")));
                                        }
                                    }
                                }
                            } else if attr_name == "cfg_attr" {
                                let segments = split_top_level_commas(&args.stream());
                                if let Some(first) = segments.first() {
                                    let pred = parse_one_cfg_expr(first);
                                    let refs_feat = self.skip_feature_bake
                                        && cfg_expr_references_feature(&pred);
                                    let attrs_str = segments[1..]
                                        .iter()
                                        .map(|seg| {
                                            seg.iter()
                                                .cloned()
                                                .collect::<proc_macro2::TokenStream>()
                                                .to_string()
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    match eval_cfg(&pred, self.features) {
                                        CfgEval::True if refs_feat => {}
                                        CfgEval::True => {
                                            self.edits.push((
                                                attr_span,
                                                format!("#{bang}[cfg_attr(all(), {attrs_str})]"),
                                            ));
                                        }
                                        CfgEval::False if refs_feat => {}
                                        CfgEval::False => self.edits.push((
                                            attr_span,
                                            format!("#{bang}[cfg_attr(any(), {attrs_str})]"),
                                        )),
                                        CfgEval::Unknown => {}
                                    }
                                }
                            }
                        }
                    }
                }
                if let TokenTree::Group(g) = &toks[i] {
                    // Skip recursion into Bracket groups — those are
                    // attribute bodies (`[cfg(...)]`) that the outer
                    // # handler already processed at the attr-form
                    // level. Recursing in would re-trigger the bare-
                    // cfg-arg path on the same `cfg(...)` and produce
                    // an overlapping edit that corrupts adjacent
                    // tokens (bytemuck's
                    // `#[cfg(feature = "zeroable_unwind_fn")]
                    // impl_for_unwind_fn!(...)` was the canonical
                    // case — partial overlap of feature name and
                    // macro name).
                    if g.delimiter() != proc_macro2::Delimiter::Bracket {
                        self.process(&g.stream());
                    }
                }
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            // `macro_rules!` DEFINITIONS get expanded at the user's
            // call site, so the cfg attrs inside need baking now
            // (against the dep's vendor-time features). Many other
            // macros (cfg_if!, pick!, …) treat `#[cfg(...)]` as part
            // of their input DSL; stripping a True cfg from a
            // `pick! { if #[cfg(X)] { … } else if #[cfg(Y)] { … } }`
            // call leaves the call malformed — only process them if
            // the macro is known to accept a flat item list.
            if im.mac.path.is_ident("macro_rules") {
                self.process(&im.mac.tokens);
                return;
            }
            // Macro INVOCATION at item position (`cfg_not_wasip1! {
            // #[cfg(feature = "net")] pub(crate) use foo; }`). If
            // the macro is one of our detected item-list macros,
            // process its body — the cfg attrs inside sit in
            // item-position and need baking against vendor-time
            // features.
            // Process the macro body: bake both `#[cfg(...)]` attrs
            // AND bare `cfg(...)` args. The cfg-attr branch handles
            // common third-party DSL macros (bitflags!, lazy_static!)
            // that emit `#[cfg(...)] field` from input tokens. The
            // bare-cfg-arg branch handles either's
            // `impl_specific_ref_and_mut!(Ty, cfg(feature = "std"))`
            // pattern. Even cfg_if! / pick! style DSL macros are OK
            // here: their `#[cfg(...)]` predicates evaluate the same
            // way at vendor time as they would at user compile time
            // (modulo our known limitations around `loom` etc., which
            // we already handle).
            self.process(&im.mac.tokens);
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            // Same as visit_item_macro for non-item-position macro
            // invocations (e.g. inside expression blocks).
            if mac.path.is_ident("macro_rules") {
                return;
            }
            self.process(&mac.tokens);
        }
    }
    // The visitor processes EVERY non-macro_rules invocation (the
    // bare-cfg-arg + cfg-attr passes are conservative enough to be
    // safe across DSL macros). `extra_item_list_macros` from the
    // cross-file pre-scan is no longer consulted here, but kept on
    // the function signature because `inject_imports`'s
    // `mods_inside_item_list_macros` pass DOES need the set.
    let _ = extra_item_list_macros;
    let mut v = V {
        features,
        paired_skip_bake,
        skip_feature_bake: false,
        edits,
        deleted,
    };
    v.visit_file(file);
}

/// Recursively walk `manifest_dir/src` collecting the names of every
/// `macro_rules!` definition whose matcher accepts `$($i:item)*`.
/// Used by `collect_macro_body_cfg_rewrites` to know which macro
/// invocations are safe to recurse into for cfg-attr baking — see
/// the comment on `collect_item_list_macro_names`.
/// Pre-scan results for the per-dep cfg-attr rewriting passes.
struct DepMacroScan {
    /// `macro_rules!` whose matcher accepts `$($i:item)*` — safe to
    /// recurse into invocations for cfg-attr baking.
    item_list_macros: HashSet<String>,
    /// Macros whose body cfgs should NOT be baked because a paired
    /// `cfg_not_X` (or vice versa) macro exists. Baking the positive
    /// branch's True cfg leaves the negation's Unknown cfg active too,
    /// producing duplicate definitions at user compile time. Tokio's
    /// `cfg_signal_internal_and_unix!` / `cfg_not_signal_internal!`
    /// pair (both define `create_signal_driver`) is the canonical
    /// case. Detected by name pattern: `cfg_X` ↔ `cfg_not_X`.
    paired_skip_bake: HashSet<String>,
    /// `macro_rules!` whose matcher uses the `$IDENT:ident ::
    /// $IDENT:ident` literal pattern — invocations of these MUST
    /// receive the source's bare two-ident form, not our rewritten
    /// `crate::IDENT::IDENT` three-ident form (the matcher rejects).
    /// mio's `debug_detail!` and `impl_debug!` are the canonical
    /// cases. Cross-file scan because the macro definition and call
    /// site usually live in different files.
    ident_pair_matcher_macros: HashSet<String>,
    /// Names of `#[macro_export] macro_rules! NAME` declarations
    /// (anywhere in the dep). After flatten strips `#[macro_export]`
    /// and lifts the macro via `pub(crate) use NAME;`, any sibling
    /// `pub use NAME;` / `pub use NAME as ALIAS;` re-export becomes
    /// E0364 (cannot re-export a `pub(crate)` item as `pub`). Used
    /// by `collect_macro_export_reexport_demotions` to demote those
    /// re-exports to `pub(crate)`. Cross-file because the macro
    /// definition and re-export commonly live in different files
    /// (anyhow's `macros.rs` defines the macro; `lib.rs` re-exports
    /// it under aliases). Names with built-in-attr collisions
    /// (`warn`, `deny`, …) are excluded — they keep `#[macro_export]`
    /// per `walk_macro_export_rewrites`.
    macro_export_names: HashSet<String>,
}

fn scan_dep_for_item_list_macros(manifest_dir: &Path) -> DepMacroScan {
    let mut item_list = HashSet::new();
    let mut all_macros = HashSet::new();
    let mut ident_pair = HashSet::new();
    let mut macro_export_names = HashSet::new();
    let src = manifest_dir.join("src");
    if src.is_dir() {
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if !text.contains("macro_rules!") {
                    continue;
                }
                let Ok(file) = syn::parse_file(&text) else {
                    continue;
                };
                for name in collect_item_list_macro_names(&file) {
                    item_list.insert(name);
                }
                for name in collect_all_macro_rules_names(&file) {
                    all_macros.insert(name);
                }
                for name in collect_ident_pair_matcher_macro_names(&file) {
                    ident_pair.insert(name);
                }
                for name in collect_macro_export_names(&file) {
                    macro_export_names.insert(name);
                }
            }
        }
    }
    // Derive paired set: for any X, if both `cfg_X` and `cfg_not_X`
    // exist, mark BOTH as skip-bake. The naming convention is
    // tokio-specific but matches the structural pattern (mutually-
    // exclusive expansions of the same item names) wherever it
    // appears.
    let mut paired_skip_bake = HashSet::new();
    for name in &all_macros {
        if let Some(rest) = name.strip_prefix("cfg_not_") {
            let positive = format!("cfg_{rest}");
            if all_macros.contains(&positive) {
                paired_skip_bake.insert(positive);
                paired_skip_bake.insert(name.clone());
            }
        }
    }
    DepMacroScan {
        item_list_macros: item_list,
        paired_skip_bake,
        ident_pair_matcher_macros: ident_pair,
        macro_export_names,
    }
}

/// Names of `#[macro_export] macro_rules! NAME` items anywhere in this
/// file, excluding names that collide with built-in attributes (those
/// keep `#[macro_export]` per `walk_macro_export_rewrites`).
fn collect_macro_export_names(file: &syn::File) -> HashSet<String> {
    use syn::visit::Visit;
    struct V {
        names: HashSet<String>,
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            if !im.mac.path.is_ident("macro_rules") {
                return;
            }
            let Some(name) = &im.ident else { return };
            let name_str = name.to_string();
            if is_builtin_attr_macro_name(&name_str) {
                return;
            }
            if im.attrs.iter().any(|a| a.path().is_ident("macro_export")) {
                self.names.insert(name_str);
            }
        }
    }
    let mut v = V {
        names: HashSet::new(),
    };
    v.visit_file(file);
    v.names
}

/// Names of every `macro_rules!` definition in the file, regardless
/// of matcher shape. Used to detect cfg_X ↔ cfg_not_X pairs.
fn collect_all_macro_rules_names(file: &syn::File) -> HashSet<String> {
    use syn::visit::Visit;
    struct V {
        names: HashSet<String>,
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            if !im.mac.path.is_ident("macro_rules") {
                return;
            }
            if let Some(name) = &im.ident {
                self.names.insert(name.to_string());
            }
        }
    }
    let mut v = V {
        names: HashSet::new(),
    };
    v.visit_file(file);
    v.names
}

/// Identify `macro_rules!` definitions whose FIRST matcher arm
/// accepts a flat item list — `($($IDENT:item)*)` (with arbitrary
/// metavariable name and `*` / `+` repetition). Tokio's
/// `cfg_io_driver!`, `cfg_net!`, `cfg_signal_internal!`, etc. all
/// match this shape; their bodies emit `#[cfg(...)] $item` per
/// element. Knowing the set lets the cfg-attr rewriter recurse into
/// invocations of these macros — the cfgs in their args sit in
/// item-position and need baking against vendor-time features.
///
/// Conservative: only the FIRST arm is checked (good enough for
/// tokio's single-arm cfg macros), and the matcher must be exactly
/// the item-list shape (no extra tokens before/after the
/// repetition). cfg_if! / pick! have a different first-arm shape
/// (`if #[cfg(...)] { … }`) and are not flagged.
fn collect_item_list_macro_names(file: &syn::File) -> HashSet<String> {
    use syn::visit::Visit;
    struct V {
        names: HashSet<String>,
    }
    impl V {
        fn check_macro_rules(&mut self, name: &str, body: &proc_macro2::TokenStream) {
            let toks: Vec<TokenTree> = body.clone().into_iter().collect();
            // First Group is the matcher arm: `($($i:item)*)` etc.
            let Some(TokenTree::Group(matcher)) = toks.first() else {
                return;
            };
            if matcher.delimiter() != proc_macro2::Delimiter::Parenthesis {
                return;
            }
            if matcher_is_item_list(&matcher.stream()) {
                self.names.insert(name.to_string());
            }
        }
    }
    /// True if the matcher tokens are exactly `$($IDENT:item)*` or
    /// `$($IDENT:item)+` with arbitrary IDENT and surrounding
    /// whitespace. No other tokens before/after.
    fn matcher_is_item_list(stream: &proc_macro2::TokenStream) -> bool {
        let toks: Vec<TokenTree> = stream.clone().into_iter().collect();
        // Expect: `$ ( $IDENT : item ) *` (or `+`)
        if toks.len() != 3 {
            return false;
        }
        let TokenTree::Punct(dollar) = &toks[0] else {
            return false;
        };
        if dollar.as_char() != '$' {
            return false;
        }
        let TokenTree::Group(inner) = &toks[1] else {
            return false;
        };
        if inner.delimiter() != proc_macro2::Delimiter::Parenthesis {
            return false;
        }
        let TokenTree::Punct(rep) = &toks[2] else {
            return false;
        };
        if !matches!(rep.as_char(), '*' | '+') {
            return false;
        }
        // Inner: `$ IDENT : item`
        let inner_toks: Vec<TokenTree> = inner.stream().into_iter().collect();
        if inner_toks.len() != 4 {
            return false;
        }
        let TokenTree::Punct(p1) = &inner_toks[0] else {
            return false;
        };
        if p1.as_char() != '$' {
            return false;
        }
        if !matches!(&inner_toks[1], TokenTree::Ident(_)) {
            return false;
        }
        let TokenTree::Punct(p2) = &inner_toks[2] else {
            return false;
        };
        if p2.as_char() != ':' {
            return false;
        }
        let TokenTree::Ident(kind) = &inner_toks[3] else {
            return false;
        };
        kind == "item"
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            if !im.mac.path.is_ident("macro_rules") {
                return;
            }
            // The macro's name comes from the ItemMacro's `ident`
            // field (`macro_rules! NAME { ... }`).
            let Some(name) = &im.ident else {
                return;
            };
            self.check_macro_rules(&name.to_string(), &im.mac.tokens);
        }
    }
    let mut v = V {
        names: HashSet::new(),
    };
    v.visit_file(file);
    v.names
}

/// Walk every `macro_rules!` body (recursively, including macros nested in
/// inline mods) and rewrite `$crate` → `$crate::<crate_name>`. Refuses if
/// `$crate` appears in a position where prepending `:: <name>` would be
/// syntactically invalid (i.e. not followed by `::`).
fn collect_dollar_crate_rewrites(
    file: &syn::File,
    crate_name: &str,
    siblings: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) -> Result<()> {
    use syn::visit::Visit;
    struct V<'a> {
        crate_name: &'a str,
        siblings: &'a HashSet<String>,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
        refusal: Option<String>,
    }
    impl<'a> V<'a> {
        fn process_tokens_inner(
            &mut self,
            ts: &proc_macro2::TokenStream,
            self_macro_name: Option<&str>,
        ) {
            if self.refusal.is_some() {
                return;
            }
            let tokens: Vec<TokenTree> = ts.clone().into_iter().collect();
            for i in 0..tokens.len() {
                // `local_inner_macros`: a macro_rules! marked
                // `#[macro_export(local_inner_macros)]` auto-prefixes
                // recursive calls to its own name with `$crate::`. After
                // we strip the attribute, that auto-prefix is lost, so
                // expansions at user call sites can't find the macro.
                // Manually prefix recursive `MACRO_NAME!()` invocations
                // — our `$crate::` rewrite below then turns
                // `$crate::MACRO_NAME!` into `$crate::<crate_name>::MACRO_NAME!`.
                if let Some(self_name) = self_macro_name
                    && let TokenTree::Ident(id) = &tokens[i]
                    && id == self_name
                    && matches!(
                        tokens.get(i + 1),
                        Some(TokenTree::Punct(p)) if p.as_char() == '!',
                    )
                    && !matches!(
                        tokens.get(i.wrapping_sub(1)),
                        Some(TokenTree::Punct(p)) if p.as_char() == '$' || p.as_char() == ':',
                    )
                {
                    let span = id.span().byte_range();
                    if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                        self.edits.push((
                            span.start..span.start,
                            format!("$crate::{}::", self.crate_name),
                        ));
                    }
                }
                // `$crate` is Punct('$') + Ident("crate"). Note: Spacing on
                // `$` is Alone here — Spacing::Joint only describes adjacency
                // to another Punct, not to an Ident, so we don't gate on it.
                if let TokenTree::Punct(p) = &tokens[i]
                    && p.as_char() == '$'
                    && let Some(TokenTree::Ident(id)) = tokens.get(i + 1)
                    && id == "crate"
                {
                    // Verify it's followed by `::` so the rewrite is syntactically safe.
                    let followed_by_colon_colon = matches!(
                        tokens.get(i + 2),
                        Some(TokenTree::Punct(c)) if c.as_char() == ':' && c.spacing() == proc_macro2::Spacing::Joint,
                    ) && matches!(
                        tokens.get(i + 3),
                        Some(TokenTree::Punct(c)) if c.as_char() == ':',
                    );
                    if !followed_by_colon_colon {
                        self.refusal = Some(format!(
                            "$crate is not followed by `::` (rewriting to `$crate::{}` would be syntactically invalid here)",
                            self.crate_name
                        ));
                        return;
                    }
                    // Skip `$crate::alloc/core/std::*` — these refer
                    // to the user-crate-level extern prelude (where
                    // alloc/core/std live after our hoisting), not a
                    // wrapping-mod-internal item. smallvec's
                    // `$crate::alloc::vec!` is the canonical case.
                    let next_is_stdlib = matches!(
                        tokens.get(i + 4),
                        Some(TokenTree::Ident(next)) if matches!(
                            next.to_string().as_str(),
                            "alloc" | "core" | "std"
                        ),
                    );
                    if next_is_stdlib {
                        continue;
                    }
                    let span = id.span().byte_range();
                    if is_in_deleted(&span, self.deleted) {
                        continue;
                    }
                    // Insert `::<crate_name>` immediately after the `crate` ident.
                    let pos = span.end;
                    self.edits
                        .push((pos..pos, format!("::{}", self.crate_name)));
                }
                // Bare `crate::` and `SIBLING::` inside a macro body.
                // Some crates write `crate::foo` instead of canonical
                // `$crate::foo` (semantics: same in the original crate;
                // would expand to the user-crate root in vendored form).
                // Sibling references inside macro bodies follow the same
                // rule as path expressions outside macros — needs to be
                // rooted at the crate root.
                //
                // Skip-rule: a single previous `:` could mean either
                // type-ascription (`field: crate::Foo` — REWRITE) or a
                // mid-path `::IDENT::IDENT` segment (`Foo::crate::bar`
                // — DON'T rewrite, the second `crate` is just an ident).
                // Disambiguate by requiring the previous TWO tokens to
                // be `::` (joint then alone) before treating as a non-
                // leading skip. Same shape fix as
                // `collect_macro_invocation_token_rewrites` — zerocopy's
                // `Alignment: crate::invariant::Alignment` shape (66
                // call sites in rand) was the casualty here.
                if let TokenTree::Ident(id) = &tokens[i]
                    && {
                        let prev = tokens.get(i.wrapping_sub(1));
                        let prev_prev = tokens.get(i.wrapping_sub(2));
                        let preceded_by_dollar = matches!(
                            prev,
                            Some(TokenTree::Punct(p)) if p.as_char() == '$',
                        );
                        let preceded_by_double_colon = matches!(
                            prev,
                            Some(TokenTree::Punct(p)) if p.as_char() == ':',
                        ) && matches!(
                            prev_prev,
                            Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == proc_macro2::Spacing::Joint,
                        );
                        !preceded_by_dollar && !preceded_by_double_colon
                    }
                    && matches!(
                        tokens.get(i + 1),
                        Some(TokenTree::Punct(c)) if c.as_char() == ':' && c.spacing() == proc_macro2::Spacing::Joint,
                    )
                    && matches!(
                        tokens.get(i + 2),
                        Some(TokenTree::Punct(c)) if c.as_char() == ':',
                    )
                {
                    let span = id.span().byte_range();
                    if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                        let name = id.to_string();
                        if name == "crate" {
                            let pos = span.end;
                            self.edits
                                .push((pos..pos, format!("::{}", self.crate_name)));
                        } else if self.siblings.contains(&name) {
                            self.edits.push((span, format!("crate::{name}")));
                        }
                    }
                }
                if let TokenTree::Group(g) = &tokens[i] {
                    self.process_tokens_inner(&g.stream(), self_macro_name);
                }
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            // Only rewrite inside macro_rules! definitions.
            if im.mac.path.is_ident("macro_rules") {
                let self_name = im
                    .ident
                    .as_ref()
                    .filter(|_| has_local_inner_macros(&im.attrs))
                    .map(|i| i.to_string());
                self.process_tokens_inner(&im.mac.tokens, self_name.as_deref());
            }
        }
    }
    let mut v = V {
        crate_name,
        siblings,
        edits,
        deleted,
        refusal: None,
    };
    v.visit_file(file);
    if let Some(reason) = v.refusal {
        return Err(FlattenError::other(reason));
    }
    Ok(())
}

/// Detects `#[macro_export(local_inner_macros)]` (with the
/// `local_inner_macros` modifier inside the parens). The modifier
/// auto-prefixes recursive macro calls in the body with `$crate::`,
/// which we lose when stripping the `#[macro_export]` attribute.
fn has_local_inner_macros(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("macro_export") {
            continue;
        }
        let syn::Meta::List(list) = &attr.meta else {
            continue;
        };
        for tok in list.tokens.clone() {
            if let TokenTree::Ident(id) = tok
                && id == "local_inner_macros"
            {
                return true;
            }
        }
    }
    false
}

/// For each `#[macro_export] macro_rules! name { ... }`, strip the
/// macro_export attribute and insert `pub use name;` after the macro item.
/// This keeps `<crate_name>::name!()` resolvable from outside the vendored
/// mod (without `#[macro_export]` lifting the macro to the wrong crate root).
fn collect_macro_export_rewrites(
    file: &syn::File,
    features: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    walk_macro_export_rewrites(&file.items, features, deleted, edits);
}

/// Recursive walker — needs parent-scope context (the sibling items list)
/// so it can spot a pre-existing `pub use NAME;` and avoid emitting a
/// conflicting `pub(crate) use NAME;`.
fn walk_macro_export_rewrites(
    items: &[syn::Item],
    features: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::spanned::Spanned;
    for item in items {
        if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            walk_macro_export_rewrites(inner, features, deleted, edits);
        }
        let syn::Item::Macro(im) = item else { continue };
        if !im.mac.path.is_ident("macro_rules") {
            continue;
        }
        let Some(name) = &im.ident else { continue };
        let item_span = im.span().byte_range();
        if is_in_deleted(&item_span, deleted) {
            continue;
        }
        // Macros named after built-in attributes (`warn`, `deny`,
        // `allow`, …) trigger E0659 ambiguity in any `pub(crate) use`
        // re-export. KEEP `#[macro_export]` on these instead.
        if is_builtin_attr_macro_name(&name.to_string()) {
            continue;
        }
        let mut had_export = false;
        for attr in &im.attrs {
            if attr.path().is_ident("macro_export")
                && let Some(span) = attr_byte_range(attr)
            {
                edits.push((span, String::new()));
                had_export = true;
            }
        }
        if !had_export {
            continue;
        }
        // If the dep already re-exports the macro at the same scope:
        //   - `pub use NAME;`  → was relying on the macro being
        //     `#[macro_export]`'d to crate root; we're stripping that, so
        //     a bare `pub` re-export of a now-`pub(crate)` macro fails
        //     with E0364. Downgrade the dep's `pub` to `pub(crate)`,
        //     skip our own emission.
        //   - `pub(crate) use NAME;` → already what we'd emit; skip.
        match dep_reexport_visibility(items, name) {
            Some(ReexportVis::PubCrate) => continue,
            Some(ReexportVis::Pub(pub_span)) => {
                // Edit `pub` → `pub(crate)` in place.
                edits.push((pub_span, "pub(crate)".to_string()));
                continue;
            }
            None => {}
        }
        // Sibling collision case: a same-name top-level item (most
        // commonly `pub mod NAME`) already binds NAME at this scope.
        // tracing-core has both `pub mod metadata` (type namespace)
        // and `macro_rules! metadata` (macro namespace). A bare
        // `pub(crate) use metadata;` re-exports BOTH namespaces from
        // the local scope, which collides with the existing mod
        // (E0255 in type namespace).
        //
        // Workaround: rename the macro_rules to a `__cf_NAME` ident
        // (in the macro namespace alone) and emit `pub(crate) use
        // __cf_NAME as NAME;`. The `use ... as NAME` alias references
        // `__cf_NAME` which is ONLY a macro — so the re-export only
        // populates the macro namespace, leaving the existing
        // `pub mod NAME` in the type namespace untouched. Internal
        // callers using `$crate::DEP::NAME!()` continue to resolve
        // via the alias.
        let renamed = if sibling_item_with_name_exists(items, name) {
            let new_ident = format!("__cf_{name}");
            let span = name.span().byte_range();
            edits.push((span, new_ident.clone()));
            Some(new_ident)
        } else {
            None
        };
        // Carry over any `#[cfg(...)]` attrs from the macro_rules! to
        // the synthesised use-export — but evaluate them against the
        // dep's resolved `features` first, mirroring what
        // `collect_cfg_attr_rewrites` does for real attrs. Three cases:
        //   - True : strip the cfg, emit unconditional `pub(crate) use`
        //   - False: skip emission entirely (the macro itself will be
        //            cfg-False'd, the use would dangle anyway)
        //   - Unknown: emit with simplified cfg so downstream rustc sees
        //            the right gate (windows-sys's `link!` per-target_arch
        //            variants are the canonical case)
        let mut cfg_pieces: Vec<String> = Vec::new();
        let mut cfg_was_false = false;
        for attr in &im.attrs {
            if !attr.path().is_ident("cfg") {
                continue;
            }
            let syn::Meta::List(list) = &attr.meta else {
                continue;
            };
            let expr = parse_cfg_expr(&list.tokens);
            match eval_cfg(&expr, features) {
                CfgEval::True => {} // drop — cfg is satisfied
                CfgEval::False => {
                    cfg_was_false = true;
                    break;
                }
                CfgEval::Unknown => {
                    if let Some(simplified) = simplify_cfg_expr(&expr, features) {
                        cfg_pieces.push(format!("#[cfg({simplified})]"));
                    }
                }
            }
        }
        if cfg_was_false {
            continue;
        }
        let cfg_prefix = if cfg_pieces.is_empty() {
            String::new()
        } else {
            format!("{} ", cfg_pieces.join(" "))
        };
        let end = item_span.end;
        let use_text = match &renamed {
            Some(new_ident) => {
                format!("\n{cfg_prefix}pub(crate) use {new_ident} as {name};\n")
            }
            None => format!("\n{cfg_prefix}pub(crate) use {name};\n"),
        };
        edits.push((end..end, use_text));
    }
}

/// Counterpart to [`collect_macro_export_rewrites`] for `macro_rules!`
/// definitions nested inside a wrapper-macro invocation.
///
/// tokio's `doc!` is the canonical case:
///
/// ```ignore
/// macro_rules! doc {
///     ($select:item) => { #[macro_export] $select };
/// }
/// doc! { macro_rules! select { () => { 42 }; } }
/// ```
///
/// `#[macro_export]` is added by `doc!`'s expansion at compile time —
/// `collect_macro_export_rewrites` doesn't see it because it walks
/// `Item::Macro` nodes only and the inner `macro_rules! select` lives
/// inside `doc!`'s opaque token stream. Without a synthesised
/// `pub(crate) use select;` the caller's `dep::select!()` dangles even
/// though `select!` does end up at the user's crate root.
///
/// Pre-scan locates wrapper macros (those whose body re-emits the
/// captured `$item` argument with a `#[macro_export]` attribute), then
/// the main pass walks each wrapper invocation's tokens for
/// `macro_rules ! NAME { ... }` triples and emits the use-export at
/// the surrounding mod scope.
fn collect_wrapper_macro_export_rewrites(
    file: &syn::File,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    let mut wrapper_names: HashSet<String> = HashSet::new();
    collect_macro_export_wrapper_names(&file.items, &mut wrapper_names);
    if wrapper_names.is_empty() {
        return;
    }
    walk_wrapper_invocations(&file.items, &wrapper_names, deleted, edits);
}

/// Find `macro_rules! NAME { ... }` definitions whose body contains a
/// `#[macro_export]` attribute. These are wrapper-macros that auto-add
/// `#[macro_export]` to whatever they re-emit.
fn collect_macro_export_wrapper_names(items: &[syn::Item], out: &mut HashSet<String>) {
    for item in items {
        if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            collect_macro_export_wrapper_names(inner, out);
        }
        let syn::Item::Macro(im) = item else { continue };
        if !im.mac.path.is_ident("macro_rules") {
            continue;
        }
        let Some(name) = &im.ident else { continue };
        if body_contains_macro_export_attr(&im.mac.tokens) {
            out.insert(name.to_string());
        }
    }
}

/// True if `stream` contains a `# [ macro_export ]` token sequence at
/// any depth (we recurse into every Group). This is a conservative
/// heuristic — false positives just mean an extra harmless
/// `pub(crate) use NAME;` synth.
fn body_contains_macro_export_attr(stream: &proc_macro2::TokenStream) -> bool {
    use proc_macro2::TokenTree;
    let toks: Vec<TokenTree> = stream.clone().into_iter().collect();
    for i in 0..toks.len() {
        if let TokenTree::Punct(p) = &toks[i]
            && p.as_char() == '#'
            && let Some(TokenTree::Group(bracket)) = toks.get(i + 1)
            && bracket.delimiter() == proc_macro2::Delimiter::Bracket
        {
            let inner: Vec<TokenTree> = bracket.stream().into_iter().collect();
            if let Some(TokenTree::Ident(id)) = inner.first()
                && id == "macro_export"
            {
                return true;
            }
        }
        if let TokenTree::Group(g) = &toks[i]
            && body_contains_macro_export_attr(&g.stream())
        {
            return true;
        }
    }
    false
}

/// For each item-position invocation of a wrapper macro, scan its
/// tokens for `macro_rules ! NAME { ... }` and emit `pub(crate) use
/// NAME;` after the invocation's end. Recurses into nested mods.
fn walk_wrapper_invocations(
    items: &[syn::Item],
    wrappers: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use proc_macro2::TokenTree;
    use syn::spanned::Spanned;
    for item in items {
        if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            walk_wrapper_invocations(inner, wrappers, deleted, edits);
        }
        let syn::Item::Macro(im) = item else { continue };
        let Some(macro_name) = im.mac.path.get_ident() else {
            continue;
        };
        if !wrappers.contains(&macro_name.to_string()) {
            continue;
        }
        let item_span = im.span().byte_range();
        if is_in_deleted(&item_span, deleted) {
            continue;
        }
        let toks: Vec<TokenTree> = im.mac.tokens.clone().into_iter().collect();
        for j in 0..toks.len().saturating_sub(2) {
            let TokenTree::Ident(kw) = &toks[j] else {
                continue;
            };
            if kw != "macro_rules" {
                continue;
            }
            let Some(TokenTree::Punct(bang)) = toks.get(j + 1) else {
                continue;
            };
            if bang.as_char() != '!' {
                continue;
            }
            let Some(TokenTree::Ident(inner_name)) = toks.get(j + 2) else {
                continue;
            };
            // Sanity check: a real macro_rules has a body group right after.
            let body_kind = toks.get(j + 3).map(|t| match t {
                TokenTree::Group(g) => Some(g.delimiter()),
                _ => None,
            });
            if body_kind.flatten().is_none() {
                continue;
            }
            let inner_name = inner_name.to_string();
            if is_builtin_attr_macro_name(&inner_name) {
                continue;
            }
            let end = item_span.end;
            edits.push((end..end, format!("\npub(crate) use {inner_name};\n")));
        }
    }
}

/// True if any sibling item at the same scope as the macro_rules being
/// stripped already binds `name` in the type or value namespace —
/// namely a `mod NAME`, `struct NAME`, `enum NAME`, `union NAME`,
/// `trait NAME`, `type NAME`, `const NAME`, `static NAME`, or `fn
/// NAME`. Used to avoid the synth collision in
/// `walk_macro_export_rewrites`.
fn sibling_item_with_name_exists(items: &[syn::Item], name: &syn::Ident) -> bool {
    for it in items {
        let n = match it {
            syn::Item::Mod(m) => &m.ident,
            syn::Item::Struct(s) => &s.ident,
            syn::Item::Enum(e) => &e.ident,
            syn::Item::Union(u) => &u.ident,
            syn::Item::Trait(t) => &t.ident,
            syn::Item::Type(t) => &t.ident,
            syn::Item::Const(c) => &c.ident,
            syn::Item::Static(s) => &s.ident,
            syn::Item::Fn(f) => &f.sig.ident,
            _ => continue,
        };
        if n == name {
            return true;
        }
    }
    false
}

enum ReexportVis {
    /// `pub use NAME;` — found at the byte range of the `pub` keyword so
    /// the caller can downgrade it.
    Pub(Range<usize>),
    /// `pub(crate) use NAME;` — already the visibility we'd emit ourselves.
    PubCrate,
}

/// Inspect `items` for a sibling `use NAME;` re-exporting the same name
/// as the macro_rules we're about to lift. Returns the visibility flavour
/// (with the `pub` keyword's byte range for the Pub case so the caller
/// can edit it). Plain `use NAME;` (inherited visibility) returns None.
fn dep_reexport_visibility(items: &[syn::Item], name: &syn::Ident) -> Option<ReexportVis> {
    use syn::spanned::Spanned;
    for it in items {
        let syn::Item::Use(u) = it else { continue };
        if !matches!(&u.tree, syn::UseTree::Name(n) if n.ident == *name) {
            continue;
        }
        match &u.vis {
            syn::Visibility::Public(token) => {
                return Some(ReexportVis::Pub(token.span().byte_range()));
            }
            syn::Visibility::Restricted(_) => {
                return Some(ReexportVis::PubCrate);
            }
            syn::Visibility::Inherited => {}
        }
    }
    None
}

/// For every `pub use NAME;` or `pub use NAME as ALIAS;` in this file
/// whose `NAME` is a `#[macro_export]` macro defined elsewhere in the
/// same dep, downgrade `pub` to `pub(crate)`. The in-file demoter in
/// `walk_macro_export_rewrites` only sees re-exports at the SAME
/// scope as the macro definition; this pass handles the cross-file
/// case (anyhow's `lib.rs` re-exports `macros.rs`'s `anyhow!` as
/// `format_err`).
fn collect_cross_file_macro_export_demotions(
    file: &syn::File,
    macro_export_names: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    fn walk(
        items: &[syn::Item],
        names: &HashSet<String>,
        deleted: &[Range<usize>],
        edits: &mut Vec<(Range<usize>, String)>,
    ) {
        use syn::spanned::Spanned;
        for item in items {
            if let syn::Item::Mod(m) = item
                && let Some((_, inner)) = &m.content
            {
                walk(inner, names, deleted, edits);
            }
            let syn::Item::Use(u) = item else { continue };
            let target = match &u.tree {
                syn::UseTree::Name(n) => &n.ident,
                syn::UseTree::Rename(r) => &r.ident,
                _ => continue,
            };
            if !names.contains(&target.to_string()) {
                continue;
            }
            let syn::Visibility::Public(pub_token) = &u.vis else {
                continue;
            };
            let span = pub_token.span().byte_range();
            if is_in_deleted(&span, deleted) {
                continue;
            }
            edits.push((span, "pub(crate)".to_string()));
        }
    }
    walk(&file.items, macro_export_names, deleted, edits);
}

/// Edition-2024 match-ergonomics fix. When the scrutinee of a `match`
/// is `&EXPR` or `&mut EXPR`, edition-2024 default-binding-mode
/// rejects a non-reference arm pattern that also binds `mut FIELD`
/// — the implicit borrow conflicts with the explicit `mut`. Wrap the
/// pattern with the matching `&` / `&mut` reference pattern so the
/// borrow is explicit and the `mut` binding survives. anyhow's
/// `Chain::next_back` and `Chain::len` (which `match &mut self.state`
/// / `match &self.state` against `Linked { mut next }`) are the
/// canonical cases.
fn collect_implicit_borrow_match_rewrites(
    file: &syn::File,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
            syn::visit::visit_expr_match(self, m);
            let syn::Expr::Reference(r) = &*m.expr else {
                return;
            };
            let prefix = if r.mutability.is_some() { "&mut " } else { "&" };
            for arm in &m.arms {
                if !pat_needs_ref_wrap(&arm.pat) {
                    continue;
                }
                use syn::spanned::Spanned;
                let span = arm.pat.span().byte_range();
                if is_in_deleted(&span, self.deleted) {
                    continue;
                }
                // Zero-width insert at the start of the pat span.
                self.edits
                    .push((span.start..span.start, prefix.to_string()));
            }
        }
    }
    V { edits, deleted }.visit_file(file);
}

/// True if `pat` is a non-reference pattern that binds at least one
/// field/local by `mut` (not `ref mut`). Reference patterns and
/// patterns without any `mut` binding are fine — only the
/// borrow+mut combination triggers the 2024 rule.
fn pat_needs_ref_wrap(pat: &syn::Pat) -> bool {
    if matches!(pat, syn::Pat::Reference(_)) {
        return false;
    }
    use syn::visit::Visit;
    struct V {
        found: bool,
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
            if p.by_ref.is_none() && p.mutability.is_some() {
                self.found = true;
            }
        }
    }
    let mut v = V { found: false };
    v.visit_pat(pat);
    v.found
}

/// Detects macro invocations of the form `SIBLING::CONFLICT_NAME!(...)`
/// where SIBLING is a vendored sibling crate and CONFLICT_NAME is one
/// of the built-in-attr-shadowed names (warn, deny, allow, …) that
/// we KEEP `#[macro_export]` on (so they lift to the user crate root,
/// not to `crate::SIBLING::NAME`). Rewrite the entire macro path to
/// `crate::CONFLICT_NAME` so the call resolves correctly. log's
/// `log::warn!()` from rapier2d is the canonical example.
fn collect_builtin_attr_macro_call_rewrites(
    file: &syn::File,
    siblings: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        siblings: &'a HashSet<String>,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            // Look for `SIBLING::CONFLICT!()` — exactly two segments,
            // first is a sibling, second is a built-in-attr-named
            // macro. Rewrite the WHOLE path span to `crate::CONFLICT`.
            if mac.path.leading_colon.is_some() {
                return;
            }
            let segs: Vec<&syn::PathSegment> = mac.path.segments.iter().collect();
            if segs.len() != 2 {
                return;
            }
            let sibling_name = segs[0].ident.to_string();
            let macro_name = segs[1].ident.to_string();
            if !self.siblings.contains(&sibling_name) {
                return;
            }
            if !is_builtin_attr_macro_name(&macro_name) {
                return;
            }
            let span_start = segs[0].ident.span().byte_range().start;
            let span_end = segs[1].ident.span().byte_range().end;
            let span = span_start..span_end;
            if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                // `#[macro_export]` lifts the macro to the user crate
                // root (we keep `#[macro_export]` for these conflict-named
                // macros instead of stripping it). Reach via `crate::NAME`
                // — this works as long as the macro itself isn't
                // "macro-expanded" by an attached tool attribute like
                // `#[clippy::format_args]` (those get stripped by the
                // lint-attr pass).
                self.edits.push((span, format!("crate::{macro_name}")));
            }
        }
    }
    let mut v = V {
        siblings,
        edits,
        deleted,
    };
    v.visit_file(file);
}

/// Names that conflict with built-in lint attributes / cfg names. A
/// `pub(crate) use NAME;` re-export of a same-named macro triggers
/// E0659 ambiguity (the macro and the built-in attr are both
/// candidates and `use` can't disambiguate). Keep `#[macro_export]`
/// for these so they lift to the user's crate root via Rust's normal
/// export mechanism, and rewrite call sites accordingly.
pub fn is_builtin_attr_macro_name(name: &str) -> bool {
    matches!(
        name,
        "warn"
            | "deny"
            | "allow"
            | "forbid"
            | "expect"
            | "cfg"
            | "cfg_attr"
            | "test"
            | "ignore"
            | "should_panic"
            | "bench"
            | "doc"
            | "inline"
            | "derive"
            | "repr"
            | "must_use"
            | "no_mangle"
            | "automatically_derived"
            | "non_exhaustive"
            | "link"
            | "link_name"
            | "link_section"
            | "used"
            | "no_main"
            | "track_caller"
            | "panic_handler"
            | "global_allocator"
            | "target_feature"
            | "thread_local"
            | "no_std"
            | "feature"
            | "macro_use"
            | "macro_export"
            | "path"
    )
}

/// Walk items recursively; collect byte ranges to delete.
fn walk_items_for_deletion(
    items: &[syn::Item],
    features: &HashSet<String>,
    deleted: &mut Vec<Range<usize>>,
) {
    use syn::spanned::Spanned;
    for item in items {
        let attrs = item_outer_attrs(item);
        if any_cfg_evaluates_false(attrs, features) {
            deleted.push(item.span().byte_range());
            continue;
        }
        match item {
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk_items_for_deletion(inner, features, deleted);
                }
            }
            syn::Item::Impl(i) => walk_impl_items_for_deletion(&i.items, features, deleted),
            syn::Item::Trait(t) => walk_trait_items_for_deletion(&t.items, features, deleted),
            _ => {}
        }
    }
}

fn walk_impl_items_for_deletion(
    items: &[syn::ImplItem],
    features: &HashSet<String>,
    deleted: &mut Vec<Range<usize>>,
) {
    use syn::spanned::Spanned;
    for item in items {
        let attrs = impl_item_outer_attrs(item);
        if any_cfg_evaluates_false(attrs, features) {
            deleted.push(item.span().byte_range());
        }
    }
}

fn walk_trait_items_for_deletion(
    items: &[syn::TraitItem],
    features: &HashSet<String>,
    deleted: &mut Vec<Range<usize>>,
) {
    use syn::spanned::Spanned;
    for item in items {
        let attrs = trait_item_outer_attrs(item);
        if any_cfg_evaluates_false(attrs, features) {
            deleted.push(item.span().byte_range());
        }
    }
}

fn any_cfg_evaluates_false(attrs: &[syn::Attribute], features: &HashSet<String>) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("cfg") {
            continue;
        }
        let syn::Meta::List(list) = &attr.meta else {
            continue;
        };
        let expr = parse_cfg_expr(&list.tokens);
        if eval_cfg(&expr, features) == CfgEval::False {
            return true;
        }
    }
    false
}

fn item_outer_attrs(item: &syn::Item) -> &[syn::Attribute] {
    use syn::Item::*;
    match item {
        Const(i) => &i.attrs,
        Enum(i) => &i.attrs,
        ExternCrate(i) => &i.attrs,
        Fn(i) => &i.attrs,
        ForeignMod(i) => &i.attrs,
        Impl(i) => &i.attrs,
        Macro(i) => &i.attrs,
        Mod(i) => &i.attrs,
        Static(i) => &i.attrs,
        Struct(i) => &i.attrs,
        Trait(i) => &i.attrs,
        TraitAlias(i) => &i.attrs,
        Type(i) => &i.attrs,
        Union(i) => &i.attrs,
        Use(i) => &i.attrs,
        _ => &[],
    }
}

fn impl_item_outer_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    use syn::ImplItem::*;
    match item {
        Const(i) => &i.attrs,
        Fn(i) => &i.attrs,
        Type(i) => &i.attrs,
        Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn trait_item_outer_attrs(item: &syn::TraitItem) -> &[syn::Attribute] {
    use syn::TraitItem::*;
    match item {
        Const(i) => &i.attrs,
        Fn(i) => &i.attrs,
        Type(i) => &i.attrs,
        Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn attr_byte_range(attr: &syn::Attribute) -> Option<Range<usize>> {
    let start = attr.pound_token.span.byte_range().start;
    let end = attr.bracket_token.span.close().byte_range().end;
    if end > start { Some(start..end) } else { None }
}

fn collect_crate_path_rewrites(
    file: &syn::File,
    crate_name: &str,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        crate_name: &'a str,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'a> V<'a> {
        fn maybe_rewrite_path(&mut self, path: &syn::Path) {
            if path.leading_colon.is_some() {
                return;
            }
            let Some(first) = path.segments.first() else {
                return;
            };
            if first.ident != "crate" {
                return;
            }
            // Skip `crate::alloc/core/std::*` — these reference the
            // user crate's extern prelude (where alloc/core/std live
            // after our hoisting), NOT a wrapping-mod-internal item.
            // hashbrown's `use crate::alloc::alloc::{...}` is the
            // canonical example.
            if let Some(second) = path.segments.iter().nth(1)
                && matches!(second.ident.to_string().as_str(), "alloc" | "core" | "std")
            {
                return;
            }
            let span = first.ident.span().byte_range();
            if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                self.edits
                    .push((span, format!("crate::{}", self.crate_name)));
            }
        }

        /// `use` paths use `syn::UseTree`, not `syn::Path`, so visit_path
        /// alone misses `use crate::foo::Bar`. Recurse through UseTree to
        /// catch the leading `crate` ident in any `UseTree::Path`.
        fn process_use_tree(&mut self, tree: &syn::UseTree) {
            match tree {
                syn::UseTree::Path(path) => {
                    if path.ident == "crate" {
                        // Same skip as `maybe_rewrite_path`: don't
                        // hijack `use crate::alloc::*;` style paths.
                        let next_is_stdlib = match &*path.tree {
                            syn::UseTree::Path(inner) => {
                                matches!(inner.ident.to_string().as_str(), "alloc" | "core" | "std")
                            }
                            syn::UseTree::Name(n) => {
                                matches!(n.ident.to_string().as_str(), "alloc" | "core" | "std")
                            }
                            _ => false,
                        };
                        if !next_is_stdlib {
                            let span = path.ident.span().byte_range();
                            if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                                self.edits
                                    .push((span, format!("crate::{}", self.crate_name)));
                            }
                        }
                    }
                    self.process_use_tree(&path.tree);
                }
                syn::UseTree::Group(group) => {
                    for item in &group.items {
                        self.process_use_tree(item);
                    }
                }
                _ => {}
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.maybe_rewrite_path(path);
            syn::visit::visit_path(self, path);
        }
        fn visit_visibility(&mut self, vis: &'ast syn::Visibility) {
            // `pub(crate)` is a shorthand keyword, not a path expression —
            // never rewrite it. `pub(in crate::foo)` IS a path; recurse.
            if let syn::Visibility::Restricted(r) = vis
                && r.in_token.is_some()
            {
                self.maybe_rewrite_path(&r.path);
            }
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            self.process_use_tree(&item.tree);
        }
    }
    let mut v = V {
        crate_name,
        edits,
        deleted,
    };
    v.visit_file(file);
}

/// In Rust 2018+, `use foo::Bar` is an absolute path — it resolves against
/// the extern prelude (Cargo.toml deps) and crate root, NOT lexical scope.
/// Path expressions like `foo::Bar::new()` resolve against lexical scope,
/// but a `use crate::foo;` injection only helps the immediately enclosing
/// mod, not nested mods inside it. So when a vendored crate references
/// a sibling vendored crate (`anstyle`, `clap_lex`, etc.), both forms
/// need rewriting.
///
/// Rewriter:
///   - For every `use` tree whose leading segment is a sibling crate name,
///     rewrite the leading ident to `crate::<name>`.
///   - For every path expression (`SIBLING::Foo::bar`) without a leading
///     `::`, rewrite the leading segment the same way. Skipped when the
///     vendored crate would shadow the sibling with its own local item
///     (e.g. `mod sibling_name` declared inside).
fn collect_sibling_use_rewrites(
    file: &syn::File,
    siblings: &HashSet<String>,
    aliases: &HashMap<String, String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    if siblings.is_empty() && aliases.is_empty() {
        return;
    }
    use syn::visit::Visit;
    struct V<'a> {
        siblings: &'a HashSet<String>,
        aliases: &'a HashMap<String, String>,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'a> V<'a> {
        fn rewrite_leading(&mut self, ident: &proc_macro2::Ident) {
            let n = ident.to_string();
            // Prefer alias resolution when both apply (rare); aliases
            // were defined explicitly by the dep author.
            let replacement = if let Some(real) = self.aliases.get(&n) {
                if self.siblings.contains(real) {
                    Some(format!("crate::{real}"))
                } else {
                    Some(real.clone())
                }
            } else if self.siblings.contains(&n) {
                Some(format!("crate::{n}"))
            } else {
                None
            };
            if let Some(r) = replacement {
                let span = ident.span().byte_range();
                if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                    self.edits.push((span, r));
                }
            }
        }
        /// Only rewrite the LEADING ident of a use tree — the first
        /// segment of the path. Don't recurse into UseTree::Path's tree
        /// field; those nested idents are leaf import names (or further
        /// path segments) that, if rewritten, would emit invalid syntax
        /// like `use foo::crate::bar;` (E0433).
        fn process_use(&mut self, tree: &syn::UseTree) {
            match tree {
                syn::UseTree::Path(path) => {
                    let name = path.ident.to_string();
                    if name != "crate" && name != "self" && name != "super" {
                        self.rewrite_leading(&path.ident);
                    }
                }
                syn::UseTree::Name(name) => self.rewrite_leading(&name.ident),
                syn::UseTree::Rename(rename) => self.rewrite_leading(&rename.ident),
                syn::UseTree::Group(group) => {
                    for item in &group.items {
                        self.process_use(item);
                    }
                }
                syn::UseTree::Glob(_) => {}
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            // `use ::FOO::Bar` (with leading_colon) is handled later by
            // `rewrite_absolute_sibling_paths`, which replaces `::FOO`
            // with `crate::FOO`. Touching the inner ident here would
            // produce `::crate::FOO` (E0433: `crate` after `::`).
            if item.leading_colon.is_some() {
                return;
            }
            self.process_use(&item.tree);
        }
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path.leading_colon.is_none()
                && let Some(first) = path.segments.first()
                && path.segments.len() >= 2
            {
                let name = first.ident.to_string();
                if name != "crate" && name != "self" && name != "super" {
                    self.rewrite_leading(&first.ident);
                }
            }
            syn::visit::visit_path(self, path);
        }
    }
    let mut v = V {
        siblings,
        aliases,
        edits,
        deleted,
    };
    v.visit_file(file);
}

/// Scan a 2015-edition dep's lib.rs for items that live at the crate
/// root and could be referenced via bare `use NAME;` paths from
/// elsewhere in the dep. Used by [`collect_edition_2015_bare_path_rewrites`]
/// to rewrite those bare paths to `crate::DEPNAME::NAME` so they
/// resolve under 2018+ semantics.
///
/// Includes `mod` declarations (both `mod NAME;` and `pub mod NAME { ... }`),
/// `pub use ...::NAME;` re-exports (the leaf), `pub struct/enum/fn/const/...`
/// items, and macro_rules definitions. Skips `use` items without `pub`
/// (those don't put names in the crate-root namespace from outside).
fn collect_crate_root_item_names(lib_path: &Path) -> HashSet<String> {
    let Ok(src) = std::fs::read_to_string(lib_path) else {
        return HashSet::new();
    };
    let Ok(file) = syn::parse_file(&src) else {
        return HashSet::new();
    };
    let mut names = HashSet::new();
    fn walk_pub_use_leaves(tree: &syn::UseTree, parent: Option<&str>, names: &mut HashSet<String>) {
        match tree {
            syn::UseTree::Path(p) => {
                walk_pub_use_leaves(&p.tree, Some(&p.ident.to_string()), names)
            }
            syn::UseTree::Name(n) => {
                let n = n.ident.to_string();
                if n == "self" {
                    if let Some(parent) = parent {
                        names.insert(parent.to_string());
                    }
                } else {
                    names.insert(n);
                }
            }
            syn::UseTree::Rename(r) => {
                names.insert(r.rename.to_string());
            }
            syn::UseTree::Group(g) => {
                for it in &g.items {
                    walk_pub_use_leaves(it, parent, names);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    for item in &file.items {
        match item {
            syn::Item::Mod(m) => {
                names.insert(m.ident.to_string());
            }
            syn::Item::Struct(s) => {
                names.insert(s.ident.to_string());
            }
            syn::Item::Enum(e) => {
                names.insert(e.ident.to_string());
            }
            syn::Item::Fn(f) => {
                names.insert(f.sig.ident.to_string());
            }
            syn::Item::Const(c) => {
                names.insert(c.ident.to_string());
            }
            syn::Item::Static(s) => {
                names.insert(s.ident.to_string());
            }
            syn::Item::Trait(t) => {
                names.insert(t.ident.to_string());
            }
            syn::Item::Type(t) => {
                names.insert(t.ident.to_string());
            }
            syn::Item::Union(u) => {
                names.insert(u.ident.to_string());
            }
            syn::Item::Use(u) => {
                walk_pub_use_leaves(&u.tree, None, &mut names);
            }
            syn::Item::Macro(m) => {
                if let Some(name) = &m.ident
                    && m.mac.path.is_ident("macro_rules")
                {
                    names.insert(name.to_string());
                }
            }
            _ => {}
        }
    }
    names
}

/// For 2015-edition deps: rewrite bare `use NAME;` and `use NAME::Foo;`
/// (and bare path expressions `NAME::foo()`) to `use crate::DEPNAME::NAME;`
/// when NAME is a crate-root item of the dep. In 2015 the bare path is
/// implicitly relative to the crate root; in our 2018+ flat output it
/// would be looked up in the extern prelude (which doesn't have it),
/// so we make the path explicit.
///
/// Skips identifiers already handled by other passes (siblings,
/// aliases, `crate/self/super`, `alloc/core/std` extern prelude) to
/// avoid over-rewriting and double-edits.
fn collect_edition_2015_bare_path_rewrites(
    file: &syn::File,
    crate_name: &str,
    crate_root_items: &HashSet<String>,
    siblings: &HashSet<String>,
    aliases: &HashMap<String, String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    if crate_root_items.is_empty() {
        return;
    }
    use syn::visit::Visit;
    struct V<'a> {
        crate_name: &'a str,
        crate_root_items: &'a HashSet<String>,
        siblings: &'a HashSet<String>,
        aliases: &'a HashMap<String, String>,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'a> V<'a> {
        fn maybe_rewrite_leading(&mut self, ident: &proc_macro2::Ident) {
            let name = ident.to_string();
            if matches!(
                name.as_str(),
                "crate" | "self" | "super" | "alloc" | "core" | "std"
            ) {
                return;
            }
            if self.siblings.contains(&name) || self.aliases.contains_key(&name) {
                return;
            }
            if !self.crate_root_items.contains(&name) {
                return;
            }
            let span = ident.span().byte_range();
            if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                self.edits
                    .push((span, format!("crate::{}::{}", self.crate_name, name)));
            }
        }
        fn process_use(&mut self, tree: &syn::UseTree) {
            match tree {
                syn::UseTree::Path(p) => self.maybe_rewrite_leading(&p.ident),
                syn::UseTree::Name(n) => self.maybe_rewrite_leading(&n.ident),
                syn::UseTree::Rename(r) => self.maybe_rewrite_leading(&r.ident),
                syn::UseTree::Group(g) => {
                    for it in &g.items {
                        self.process_use(it);
                    }
                }
                syn::UseTree::Glob(_) => {}
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            self.process_use(&item.tree);
        }
        // For non-use paths we only rewrite paths with a LEADING `::`
        // (`::undo_log::Snapshot` from ena's
        // `pub struct Snapshot<S = ::undo_log::Snapshot>`). Leading-colon
        // paths look at the extern prelude and never shadow lexical
        // type parameters, so rewriting them is safe.
        // Bare paths like `B::zero()` inside fn bodies could be type
        // parameters — without scope tracking we can't tell — so we
        // skip those.
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path.leading_colon.is_some()
                && let Some(first) = path.segments.first()
            {
                let name = first.ident.to_string();
                if !matches!(name.as_str(), "alloc" | "core" | "std")
                    && self.crate_root_items.contains(&name)
                    && !self.siblings.contains(&name)
                    && !self.aliases.contains_key(&name)
                {
                    let span_start = path
                        .leading_colon
                        .as_ref()
                        .map(|c| c.spans[0].byte_range().start)
                        .unwrap_or(first.ident.span().byte_range().start);
                    let span_end = first.ident.span().byte_range().end;
                    let span = span_start..span_end;
                    if span.end > span.start && !is_in_deleted(&span, self.deleted) {
                        self.edits
                            .push((span, format!("crate::{}::{}", self.crate_name, name)));
                    }
                }
            }
            syn::visit::visit_path(self, path);
        }
    }
    let mut v = V {
        crate_name,
        crate_root_items,
        siblings,
        aliases,
        edits,
        deleted,
    };
    v.visit_file(file);
}

/// Walk a single source file and collect every `extern crate FOO as BAR;`
/// alias as a `BAR → FOO` map entry. Used to rewrite later `use BAR::*`
/// and `BAR::Foo` references back to the real crate name (which lives
/// in the user's extern prelude).
///
/// Skips declarations behind `#[cfg(...)]` predicates that evaluate
/// False with the given feature set. Crates like rapier2d use feature
/// gates to pick one of several `extern crate ... as parry;`
/// declarations — without filtering, the last lexical entry would win
/// (typically the wrong one for the active features).
fn collect_extern_crate_aliases(
    lib_path: &Path,
    features: &HashSet<String>,
) -> Option<HashMap<String, String>> {
    let src = std::fs::read_to_string(lib_path).ok()?;
    let file = syn::parse_file(&src).ok()?;
    let mut aliases = HashMap::new();
    for item in &file.items {
        if let syn::Item::ExternCrate(ec) = item
            && let Some((_, alias)) = &ec.rename
            && !any_cfg_evaluates_false(&ec.attrs, features)
        {
            aliases.insert(alias.to_string(), ec.ident.to_string());
        }
    }
    Some(aliases)
}

/// Walk lib.rs and collect every `#[macro_use] extern crate FOO;`. These
/// declarations bring FOO's exported macros into scope at every level of
/// the dep — the per-file injector replicates that for vendored
/// submodules. Each entry is the import path to inject — `crate::FOO`
/// for sibling-vendored crates, plain `FOO` for externals.
///
/// Honors `#[cfg(...)]` against the dep's enabled features:
///   - cfg evaluates True (or no cfg) → include the import
///   - cfg evaluates False → skip silently
///   - cfg evaluates Unknown (compiler-set predicate, target_os, …) →
///     skip conservatively. We don't know which user configurations
///     will be active, and injecting an import that resolves to a
///     non-existent path under some configs causes spurious failures.
fn collect_macro_use_externals(
    lib_path: &Path,
    siblings: &HashSet<String>,
    features: &HashSet<String>,
) -> Vec<String> {
    use crate::cfg::{CfgEval, eval_cfg, parse_cfg_expr};
    let Ok(src) = std::fs::read_to_string(lib_path) else {
        return Vec::new();
    };
    let Ok(file) = syn::parse_file(&src) else {
        return Vec::new();
    };
    let mut found: Vec<String> = Vec::new();
    for item in &file.items {
        if let syn::Item::ExternCrate(ec) = item
            && ec.attrs.iter().any(|a| a.path().is_ident("macro_use"))
            && {
                // True if every cfg attr evaluates True against
                // `features` (no cfg means no constraint). Skip if any
                // is False or Unknown.
                ec.attrs.iter().all(|a| {
                    if !a.path().is_ident("cfg") {
                        return true;
                    }
                    let syn::Meta::List(list) = &a.meta else {
                        return false;
                    };
                    matches!(
                        eval_cfg(&parse_cfg_expr(&list.tokens), features),
                        CfgEval::True
                    )
                })
            }
        {
            let name = ec.ident.to_string();
            // alloc/core/std macros (`vec!`, `format!`, `println!`,
            // `assert_eq!`, …) are already in scope at every call site
            // via std's prelude. Injecting `use alloc::*;` etc. would
            // additionally bring the inner `alloc::alloc` submod into
            // scope, conflicting with the hoisted `extern crate alloc;`
            // (E0659 "alloc is ambiguous").
            if matches!(name.as_str(), "alloc" | "core" | "std") {
                continue;
            }
            if siblings.contains(&name) {
                found.push(format!("crate::{name}"));
            } else {
                found.push(name);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Inject imports at the top of a rewritten source (after any leading
/// inner attrs). Two flavours:
///   - `use PATH::*;` for each macro_use'd dep (PATH is `crate::FOO`
///     for siblings, `FOO` for externals). Replicates the legacy
///     `#[macro_use] extern crate FOO;` semantic at every file scope.
///   - `use crate::SIBLING;` for each sibling vendored crate. Required
///     so proc-macro expansions that produce bare `SIBLING::Type`
///     resolve at the call site (the vendored macro's expansion uses
///     call-site hygiene for path lookup, so `nalgebra::SVector`
///     emitted by `nalgebra_macros::vector!()` needs `nalgebra` in
///     scope wherever the macro is invoked).
///
/// Skips siblings that the file already imports (under any visibility)
/// to avoid E0252 "name defined multiple times" — a rewritten `pub use
/// crate::SIBLING;` would collide with our injection at the same scope.
fn inject_imports(
    src: &str,
    macro_use_paths: &[String],
    sibling_paths: &[String],
    item_list_macros: &HashSet<String>,
    sibling_inner_cfgs: &HashMap<String, String>,
) -> String {
    if macro_use_paths.is_empty() && sibling_paths.is_empty() {
        return src.to_string();
    }
    let already_imported = imported_sibling_names(src);
    let mut already_declared = top_level_item_names(src);
    for name in mods_inside_item_list_macros(src, item_list_macros) {
        already_declared.insert(name);
    }
    let mut injection = String::new();
    for path in macro_use_paths {
        injection.push_str(&format!("use {path}::*;\n"));
    }
    for path in sibling_paths {
        // path is `crate::SIBLING` — extract the trailing ident.
        let name = path.rsplit("::").next().unwrap_or(path);
        if already_imported.contains(name) || already_declared.contains(name) {
            continue;
        }
        // If the sibling crate's lib.rs has an inactive-on-this-host
        // `#![cfg(...)]` inner attribute (crossterm_winapi's
        // `#![cfg(windows)]` is the canonical case), gate the
        // injection with the same cfg. Without this, on a
        // non-matching host the sibling's body evaporates at user
        // compile time and our `use crate::SIBLING;` fails.
        if let Some(pred) = sibling_inner_cfgs.get(name) {
            injection.push_str(&format!(
                "#[cfg({pred})]\n#[allow(unused_imports)] use {path};\n"
            ));
        } else {
            injection.push_str(&format!("#[allow(unused_imports)] use {path};\n"));
        }
    }
    if injection.is_empty() {
        return src.to_string();
    }
    let inject_at = safe_inject_point(src);
    let mut out = String::with_capacity(src.len() + injection.len());
    out.push_str(&src[..inject_at]);
    out.push_str(&injection);
    out.push_str(&src[inject_at..]);
    out
}

/// Find `mod NAME { ... }` declarations nested inside item-list
/// macro invocations (e.g. tokio's `cfg_rt! { mod sync_wrapper { ... } }`).
/// syn::parse_file treats macro tokens as opaque so these mods
/// aren't visible to `top_level_item_names`. The sibling-import
/// injection at the top of the file would collide with them
/// otherwise. Walks proc_macro2 tokens, similar to
/// `inline_mods_inside_macros`.
fn mods_inside_item_list_macros(src: &str, item_list_macros: &HashSet<String>) -> HashSet<String> {
    use proc_macro2::TokenTree;
    use std::str::FromStr;
    let mut out = HashSet::new();
    let Ok(stream) = proc_macro2::TokenStream::from_str(src) else {
        return out;
    };
    fn walk(
        toks: &[TokenTree],
        in_item_list_macro: bool,
        item_list_macros: &HashSet<String>,
        out: &mut HashSet<String>,
    ) {
        let mut i = 0;
        while i < toks.len() {
            // Detect `mod IDENT {` or `mod IDENT ;` triplets when we're
            // inside an item-list macro body.
            if in_item_list_macro
                && let Some(TokenTree::Ident(kw)) = toks.get(i)
                && kw == "mod"
                && let Some(TokenTree::Ident(name)) = toks.get(i + 1)
                && let Some(next) = toks.get(i + 2)
                && (matches!(next, TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Brace)
                    || matches!(next, TokenTree::Punct(p) if p.as_char() == ';'))
            {
                out.insert(name.to_string());
                i += 3;
                continue;
            }
            // Detect macro invocation `IDENT ! GROUP` or
            // `IDENT :: IDENT ! GROUP` (path-style).
            let is_invocation = matches!(toks.get(i), Some(TokenTree::Ident(_)))
                && matches!(
                    toks.get(i + 1),
                    Some(TokenTree::Punct(p)) if p.as_char() == '!'
                )
                && matches!(toks.get(i + 2), Some(TokenTree::Group(_)));
            if is_invocation {
                let macro_name = match &toks[i] {
                    TokenTree::Ident(id) => id.to_string(),
                    _ => unreachable!(),
                };
                if let Some(TokenTree::Group(g)) = toks.get(i + 2) {
                    let inner_toks: Vec<TokenTree> = g.stream().into_iter().collect();
                    let recurse_in_macro =
                        in_item_list_macro || item_list_macros.contains(&macro_name);
                    walk(&inner_toks, recurse_in_macro, item_list_macros, out);
                }
                i += 3;
                continue;
            }
            // Recurse into ANY Group (delim block) so nested macros
            // and nested groups are walked.
            if let Some(TokenTree::Group(g)) = toks.get(i) {
                let inner_toks: Vec<TokenTree> = g.stream().into_iter().collect();
                walk(&inner_toks, in_item_list_macro, item_list_macros, out);
            }
            i += 1;
        }
    }
    let toks: Vec<TokenTree> = stream.into_iter().collect();
    walk(&toks, false, item_list_macros, &mut out);
    out
}

/// Top-level item names declared in the file (mods, structs, enums,
/// type aliases, fns, statics, consts, traits). Used to avoid
/// injecting `use crate::SIBLING;` when the file already declares
/// `mod SIBLING { ... }` (nalgebra's `pub mod glam` for its
/// glam-interop bindings is the canonical example).
fn top_level_item_names(src: &str) -> HashSet<String> {
    let Ok(file) = syn::parse_file(src) else {
        return HashSet::new();
    };
    let mut names = HashSet::new();
    for item in &file.items {
        let name = match item {
            syn::Item::Mod(i) => Some(i.ident.to_string()),
            syn::Item::Struct(i) => Some(i.ident.to_string()),
            syn::Item::Enum(i) => Some(i.ident.to_string()),
            syn::Item::Type(i) => Some(i.ident.to_string()),
            syn::Item::Fn(i) => Some(i.sig.ident.to_string()),
            syn::Item::Static(i) => Some(i.ident.to_string()),
            syn::Item::Const(i) => Some(i.ident.to_string()),
            syn::Item::Trait(i) => Some(i.ident.to_string()),
            syn::Item::Union(i) => Some(i.ident.to_string()),
            _ => None,
        };
        if let Some(n) = name {
            names.insert(n);
        }
    }
    names
}

/// Names that the file binds at its top scope via a `use` item. Only
/// LEAF names — for `use crate::nalgebra::DVector;`, only `DVector` is
/// bound (not `nalgebra`); for `use crate::nalgebra;` the sibling name
/// itself is bound; for `use crate::nalgebra::*;` nothing definite is
/// bound (we conservatively return an empty set for globs).
///
/// Used by `inject_imports` to avoid emitting a duplicate
/// `use crate::SIBLING;` when the file already imports the sibling
/// directly (E0252 "name defined multiple times").
fn imported_sibling_names(src: &str) -> HashSet<String> {
    let Ok(file) = syn::parse_file(src) else {
        return HashSet::new();
    };
    let mut names = HashSet::new();
    fn walk(tree: &syn::UseTree, parent_segment: Option<&str>, names: &mut HashSet<String>) {
        match tree {
            // `a::b::c` — the binding is whatever `c` resolves to;
            // recurse remembering `b` as the parent (so a `self` leaf
            // inside knows it imports `b`).
            syn::UseTree::Path(p) => walk(&p.tree, Some(&p.ident.to_string()), names),
            syn::UseTree::Name(n) => {
                let n = n.ident.to_string();
                // `use a::b::{self};` binds `b`, not `self`.
                if n == "self" {
                    if let Some(parent) = parent_segment {
                        names.insert(parent.to_string());
                    }
                } else {
                    names.insert(n);
                }
            }
            syn::UseTree::Rename(r) => {
                names.insert(r.rename.to_string());
            }
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    walk(item, parent_segment, names);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    for item in &file.items {
        if let syn::Item::Use(u) = item {
            walk(&u.tree, None, &mut names);
        }
    }
    names
}

/// Convert a Cargo package name (which may contain `-`) into a Rust
/// identifier suitable for `mod NAME` and path expressions. Cargo
/// auto-applies the same transformation when materialising deps as
/// `extern crate NAME;`, so this matches what the user code already
/// expects.
pub fn to_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Rewriter for `crate::FOO` and `SIBLING::FOO` paths that appear inside
/// macro invocations (e.g. `ok!(...)`, `assert_eq!(...)`). syn's AST
/// walker skips tokens inside macro invocations, so the AST-based
/// rewriters miss these paths and they end up unresolved at compile
/// time. This pass walks the raw token stream of every non-`macro_rules!`
/// macro invocation and rewrites:
///   - `crate::` → `crate::<crate_name>::`
///   - `SIBLING::` → `crate::SIBLING::` (when SIBLING is a vendored
///     sibling crate's mod name)
fn collect_macro_invocation_token_rewrites(
    file: &syn::File,
    crate_name: &str,
    siblings: &HashSet<String>,
    aliases: &HashMap<String, String>,
    dep_wide_ident_pair_matcher_macros: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    // Union per-file scan (catches in-same-file definition+call) with
    // dep-wide scan (catches mio's `impl_debug!` defined in lib.rs and
    // called in sys/unix/net.rs). The dep-wide scan is strict — only
    // marks macros whose EVERY arm matcher uses pure ident-pair shape
    // (no `$x:path`/`$x:expr`/etc.); over-permissive marking would
    // skip rewrites that proc-macro expansions need.
    let mut ident_pair_matcher_macros = collect_ident_pair_matcher_macro_names(file);
    for n in dep_wide_ident_pair_matcher_macros {
        ident_pair_matcher_macros.insert(n.clone());
    }
    struct V<'a> {
        crate_name: &'a str,
        siblings: &'a HashSet<String>,
        aliases: &'a HashMap<String, String>,
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
        ident_pair_matcher_macros: &'a HashSet<String>,
    }
    impl<'a> V<'a> {
        fn process_tokens(&mut self, ts: &proc_macro2::TokenStream) {
            let tokens: Vec<TokenTree> = ts.clone().into_iter().collect();
            // First pass: identify byte ranges that belong to nested
            // ident-pair-matcher macro invocation args. Skip rewriting
            // anything inside those ranges below.
            let mut skip_ranges: Vec<Range<usize>> = Vec::new();
            for i in 0..tokens.len() {
                if let TokenTree::Ident(id) = &tokens[i]
                    && self.ident_pair_matcher_macros.contains(&id.to_string())
                    && matches!(tokens.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!')
                    && let Some(TokenTree::Group(g)) = tokens.get(i + 2)
                {
                    skip_ranges.push(g.span().byte_range());
                }
            }
            let in_skip = |span: &Range<usize>| -> bool {
                skip_ranges
                    .iter()
                    .any(|r| span.start >= r.start && span.end <= r.end)
            };
            for i in 0..tokens.len() {
                if let TokenTree::Ident(id) = &tokens[i] {
                    let followed_by_colon_colon = matches!(
                        tokens.get(i + 1),
                        Some(TokenTree::Punct(c)) if c.as_char() == ':' && c.spacing() == proc_macro2::Spacing::Joint,
                    ) && matches!(
                        tokens.get(i + 2),
                        Some(TokenTree::Punct(c)) if c.as_char() == ':',
                    );
                    if followed_by_colon_colon {
                        // Skip when this ident is a non-leading path
                        // segment, i.e. preceded by `::` (two joined
                        // colons). A bare single `:` is type-ascription
                        // syntax (`field: crate::Foo`, `arg: SIB::Foo`)
                        // and IS a rewrite target — don't conflate the
                        // two. Also skip macro metavariables (`$crate`).
                        let prev = tokens.get(i.wrapping_sub(1));
                        let prev_prev = tokens.get(i.wrapping_sub(2));
                        let preceded_by_dollar = matches!(
                            prev,
                            Some(TokenTree::Punct(p)) if p.as_char() == '$',
                        );
                        let preceded_by_double_colon = matches!(
                            prev,
                            Some(TokenTree::Punct(p)) if p.as_char() == ':',
                        ) && matches!(
                            prev_prev,
                            Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == proc_macro2::Spacing::Joint,
                        );
                        if !preceded_by_dollar && !preceded_by_double_colon {
                            let span = id.span().byte_range();
                            if span.end > span.start
                                && !is_in_deleted(&span, self.deleted)
                                && !in_skip(&span)
                            {
                                let name = id.to_string();
                                if name == "crate" {
                                    let pos = span.end;
                                    self.edits
                                        .push((pos..pos, format!("::{}", self.crate_name)));
                                } else if let Some(real) = self.aliases.get(&name) {
                                    let r = if self.siblings.contains(real) {
                                        format!("crate::{real}")
                                    } else {
                                        real.clone()
                                    };
                                    self.edits.push((span, r));
                                } else if self.siblings.contains(&name) {
                                    self.edits.push((span, format!("crate::{name}")));
                                }
                            }
                        }
                    }
                }
                if let TokenTree::Group(g) = &tokens[i] {
                    // Skip recursion if this Group is the args of a
                    // nested ident-pair-matcher macro invocation — the
                    // rewrite would change `libc::EVFILT_READ` to
                    // `crate::libc::EVFILT_READ` and break the matcher.
                    let span = g.span().byte_range();
                    if !in_skip(&span) {
                        self.process_tokens(&g.stream());
                    }
                }
            }
        }
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            // macro_rules! bodies have their own pass (`$crate` rewriting).
            if mac.path.is_ident("macro_rules") {
                return;
            }
            // Skip if this macro's matcher uses `$IDENT:ident :: $IDENT:ident`
            // shape — the rewrite would produce three idents that the matcher
            // rejects, and the sibling-import injection makes the original
            // path resolve naturally.
            if let Some(name) = mac.path.get_ident()
                && self.ident_pair_matcher_macros.contains(&name.to_string())
            {
                return;
            }
            self.process_tokens(&mac.tokens);
        }
    }
    let mut v = V {
        crate_name,
        siblings,
        aliases,
        edits,
        deleted,
        ident_pair_matcher_macros: &ident_pair_matcher_macros,
    };
    v.visit_file(file);
}

/// Pre-scan dep file for `macro_rules! NAME { ... }` definitions whose
/// matcher arms include the literal token sequence `$IDENT:ident ::
/// $IDENT:ident`. Returns the set of NAMEs. Caller skips path rewrites
/// inside invocations of these macros — the rewrite would change a
/// two-ident path (e.g. `libc::EVFILT_READ`) into a three-ident path
/// (`crate::libc::EVFILT_READ`) the matcher can't accept.
fn collect_ident_pair_matcher_macro_names(file: &syn::File) -> HashSet<String> {
    use syn::visit::Visit;
    struct V {
        names: HashSet<String>,
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_item_macro(&mut self, im: &'ast syn::ItemMacro) {
            if !im.mac.path.is_ident("macro_rules") {
                return;
            }
            if let Some(name) = &im.ident
                && all_arms_ident_pair_only(&im.mac.tokens)
            {
                self.names.insert(name.to_string());
            }
        }
    }
    let mut v = V {
        names: HashSet::new(),
    };
    v.visit_file(file);
    v.names
}

/// True if ANY arm of the macro_rules body has a matcher
/// containing the `$IDENT:ident :: $IDENT:ident` shape. Marking a
/// macro as ident-pair-matcher tells the path rewriter to skip
/// rewrites inside its invocation args.
///
/// Tagging is safe even for macros that ALSO accept other shapes
/// (e.g. mio's `impl_debug!` matcher mixes `:path`, `:meta`, and
/// `:ident :: :ident`): the `$libc:ident` constraint inside the
/// repetition rejects multi-segment paths regardless of the other
/// specifiers in the matcher, so the rewrite would always break
/// these invocations. Skip is harmless for callers using non-
/// ident-pair shapes — the rewrite only fires on `IDENT::IDENT`
/// patterns anyway.
fn all_arms_ident_pair_only(body: &proc_macro2::TokenStream) -> bool {
    let toks: Vec<TokenTree> = body.clone().into_iter().collect();
    has_ident_pair_shape(&toks)
}

/// True if the token sequence (or any nested Group) contains the
/// 10-token shape `$ IDENT : ident :: $ IDENT : ident` (where `::`
/// is two Punct tokens). Fragment specifier `ident` is a literal
/// keyword in matcher syntax — it can ONLY appear in macro_rules
/// matchers, not in expansion bodies, so detection is unambiguous.
fn has_ident_pair_shape(toks: &[TokenTree]) -> bool {
    for w in toks.windows(10) {
        let is_pair = matches!(&w[0], TokenTree::Punct(p) if p.as_char() == '$')
            && matches!(&w[1], TokenTree::Ident(_))
            && matches!(&w[2], TokenTree::Punct(p) if p.as_char() == ':')
            && matches!(&w[3], TokenTree::Ident(id) if id == "ident")
            && matches!(&w[4], TokenTree::Punct(p) if p.as_char() == ':')
            && matches!(&w[5], TokenTree::Punct(p) if p.as_char() == ':')
            && matches!(&w[6], TokenTree::Punct(p) if p.as_char() == '$')
            && matches!(&w[7], TokenTree::Ident(_))
            && matches!(&w[8], TokenTree::Punct(p) if p.as_char() == ':')
            && matches!(&w[9], TokenTree::Ident(id) if id == "ident");
        if is_pair {
            return true;
        }
    }
    for tt in toks {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            if has_ident_pair_shape(&inner) {
                return true;
            }
        }
    }
    false
}

/// Strip lint attrs (`#[deny(...)]`, `#[warn(...)]`, `#[forbid(...)]`)
/// from vendored sources. After [`collect_mod_visibility_bumps`] turns
/// every `mod` into `pub mod`, lints like `missing_docs` start firing
/// on previously-private mods, which a vendored dep with
/// `#![deny(missing_docs)]` would treat as compile errors. Strip the
/// lint config rather than fight individual lints.
fn collect_lint_attr_removals(
    file: &syn::File,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            let path = attr.path();
            // Strip lints that would flip to errors under bumped
            // visibility (deny/warn/forbid). KEEP `#[allow(...)]` —
            // those suppress lints that the dep author knew about and
            // explicitly opted out of (e.g. `dangerous_implicit_autorefs`
            // in allocator-api2's raw-pointer code). Stripping the
            // allow re-enables the lint, which is now hard-error in
            // newer rustc.
            //
            // Also strip tool attributes (`#[clippy::*]`, `#[rustfmt::*]`)
            // — these are inert hints, but their presence on a
            // `macro_rules!` item marks the macro as "macro-expanded"
            // for the purposes of `#[macro_export]` resolution, which
            // disables `crate::macro_name!()` access entirely.
            let is_lint = path.is_ident("deny") || path.is_ident("warn") || path.is_ident("forbid");
            let is_tool_attr = path.segments.len() >= 2
                && matches!(
                    path.segments.first().map(|s| s.ident.to_string()),
                    Some(ref s) if s == "clippy" || s == "rustfmt"
                );
            if !is_lint && !is_tool_attr {
                return;
            }
            let Some(span) = attr_byte_range(attr) else {
                return;
            };
            if !is_in_deleted(&span, self.deleted) {
                self.edits.push((span, String::new()));
            }
        }
    }
    let mut v = V { edits, deleted };
    v.visit_file(file);
}

/// Downgrade `pub use ::core as __NAME;` (and `::std`, `::alloc`)
/// re-exports to `pub(crate) use`. Re-exporting an extern crate as
/// `pub` from a non-root module is rejected by rustc with E0365
/// ("extern crate ... is private and cannot be re-exported").
/// bytemuck's `pub use ::core as __core;` (used by its `Pod` derive
/// macros) is the canonical example.
fn collect_extern_crate_reexport_downgrades(
    file: &syn::File,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    // Original target shape: a vendored dep with `extern crate FOO;` +
    // `pub use FOO;` / `pub use FOO::*;` / `pub use FOO as ALIAS;`,
    // where FOO was an alias of `core` / `std` / `alloc` (or those
    // were stripped to bare). The bare `pub use stdlib;` would
    // re-export the stdlib root publicly through the wrapping
    // `pub mod <crate>` — not desirable. Downgrade those to
    // `pub(crate)`.
    //
    // Crucially do NOT downgrade `pub use std::os::unix::io::AsFd;`
    // and similar specific-item re-exports — rustix relies on these
    // for its own pub re-export chain; downgrading produces E0365
    // ("crate public, cannot re-export outside") in callers.
    fn is_extern_crate_shape(tree: &syn::UseTree) -> bool {
        match tree {
            // `pub use stdlib;`
            syn::UseTree::Name(n) => {
                matches!(n.ident.to_string().as_str(), "core" | "std" | "alloc")
            }
            // `pub use stdlib as ALIAS;`
            syn::UseTree::Rename(r) => {
                matches!(r.ident.to_string().as_str(), "core" | "std" | "alloc")
            }
            // `pub use stdlib::*;` — stdlib root → glob.
            syn::UseTree::Path(p) => {
                matches!(p.ident.to_string().as_str(), "core" | "std" | "alloc")
                    && matches!(&*p.tree, syn::UseTree::Glob(_))
            }
            _ => false,
        }
    }
    fn walk(
        items: &[syn::Item],
        deleted: &[Range<usize>],
        edits: &mut Vec<(Range<usize>, String)>,
    ) {
        for item in items {
            if let syn::Item::Use(u) = item
                && matches!(u.vis, syn::Visibility::Public(_))
                && is_extern_crate_shape(&u.tree)
            {
                let span = match &u.vis {
                    syn::Visibility::Public(p) => p.span.byte_range(),
                    _ => continue,
                };
                if span.end > span.start && !is_in_deleted(&span, deleted) {
                    edits.push((span, "pub(crate)".to_string()));
                }
            }
            if let syn::Item::Mod(m) = item
                && let Some((_, inner)) = &m.content
            {
                walk(inner, deleted, edits);
            }
        }
    }
    walk(&file.items, deleted, edits);
}

/// Delete `(pub)? use ... as core/std/alloc;` items from vendored sources.
/// These are aliases that shadow the extern prelude — fine in the original
/// crate where the alias was at crate root, but inside our wrapping
/// `mod <crate>` they cause name resolution in submodules to prefer the
/// alias over the actual `core`/`std`/`alloc` from the user's extern prelude
/// (e.g. `use core::cmp` from `nalgebra::base::ops` resolves to
/// `nalgebra::core` = `nalgebra::base`, then fails because `cmp` isn't
/// there). nalgebra's deprecated `pub use base as core;` is the canonical
/// example.
///
/// Adds the deletion to the shared `deleted` list rather than emitting a
/// direct edit, so other passes (cfg-attr eval, lint stripping, …)
/// skip touching attrs on the deleted item — overlapping edits would
/// over-delete adjacent bytes when applied.
fn collect_stdlib_alias_deletions(file: &syn::File, deleted: &mut Vec<Range<usize>>) {
    use syn::spanned::Spanned;
    fn alias_is_stdlib(tree: &syn::UseTree) -> bool {
        match tree {
            syn::UseTree::Rename(r) => {
                let n = r.rename.to_string();
                matches!(n.as_str(), "core" | "std" | "alloc")
            }
            syn::UseTree::Group(g) => g.items.iter().any(alias_is_stdlib),
            _ => false,
        }
    }
    for item in &file.items {
        if let syn::Item::Use(u) = item
            && matches!(u.vis, syn::Visibility::Public(_))
            && alias_is_stdlib(&u.tree)
        {
            // Only delete `pub use X as core/std/alloc;` — pub-visibility
            // aliases at lib.rs root that shadow the extern prelude for
            // every submod. Private `use X as core;` (e.g. rand_chacha's
            // `#[cfg(feature = "std")] use std as core;` paired with
            // `use self::core::fmt;`) is local to the file and load-bearing.
            deleted.push(item.span().byte_range());
        }
    }
}

/// Bump every private (`Inherited`) `mod NAME` declaration to
/// `pub mod NAME`. Required so the macro-export re-export chain
/// (emitted at the vendored mod's root by [`collect_macro_export_paths`])
/// can traverse private ancestor mods, AND so that `pub use` re-exports
/// of items reached via vendored sibling mods don't trip E0365
/// ("only public within the crate"). The user-facing public API
/// boundary doesn't matter for flat-file outputs (typically binaries
/// or single-file gists), so loosening visibility is safe here.
fn collect_mod_visibility_bumps(
    file: &syn::File,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    bump_in_items(&file.items, deleted, edits);
}

fn bump_in_items(
    items: &[syn::Item],
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    // Names re-exported via `pub use ...::NAME;` at this scope. Bumping
    // a same-named `mod NAME` to `pub mod` would collide with the
    // re-export in the type namespace. parry's
    // `mod intersection_test;` + `pub use intersection_test::intersection_test;`
    // is the canonical example.
    let pub_use_leaves = collect_pub_use_leaves(items);
    for item in items {
        if let syn::Item::Mod(m) = item {
            let name = m.ident.to_string();
            if matches!(m.vis, syn::Visibility::Inherited) && !pub_use_leaves.contains(&name) {
                let span = m.mod_token.span.byte_range();
                if span.end > span.start && !is_in_deleted(&span, deleted) {
                    edits.push((span, "pub mod".to_string()));
                }
            }
            if let Some((_, inner)) = &m.content {
                bump_in_items(inner, deleted, edits);
            }
        }
    }
}

fn collect_pub_use_leaves(items: &[syn::Item]) -> HashSet<String> {
    let mut leaves = HashSet::new();
    fn walk(tree: &syn::UseTree, parent: Option<&str>, leaves: &mut HashSet<String>) {
        match tree {
            syn::UseTree::Path(p) => walk(&p.tree, Some(&p.ident.to_string()), leaves),
            syn::UseTree::Name(n) => {
                let n = n.ident.to_string();
                if n == "self" {
                    if let Some(parent) = parent {
                        leaves.insert(parent.to_string());
                    }
                } else {
                    leaves.insert(n);
                }
            }
            syn::UseTree::Rename(r) => {
                leaves.insert(r.rename.to_string());
            }
            syn::UseTree::Group(g) => {
                for it in &g.items {
                    walk(it, parent, leaves);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    for item in items {
        if let syn::Item::Use(u) = item {
            // Treat `pub use` and `pub(crate) use` (the visibility our
            // own macro-export rewrite synthesises) the same way for
            // collision detection. A bumped `mod NAME` colliding with
            // a same-scope `pub(crate) use NAME` is just as broken as
            // colliding with `pub use NAME` (REVIEW B5).
            let is_pub_or_pub_crate = match &u.vis {
                syn::Visibility::Public(_) => true,
                syn::Visibility::Restricted(r) => {
                    r.path.get_ident().map(|id| id == "crate").unwrap_or(false)
                }
                syn::Visibility::Inherited => false,
            };
            if is_pub_or_pub_crate {
                walk(&u.tree, None, &mut leaves);
            }
        }
    }
    leaves
}

/// Remove `#[doc = MACRO!(...)]` attributes from the vendored source.
/// `include_str!`/`include_bytes!` paths in doc attrs are relative to the
/// dep's source directory and break compilation in the user's tree.
/// Static string `#[doc = "..."]` attrs are kept.
fn collect_macro_doc_attr_removals(
    file: &syn::File,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::visit::Visit;
    struct V<'a> {
        edits: &'a mut Vec<(Range<usize>, String)>,
        deleted: &'a [Range<usize>],
    }
    impl<'ast, 'a> Visit<'ast> for V<'a> {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            if !attr.path().is_ident("doc") {
                return;
            }
            let syn::Meta::NameValue(nv) = &attr.meta else {
                return;
            };
            if !matches!(nv.value, syn::Expr::Macro(_)) {
                return;
            }
            let Some(span) = attr_byte_range(attr) else {
                return;
            };
            if is_in_deleted(&span, self.deleted) {
                return;
            }
            self.edits.push((span, String::new()));
        }
    }
    let mut v = V { edits, deleted };
    v.visit_file(file);
}

/// Pre-pass: items that `collect_extern_crate_removals` will fully
/// strip (no `use ...` replacement) get added to the shared `deleted`
/// list with their FULL item span (including visibility, attrs, and
/// anything else syn includes in `Item::span()`). This serves two
/// purposes:
///   1. Other passes (cfg-attr eval, lint stripping, …) skip attrs on
///      these items, avoiding overlapping-edit over-deletion.
///   2. The full span includes leading `pub` and outer attrs, so
///      `pub extern crate FOO;` strips cleanly without orphaning
///      the `pub` keyword.
fn collect_extern_crate_strip_deletions(
    file: &syn::File,
    _siblings: &HashSet<String>,
    deleted: &mut Vec<Range<usize>>,
) {
    // Snapshot the cfg-False / stdlib-alias deletions added by earlier
    // passes so we can skip recursing into items that are already going
    // to disappear. Without this, an `extern crate std;` inside a
    // cfg-False `mod tests { ... }` would get its own entry pushed,
    // overlapping the outer mod's deletion — and `replace_range` blows
    // up when the inner edit shrinks the string before the outer one
    // is applied.
    let already_deleted = deleted.clone();
    walk_extern_crate_strip(&file.items, &already_deleted, deleted);
}

fn walk_extern_crate_strip(
    items: &[syn::Item],
    already_deleted: &[Range<usize>],
    deleted: &mut Vec<Range<usize>>,
) {
    use syn::spanned::Spanned;
    for item in items {
        let item_span = item.span().byte_range();
        if is_in_deleted(&item_span, already_deleted) {
            continue;
        }
        if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            walk_extern_crate_strip(inner, already_deleted, deleted);
        }
        if let syn::Item::ExternCrate(ec) = item {
            let crate_name = ec.ident.to_string();
            let is_stdlib = matches!(crate_name.as_str(), "alloc" | "core" | "std");
            let alias_is_stdlib = ec
                .rename
                .as_ref()
                .is_some_and(|(_, a)| matches!(a.to_string().as_str(), "alloc" | "core" | "std"));
            let has_alias = ec.rename.is_some();
            // Stripped (no replacement):
            //   - plain `extern crate alloc;` (stdlib, no alias) —
            //     hoisted to user crate root.
            //   - `extern crate core as std;` etc. — the no_std → std
            //     shim alias. The user's flat output has real
            //     std/core/alloc via extern prelude; the alias would
            //     shadow extern prelude and break globs.
            // KEPT (handled by extern_crate_removals):
            //   - `extern crate alloc as __alloc;` (downcast-rs's
            //     `__alloc::boxed::Box` from its macro body).
            let stripped = is_stdlib && (!has_alias || alias_is_stdlib);
            if stripped {
                deleted.push(item.span().byte_range());
            }
        }
    }
}

fn collect_extern_crate_removals(
    file: &syn::File,
    siblings: &HashSet<String>,
    inlined_proc_macros: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    walk_extern_crate(&file.items, siblings, inlined_proc_macros, deleted, edits);
}

fn walk_extern_crate(
    items: &[syn::Item],
    siblings: &HashSet<String>,
    inlined_proc_macros: &HashSet<String>,
    deleted: &[Range<usize>],
    edits: &mut Vec<(Range<usize>, String)>,
) {
    for item in items {
        if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            walk_extern_crate(inner, siblings, inlined_proc_macros, deleted, edits);
        }
        if let syn::Item::ExternCrate(ec) = item {
            let crate_name = ec.ident.to_string();
            let has_macro_use = ec.attrs.iter().any(|a| a.path().is_ident("macro_use"));
            let is_stdlib = matches!(crate_name.as_str(), "alloc" | "core" | "std");
            let start = ec.extern_token.span.byte_range().start;
            let end = ec.semi_token.span.byte_range().end;
            let span = start..end;
            if is_in_deleted(&span, deleted) {
                continue;
            }
            // `extern crate FOO as BAR;` introduces an alias `BAR` for
            // crate FOO. Stripping the line drops the alias, breaking
            // every later `use BAR::Foo` (e.g. nalgebra's
            // `extern crate num_traits as num;` followed by
            // `use num::Zero;`). Replace with the equivalent `use`.
            let alias = ec.rename.as_ref().map(|(_, ident)| ident.to_string());
            // `#[macro_use] extern crate SIBLING;` is the legacy 2015
            // way to bring SIBLING's exported macros into scope at the
            // crate root. Stripping the line drops the macro import too,
            // breaking call sites that use the macros bare.
            let replacement = match (has_macro_use, &alias) {
                (true, _) if siblings.contains(&crate_name) => {
                    // sibling + macro_use: glob to get the macros AND
                    // re-export the sibling so submods can reach it
                    // via `crate::SELF::SIBLING::Foo`.
                    Some(format!(
                        "pub(crate) use crate::{crate_name}; use crate::{crate_name}::*;"
                    ))
                }
                (true, _) if is_stdlib => {
                    // alloc/core/std + macro_use: skip the glob —
                    // `vec!` / `format!` / `println!` / `assert_eq!`
                    // come through std's prelude unconditionally, and
                    // the glob would shadow `extern crate alloc;`
                    // (hoisted to user main.rs root) with the inner
                    // `alloc::alloc` submod (E0659).
                    None
                }
                (true, _) => {
                    // external + macro_use: re-export the dep so
                    // `crate::SELF::FOO::Bar` references in submods
                    // resolve, AND glob in for the macros.
                    Some(format!("pub(crate) use {crate_name}; use {crate_name}::*;"))
                }
                (_, Some(alias)) if siblings.contains(&crate_name) => {
                    Some(format!("use crate::{crate_name} as {alias};"))
                }
                (_, Some(alias))
                    if is_stdlib && matches!(alias.as_str(), "alloc" | "core" | "std") =>
                {
                    // Stdlib alias-of-stdlib (e.g. `extern crate core as std;`
                    // — the no_std → pseudo-std shim). The user's flat
                    // output has the real `std`/`core`/`alloc` available
                    // via extern prelude, so the alias would just
                    // shadow extern prelude and break glob imports
                    // (`use crate::log::*` brings the alias `std` into
                    // scope, conflicts with `extern crate std;`). Strip
                    // the item.
                    None
                }
                (_, Some(alias)) if is_stdlib => {
                    // Stdlib aliased to a non-stdlib name (e.g.
                    // `extern crate alloc as __alloc;`): re-export of
                    // an extern crate as pub from a non-root mod hits
                    // E0365. Force-downgrade to pub(crate). Use
                    // `::alloc/core/std` (leading colon) to
                    // disambiguate from any glob-imported
                    // sibling-internal `alloc` mod (E0659).
                    Some(format!("pub(crate) use ::{crate_name} as {alias};"))
                }
                (_, Some(alias)) => {
                    // non-sibling alias (external dep): re-introduce the
                    // alias as a normal `use`. The crate is in the
                    // user's extern prelude.
                    Some(format!("use {crate_name} as {alias};"))
                }
                (false, None) if is_stdlib => None,
                (false, None) if siblings.contains(&crate_name) => {
                    // sibling, no alias: re-introduce as
                    // `pub(crate) use crate::FOO;` so submods reaching
                    // through `crate::SELF::FOO::Bar` resolve.
                    Some(format!("pub(crate) use crate::{crate_name};"))
                }
                (false, None) if inlined_proc_macros.contains(&crate_name) => {
                    // External proc-macro that --expand / --expand-deep
                    // already inlined out. Don't synthesise a
                    // `pub(crate) use FOO;` — the crate is no longer in
                    // the user's extern prelude (proc-macros aren't
                    // typically declared in `[dependencies]`), so the
                    // import would dangle. serde's `extern crate
                    // serde_derive;` after --expand-deep is the
                    // canonical case.
                    None
                }
                (false, None) => {
                    // external, no alias: re-introduce as
                    // `pub(crate) use FOO;` (note: pub(crate), not just
                    // `use`) so submods that have
                    // `use crate::SELF::FOO::Bar` (rewritten from
                    // original `use crate::FOO::Bar` via 2015-style
                    // `extern crate FOO;`) keep resolving.
                    // parry2d's `extern crate approx;` plus internal
                    // `use crate::approx::AbsDiffEq;` is the typical
                    // case.
                    Some(format!("pub(crate) use {crate_name};"))
                }
            };
            match replacement {
                Some(r) => {
                    // The replacement string already encodes its own
                    // visibility (`pub(crate) use ...` or `use ...`).
                    // Swallow any leading `pub` from the original
                    // `pub extern crate ...;` so we don't end up with
                    // `pub pub(crate) use ...`. Stop short of cfg
                    // attrs (the cfg pass owns those) — its strip edit
                    // would over-delete if the spans overlapped.
                    let vis_start = match &ec.vis {
                        syn::Visibility::Public(p) => p.span.byte_range().start,
                        syn::Visibility::Restricted(r) => r.pub_token.span.byte_range().start,
                        syn::Visibility::Inherited => span.start,
                    };
                    let target_span = vis_start..span.end;
                    edits.push((target_span, r));
                    // Strip only the attrs that are valid on
                    // `extern crate` but invalid on a `use` item
                    // (`#[macro_use]`). Other outer attrs (`#[cfg]`,
                    // `#[deny]`, …) are handled by their own passes —
                    // duplicating those edits over-deletes when the
                    // edit list is applied (see commit history).
                    for attr in &ec.attrs {
                        if attr.path().is_ident("macro_use")
                            && let Some(span) = attr_byte_range(attr)
                            && !is_in_deleted(&span, deleted)
                        {
                            edits.push((span, String::new()));
                        }
                    }
                }
                None => {
                    edits.push((span, String::new()));
                }
            }
        }
    }
}

/// Walk the dep's source for `extern crate alloc;` / `core` / `std`
/// declarations (at any depth). The names get hoisted to the user
/// crate's root so the import lands in the crate-root extern prelude.
fn collect_extern_std_libs(src: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let mut found: Vec<String> = Vec::new();
    walk_for_extern_std(&file.items, &mut found);
    found.sort();
    found.dedup();
    found
}

fn walk_for_extern_std(items: &[syn::Item], found: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::ExternCrate(ec) => {
                let name = ec.ident.to_string();
                if matches!(name.as_str(), "alloc" | "core" | "std") {
                    found.push(name);
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk_for_extern_std(inner, found);
                }
            }
            _ => {}
        }
    }
}
