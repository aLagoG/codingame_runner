//! Snapshot tests pinning the exact flattened output for canonical fixtures.
//!
//! Output format is intentionally locked here. If you're changing the
//! formatting on purpose, run `cargo insta review` to accept the new
//! snapshots; if these change unexpectedly, that's a regression.

use flatten::parse_package;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn make_crate(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, contents) in files {
        let full = dir.path().join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, contents).unwrap();
    }
    dir
}

fn flatten(dir: &Path) -> String {
    let pkg = parse_package(dir).expect("parse_package");
    pkg.source.to_string()
}

#[test]
fn snap_basic_inlining() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod foo;\n\npub use foo::greet;\n"),
        ("src/foo.rs", "pub fn greet(name: &str) {\n    println!(\"hi, {name}\");\n}\n"),
    ]);
    insta::assert_snapshot!(flatten(dir.path()));
}

#[test]
fn snap_nested_via_foo_rs() {
    let dir = make_crate(&[
        ("src/lib.rs", "pub mod outer;\n"),
        ("src/outer.rs", "pub mod inner;\n\npub fn outer_only() {}\n"),
        ("src/outer/inner.rs", "pub fn deep() {}\n"),
    ]);
    insta::assert_snapshot!(flatten(dir.path()));
}

#[test]
fn snap_visibility_variants() {
    let dir = make_crate(&[
        ("src/lib.rs", "pub mod a;\npub(crate) mod b;\nmod c;\n"),
        ("src/a.rs", "pub fn a() {}\n"),
        ("src/b.rs", "pub fn b() {}\n"),
        ("src/c.rs", "pub fn c() {}\n"),
    ]);
    insta::assert_snapshot!(flatten(dir.path()));
}

#[test]
fn snap_cfg_skipped_mod_is_preserved() {
    // `mod absent;` carries a cfg and the file doesn't exist — should be
    // left in the output verbatim with a warning (warning is not in output).
    let dir = make_crate(&[(
        "src/lib.rs",
        "#[cfg(any())]\nmod absent;\n\npub fn a() {}\n",
    )]);
    insta::assert_snapshot!(flatten(dir.path()));
}

#[test]
fn snap_inline_mod_with_external_nested() {
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "pub mod outer {\n    pub mod inner;\n}\n",
        ),
        ("src/outer/inner.rs", "pub fn deep() {}\n"),
    ]);
    insta::assert_snapshot!(flatten(dir.path()));
}

#[test]
fn snap_path_attr_resolves() {
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "#[path = \"renamed.rs\"]\nmod foo;\n",
        ),
        ("src/renamed.rs", "pub fn x() {}\n"),
    ]);
    insta::assert_snapshot!(flatten(dir.path()));
}
