// FlattenError carries miette source spans (NamedSource owns the source
// string), which makes the enum heavy. Boxing the Result variant would
// fragment every call site for a non-hot path; silence the lint instead.
#![allow(clippy::result_large_err)]

//! Flatten a Rust crate's source tree into a single `.rs` file.
//!
//! Given a crate directory, [`parse_package`] (auto-selects the target) or
//! [`parse_target`] (explicit selection) reads the entry source file
//! (`lib.rs`, `main.rs`, `examples/*.rs`, etc.), recursively inlines every
//! external `mod NAME;` declaration found via syn, and returns a
//! [`FlattenedPackage`] whose `source` field can be written via
//! [`SourceFile::to_file`] or stringified via `Display`.
//!
//! The tool intentionally does **not** vendor external crates: anything
//! reachable through `Cargo.toml` `[dependencies]` stays as a `use foo::...`
//! reference. It also does not evaluate `#[cfg(...)]` — cfg-gated mods whose
//! files are missing are warn-skipped (the original `mod foo;` line stays in
//! the output) so the flat file can later be compiled under any cfg.
//!
//! See `ROADMAP.md` for in- and out-of-scope features.
//!
//! # Example
//!
//! ```no_run
//! use flatten::{parse_package, TargetSelector, parse_target};
//!
//! // Auto-select.
//! let pkg = parse_package("./my-crate")?;
//! pkg.source.to_file("./my-crate.rs")?;
//!
//! // Or pick a specific target.
//! let pkg = parse_target(".", &TargetSelector::Bin("server".into()))?;
//! print!("{}", pkg.source);
//! # Ok::<(), flatten::FlattenError>(())
//! ```

use cargo_toml::{Manifest, Product};
use std::path::{Path, PathBuf};
use tracing::info;

pub(crate) mod cfg;
pub(crate) mod edits;
mod error;
pub mod external_file;
pub mod minify;
mod scanner;
mod source_file;
pub mod vendor;

pub use error::{FlattenError, Result};
pub use source_file::{ModuleTreeNode, ParseOptions, SkippedMod, SourceFile, drain_skipped_mods};

/// Which kind of target was flattened.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PackageType {
    Lib,
    Bin,
    Example,
    Test,
}

impl PackageType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::Bin => "bin",
            Self::Example => "example",
            Self::Test => "test",
        }
    }
}

/// Which target inside a package to flatten.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum TargetSelector {
    /// Pick automatically: the unique bin if there's exactly one, else the
    /// lib if present, else error.
    #[default]
    Auto,
    Lib,
    /// Bin by name (e.g. `--bin foo` ⇒ `Bin("foo".into())`).
    Bin(String),
    Example(String),
    Test(String),
}

/// Result of flattening a target.
#[derive(Debug)]
pub struct FlattenedPackage {
    pub kind: PackageType,
    /// Cargo package name (`[package].name`), or directory basename when no
    /// `Cargo.toml` is present. Used for the banner.
    pub crate_name: String,
    /// Target name (e.g. lib name, bin name). Used for the output filename.
    pub target_name: String,
    pub source: SourceFile,
    /// Entry source file made relative to the crate root (e.g. `src/lib.rs`).
    /// Useful as the root label for [`SourceFile::tree`].
    pub entry_path: PathBuf,
}

/// Flatten the auto-selected target (the unique bin, else the lib).
pub fn parse_package(path: impl AsRef<Path>) -> Result<FlattenedPackage> {
    parse_target(path, &TargetSelector::Auto)
}

/// If `crate_root` has a `[lib]` target (whether implicit `src/lib.rs`
/// or explicit), return its flattened form. Used by `vendor_package`
/// to auto-inline a bin's same-package lib so `use <self_pkg>::X;` in
/// `main.rs` resolves in the flat output. Returns `Ok(None)` when
/// there is no manifest or no lib target.
pub fn parse_self_lib(crate_root: impl AsRef<Path>) -> Result<Option<FlattenedPackage>> {
    let crate_root = crate_root.as_ref();
    let manifest = load_manifest(crate_root)?;
    let has_lib = manifest
        .as_ref()
        .map(|m| m.lib.is_some() || crate_root.join("src/lib.rs").is_file())
        .unwrap_or(false);
    if !has_lib {
        return Ok(None);
    }
    Ok(Some(parse_target(crate_root, &TargetSelector::Lib)?))
}

/// Flatten a specific target.
pub fn parse_target(path: impl AsRef<Path>, selector: &TargetSelector) -> Result<FlattenedPackage> {
    let path = path.as_ref();
    let path = path.canonicalize().map_err(|e| {
        FlattenError::other(format!(
            "Path must be valid and exist. Path: `{}` ({e})",
            path.display()
        ))
    })?;

    if !path.is_dir() {
        return Err(FlattenError::other(format!(
            "Provided path must be a directory. Path: `{}`",
            path.display()
        )));
    }

    let manifest = load_manifest(&path)?;
    let resolved = resolve_target(&path, manifest.as_ref(), selector)?;

    let crate_name = manifest
        .as_ref()
        .and_then(|m| m.package.as_ref().map(|p| p.name.clone()))
        .unwrap_or_else(|| dir_basename(&path));

    info!(
        "Parsing {} `{}` at `{}`",
        resolved.kind.as_str(),
        resolved.target_name,
        resolved.entry.display()
    );

    let entry_path = resolved
        .entry
        .strip_prefix(&path)
        .map(PathBuf::from)
        .unwrap_or_else(|_| resolved.entry.clone());

    Ok(FlattenedPackage {
        kind: resolved.kind,
        crate_name,
        target_name: resolved.target_name,
        source: SourceFile::from_file_with_root(&resolved.entry, &path)?,
        entry_path,
    })
}

fn load_manifest(crate_root: &Path) -> Result<Option<Manifest>> {
    let manifest_path = crate_root.join("Cargo.toml");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let mut manifest = Manifest::from_path(&manifest_path).map_err(|e| {
        FlattenError::other(format!("Failed parsing `{}`: {e}", manifest_path.display()))
    })?;
    manifest.complete_from_path(&manifest_path).ok();
    Ok(Some(manifest))
}

struct ResolvedTarget {
    kind: PackageType,
    target_name: String,
    entry: PathBuf,
}

fn resolve_target(
    crate_root: &Path,
    manifest: Option<&Manifest>,
    selector: &TargetSelector,
) -> Result<ResolvedTarget> {
    match (manifest, selector) {
        (None, TargetSelector::Auto) => fallback_auto(crate_root),
        (None, _) => Err(FlattenError::other(format!(
            "Cargo.toml not found at `{}`; explicit target selectors require a manifest",
            crate_root.join("Cargo.toml").display()
        ))),
        (Some(m), sel) => resolve_with_manifest(crate_root, m, sel),
    }
}

fn fallback_auto(crate_root: &Path) -> Result<ResolvedTarget> {
    let main = crate_root.join("src/main.rs");
    let lib = crate_root.join("src/lib.rs");
    let name = dir_basename(crate_root);
    if main.is_file() {
        Ok(ResolvedTarget {
            kind: PackageType::Bin,
            target_name: name,
            entry: main,
        })
    } else if lib.is_file() {
        Ok(ResolvedTarget {
            kind: PackageType::Lib,
            target_name: name,
            entry: lib,
        })
    } else {
        Err(FlattenError::other(format!(
            "At least one of main.rs or lib.rs should exist on path: `{}`",
            crate_root.join("src").display()
        )))
    }
}

fn resolve_with_manifest(
    crate_root: &Path,
    manifest: &Manifest,
    selector: &TargetSelector,
) -> Result<ResolvedTarget> {
    match selector {
        TargetSelector::Lib => {
            let lib = manifest
                .lib
                .as_ref()
                .ok_or_else(|| FlattenError::other("This crate has no [lib] target"))?;
            resolve_lib(lib, manifest, crate_root)
        }
        TargetSelector::Bin(name) => {
            let bin = find_named(&manifest.bin, name).ok_or_else(|| {
                FlattenError::other(format!(
                    "No `[[bin]]` named `{}`. Available: {}",
                    name,
                    list_names(&manifest.bin)
                ))
            })?;
            Ok(ResolvedTarget {
                kind: PackageType::Bin,
                target_name: name.clone(),
                entry: target_path(bin, crate_root, "src/main.rs")?,
            })
        }
        TargetSelector::Example(name) => resolve_named(
            crate_root,
            &manifest.example,
            name,
            PackageType::Example,
            "[[example]]",
        ),
        TargetSelector::Test(name) => resolve_named(
            crate_root,
            &manifest.test,
            name,
            PackageType::Test,
            "[[test]]",
        ),
        TargetSelector::Auto => match (manifest.bin.len(), manifest.lib.is_some()) {
            (1, _) => {
                let bin = &manifest.bin[0];
                let name = bin.name.clone().unwrap_or_else(|| dir_basename(crate_root));
                Ok(ResolvedTarget {
                    kind: PackageType::Bin,
                    target_name: name,
                    entry: target_path(bin, crate_root, "src/main.rs")?,
                })
            }
            (0, true) => {
                let lib = manifest.lib.as_ref().unwrap();
                resolve_lib(lib, manifest, crate_root)
            }
            (0, false) => Err(FlattenError::other(format!(
                "No bin or lib targets found in `{}`",
                crate_root.join("Cargo.toml").display()
            ))),
            (n, _) => {
                if let Some(default_run) = manifest
                    .package
                    .as_ref()
                    .and_then(|p| p.default_run.as_deref())
                    && let Some(bin) = find_named(&manifest.bin, default_run)
                {
                    return Ok(ResolvedTarget {
                        kind: PackageType::Bin,
                        target_name: default_run.to_string(),
                        entry: target_path(bin, crate_root, "src/main.rs")?,
                    });
                }
                Err(FlattenError::other(format!(
                    "Found {} bin targets ({}); pass --bin NAME to choose one or --lib for the library",
                    n,
                    list_names(&manifest.bin)
                )))
            }
        },
    }
}

fn resolve_lib(lib: &Product, manifest: &Manifest, crate_root: &Path) -> Result<ResolvedTarget> {
    Ok(ResolvedTarget {
        kind: PackageType::Lib,
        target_name: target_name(lib, manifest).unwrap_or_else(|| dir_basename(crate_root)),
        entry: target_path(lib, crate_root, "src/lib.rs")?,
    })
}

fn resolve_named(
    crate_root: &Path,
    products: &[Product],
    name: &str,
    kind: PackageType,
    label: &str,
) -> Result<ResolvedTarget> {
    let target = find_named(products, name).ok_or_else(|| {
        FlattenError::other(format!(
            "No `{}` named `{}`. Available: {}",
            label,
            name,
            list_names(products)
        ))
    })?;
    let default_path = match kind {
        PackageType::Example => format!("examples/{name}.rs"),
        PackageType::Test => format!("tests/{name}.rs"),
        _ => unreachable!(),
    };
    Ok(ResolvedTarget {
        kind,
        target_name: name.to_string(),
        entry: target_path(target, crate_root, &default_path)?,
    })
}

fn find_named<'a>(products: &'a [Product], name: &str) -> Option<&'a Product> {
    products.iter().find(|p| p.name.as_deref() == Some(name))
}

fn list_names(products: &[Product]) -> String {
    let names: Vec<&str> = products.iter().filter_map(|p| p.name.as_deref()).collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

fn target_name(product: &Product, manifest: &Manifest) -> Option<String> {
    product
        .name
        .clone()
        .or_else(|| manifest.package.as_ref().map(|p| p.name.clone()))
}

fn target_path(product: &Product, crate_root: &Path, default: &str) -> Result<PathBuf> {
    let rel = product.path.as_deref().unwrap_or(default);
    let full = crate_root.join(rel);
    if !full.is_file() {
        return Err(FlattenError::other(format!(
            "Target source file not found: `{}` (expected at `{}`)",
            rel,
            full.display()
        )));
    }
    Ok(full)
}

fn dir_basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "package".to_string())
}
