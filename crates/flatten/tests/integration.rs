//! Integration tests for flatten.
//!
//! Tests build synthetic crates in tempdirs and exercise the public API.
//! The bottom of the file has smoke tests against real crates cloned into
//! `test-crates/` (gitignored) — they silently skip when absent.

use flatten::vendor::{self, Classification, ExternalReason, VendorOptions};
use flatten::{PackageType, TargetSelector, parse_package, parse_target};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a temporary crate from a list of `(relative path, contents)` tuples.
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

/// Flatten the crate at `dir` and return the in-memory string output.
fn flatten_str(dir: &Path) -> (PackageType, String) {
    let pkg = parse_package(dir).expect("parse_package");
    (pkg.kind, pkg.source.to_string())
}

/// Flatten the crate at `dir`, write to disk, and return the file's contents.
fn flatten_to_file(dir: &Path, out: &Path) -> String {
    let pkg = parse_package(dir).expect("parse_package");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    pkg.source.to_file(out).expect("to_file");
    fs::read_to_string(out).unwrap()
}

/// Strip whitespace-only differences for tolerant comparison.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains_normalized(haystack: &str, needle: &str) -> bool {
    collapse_ws(haystack).contains(&collapse_ws(needle))
}

// ---------------------------------------------------------------------------
// Entry-point detection
// ---------------------------------------------------------------------------

#[test]
fn detects_lib_crate() {
    let dir = make_crate(&[("src/lib.rs", "pub fn x() {}\n")]);
    let (kind, out) = flatten_str(dir.path());
    assert_eq!(kind, PackageType::Lib);
    assert_eq!(out, "pub fn x() {}\n");
}

#[test]
fn detects_bin_crate() {
    let dir = make_crate(&[("src/main.rs", "fn main() {}\n")]);
    let (kind, out) = flatten_str(dir.path());
    assert_eq!(kind, PackageType::Bin);
    assert_eq!(out, "fn main() {}\n");
}

#[test]
fn prefers_main_when_both_exist() {
    // Documents current behavior: main.rs wins over lib.rs.
    // Eventually we may want to flatten both.
    let dir = make_crate(&[
        ("src/lib.rs", "pub fn lib_only() {}\n"),
        ("src/main.rs", "fn main() {}\n"),
    ]);
    let (kind, out) = flatten_str(dir.path());
    assert_eq!(kind, PackageType::Bin);
    assert!(out.contains("fn main()"));
    assert!(!out.contains("lib_only"));
}

#[test]
fn errors_when_no_entry_point() {
    let dir = make_crate(&[("src/something.rs", "")]);
    let err = parse_package(dir.path()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("main.rs") && msg.contains("lib.rs"),
        "error should mention both entry points, got: {msg}"
    );
}

#[test]
fn errors_when_path_does_not_exist() {
    let bogus = PathBuf::from("/this/path/does/not/exist/anywhere");
    let err = parse_package(&bogus).unwrap_err();
    assert!(format!("{err:#}").contains("Path must be valid"));
}

#[test]
fn errors_when_path_is_a_file_not_a_dir() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("some.rs");
    fs::write(&file, "").unwrap();
    let err = parse_package(&file).unwrap_err();
    assert!(format!("{err:#}").contains("must be a directory"));
}

// ---------------------------------------------------------------------------
// Basic mod inlining
// ---------------------------------------------------------------------------

#[test]
fn inlines_single_external_mod() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod foo;\npub use foo::x;\n"),
        ("src/foo.rs", "pub fn x() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod foo {"), "got:\n{out}");
    assert!(contains_normalized(&out, "pub fn x() {}"), "got:\n{out}");
    assert!(contains_normalized(&out, "pub use foo::x;"), "got:\n{out}");
    // The original mod-with-semicolon should be gone; the closing `}` of the
    // inlined block should appear before the `pub use`.
    let close_idx = out.find('}').expect("expected closing brace from inlined mod");
    let use_idx = out.find("pub use").unwrap();
    assert!(close_idx < use_idx, "got:\n{out}");
}

#[test]
fn does_not_inline_when_no_external_mods() {
    let dir = make_crate(&[("src/lib.rs", "// nothing to do here\npub fn a() {}\n")]);
    let (_, out) = flatten_str(dir.path());
    assert_eq!(out, "// nothing to do here\npub fn a() {}\n");
}

#[test]
fn inlines_multiple_top_level_mods() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
        ("src/a.rs", "pub const A: u32 = 1;\n"),
        ("src/b.rs", "pub const B: u32 = 2;\n"),
        ("src/c.rs", "pub const C: u32 = 3;\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod a {"));
    assert!(contains_normalized(&out, "pub const A: u32 = 1;"));
    assert!(contains_normalized(&out, "mod b {"));
    assert!(contains_normalized(&out, "pub const B: u32 = 2;"));
    assert!(contains_normalized(&out, "mod c {"));
    assert!(contains_normalized(&out, "pub const C: u32 = 3;"));
}

#[test]
fn errors_when_referenced_mod_file_missing() {
    let dir = make_crate(&[("src/lib.rs", "mod missing;\n")]);
    let err = parse_package(dir.path()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("missing") && msg.contains("not found"),
        "error should mention the missing mod by name, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// `include!()` and `include_str!()` resolution
// ---------------------------------------------------------------------------

#[test]
fn resolves_include_macro_at_item_position() {
    let dir = make_crate(&[
        ("src/lib.rs", "include!(\"items.rs\");\npub use my_const;\n"),
        ("src/items.rs", "pub const MY_CONST: i32 = 42;\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(
        out.contains("pub const MY_CONST: i32 = 42"),
        "expected included items inlined; got:\n{out}"
    );
    assert!(
        !out.contains("include!"),
        "expected no remaining include! call; got:\n{out}"
    );
}

#[test]
fn resolves_include_str_macro_at_expression_position() {
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "pub const README: &str = include_str!(\"../README.txt\");\n",
        ),
        ("README.txt", "Hello, world!\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(
        out.contains("\"Hello, world!\\n\""),
        "expected escaped string literal; got:\n{out}"
    );
    assert!(
        !out.contains("include_str!"),
        "expected no remaining include_str! call; got:\n{out}"
    );
}

#[test]
fn include_str_escapes_quotes_and_backslashes() {
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "pub const S: &str = include_str!(\"data.txt\");\n",
        ),
        ("src/data.txt", "He said \"hi\\there\"\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    // `"` → `\"`, `\` → `\\`, `\n` → `\n`
    assert!(
        out.contains("\"He said \\\"hi\\\\there\\\"\\n\""),
        "expected proper escaping; got:\n{out}"
    );
}

#[test]
fn include_recurses_relative_to_included_file() {
    // a/lib.rs `include!("inner/b.rs")` → b.rs `include!("c.rs")` resolves
    // to a/inner/c.rs (next to b.rs), not a/c.rs.
    let dir = make_crate(&[
        ("src/lib.rs", "include!(\"inner/b.rs\");\n"),
        (
            "src/inner/b.rs",
            "pub const FROM_B: i32 = 1;\ninclude!(\"c.rs\");\n",
        ),
        ("src/inner/c.rs", "pub const FROM_C: i32 = 2;\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(out.contains("pub const FROM_B: i32 = 1"));
    assert!(out.contains("pub const FROM_C: i32 = 2"));
    assert!(!out.contains("include!"));
}

#[test]
fn included_items_participate_in_mod_resolution() {
    // include!()'d file contains a `mod NAME;` declaration. After include
    // expansion, the mod scanner should see it and inline the submod.
    let dir = make_crate(&[
        ("src/lib.rs", "include!(\"items.rs\");\n"),
        ("src/items.rs", "pub mod sub;\n"),
        ("src/sub.rs", "pub fn marker_from_sub() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(
        out.contains("marker_from_sub"),
        "expected sub-mod inlined via include; got:\n{out}"
    );
}

#[test]
fn include_macro_missing_file_errors() {
    let dir = make_crate(&[(
        "src/lib.rs",
        "include!(\"does_not_exist.rs\");\n",
    )]);
    let pkg = parse_package(dir.path());
    let err = pkg.err().expect("expected an error for missing include");
    let msg = format!("{err}");
    assert!(
        msg.contains("does_not_exist") && msg.contains("include!"),
        "expected helpful missing-file error, got: {msg}"
    );
}

#[test]
fn include_str_macro_missing_file_errors() {
    let dir = make_crate(&[(
        "src/lib.rs",
        "pub const S: &str = include_str!(\"missing.txt\");\n",
    )]);
    let err = parse_package(dir.path()).err().expect("expected an error");
    assert!(
        format!("{err}").contains("include_str!"),
        "expected include_str! mention in error: {err}"
    );
}

#[test]
fn include_cycle_errors_with_depth_limit() {
    // REVIEW A5 regression: a -> b -> a should error gracefully via the
    // include-recursion bound, not blow the stack. The cycle here is
    // direct (a includes b, b includes a) — depth grows quickly.
    let dir = make_crate(&[
        ("src/lib.rs", "include!(\"a.rs\");\n"),
        ("src/a.rs", "include!(\"b.rs\");\n"),
        ("src/b.rs", "include!(\"a.rs\");\n"),
    ]);
    let err = parse_package(dir.path())
        .err()
        .expect("expected cycle error, not stack overflow");
    let msg = format!("{err}");
    assert!(
        msg.contains("nesting depth") || msg.contains("cycle"),
        "expected depth-limit / cycle mention in error: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Visibility variants
// ---------------------------------------------------------------------------

#[test]
fn inlines_pub_mod() {
    let dir = make_crate(&[
        ("src/lib.rs", "pub mod foo;\n"),
        ("src/foo.rs", "pub fn f() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "pub mod foo {"), "got:\n{out}");
    assert!(contains_normalized(&out, "pub fn f() {}"));
    // The `;` should be gone — only one `mod foo` declaration.
    assert!(!out.contains("mod foo;"), "stray `mod foo;` left in output");
}

#[test]
fn inlines_pub_crate_mod() {
    let dir = make_crate(&[
        ("src/lib.rs", "pub(crate) mod foo;\n"),
        ("src/foo.rs", "pub fn f() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(
        contains_normalized(&out, "pub(crate) mod foo {"),
        "pub(crate) was not inlined; got:\n{out}"
    );
    assert!(!out.contains("mod foo;"));
}

#[test]
fn inlines_pub_super_mod() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod outer;\n"),
        ("src/outer.rs", "pub(super) mod inner;\n"),
        ("src/outer/inner.rs", "pub fn f() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(
        contains_normalized(&out, "pub(super) mod inner {"),
        "pub(super) was not inlined; got:\n{out}"
    );
    assert!(contains_normalized(&out, "pub fn f() {}"));
}

#[test]
fn inlines_pub_in_path_mod() {
    let dir = make_crate(&[
        ("src/lib.rs", "pub(in crate::vis) mod foo;\npub mod vis { pub use super::foo::*; }\n"),
        ("src/foo.rs", "pub fn f() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(
        contains_normalized(&out, "pub(in crate::vis) mod foo {"),
        "pub(in path) was not inlined; got:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// Nested modules and on-disk layout
// ---------------------------------------------------------------------------

#[test]
fn inlines_nested_mods_via_foo_rs() {
    // 2018+ edition layout: `foo.rs` declares submods; their files live in `foo/`.
    let dir = make_crate(&[
        ("src/lib.rs", "mod foo;\n"),
        ("src/foo.rs", "pub mod bar;\n"),
        ("src/foo/bar.rs", "pub fn b() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod foo {"));
    assert!(contains_normalized(&out, "pub mod bar {"));
    assert!(contains_normalized(&out, "pub fn b() {}"));
}

#[test]
fn supports_old_style_mod_rs_directories() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod foo;\n"),
        ("src/foo/mod.rs", "pub mod bar;\npub fn outer() {}\n"),
        ("src/foo/bar.rs", "pub fn b() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod foo {"));
    assert!(contains_normalized(&out, "pub fn outer() {}"));
    assert!(contains_normalized(&out, "pub mod bar {"));
    assert!(contains_normalized(&out, "pub fn b() {}"));
}

#[test]
fn errors_when_both_foo_rs_and_foo_mod_rs_exist() {
    // Per the Reference, having both `foo.rs` and `foo/mod.rs` for the same
    // mod is an error. We surface it instead of silently picking one.
    let dir = make_crate(&[
        ("src/lib.rs", "mod foo;\n"),
        ("src/foo.rs", "pub fn from_foo_rs() {}\n"),
        ("src/foo/mod.rs", "pub fn from_mod_rs() {}\n"),
    ]);
    let err = parse_package(dir.path()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("ambiguous") && msg.contains("foo"),
        "expected ambiguity error, got: {msg}"
    );
}

#[test]
fn deeply_nested_modules() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod a;\n"),
        ("src/a.rs", "pub mod b;\n"),
        ("src/a/b.rs", "pub mod c;\n"),
        ("src/a/b/c.rs", "pub mod d;\n"),
        ("src/a/b/c/d.rs", "pub const X: u32 = 42;\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod a {"));
    assert!(contains_normalized(&out, "pub mod b {"));
    assert!(contains_normalized(&out, "pub mod c {"));
    assert!(contains_normalized(&out, "pub mod d {"));
    assert!(contains_normalized(&out, "pub const X: u32 = 42;"));
}

// ---------------------------------------------------------------------------
// Things that look like mods but aren't external file modules
// ---------------------------------------------------------------------------

#[test]
fn leaves_inline_mod_blocks_alone() {
    // `mod foo { ... }` (with body) is already inline — no file lookup needed.
    let src = "pub mod inline_one { pub fn a() {} }\n\
               pub mod inline_two {\n    pub fn b() {}\n}\n";
    let dir = make_crate(&[("src/lib.rs", src)]);
    let (_, out) = flatten_str(dir.path());
    assert_eq!(out, src, "inline mod blocks should pass through unchanged");
}

#[test]
fn line_comments_do_not_false_match() {
    // A line that begins with `//` then `mod foo;` should not be treated as a decl.
    let dir = make_crate(&[("src/lib.rs", "// mod ghost;\npub fn a() {}\n")]);
    let (_, out) = flatten_str(dir.path());
    assert_eq!(out, "// mod ghost;\npub fn a() {}\n");
}

#[test]
fn doctest_inside_doc_comment_does_not_false_match() {
    let src = "/// Example:\n\
               /// ```\n\
               /// pub mod foo;\n\
               /// ```\n\
               pub fn a() {}\n";
    let dir = make_crate(&[("src/lib.rs", src)]);
    let (_, out) = flatten_str(dir.path());
    assert_eq!(out, src);
}

// ---------------------------------------------------------------------------
// Output / file IO
// ---------------------------------------------------------------------------

#[test]
fn to_file_truncates_existing_longer_content() {
    // Regression for the missing .truncate(true) bug.
    let dir = make_crate(&[
        ("src/lib.rs", "mod foo;\n"),
        ("src/foo.rs", "pub fn x() {}\n"),
    ]);
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("flat.rs");

    // Pre-fill with 50 KB of garbage so any non-truncating write leaves a tail.
    fs::write(&out_path, "x".repeat(50_000)).unwrap();

    let written = flatten_to_file(dir.path(), &out_path);
    assert!(written.contains("pub fn x()"), "expected new content, got:\n{written}");
    assert!(!written.contains("xxxxx"), "truncate bug — old bytes leaked through");
    assert!(written.len() < 200, "output should be small, got {} bytes", written.len());
}

#[test]
fn to_file_creates_when_absent() {
    let dir = make_crate(&[("src/lib.rs", "pub fn a() {}\n")]);
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("nested/dir/flat.rs");
    let written = flatten_to_file(dir.path(), &out_path);
    assert_eq!(written, "pub fn a() {}\n");
}

#[test]
fn to_file_matches_to_string_byte_for_byte() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod a;\nmod b;\n"),
        ("src/a.rs", "pub fn a() {}\n"),
        ("src/b.rs", "pub mod c;\n"),
        ("src/b/c.rs", "pub fn c() {}\n"),
    ]);
    let (_, str_out) = flatten_str(dir.path());

    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("flat.rs");
    let file_out = flatten_to_file(dir.path(), &out_path);

    assert_eq!(str_out, file_out);
}

// ---------------------------------------------------------------------------
// End-to-end: the flat output should actually compile with rustc.
//
// These run rustc and skip if it's not on PATH (e.g. on a sandboxed CI).
// ---------------------------------------------------------------------------

fn rustc_available() -> bool {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn flat_output_compiles_for_simple_lib() {
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_crate(&[
        ("src/lib.rs", "pub mod inner;\npub use inner::secret;\n"),
        ("src/inner.rs", "pub const secret: u32 = 7;\n"),
    ]);
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("flat.rs");
    flatten_to_file(dir.path(), &out_path);

    let status = std::process::Command::new("rustc")
        .args(["--edition=2021", "--crate-type=lib", "--emit=metadata", "-o"])
        .arg(out_dir.path().join("flat.rmeta"))
        .arg(&out_path)
        .status()
        .unwrap();
    assert!(status.success(), "rustc rejected the flat output");
}

#[test]
fn flat_output_compiles_for_nested_visibility() {
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_crate(&[
        ("src/lib.rs", "pub mod outer;\n"),
        ("src/outer.rs", "pub(crate) mod helpers;\npub fn run() -> u32 { helpers::value() }\n"),
        ("src/outer/helpers.rs", "pub(super) fn value() -> u32 { 99 }\n"),
    ]);
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("flat.rs");
    flatten_to_file(dir.path(), &out_path);

    let status = std::process::Command::new("rustc")
        .args(["--edition=2021", "--crate-type=lib", "--emit=metadata", "-o"])
        .arg(out_dir.path().join("flat.rmeta"))
        .arg(&out_path)
        .status()
        .unwrap();
    assert!(status.success(), "rustc rejected the flat output");
}

// ---------------------------------------------------------------------------
// What used to be the "known limitations" section of the regex scanner —
// these now all pass thanks to the syn-based scanner.
// ---------------------------------------------------------------------------

#[test]
fn does_not_match_mod_inside_string_literal() {
    let dir = make_crate(&[(
        "src/lib.rs",
        "pub const SAMPLE: &str = \"\nmod ghost;\n\";\npub fn a() {}\n",
    )]);
    let (_, out) = flatten_str(dir.path());
    assert!(out.contains("mod ghost"), "string literal should pass through");
    assert!(out.contains("pub fn a()"));
    // No file lookup should have been triggered for the literal text.
    assert!(!out.contains("// ==="));
}

#[test]
fn does_not_match_mod_inside_block_comment() {
    let dir = make_crate(&[(
        "src/lib.rs",
        "/*\n  mod ghost;\n*/\npub fn a() {}\n",
    )]);
    let (_, out) = flatten_str(dir.path());
    assert!(out.contains("mod ghost"));
    assert!(out.contains("pub fn a()"));
    assert!(!out.contains("// ==="));
}

#[test]
fn respects_path_attribute_same_line() {
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "#[path = \"weird_name.rs\"] mod foo;\npub use foo::v;\n",
        ),
        ("src/weird_name.rs", "pub const v: u32 = 1;\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod foo {"));
    assert!(contains_normalized(&out, "pub const v: u32 = 1;"));
}

#[test]
fn respects_path_attribute_separate_lines() {
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "#[path = \"weird_name.rs\"]\nmod foo;\npub use foo::v;\n",
        ),
        ("src/weird_name.rs", "pub const v: u32 = 1;\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod foo {"));
    assert!(contains_normalized(&out, "pub const v: u32 = 1;"));
}

#[test]
fn cfg_skipped_mods_summary_lists_them_on_stderr() {
    // Two cfg-gated mods whose target files don't exist. The CLI
    // should emit a single consolidated "module(s) skipped" summary
    // on stderr listing both with their declaring file. Without this
    // diagnostic, downstream cargo-build's `cannot find type` errors
    // wouldn't connect back to the cfg-skip.
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("skip_summary_test")),
        (
            "src/lib.rs",
            "#[cfg(any())]\nmod alpha;\n#[cfg(any())]\nmod beta;\npub fn x() {}\n",
        ),
    ]);

    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(dir.path())
        .args(["--lib", "--stdout", "--no-banner"])
        .output()
        .expect("spawn flatten");
    assert!(output.status.success(), "flatten failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("2 module(s) skipped"),
        "expected aggregated skip-count summary, got:\n{stderr}"
    );
    assert!(
        stderr.contains("mod alpha") && stderr.contains("mod beta"),
        "expected both skipped mod names in summary, got:\n{stderr}"
    );
    assert!(
        stderr.contains("Workarounds"),
        "expected workaround hints, got:\n{stderr}"
    );
}

#[test]
fn cfg_gated_missing_mod_is_skipped_with_warning() {
    // A cfg-gated mod whose file does not exist gets its trailing `;`
    // replaced with `{ /* cfg-skipped */ }` so the flat output is
    // self-contained — leaving `mod never_exists;` would make
    // downstream cargo-build try to load `never_exists.rs` from disk
    // and fail because the flat output is single-file.
    let dir = make_crate(&[(
        "src/lib.rs",
        "#[cfg(any())]\nmod never_exists;\npub fn a() {}\n",
    )]);
    let (_, out) = flatten_str(dir.path());
    assert!(out.contains("pub fn a()"));
    // The cfg attribute is preserved (we can't evaluate it), the mod
    // declaration is preserved (the cfg might be true downstream),
    // but the `;` is replaced with an empty body so cargo-build never
    // tries to read the missing file.
    assert!(out.contains("#[cfg(any())]"), "got:\n{out}");
    assert!(out.contains("mod never_exists"), "got:\n{out}");
    assert!(!out.contains("mod never_exists;"), "expected `;` swapped for body, got:\n{out}");
    assert!(out.contains("cfg-skipped"), "got:\n{out}");
}

// ---------------------------------------------------------------------------
// New capabilities unlocked by the syn migration
// ---------------------------------------------------------------------------

#[test]
fn path_attr_resolves_relative_to_containing_dir_in_non_mod_rs_file() {
    // `bar.rs` is a non-mod-rs file. The default submod search dir is
    // `src/bar/`, but `#[path = "..."]` is relative to *bar.rs's containing
    // directory* — `src/`. This is the rule that surprises everyone.
    let dir = make_crate(&[
        ("src/lib.rs", "mod bar;\n"),
        ("src/bar.rs", "#[path = \"renamed.rs\"]\nmod inner;\n"),
        ("src/renamed.rs", "pub fn x() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod inner {"));
    assert!(contains_normalized(&out, "pub fn x() {}"));
}

#[test]
fn external_mod_nested_inside_inline_mod_block() {
    // `lib.rs` contains an inline `mod outer { ... }` whose body declares
    // `mod inner;`. Resolution should look in `src/outer/inner.rs`.
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "pub mod outer {\n    pub mod inner;\n}\n",
        ),
        ("src/outer/inner.rs", "pub fn x() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "pub mod outer {"));
    assert!(contains_normalized(&out, "pub mod inner {"));
    assert!(contains_normalized(&out, "pub fn x() {}"));
}

#[test]
fn external_mod_nested_inside_inline_mod_in_non_mod_rs_file() {
    // `bar.rs` (non-mod-rs) contains inline `mod outer { mod inner; }`.
    // Resolution: submod_search_dir(bar) = src/bar/, plus `outer/`, then
    // `inner.rs` — i.e. `src/bar/outer/inner.rs`.
    let dir = make_crate(&[
        ("src/lib.rs", "mod bar;\n"),
        ("src/bar.rs", "pub mod outer {\n    pub mod inner;\n}\n"),
        ("src/bar/outer/inner.rs", "pub fn x() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "pub mod inner {"));
    assert!(contains_normalized(&out, "pub fn x() {}"));
}

#[test]
fn errors_on_unparseable_source() {
    let dir = make_crate(&[("src/lib.rs", "this is { not valid rust\n")]);
    let err = parse_package(dir.path()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("Parse error") || msg.contains("parse"),
        "expected parse error, got: {msg}"
    );
}

#[test]
fn extern_crate_is_left_alone() {
    // syn parses `extern crate alloc;` as Item::ExternCrate, not Item::Mod —
    // so the scanner ignores it and the line passes through verbatim.
    let dir = make_crate(&[(
        "src/lib.rs",
        "extern crate alloc;\npub fn a() {}\n",
    )]);
    let (_, out) = flatten_str(dir.path());
    assert!(out.contains("extern crate alloc;"));
    assert!(out.contains("pub fn a()"));
}

#[test]
fn cfg_attr_on_mod_with_existing_file_is_preserved() {
    // cfg attributes are preserved verbatim on the inlined block — they
    // gate the inlined contents at compile time of the flat output.
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "#[cfg(target_os = \"linux\")]\nmod linux;\npub fn a() {}\n",
        ),
        ("src/linux.rs", "pub fn l() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "#[cfg(target_os = \"linux\")]"));
    assert!(contains_normalized(&out, "mod linux {"));
    assert!(contains_normalized(&out, "pub fn l() {}"));
}

// ---------------------------------------------------------------------------
// Manifest awareness — Cargo.toml-driven target selection
// ---------------------------------------------------------------------------

fn minimal_manifest(name: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"
    )
}

#[test]
fn uses_crate_name_from_cargo_toml() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("my_neat_crate")),
        ("src/lib.rs", "pub fn x() {}\n"),
    ]);
    let pkg = parse_package(dir.path()).expect("parse_package");
    assert_eq!(pkg.crate_name, "my_neat_crate");
    assert_eq!(pkg.target_name, "my_neat_crate");
    assert_eq!(pkg.kind, PackageType::Lib);
}

#[test]
fn auto_picks_lib_when_only_lib_exists() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("liby")),
        ("src/lib.rs", "pub fn x() {}\n"),
    ]);
    let pkg = parse_package(dir.path()).unwrap();
    assert_eq!(pkg.kind, PackageType::Lib);
}

#[test]
fn auto_picks_the_only_bin_with_manifest() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("biny")),
        ("src/main.rs", "fn main() {}\n"),
    ]);
    let pkg = parse_package(dir.path()).unwrap();
    assert_eq!(pkg.kind, PackageType::Bin);
    assert_eq!(pkg.target_name, "biny");
}

#[test]
fn auto_errors_with_multiple_bins_no_lib() {
    // Two bins discovered via the src/bin/ convention. Auto-selection fails
    // because we don't know which one to pick.
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("multi")),
        ("src/bin/alpha.rs", "fn main() {}\n"),
        ("src/bin/beta.rs", "fn main() {}\n"),
    ]);
    let err = parse_package(dir.path()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("--bin") && (msg.contains("alpha") || msg.contains("beta")),
        "expected hint to use --bin, got: {msg}"
    );
}

#[test]
fn select_named_bin() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("multi")),
        ("src/bin/alpha.rs", "fn main() { println!(\"a\"); }\n"),
        ("src/bin/beta.rs", "fn main() { println!(\"b\"); }\n"),
    ]);
    let pkg = parse_target(dir.path(), &TargetSelector::Bin("beta".into())).unwrap();
    assert_eq!(pkg.kind, PackageType::Bin);
    assert_eq!(pkg.target_name, "beta");
    let out = pkg.source.to_string();
    assert!(out.contains("println!(\"b\")"));
    assert!(!out.contains("println!(\"a\")"));
}

#[test]
fn select_lib_when_both_lib_and_bin_exist() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("hybrid")),
        ("src/lib.rs", "pub fn from_lib() {}\n"),
        ("src/main.rs", "fn main() {}\n"),
    ]);
    let pkg = parse_target(dir.path(), &TargetSelector::Lib).unwrap();
    assert_eq!(pkg.kind, PackageType::Lib);
    assert!(pkg.source.to_string().contains("from_lib"));
}

#[test]
fn select_named_example() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("ex_pkg")),
        ("src/lib.rs", "pub fn x() {}\n"),
        ("examples/demo.rs", "fn main() { println!(\"demo\"); }\n"),
    ]);
    let pkg = parse_target(dir.path(), &TargetSelector::Example("demo".into())).unwrap();
    assert_eq!(pkg.kind, PackageType::Example);
    assert_eq!(pkg.target_name, "demo");
    assert!(pkg.source.to_string().contains("\"demo\""));
}

#[test]
fn select_named_test() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("test_pkg")),
        ("src/lib.rs", "pub fn x() {}\n"),
        ("tests/it.rs", "#[test]\nfn t() {}\n"),
    ]);
    let pkg = parse_target(dir.path(), &TargetSelector::Test("it".into())).unwrap();
    assert_eq!(pkg.kind, PackageType::Test);
    assert_eq!(pkg.target_name, "it");
}

#[test]
fn errors_when_named_bin_does_not_exist() {
    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("p")),
        ("src/bin/alpha.rs", "fn main() {}\n"),
    ]);
    let err = parse_target(dir.path(), &TargetSelector::Bin("missing".into())).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("missing") && msg.contains("alpha"),
        "got: {msg}"
    );
}

#[test]
fn honors_explicit_lib_path_in_manifest() {
    let manifest = format!(
        "{}\n[lib]\npath = \"weird/place.rs\"\n",
        minimal_manifest("custom_lib")
    );
    let dir = make_crate(&[
        ("Cargo.toml", &manifest),
        ("weird/place.rs", "pub fn x() {}\nmod inner;\n"),
        ("weird/place/inner.rs", "pub fn y() {}\n"),
    ]);
    let pkg = parse_package(dir.path()).unwrap();
    assert_eq!(pkg.kind, PackageType::Lib);
    let out = pkg.source.to_string();
    assert!(contains_normalized(&out, "pub fn x() {}"));
    assert!(contains_normalized(&out, "pub fn y() {}"));
}

#[test]
fn auto_uses_default_run_when_multiple_bins() {
    // [package].default-run names which bin to pick when there are several.
    let manifest = "\
[package]
name = \"multi\"
version = \"0.0.0\"
edition = \"2021\"
default-run = \"beta\"
";
    let dir = make_crate(&[
        ("Cargo.toml", manifest),
        ("src/bin/alpha.rs", "fn main() {}\n"),
        ("src/bin/beta.rs", "fn main() { println!(\"b\"); }\n"),
    ]);
    let pkg = parse_package(dir.path()).expect("default-run should resolve ambiguity");
    assert_eq!(pkg.kind, PackageType::Bin);
    assert_eq!(pkg.target_name, "beta");
    assert!(pkg.source.to_string().contains("println!(\"b\")"));
}

#[test]
fn path_attr_inside_inline_mod_resolves() {
    // The classic platform-impl pattern: an inline mod whose body declares
    // a `#[path = "..."]` external mod. Per the Reference, the path resolves
    // relative to the file's submod search dir + the inline mod components.
    let dir = make_crate(&[
        (
            "src/lib.rs",
            "pub mod platform {\n    #[path = \"unix.rs\"]\n    mod imp;\n}\n",
        ),
        ("src/platform/unix.rs", "pub fn impl_fn() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod imp {"));
    assert!(contains_normalized(&out, "pub fn impl_fn() {}"));
}

#[test]
fn path_attr_inside_inline_mod_in_non_mod_rs_file() {
    // Same pattern, but the inline mod is inside `bar.rs` (non-mod-rs).
    // Submod search dir for bar.rs = src/bar/, plus `platform/` from the
    // inline mod ⇒ `src/bar/platform/unix.rs`.
    let dir = make_crate(&[
        ("src/lib.rs", "mod bar;\n"),
        (
            "src/bar.rs",
            "pub mod platform {\n    #[path = \"unix.rs\"]\n    mod imp;\n}\n",
        ),
        ("src/bar/platform/unix.rs", "pub fn impl_fn() {}\n"),
    ]);
    let (_, out) = flatten_str(dir.path());
    assert!(contains_normalized(&out, "mod imp {"));
    assert!(contains_normalized(&out, "pub fn impl_fn() {}"));
}

#[test]
fn parse_package_accepts_str_via_asref() {
    // Compile-time check: &str implements AsRef<Path>, so callers shouldn't
    // need to wrap in PathBuf or Path::new explicitly.
    let dir = make_crate(&[("src/lib.rs", "pub fn x() {}\n")]);
    let path_str: &str = dir.path().to_str().unwrap();
    let pkg = parse_package(path_str).unwrap();
    assert_eq!(pkg.kind, PackageType::Lib);
}

#[test]
fn explicit_selector_without_manifest_errors() {
    // Selectors that need a manifest should fail loudly when there isn't one.
    let dir = make_crate(&[("src/bin/foo.rs", "fn main() {}\n")]);
    let err = parse_target(dir.path(), &TargetSelector::Bin("foo".into())).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("Cargo.toml"), "got: {msg}");
}

// ---------------------------------------------------------------------------
// Smoke tests against real crates cloned into `test-crates/` (gitignored).
//
// Each test silently skips if its crate is not present, so a fresh checkout
// without the fixtures still passes.
// ---------------------------------------------------------------------------

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn maybe_real_crate(name: &str) -> Option<PathBuf> {
    let p = project_dir().join("test-crates").join(name);
    let has_lib = p.join("src/lib.rs").is_file();
    let has_main = p.join("src/main.rs").is_file();
    if p.is_dir() && (has_lib || has_main) {
        Some(p)
    } else {
        None
    }
}

fn run_real_crate_smoke(name: &str) {
    let Some(path) = maybe_real_crate(name) else {
        eprintln!("skipping `{name}`: test-crates/{name} not present");
        return;
    };

    let (_, out) = flatten_str(&path);
    assert!(
        out.len() > 500,
        "expected substantial output for `{name}`, got {} bytes",
        out.len()
    );

    if !rustc_available() {
        eprintln!("skipping rustc check for `{name}`: rustc not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("flat.rs");
    fs::write(&flat, &out).unwrap();

    let output = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-type=lib",
            "--emit=metadata",
            "-A",
            "warnings",
            "-o",
        ])
        .arg(tmp.path().join("flat.rmeta"))
        .arg(&flat)
        .output()
        .unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("rustc rejected flattened `{name}`:\n{stderr}");
    }
}

#[test]
fn real_crate_itoa() {
    run_real_crate_smoke("itoa");
}

#[test]
fn real_crate_anyhow() {
    run_real_crate_smoke("anyhow");
}

#[test]
fn real_crate_bitflags() {
    run_real_crate_smoke("bitflags");
}

// ---------------------------------------------------------------------------
// Phase V0: --vendor-report classifier
// ---------------------------------------------------------------------------

#[test]
fn vendor_report_against_script_with_deps_lists_itoa_as_vendorable() {
    let fixture = project_dir().join("test-crates/script-with-deps");
    if !fixture.join("Cargo.toml").is_file() {
        eprintln!("skipping: test-crates/script-with-deps not present");
        return;
    }
    let report = vendor::report(&fixture).expect("vendor report");
    assert_eq!(report.root_name, "script-with-deps");

    let itoa = report
        .deps
        .iter()
        .find(|d| d.name == "itoa")
        .expect("itoa should appear as a dep");
    assert!(
        itoa.classification.is_vendorable(),
        "itoa should be Vendorable, got {:?}",
        itoa.classification
    );
}

#[test]
fn vendor_report_self_classifies_proc_macros_as_unvendorable() {
    // Run against flatten itself. Among our transitive deps are
    // clap_derive (proc-macro) and serde (build script) — verify both
    // are correctly refused.
    let report = vendor::report(project_dir()).expect("vendor report");

    let clap_derive = report
        .deps
        .iter()
        .find(|d| d.name == "clap_derive")
        .expect("clap_derive should be a transitive dep");
    match &clap_derive.classification {
        Classification::Unvendorable(reasons) => {
            assert!(
                reasons.iter().any(|r| r.contains("proc-macro")),
                "expected proc-macro reason, got {reasons:?}"
            );
        }
        other => panic!("expected Unvendorable, got {other:?}"),
    }
}

#[test]
fn vendor_report_excludes_dev_dependencies() {
    // tempfile and insta are dev-deps of flatten; they should not
    // appear in the normal-deps walk that vendoring uses.
    let report = vendor::report(project_dir()).expect("vendor report");
    let dep_names: Vec<&str> = report.deps.iter().map(|d| d.name.as_str()).collect();
    assert!(
        !dep_names.contains(&"tempfile"),
        "tempfile is a dev-dep and should be excluded"
    );
    assert!(
        !dep_names.contains(&"insta"),
        "insta is a dev-dep and should be excluded"
    );
}

#[test]
fn vendor_report_summary_renders() {
    let report = vendor::report(project_dir().join("test-crates/script-with-deps"))
        .expect("vendor report");
    let rendered = format!("{report}");
    assert!(rendered.contains("script-with-deps"));
    assert!(rendered.contains("Vendorable"));
    assert!(rendered.contains("Unvendorable"));
    assert!(rendered.contains("Summary:"));
}

#[test]
fn vendor_report_errors_without_cargo_toml() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    let err = vendor::report(dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Cargo.toml"),
        "expected Cargo.toml error, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Phase V1: actual vendoring with rewrites
// ---------------------------------------------------------------------------

/// Build a synthetic two-crate fixture: a `user` crate that depends on a
/// pure-Rust path-dep `<dep_name>`, with the dep's lib.rs given as `dep_src`.
fn make_user_with_path_dep(dep_name: &str, dep_src: &str, user_main: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join(dep_name).join("src")).unwrap();
    fs::write(
        dir.path().join(dep_name).join("Cargo.toml"),
        format!(
            "[package]\nname = \"{dep_name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n"
        ),
    )
    .unwrap();
    fs::write(dir.path().join(dep_name).join("src/lib.rs"), dep_src).unwrap();

    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        format!(
            "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
             [dependencies]\n{dep_name} = {{ path = \"../{dep_name}\" }}\n"
        ),
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), user_main).unwrap();

    dir
}

/// Regression: a vendored dep with a cfg-False `mod tests { ... }` block
/// containing `extern crate std;` would panic in `rewrite_for_vendoring`
/// because the extern-crate strip pass collected the inner item even
/// though the surrounding mod was already in the deletion list. The two
/// overlapping deletions, applied in reverse, would shrink the string
/// out from under the wider one.
#[test]
fn vendor_handles_cfg_false_mod_with_inner_extern_crate() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n\
         #[cfg(all(feature = \"nightly\", test))]\n\
         mod tests {\n    \
             extern crate std;\n    \
             pub fn t() {}\n\
         }\n",
        "fn main() { pure_dep::x(); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed without panic");
    assert_eq!(pkg.vendored.len(), 1);
    let s = pkg.vendored[0].source.to_string();
    // The cfg-False mod tests must have been dropped entirely.
    assert!(!s.contains("mod tests"), "cfg-False mod should be deleted; got:\n{s}");
    assert!(!s.contains("extern crate std"), "inner extern crate should be deleted with the mod; got:\n{s}");
    assert!(s.contains("pub fn x"), "the kept code should remain; got:\n{s}");
}

#[test]
fn vendor_inlines_a_pure_path_dep() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn double(x: i32) -> i32 { x * 2 }\n",
        "fn main() { println!(\"{}\", pure_dep::double(21)); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    assert_eq!(pkg.vendored.len(), 1);
    assert_eq!(pkg.vendored[0].name, "pure_dep");
    let vendored_src = pkg.vendored[0].source.to_string();
    assert!(vendored_src.contains("pub fn double"));
    assert!(pkg.external.is_empty());
}

// V1 used to refuse deps with #[cfg(feature = …)]; V2 evaluates them.
// The "feature disabled / enabled" cases are covered by the V2 tests below.

// V1 used to refuse deps with #[macro_export] or $crate; V3 supports both.
// The "refuses on bad $crate position" case is covered below.

#[test]
fn vendor_refuses_on_user_mod_collision() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n",
        // user has `mod pure_dep` of its own; same name as the dep
        "mod pure_dep { pub fn shadow() {} }\nfn main() {}\n",
    );

    let err = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("pure_dep") && msg.contains("collision"),
        "expected collision refusal, got: {msg}"
    );
}

#[test]
fn vendor_rewrites_crate_paths_inside_dep() {
    // Dep has `crate::Foo` references. After vendoring those should become
    // `crate::pure_dep::Foo` in the rewritten source.
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub struct Foo;\npub fn make() -> crate::Foo { crate::Foo }\n",
        "fn main() { let _ = pure_dep::make(); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        s.contains("crate::pure_dep::Foo"),
        "expected `crate::Foo` to be rewritten; got:\n{s}"
    );
    assert!(
        !s.contains("crate::Foo"),
        "no unrewritten crate::Foo should remain; got:\n{s}"
    );
}

#[test]
fn vendor_does_not_rewrite_pub_crate_visibility() {
    // `pub(crate)` is a shorthand keyword, not a path expression. It must
    // be left intact even though it textually contains "crate".
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub(crate) fn internal() {}\npub fn external() {}\n",
        "fn main() { pure_dep::external(); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        s.contains("pub(crate) fn internal"),
        "pub(crate) should be untouched; got:\n{s}"
    );
}

#[test]
fn vendor_strips_extern_crate_decls() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "extern crate alloc;\npub fn x() {}\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        !s.contains("extern crate"),
        "extern crate should be stripped; got:\n{s}"
    );
    assert!(s.contains("pub fn x"));
}

#[test]
fn vendor_output_compiles_via_rustc() {
    if !rustc_available() {
        eprintln!("skipping: rustc not on PATH");
        return;
    }
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn one() -> i32 { add(1, 0) }\n",
        "fn main() { println!(\"{}\", pure_dep::add(2, 40)); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    // Render full output: user source + vendored mod
    let mut out = String::new();
    use std::fmt::Write as _;
    write!(&mut out, "{}", pkg.user_source).unwrap();
    writeln!(&mut out).unwrap();
    writeln!(&mut out, "mod {} {{", pkg.vendored[0].name).unwrap();
    write!(&mut out, "{}", pkg.vendored[0].source).unwrap();
    writeln!(&mut out, "\n}}").unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("flat.rs");
    fs::write(&flat, &out).unwrap();

    let output = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-type=bin",
            "--emit=metadata",
            "-A",
            "warnings",
            "-o",
        ])
        .arg(tmp.path().join("flat.rmeta"))
        .arg(&flat)
        .output()
        .unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("rustc rejected vendored output:\n{stderr}\n--- output ---\n{out}");
    }
}

// ---------------------------------------------------------------------------
// Phase V2: cfg-feature evaluation, multi-dep, edition tracking
// ---------------------------------------------------------------------------

/// Variant of make_user_with_path_dep where the dep declares a feature and
/// the user enables it.
fn make_user_with_featured_dep(
    dep_name: &str,
    dep_features: &[&str],
    enabled_features: &[&str],
    dep_src: &str,
    user_main: &str,
) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join(dep_name).join("src")).unwrap();

    let features_section = if dep_features.is_empty() {
        String::new()
    } else {
        let mut s = String::from("\n[features]\n");
        for f in dep_features {
            s.push_str(&format!("{f} = []\n"));
        }
        s
    };
    fs::write(
        dir.path().join(dep_name).join("Cargo.toml"),
        format!(
            "[package]\nname = \"{dep_name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n{features_section}"
        ),
    )
    .unwrap();
    fs::write(dir.path().join(dep_name).join("src/lib.rs"), dep_src).unwrap();

    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    let features_str = if enabled_features.is_empty() {
        String::new()
    } else {
        format!(
            ", features = [{}]",
            enabled_features
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        format!(
            "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
             [dependencies]\n{dep_name} = {{ path = \"../{dep_name}\"{features_str} }}\n"
        ),
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), user_main).unwrap();

    dir
}

#[test]
fn vendor_evaluates_feature_cfg_when_feature_enabled() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &["x"],
        "#[cfg(feature = \"x\")]\npub fn x_only() -> i32 { 1 }\n\
         #[cfg(not(feature = \"x\"))]\npub fn no_x() -> i32 { 2 }\n\
         pub fn always() -> i32 { 3 }\n",
        "fn main() { fdep::always(); fdep::x_only(); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed (V2 evaluates feature cfgs)");

    let s = pkg.vendored[0].source.to_string();
    assert!(s.contains("pub fn x_only"), "x_only kept: {s}");
    assert!(s.contains("pub fn always"), "always kept: {s}");
    // The `not(feature = "x")` cfg evaluates to False; the gated item is
    // deleted from the vendored output entirely.
    assert!(!s.contains("pub fn no_x"), "no_x should be deleted; got:\n{s}");
    assert!(!s.contains("cfg(any())"), "no force-off marker should remain; got:\n{s}");
}

#[test]
fn vendor_evaluates_feature_cfg_when_feature_disabled() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[], // x not enabled
        "#[cfg(feature = \"x\")]\npub fn x_only() -> i32 { 1 }\n\
         pub fn always() -> i32 { 3 }\n",
        "fn main() { fdep::always(); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    let s = pkg.vendored[0].source.to_string();
    // cfg(feature = "x") evaluates to false → x_only is deleted entirely
    assert!(!s.contains("pub fn x_only"), "x_only should be deleted; got:\n{s}");
    assert!(s.contains("pub fn always"), "always should remain; got:\n{s}");
}

#[test]
fn vendor_leaves_unknown_cfgs_alone() {
    // target_os is host-dependent, not feature; should be left intact.
    let dir = make_user_with_featured_dep(
        "fdep",
        &[],
        &[],
        "#[cfg(target_os = \"linux\")]\npub fn linux_only() {}\n\
         pub fn always() {}\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        s.contains("cfg(target_os = \"linux\")"),
        "non-feature cfg should be preserved unchanged; got:\n{s}"
    );
}

#[test]
fn vendor_evaluates_complex_cfg_expression() {
    // any(feature = "x", feature = "y") with x enabled → True
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x", "y"],
        &["x"],
        "#[cfg(any(feature = \"x\", feature = \"y\"))]\npub fn either() {}\n\
         #[cfg(all(feature = \"x\", feature = \"y\"))]\npub fn both() {}\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed");

    let s = pkg.vendored[0].source.to_string();
    // any(x, y) → True (x enabled)
    assert!(s.contains("pub fn either()"));
    // all(x, y) → False (y not enabled) → `both` deleted entirely
    assert!(!s.contains("pub fn both"), "`both` should be deleted; got:\n{s}");
}

#[test]
fn vendor_handles_multiple_deps() {
    // Synthesize TWO path-deps + a user that uses both. They should both
    // be vendored as sibling mods.
    let dir = tempfile::tempdir().unwrap();

    for (name, body) in [
        ("dep_a", "pub fn from_a() -> i32 { 1 }\n"),
        ("dep_b", "pub fn from_b() -> i32 { 2 }\n"),
    ] {
        fs::create_dir_all(dir.path().join(name).join("src")).unwrap();
        fs::write(
            dir.path().join(name).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        fs::write(dir.path().join(name).join("src/lib.rs"), body).unwrap();
    }

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep_a = { path = \"../dep_a\" }\ndep_b = { path = \"../dep_b\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { println!(\"{}\", dep_a::from_a() + dep_b::from_b()); }\n",
    )
    .unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor should succeed for two deps");

    assert_eq!(pkg.vendored.len(), 2);
    let names: Vec<&str> = pkg.vendored.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"dep_a"));
    assert!(names.contains(&"dep_b"));
}

#[test]
fn vendor_real_itoa_through_cargo_cache() {
    // End-to-end against a real crates.io dep we know is on disk:
    // test-crates/script-with-deps depends on `itoa`. V1 refused itoa
    // because of feature cfgs; V2 evaluates them and should succeed.
    let fixture = project_dir().join("test-crates/script-with-deps");
    if !fixture.join("Cargo.toml").is_file() {
        eprintln!("skipping: test-crates/script-with-deps not present");
        return;
    }

    let pkg = vendor::vendor_package(
        &fixture,
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("V2 should vendor itoa");
    assert_eq!(pkg.vendored.len(), 1);
    assert_eq!(pkg.vendored[0].name, "itoa");
    assert_eq!(pkg.crate_name, "script-with-deps");

    if !rustc_available() {
        eprintln!("skipping rustc check: rustc not on PATH");
        return;
    }

    // Assemble and compile.
    let mut out = String::new();
    use std::fmt::Write as _;
    write!(&mut out, "{}", pkg.user_source).unwrap();
    for d in &pkg.vendored {
        writeln!(&mut out).unwrap();
        writeln!(&mut out, "mod {} {{", d.name).unwrap();
        write!(&mut out, "{}", d.source).unwrap();
        writeln!(&mut out, "\n}}").unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("flat.rs");
    fs::write(&flat, &out).unwrap();
    let output = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-type=bin",
            "--emit=metadata",
            "-A",
            "warnings",
            "-o",
        ])
        .arg(tmp.path().join("flat.rmeta"))
        .arg(&flat)
        .output()
        .unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("rustc rejected vendored itoa output:\n{stderr}");
    }
}

// ----- Deletion-vs-force-off tests --------------------------------------

#[test]
fn vendor_deletes_top_level_struct_when_cfg_false() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "#[cfg(feature = \"x\")]\npub struct Gone { pub a: i32, pub b: i32 }\n\
         pub struct Stays;\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(!s.contains("Gone"), "deleted struct should not appear: {s}");
    assert!(!s.contains("pub a: i32"), "struct fields should be gone too: {s}");
    assert!(s.contains("pub struct Stays"), "kept struct should remain: {s}");
}

#[test]
fn vendor_deletes_whole_inline_mod_when_cfg_false() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "#[cfg(feature = \"x\")]\npub mod gated {\n    pub fn one() {}\n    pub fn two() {}\n}\n\
         pub fn always() {}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(!s.contains("pub mod gated"), "gated mod should be deleted: {s}");
    assert!(!s.contains("pub fn one"), "items inside should be deleted: {s}");
    assert!(!s.contains("pub fn two"), "items inside should be deleted: {s}");
    assert!(s.contains("pub fn always"), "sibling kept: {s}");
}

#[test]
fn vendor_deletes_fn_inside_inline_mod_but_keeps_mod() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "pub mod outer {\n    #[cfg(feature = \"x\")]\n    pub fn gated() {}\n    pub fn kept() {}\n}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(s.contains("pub mod outer"), "outer mod kept: {s}");
    assert!(s.contains("pub fn kept"), "kept fn remains: {s}");
    assert!(!s.contains("pub fn gated"), "gated fn deleted: {s}");
}

#[test]
fn vendor_deletes_method_inside_impl_block() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "pub struct S;\n\
         impl S {\n\
             pub fn always(&self) {}\n\
             #[cfg(feature = \"x\")]\n\
             pub fn gated(&self) {}\n\
         }\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(s.contains("pub fn always"), "non-gated method kept: {s}");
    assert!(!s.contains("pub fn gated"), "gated method deleted: {s}");
}

#[test]
fn vendor_deletes_default_method_inside_trait_block() {
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "pub trait T {\n\
             fn always(&self);\n\
             #[cfg(feature = \"x\")]\n\
             fn gated(&self) {}\n\
         }\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(s.contains("fn always"), "abstract method kept: {s}");
    assert!(!s.contains("fn gated"), "default method deleted: {s}");
}

#[test]
fn vendor_does_not_recurse_into_deleted_mod() {
    // The outer mod `gated` is deleted; nothing inside should appear, even
    // though `inner_kept` would have evaluated True on its own. The deletion
    // wins.
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "#[cfg(feature = \"x\")]\npub mod gated {\n    pub fn inner_kept() {}\n}\n\
         pub fn always() {}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(!s.contains("pub mod gated"));
    assert!(!s.contains("inner_kept"));
}

#[test]
fn vendor_multiple_cfg_attrs_any_false_deletes_item() {
    // #[cfg(feature = "x")] #[cfg(target_os = "linux")] — x=False (regardless
    // of unix), so the item is deleted. (Multiple cfg attrs are AND'd.)
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "#[cfg(feature = \"x\")]\n#[cfg(target_os = \"linux\")]\npub fn doomed() {}\n\
         pub fn fine() {}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(!s.contains("pub fn doomed"), "should be deleted: {s}");
    assert!(s.contains("pub fn fine"));
}

#[test]
fn vendor_multiple_cfg_attrs_true_plus_unknown_strips_only_true() {
    // x=True (strip), target_os=Unknown (keep). Item survives with the
    // target_os attr still gating it for rustc.
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &["x"],
        "#[cfg(feature = \"x\")]\n#[cfg(target_os = \"linux\")]\npub fn linux_only() {}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    assert!(s.contains("pub fn linux_only"));
    assert!(
        s.contains("cfg(target_os = \"linux\")"),
        "Unknown cfg should remain: {s}"
    );
    assert!(
        !s.contains("cfg(feature = \"x\")"),
        "True cfg should be stripped: {s}"
    );
}

#[test]
fn vendor_force_off_still_used_for_field_level_cfg() {
    // Field-level `#[cfg]` deletion would need comma handling we haven't
    // built. For now, we still emit cfg(any()) for non-item False cases.
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "pub struct S {\n    pub always: i32,\n    #[cfg(feature = \"x\")]\n    pub maybe: i32,\n}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let s = pkg.vendored[0].source.to_string();
    // The struct itself stays.
    assert!(s.contains("pub struct S"));
    // The field's cfg gets force-off'd to cfg(any()).
    assert!(
        s.contains("cfg(any())"),
        "field-level False cfg should be force-off: {s}"
    );
}

#[test]
fn vendor_deletion_output_compiles() {
    if !rustc_available() {
        eprintln!("skipping: rustc not on PATH");
        return;
    }
    // Comprehensive: deletion of a fn, an inline mod, and a method; user
    // code only references survivors. Output must compile.
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "#[cfg(feature = \"x\")]\npub fn deleted_fn() -> i32 { 1 }\n\
         #[cfg(feature = \"x\")]\npub mod deleted_mod { pub fn _g() {} }\n\
         pub struct S;\n\
         impl S {\n    pub fn alive(&self) -> i32 { 7 }\n    #[cfg(feature = \"x\")]\n    pub fn deleted_method(&self) {}\n}\n\
         pub fn alive() -> i32 { 42 }\n",
        "fn main() { let s = fdep::S; println!(\"{}\", fdep::alive() + s.alive()); }\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();

    let mut out = String::new();
    use std::fmt::Write as _;
    write!(&mut out, "{}", pkg.user_source).unwrap();
    for d in &pkg.vendored {
        writeln!(&mut out).unwrap();
        writeln!(&mut out, "mod {} {{", d.name).unwrap();
        write!(&mut out, "{}", d.source).unwrap();
        writeln!(&mut out, "\n}}").unwrap();
    }

    // Sanity check: deleted symbols must not appear textually in the output.
    assert!(!out.contains("deleted_fn"), "deleted_fn leaked: {out}");
    assert!(!out.contains("deleted_mod"), "deleted_mod leaked: {out}");
    assert!(!out.contains("deleted_method"), "deleted_method leaked: {out}");
    assert!(out.contains("pub fn alive"));

    // Compile check.
    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("flat.rs");
    fs::write(&flat, &out).unwrap();
    let res = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-type=bin",
            "--emit=metadata",
            "-A",
            "warnings",
            "-o",
        ])
        .arg(tmp.path().join("flat.rmeta"))
        .arg(&flat)
        .output()
        .unwrap();
    if !res.status.success() {
        let stderr = String::from_utf8_lossy(&res.stderr);
        panic!("rustc rejected vendored output with deletions:\n{stderr}\n--- output ---\n{out}");
    }
}

#[test]
fn vendor_output_size_smaller_with_deletion() {
    // Sanity: deleting items should produce strictly less source than the
    // pre-deletion approach would have. We can verify this by comparing
    // vendored size against the size of the original dep source.
    let dep_src = "#[cfg(feature = \"x\")]\npub fn really_really_long_function_name_that_takes_up_lots_of_bytes() -> i32 { 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 }\n\
                   pub fn small() {}\n";
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        dep_src,
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();
    let vendored_size = pkg.vendored[0].source.to_string().len();
    let original_size = dep_src.len();
    assert!(
        vendored_size < original_size,
        "vendored ({vendored_size}) should be smaller than original ({original_size}) after deletion"
    );
    assert!(
        vendored_size < 60,
        "should retain only the small fn (~30 chars) plus a little; got {vendored_size}"
    );
}

#[test]
fn vendor_tracks_max_edition() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor");
    // Both crates declare edition 2021 above
    assert_eq!(pkg.max_edition, cargo_metadata::Edition::E2021);
}

// ---------------------------------------------------------------------------
// Phase V3: $crate rewriting + #[macro_export] handling
// ---------------------------------------------------------------------------

#[test]
fn vendor_rewrites_dollar_crate_to_dep_path() {
    let dir = make_user_with_path_dep(
        "macro_dep",
        "pub struct Helper;\n\
         impl Helper { pub fn new() -> Self { Self } }\n\
         macro_rules! mk { () => { $crate::Helper::new() } }\n\
         pub(crate) use mk;\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("V3 should rewrite $crate");

    let s = pkg.vendored[0].source.to_string();
    // $crate followed by ::Helper should become $crate::macro_dep::Helper
    assert!(
        s.contains("$crate :: macro_dep") || s.contains("$crate::macro_dep"),
        "expected `$crate::macro_dep` in rewritten body; got:\n{s}"
    );
}

#[test]
fn vendor_strips_macro_export_and_inserts_pub_use() {
    let dir = make_user_with_path_dep(
        "exp_dep",
        "#[macro_export]\nmacro_rules! shout { ($x:expr) => { format!(\"!! {} !!\", $x) } }\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("V3 should handle #[macro_export]");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        !s.contains("#[macro_export]"),
        "macro_export should be stripped; got:\n{s}"
    );
    assert!(
        s.contains("pub(crate) use shout;"),
        "pub(crate) use re-export should be inserted; got:\n{s}"
    );
}

#[test]
fn vendor_handles_macro_with_both_macro_export_and_dollar_crate() {
    // The classic anyhow-style: #[macro_export] + $crate paths inside.
    let dir = make_user_with_path_dep(
        "anyhow_like",
        "pub struct Error(pub String);\n\
         impl Error { pub fn msg(s: String) -> Self { Self(s) } }\n\
         #[macro_export]\nmacro_rules! bail {\n    ($e:expr) => { return Err($crate::Error::msg($e.to_string())) }\n}\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("V3 should handle both");

    let s = pkg.vendored[0].source.to_string();
    assert!(!s.contains("#[macro_export]"));
    assert!(s.contains("pub(crate) use bail;"));
    assert!(
        s.contains("$crate :: anyhow_like") || s.contains("$crate::anyhow_like"),
        "got:\n{s}"
    );
}

#[test]
fn vendor_refuses_dollar_crate_not_followed_by_double_colon() {
    // `$crate ;` in a macro body — rewriting to `$crate::dep ;` would be
    // syntactically invalid. We refuse rather than emit broken code.
    let dir = make_user_with_path_dep(
        "bad_dep",
        "macro_rules! weird { () => { $crate; } }\n",
        "fn main() {}\n",
    );
    let err = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("$crate") && msg.contains("::"),
        "expected refusal mentioning `$crate` and `::`; got: {msg}"
    );
}

#[test]
fn vendor_dollar_crate_inside_inline_mod() {
    // The macro is nested inside an inline mod; the rewriter must still
    // find and rewrite $crate references.
    let dir = make_user_with_path_dep(
        "nested",
        "pub mod inner {\n    pub struct Hidden;\n    macro_rules! mk { () => { $crate::inner::Hidden } }\n    pub(crate) use mk;\n}\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("nested macros should work");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        s.contains("$crate :: nested :: inner") || s.contains("$crate::nested::inner"),
        "got:\n{s}"
    );
}

#[test]
fn vendor_dollar_crate_in_nested_macro_call_inside_macro_body() {
    // $crate appears as an argument to another macro, inside the body of
    // the outer macro. The rewriter recurses through Group token trees.
    let dir = make_user_with_path_dep(
        "deep",
        "pub struct Foo;\n\
         macro_rules! make { ($t:ty) => { let _: $t; } }\n\
         macro_rules! outer { () => { make!($crate::Foo) } }\n\
         pub(crate) use outer;\npub(crate) use make;\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("nested $crate in arg position");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        s.contains("$crate :: deep :: Foo") || s.contains("$crate::deep::Foo"),
        "got:\n{s}"
    );
}

#[test]
fn vendor_dollar_crate_does_not_touch_non_macro_rules_invocations() {
    // `quote!` invocations inside the dep should not have their $crate-like
    // tokens (if any) rewritten — we only walk `macro_rules!` bodies.
    // Use a synthetic stand-in: the call site of a macro shouldn't be touched.
    let dir = make_user_with_path_dep(
        "nm_dep",
        "macro_rules! ident { ($($t:tt)*) => { $($t)* } }\n\
         pub fn _stub() { ident!(let _x = 1;); }\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("macro invocations stay intact");

    let s = pkg.vendored[0].source.to_string();
    // The macro body contains no $crate, so nothing to rewrite. Just sanity-
    // check that ident! call remains intact.
    assert!(s.contains("ident!"));
}

#[test]
fn vendor_rewrites_crate_path_in_type_ascription_inside_macro() {
    // Pre-fix: `collect_macro_invocation_token_rewrites` skipped the
    // rewrite of `crate::FOO` (and `SIBLING::FOO`) whenever the
    // immediately preceding token was `:`, on the assumption that the
    // colon was part of a `::crate` non-leading path segment. But a
    // single `:` is also type-ascription syntax (`field: crate::Foo`,
    // `arg: SIBLING::Foo`) — extremely common in struct fields and fn
    // signatures. Tokio's `cfg_io_driver! { mod driver { struct Cfg {
    // timer_flavor: crate::runtime::TimerFlavor } } }` was the
    // canonical real-world casualty.
    let dir = make_user_with_path_dep(
        "ta_dep",
        "pub struct Foo;\n\
         macro_rules! wrap { ($($t:tt)*) => { $($t)* } }\n\
         wrap! {\n    \
             pub struct Bar { pub f: crate::Foo }\n    \
             pub fn take(arg: crate::Foo) -> crate::Foo { arg }\n\
         }\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with type-ascription crate paths");

    let s = pkg.vendored[0].source.to_string();
    // Both `crate::Foo` references inside the wrap! body must be
    // rewritten to `crate::ta_dep::Foo`. Pre-fix, only the return-type
    // one was rewritten (no `:` in front) — the `f: crate::Foo` and
    // `arg: crate::Foo` were skipped.
    let count = s.matches("crate::ta_dep::Foo").count();
    assert!(
        count >= 3,
        "expected >=3 `crate::ta_dep::Foo` references, got {count}; source:\n{s}"
    );
}

#[test]
fn vendor_expands_include_macros_inside_macro_invocations() {
    // Regression for serde's `crate_root! { include!(concat!(env!(
    // "OUT_DIR"), "/private.rs")); }`. Pre-fix `expand_include_macros`
    // only saw `include!()` at AST item/expr position via syn's
    // visitor — anything inside another macro's tokens was opaque.
    // The flat output preserved the bare `include!(...)` which then
    // failed at user compile time with "environment variable
    // OUT_DIR not defined". Fix: walk_tokens_for_includes recurses
    // into macro invocation tokens looking for include!() patterns
    // and consumes the trailing `;` so the inlined content sits
    // cleanly at item position.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("src/generated.rs"),
        "pub const SHARED: u32 = 42;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! crate_root {\n    \
             () => {\n        \
                 include!(\"generated.rs\");\n    \
             }\n\
         }\n\
         crate_root!();\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with include! inside macro_rules body");

    let s = pkg.vendored[0].source.to_string();
    // The include! should be expanded — `pub const SHARED: u32 = 42;`
    // should be in the output.
    assert!(
        s.contains("pub const SHARED : u32 = 42") || s.contains("pub const SHARED: u32 = 42"),
        "include! inside crate_root! macro_rules body should be expanded; \
         got:\n{s}"
    );
    // The bare `include!(concat!(env!(...), ...))` should NOT be in
    // the output (would fail at user compile time with "env var
    // OUT_DIR not defined").
    assert!(
        !s.contains("env!(\"OUT_DIR\")") && !s.contains("env! (\"OUT_DIR\")"),
        "include! should have been expanded, not preserved verbatim; \
         got:\n{s}"
    );
}

#[test]
fn vendor_inject_imports_skips_mods_inside_item_list_macros() {
    // Regression for tokio's `cfg_rt! { mod sync_wrapper; ... }`.
    // The inline-macro pass splices the body to `mod sync_wrapper {
    // ... }` which is invisible to syn::parse_file (lives inside
    // the opaque macro tokens). Sibling-import injection then
    // emitted `use crate::sync_wrapper;` (sync_wrapper is also a
    // vendored sibling crate) and collided with the macro-emitted
    // `mod sync_wrapper` at the same scope (E0255 "the name
    // sync_wrapper is defined multiple times").
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src").join("util")).unwrap();
    fs::create_dir_all(dir.path().join("collide").join("src")).unwrap();
    // Sibling crate the dep is vendored alongside.
    fs::write(
        dir.path().join("collide").join("Cargo.toml"),
        "[package]\nname = \"collide\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("collide").join("src/lib.rs"),
        "pub fn from_collide() -> u32 { 1 }\n",
    )
    .unwrap();
    // Dep that defines a `cfg_rt!` macro and uses it to wrap a
    // submod called `collide`.
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ncollide = { path = \"../collide\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! cfg_rt {\n    \
             ($($item:item)*) => { $( $item )* }\n\
         }\n\
         cfg_rt! {\n    \
             mod collide;\n    \
             pub(crate) use self::collide::SHARED;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/collide.rs"),
        "pub const SHARED: u32 = 42;\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\ncollide = { path = \"../collide\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with collision pattern");

    // Find the dep's vendored source.
    let dep = pkg.vendored.iter().find(|d| d.name == "dep")
        .expect("dep should be vendored");
    let s = dep.source.to_string();
    // The `mod collide` was inlined inside `cfg_rt!`. The sibling
    // import for `collide` should NOT have been injected at the
    // top of dep's source (would collide).
    assert!(
        !s.contains("use crate::collide;"),
        "sibling import for `collide` must be skipped (mod with same name \
         lives inside cfg_rt!); got:\n{s}"
    );
}

#[test]
fn vendor_bakes_bare_cfg_passed_as_macro_argument() {
    // Regression for either's
    // `impl_specific_ref_and_mut!(::std::path::Path, cfg(feature = "std"), …)`.
    // The macro's matcher is `($t:ty, $($attr:meta)*)` (NOT
    // item-list shape, so the cfg-attr-rewrite-in-item-list-macros
    // pass doesn't catch it). The body splices each meta as
    // `#[$attr]`, so the bare `cfg(feature = "std")` arg becomes
    // `#[cfg(feature = "std")]` on the emitted impl. Pre-fix the
    // arg stayed verbatim; at user compile time the dep's "std"
    // feature is False → impl gated out → only the generic
    // `AsRef<Target>` impl remains, which can't handle unsized
    // targets (Path → [u8]) → `[u8] cannot be known at compilation
    // time`. Fix: walk every macro invocation's tokens and bake
    // bare `cfg(EXPR)` (when EXPR references a Cargo feature) the
    // same way as `#[cfg(EXPR)]` attrs.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [\"std\"]\nstd = []\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! attr_paste {\n    \
             ($($attr:meta),* ; $body:item) => {\n        \
                 $(#[$attr])* $body\n    \
             }\n\
         }\n\
         attr_paste!(cfg(feature = \"std\"); pub fn baked_in() -> u32 { 42 });\n\
         pub fn calls_baked() -> u32 { baked_in() }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with bare cfg in macro arg");

    let s = pkg.vendored[0].source.to_string();
    // The `cfg(feature = "std")` arg should have been baked to
    // `cfg(all())`. Pre-fix it stayed as `cfg(feature = "std")` and
    // got gated out at user compile time.
    assert!(
        s.contains("cfg (all ())") || s.contains("cfg(all())"),
        "expected `cfg(all())` baked into macro arg; got:\n{s}"
    );
    assert!(
        !s.contains("cfg(feature = \"std\")")
            && !s.contains("cfg (feature = \"std\")"),
        "expected no surviving `cfg(feature = \"std\")` in macro arg; got:\n{s}"
    );
}

#[test]
fn vendor_bakes_cfg_attrs_in_item_list_macro_invocations() {
    // Regression for tokio's `cfg_NAME!` family. The dep defines a
    // macro_rules with matcher `($($item:item)*) => { #[cfg(...)] $item }`,
    // and call sites pass `#[cfg(feature = "X")]` attrs in the macro
    // args that get pasted onto items in the expansion. Pre-fix the
    // cfg-attr rewriter only processed macro_rules DEFINITIONS, so
    // those args stayed verbatim and evaporated at user compile time
    // (the user crate doesn't enable the dep's "X" feature). Items
    // referenced via `pub(crate) use addr::to_socket_addrs` etc. then
    // resolved to nothing. Fix: detect macro_rules whose matcher
    // matches `$($i:item)*` and recurse into invocations of those
    // macros to bake their cfg attrs.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [\"net\"]\nnet = []\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! cfg_wrap {\n    \
             ($($item:item)*) => {\n        \
                 $( #[cfg(not(target_os = \"unknownos\"))] $item )*\n    \
             }\n\
         }\n\
         cfg_wrap! {\n    \
             #[cfg(feature = \"net\")]\n    \
             pub fn ok() -> u32 { 1 }\n\
         }\n\
         pub fn calls_ok() -> u32 { ok() }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with cfg-attr in item-list macro args");

    let s = pkg.vendored[0].source.to_string();
    // The `#[cfg(feature = "net")]` inside `cfg_wrap! { ... }` should
    // have been baked to `cfg(all())` (the dep enables `net` by
    // default at vendor time). Pre-fix it stayed as
    // `cfg(feature = "net")` and got gated out at user compile time.
    assert!(
        s.contains("cfg (all ())") || s.contains("cfg(all())"),
        "expected `cfg(all())` baked into cfg_wrap! body; got:\n{s}"
    );
    // And the original `cfg(feature = "net")` should NOT appear
    // unbaked inside the macro args anywhere.
    let baked_count = s.matches("cfg(feature = \"net\")")
        .count() + s.matches("cfg (feature = \"net\")").count();
    assert_eq!(
        baked_count, 0,
        "expected no surviving `cfg(feature = \"net\")` (vendor-time True must be baked); \
         got {baked_count} occurrences in:\n{s}"
    );
}

#[test]
fn vendor_bakes_feature_predicates_to_literal_true_false() {
    // The cfg_X / cfg_not_X mutual-exclusion pattern in tokio
    // (e.g. cfg_signal_internal! / cfg_not_signal_internal!) needs
    // the negation branch's evaluation to match the positive
    // branch's bake. Pre-fix: positive baked to cfg(all()), negation
    // contained `feature = "X"` predicates that evaluated False at
    // user time → both branches active → duplicate definitions.
    //
    // Fix: substitute Feature(name) → Literal(true_or_false) in the
    // simplifier. The negation expression cleanly resolves at vendor
    // time AND user time — Rust accepts `cfg(true)` / `cfg(false)`
    // literal predicates per the reference grammar.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [\"sig\"]\nsig = []\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! cfg_sig {\n    \
             ($($item:item)*) => { $( \
                 #[cfg(any(feature = \"sig\", feature = \"other\"))] \
                 $item \
             )* }\n\
         }\n\
         macro_rules! cfg_not_sig {\n    \
             ($($item:item)*) => { $( \
                 #[cfg(not(any(feature = \"sig\", feature = \"other\")))] \
                 $item \
             )* }\n\
         }\n\
         cfg_sig! { pub fn sig_only() -> u32 { 1 } }\n\
         cfg_not_sig! { pub fn not_sig() -> u32 { 2 } }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with cfg_X / cfg_not_X pair");

    let s = pkg.vendored[0].source.to_string();
    // The cfg_sig macro_rules body should bake to `cfg(all())`
    // (positive). The cfg_not_sig body should bake to `cfg(any())`
    // or equivalent — the literal-substituted negation collapses to
    // a False literal.
    assert!(
        s.contains("cfg(all())"),
        "expected positive branch baked to cfg(all()); got:\n{s}"
    );
    // Most importantly: NO `feature = "sig"` (or "other") should
    // survive in macro bodies — both branches must be statically
    // resolved at vendor time so the user's compile sees a clean
    // True/False answer.
    let macro_body_section: String = s
        .lines()
        .skip_while(|l| !l.contains("macro_rules! cfg_sig"))
        .take_while(|l| !l.contains("cfg_sig!") || l.contains("macro_rules"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !macro_body_section.contains("feature = \"sig\""),
        "feature predicates must be baked in macro_rules body; got:\n{macro_body_section}"
    );
}

#[test]
fn vendor_inactive_lib_rs_dep_is_vendored_with_cfg_gated_injections() {
    // Cross-target portability: crossterm_winapi (Windows-only via
    // `#![cfg(windows)]`) gets vendored anyway. The dep's body
    // evaporates at user compile time on non-matching targets via
    // its own `#![cfg]`, but vendoring it preserves it for users
    // who DO compile to that target. To keep the flat output
    // compilable on non-matching hosts, sibling-import injections
    // referring to such deps get cfg-gated with the same
    // predicate.
    //
    // Pre-fix, we pre-filtered the dep out entirely. The flat
    // output couldn't then be re-targeted: vendor on macOS, run on
    // Windows would fail because the Windows-only crate was
    // missing.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("winonly").join("src")).unwrap();
    fs::write(
        dir.path().join("winonly").join("Cargo.toml"),
        "[package]\nname = \"winonly\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("winonly").join("src/lib.rs"),
        "#![cfg(windows)]\npub fn windows_only_fn() -> u32 { 0 }\n",
    )
    .unwrap();
    // A second sibling that references winonly transitively (so its
    // sibling-import injection should be cfg-gated).
    fs::create_dir_all(dir.path().join("uses_winonly").join("src")).unwrap();
    fs::write(
        dir.path().join("uses_winonly").join("Cargo.toml"),
        "[package]\nname = \"uses_winonly\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nwinonly = { path = \"../winonly\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("uses_winonly").join("src/lib.rs"),
        "pub fn touch() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nuses_winonly = { path = \"../uses_winonly\" }\nwinonly = { path = \"../winonly\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with inactive-inner-cfg dep");

    // The dep IS vendored (preserves cross-target compilability).
    let was_vendored = pkg.vendored.iter().any(|d| d.name == "winonly");
    assert!(was_vendored, "winonly should be vendored, not skipped");
    // The other dep that lists winonly as a sibling has its
    // injection cfg-gated with the same windows predicate.
    let uses = pkg
        .vendored
        .iter()
        .find(|d| d.name == "uses_winonly")
        .expect("uses_winonly should be vendored");
    let s = uses.source.to_string();
    // Verify the gated injection. The cfg expression flows through
    // verbatim so the user's compile picks it up correctly.
    assert!(
        s.contains("#[cfg(windows)]")
            && s.contains("use crate::winonly"),
        "expected cfg(windows)-gated `use crate::winonly`; got:\n{s}"
    );
}

#[test]
fn vendor_preserves_compiler_set_cfgs_even_when_feature_collides() {
    // Per the Rust reference, compiler-set predicates (target_os,
    // unix/windows shorthands, etc.) MUST be preserved verbatim in
    // the flat output and evaluated by rustc at user compile time.
    // This is what makes the flat output target-portable — vendor
    // on macOS, run on Linux, the user's compile picks the right
    // target branches.
    //
    // Crossterm declares a Cargo feature literally named `windows`.
    // Without the is_compiler_set_predicate guard, `cfg(windows)`
    // would spuriously evaluate True on macOS via the feature-set
    // lookup and the attr would be stripped, leaving Windows-only
    // items active. The guard ensures the bare ident bypasses the
    // features check entirely.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [\"windows\"]\nwindows = []\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "#[cfg(windows)] pub fn windows_only() -> u32 { 0 }\n\
         #[cfg(unix)] pub fn unix_only() -> u32 { 1 }\n\
         #[cfg(target_os = \"linux\")] pub fn linux_only() -> u32 { 2 }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with compiler-set cfgs");

    let s = pkg.vendored[0].source.to_string();
    // ALL three compiler-set cfgs must be preserved verbatim. They
    // are NOT evaluated at vendor time. The dep's `windows` Cargo
    // feature must not be allowed to spuriously satisfy
    // `cfg(windows)`.
    for needle in [
        "cfg(windows)",
        "cfg(unix)",
        "cfg(target_os = \"linux\")",
    ] {
        assert!(
            s.contains(needle),
            "compiler-set predicate `{needle}` must be preserved verbatim; got:\n{s}"
        );
    }
    // All three functions are present (gated, but present).
    for needle in ["windows_only", "unix_only", "linux_only"] {
        assert!(
            s.contains(needle),
            "function `{needle}` should be present (gated by its cfg); got:\n{s}"
        );
    }
}

#[test]
fn vendor_bakes_compound_cfg_with_feature_and_target_predicates_inside_macro_invocation() {
    // Reproduces mio's `cfg_os_poll! { #[cfg(all(any(feature = "os-ext",
    // target_os = "freebsd"), not(target_os = "hermit")))] pub(crate)
    // mod pipe; }` shape. The compound cfg mixes a Cargo feature with
    // compiler-set predicates: the feature half MUST be baked to a
    // literal at vendor time (so the negation evaluates consistently
    // at user compile time) but every target_os predicate MUST flow
    // through verbatim.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src/sys")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [\"os-poll\"]\nos-poll = []\nos-ext = []\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! cfg_os_poll {\n    \
             ($($item:item)*) => { $( #[cfg(feature = \"os-poll\")] $item )* }\n\
         }\n\
         pub mod sys;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/mod.rs"),
        "cfg_os_poll! {\n    \
             #[cfg(all(\n        \
                 any(feature = \"os-ext\", target_os = \"freebsd\"),\n        \
                 not(target_os = \"hermit\"),\n        \
                 not(target_os = \"wasi\"),\n    \
             ))]\n    \
             pub(crate) mod pipe;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/pipe.rs"),
        "pub fn make() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with cfg_os_poll-style compound cfg");

    let s = pkg.vendored[0].source.to_string();
    // The Cargo feature half MUST be baked to a literal — `os-ext` is
    // not in the dep's enabled features (only `os-poll` is) so the
    // predicate becomes `false` at vendor time.
    assert!(
        !s.contains("feature = \"os-ext\""),
        "feature = \"os-ext\" should be baked to literal `false`; got:\n{s}"
    );
    // The compiler-set target_os predicates MUST be preserved so the
    // user's compile picks the right branches. Renderer emits without
    // spaces around `=` (proc_macro2 token-stream `to_string` style).
    for needle in [
        "target_os=\"freebsd\"",
        "not(target_os=\"hermit\")",
        "not(target_os=\"wasi\")",
    ] {
        assert!(
            s.contains(needle),
            "compiler-set predicate `{needle}` must flow through; got:\n{s}"
        );
    }
}

#[test]
fn vendor_re_exports_inner_item_via_self_path_inside_unix_macro_chain() {
    // Reproduces mio's SourceFd shape from sys/unix/mod.rs:
    //   #[cfg(feature = "os-ext")] mod sourcefd;
    //   #[cfg(feature = "os-ext")] pub use self::sourcefd::SourceFd;
    // re-exported one level up via sys/mod.rs:
    //   #[cfg(unix)] cfg_any_os_ext! {
    //     mod unix;
    //     #[cfg(feature = "os-ext")]
    //     pub use self::unix::SourceFd;
    //   }
    // and a second alias at lib.rs:
    //   #[cfg(all(unix, feature = "os-ext"))]
    //   pub mod unix { pub use crate::sys::SourceFd; }
    //
    // When the consuming crate enables `os-ext`, ALL three paths
    // (`crate::dep::sys::unix::SourceFd`, `crate::dep::sys::SourceFd`,
    // `crate::dep::unix::SourceFd`) MUST resolve in the flat output.
    // Pre-fix, several hops were lost when the bake pass collapsed
    // the cfg(unix) wrapper or stripped a `pub use` whose source mod
    // was renamed by a sibling-import injection.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src/sys/unix")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [\"os-ext\"]\nos-ext = []\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "#[macro_use]\nmod macros;\n\
         pub mod sys;\n\n\
         #[cfg(all(unix, feature = \"os-ext\"))]\n\
         pub mod unix {\n    \
             pub use crate::sys::SourceFd;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/macros.rs"),
        "macro_rules! cfg_any_os_ext {\n    \
             ($($item:item)*) => { $( \
                 #[cfg(any(feature = \"os-ext\", feature = \"net\"))] \
                 $item \
             )* }\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/mod.rs"),
        "#[cfg(unix)]\n\
         cfg_any_os_ext! {\n    \
             mod unix;\n    \
             #[cfg(feature = \"os-ext\")]\n    \
             pub use self::unix::SourceFd;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/unix/mod.rs"),
        "#[cfg(feature = \"os-ext\")]\nmod sourcefd;\n\n\
         #[cfg(feature = \"os-ext\")]\npub use self::sourcefd::SourceFd;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/unix/sourcefd.rs"),
        "pub struct SourceFd;\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor mio-style SourceFd dep");

    let s = pkg.vendored[0].source.to_string();
    // The `pub struct SourceFd` definition must appear once.
    assert!(
        s.contains("pub struct SourceFd"),
        "expected SourceFd struct in flat output:\n{s}"
    );
    // All three re-export hops must survive (each gated by its
    // original cfg, which the user's compile resolves).
    assert!(
        s.contains("pub use self::sourcefd::SourceFd"),
        "expected sys/unix/mod.rs hop to survive:\n{s}"
    );
    assert!(
        s.contains("pub use self::unix::SourceFd"),
        "expected sys/mod.rs hop to survive:\n{s}"
    );
    // The lib.rs hop's `crate::sys::SourceFd` gets remapped to
    // `crate::dep::sys::SourceFd` because the dep is vendored at
    // `crate::dep::*`. The path-rewrite is what makes the alias
    // resolve; without it the hop dangles.
    assert!(
        s.contains("pub use crate::dep::sys::SourceFd"),
        "expected lib.rs hop with remapped crate-path:\n{s}"
    );
}

#[test]
fn vendor_pub_crate_mod_supports_internal_path_re_export() {
    // Reproduces tokio's AbortHandle re-export shape:
    //   runtime/mod.rs:        pub(crate) mod task;
    //   runtime/task/mod.rs:   mod abort; pub use self::abort::AbortHandle;
    //   runtime/task/abort.rs: pub struct AbortHandle;
    //   task/mod.rs:           pub use crate::runtime::task::AbortHandle;
    //
    // The `pub(crate) mod task` is internal to tokio but visible to
    // tokio's `task::mod` re-export. After vendoring at `crate::dep`,
    // both `crate::dep::task::AbortHandle` (the public alias) and
    // `crate::dep::runtime::task::AbortHandle` (the internal-but-
    // pub-within-user-crate path) MUST resolve. Pre-fix, internal
    // path refs through `pub(crate) mod` chains broke when a sibling-
    // import injection collided with the mod name.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src/runtime/task")).unwrap();
    fs::create_dir_all(dir.path().join("dep").join("src/task")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "pub mod runtime;\npub mod task;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/runtime/mod.rs"),
        "pub(crate) mod task;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/runtime/task/mod.rs"),
        "mod abort;\npub use self::abort::AbortHandle;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/runtime/task/abort.rs"),
        "pub struct AbortHandle;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/task/mod.rs"),
        "pub use crate::runtime::task::AbortHandle;\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor pub(crate) re-export pattern");

    let s = pkg.vendored[0].source.to_string();
    assert!(
        s.contains("pub struct AbortHandle"),
        "AbortHandle struct must appear:\n{s}"
    );
    assert!(
        s.contains("pub use self::abort::AbortHandle"),
        "internal `pub use self::abort::AbortHandle` must survive:\n{s}"
    );
    // Path remap rewrites `crate::runtime::task` → `crate::dep::runtime::task`.
    assert!(
        s.contains("pub use crate::dep::runtime::task::AbortHandle"),
        "external `pub use crate::*::runtime::task::AbortHandle` (remapped) must survive:\n{s}"
    );
    // The `pub(crate) mod task` declaration inside runtime survives —
    // visibility is preserved so the alias above can reach it.
    assert!(
        s.contains("pub(crate) mod task"),
        "`pub(crate) mod task` must survive (gives the alias something to reach):\n{s}"
    );
}

#[test]
fn vendor_macro_rules_inside_doc_wrapper_emits_use_export() {
    // Reproduces tokio's doc!{macro_rules! select{...}} pattern.
    // The doc! wrapper always adds `#[macro_export]` to its $item arg,
    // so `select!` ends up at the crate root after expansion. Caller
    // crates (axum) invoke `tokio::select!`. After vendoring tokio at
    // `crate::tokio::*`, `tokio::select!` must still resolve — but
    // collect_macro_export_rewrites only sees direct `#[macro_export]
    // macro_rules! NAME` shapes, not macro_rules nested in a wrapper
    // invocation.
    //
    // Pre-fix: the synthesised `pub(crate) use NAME;` was missing for
    // wrapper-nested macros, so caller paths like `tokio::select!`
    // dangled.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "macro_rules! doc {\n    \
             ($select:item) => { #[macro_export] $select };\n\
         }\n\
         doc! { macro_rules! select { () => { 42 }; } }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user").join("src/main.rs"),
        "fn main() { let _ = dep::select!(); }\n",
    )
    .unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor doc!{macro_rules! select{...}} pattern");

    let s = pkg.vendored[0].source.to_string();
    // The macro_rules definition must appear in the flat output.
    assert!(
        s.contains("macro_rules ! select")
            || s.contains("macro_rules! select"),
        "macro_rules! select definition missing:\n{s}"
    );
    // The synthesised re-export under the dep's namespace must be
    // present so callers can reach the macro via `dep::select!`.
    assert!(
        s.contains("pub(crate) use select"),
        "expected synthesised `pub(crate) use select;` so callers can reach it via dep::select!:\n{s}"
    );
}

#[test]
fn real_vendor_signal_hook_registry_parses_clean() {
    // signal-hook-registry shows up transitively via tokio's `signal`
    // and `process` features. Repeat-offender "expected `;`" parse
    // failures during vendoring would block any tokio-using crate
    // from flattening without `--external-preset infra`. This test
    // surfaces the parse-failure regression directly: vendor with NO
    // preset, just `--vendor`, and assert success.
    if !cargo_available() {
        eprintln!("skipping: cargo not available");
        return;
    }
    let user = synth_user_with_deps(
        "signal-hook-registry = \"1\"\n",
        "fn main() { let _ = signal_hook_registry::SIGNALS_FORBIDDEN.iter(); }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &["--vendor", "--no-banner", "--stdout"],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Make the failure mode obvious: distinguish parse failure from
        // other vendoring failures. The historic bug surfaced as a
        // "expected `;`" syn parse error.
        assert!(
            !stderr.contains("expected `;`"),
            "signal-hook-registry parse regression — vendoring failed with `expected ;`:\n{stderr}"
        );
        eprintln!("skipping: vendoring failed for non-parse reason: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    assert!(
        flat.contains("signal_hook_registry") || flat.contains("pub mod signal_hook_registry"),
        "expected signal_hook_registry to be in vendored output"
    );
}

#[test]
fn vendor_inlines_mod_whose_target_has_inactive_inner_cfg() {
    // Cross-target portability: a mod whose target file opens with
    // `#![cfg(target_os = "windows")]` (windows-sys' `mod Wdk;` is
    // the canonical case) gets inlined unconditionally. The file's
    // inner `#![cfg(...)]` attr becomes the inner attr of the
    // spliced `mod NAME { ... }` block — gates the contents at
    // user compile time. Vendor on macOS, run on Linux: the cfg
    // flows through and rustc evaluates correctly.
    //
    // Pre-fix, the mod was warn-skipped on non-matching hosts,
    // making the flat output host-locked.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "pub mod platform;\n",
    )
    .unwrap();
    // target_os = "noosanywhere" is never satisfied at any host's
    // compile time — so the `pub fn never_built` is correctly
    // gated out, but its DECLARATION is preserved in the flat
    // output (so the cfg flows through to the user's compiler).
    fs::write(
        dir.path().join("dep").join("src/platform.rs"),
        "#![cfg(target_os = \"noosanywhere\")]\n\
         pub fn never_built() -> u32 { 0 }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("inner-cfg-gated mod must be inlined");

    let s = pkg.vendored[0].source.to_string();
    // The body IS inlined verbatim (declaration + cfg attr both).
    assert!(
        s.contains("never_built"),
        "expected `never_built` to be inlined (cfg-gated by inner attr); got:\n{s}"
    );
    // The inner #![cfg(...)] attr is preserved so user time gates
    // out the contents.
    assert!(
        s.contains("cfg(target_os = \"noosanywhere\")")
            || s.contains("cfg (target_os = \"noosanywhere\")"),
        "expected inner cfg attr preserved; got:\n{s}"
    );
    // No skip placeholder.
    assert!(
        !s.contains("cfg-skipped: source for `mod platform`"),
        "mod should be inlined, not skipped; got:\n{s}"
    );
}

#[test]
fn vendor_emits_all_cfg_attr_path_candidates_for_top_level_mod() {
    // Cross-target portability: socket2's `#[cfg_attr(unix, path =
    // "sys/unix.rs")] #[cfg_attr(windows, path = "sys/windows.rs")]
    // mod sys;` should produce BOTH `#[cfg(unix)] mod sys { ... }`
    // AND `#[cfg(windows)] mod sys { ... }` in the flat output. The
    // user's compile picks one based on their target. Pre-fix, only
    // the host's branch was inlined → vendor on macOS, run on
    // Linux failed (Linux mod's contents weren't there).
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src").join("sys")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "#[cfg_attr(unix, path = \"sys/unix.rs\")]\n\
         #[cfg_attr(windows, path = \"sys/windows.rs\")]\n\
         pub mod sys;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/unix.rs"),
        "pub const SENTINEL: u32 = 1;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/windows.rs"),
        "pub const SENTINEL: u32 = 2;\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with multi-cfg path attrs");

    let s = pkg.vendored[0].source.to_string();
    // Both unix and windows branches present, each cfg-gated and
    // each contains its respective SENTINEL value.
    assert!(
        s.contains("#[cfg(unix)]") || s.contains("#[cfg (unix)]"),
        "expected unix cfg-gate; got:\n{s}"
    );
    assert!(
        s.contains("#[cfg(windows)]") || s.contains("#[cfg (windows)]"),
        "expected windows cfg-gate; got:\n{s}"
    );
    assert!(
        s.contains("pub const SENTINEL: u32 = 1"),
        "expected unix SENTINEL = 1; got:\n{s}"
    );
    assert!(
        s.contains("pub const SENTINEL: u32 = 2"),
        "expected windows SENTINEL = 2; got:\n{s}"
    );
}

#[test]
fn vendor_resolves_cfg_attr_path_for_top_level_mod() {
    // socket2's `#[cfg_attr(unix, path = "sys/unix.rs")]
    // #[cfg_attr(windows, path = "sys/windows.rs")] mod sys;` was
    // silently dropped by `extract_path_attr` (which only recognised
    // plain `#[path]`). The mod then fell through to standard
    // resolution, which couldn't find `sys.rs` or `sys/mod.rs`, hit
    // the `has_cfg` warn-skip branch, and emitted an empty
    // `mod sys {}` — leaving downstream cargo-build with hundreds of
    // `cannot find value SOL_SOCKET in module sys` errors.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("dep").join("src").join("sys")).unwrap();
    fs::write(
        dir.path().join("dep").join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/lib.rs"),
        "#[cfg_attr(unix, path = \"sys/unix.rs\")]\n\
         #[cfg_attr(windows, path = \"sys/windows.rs\")]\n\
         pub mod sys;\n\
         pub fn touch() -> u32 { sys::SENTINEL }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/unix.rs"),
        "pub const SENTINEL: u32 = 1;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dep").join("src/sys/windows.rs"),
        "pub const SENTINEL: u32 = 2;\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("user").join("src")).unwrap();
    fs::write(
        dir.path().join("user").join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("user").join("src/main.rs"), "fn main() {}\n").unwrap();

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with cfg_attr path on top-level mod");

    let s = pkg.vendored[0].source.to_string();
    // The unix file's body should be inlined (we're on macOS or
    // Linux in CI; both are unix).
    assert!(
        s.contains("pub const SENTINEL: u32 = 1;"),
        "expected unix body inlined, got:\n{s}"
    );
    // The cfg-skipped placeholder must NOT appear — pre-fix that's
    // what the `mod sys` would degenerate to.
    assert!(
        !s.contains("cfg-skipped: source for `mod sys`"),
        "mod sys should resolve via cfg_attr path, not be cfg-skipped"
    );
}

#[test]
fn vendor_macro_export_inside_inline_mod() {
    let dir = make_user_with_path_dep(
        "nested_exp",
        "pub mod inner {\n    #[macro_export]\n    macro_rules! shout { ($e:expr) => { format!(\"!{}!\", $e) } }\n}\n",
        "fn main() {}\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("nested macro_export should be handled");

    let s = pkg.vendored[0].source.to_string();
    assert!(!s.contains("#[macro_export]"));
    assert!(s.contains("pub(crate) use shout;"));
}

#[test]
fn vendor_skips_macro_inside_deleted_item() {
    // A cfg-False mod containing a macro_rules with $crate. The mod gets
    // deleted, so we should not emit any edits inside it (no spurious
    // pub use, no broken $crate rewrite).
    let dir = make_user_with_featured_dep(
        "fdep",
        &["x"],
        &[],
        "#[cfg(feature = \"x\")]\npub mod gated {\n    pub struct G;\n    #[macro_export]\n    macro_rules! mk { () => { $crate::gated::G } }\n}\npub fn live() {}\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("V3+V2 interaction");
    let s = pkg.vendored[0].source.to_string();
    // Mod and macro both gone.
    assert!(!s.contains("pub mod gated"));
    assert!(!s.contains("macro_rules!"));
    assert!(!s.contains("pub(crate) use mk;"));
    assert!(s.contains("pub fn live"));
}

#[test]
fn vendor_macro_with_dollar_crate_compiles_via_rustc() {
    if !rustc_available() {
        eprintln!("skipping: rustc not on PATH");
        return;
    }
    let dir = make_user_with_path_dep(
        "tinylog",
        "pub struct Logger;\nimpl Logger { pub fn record(s: &str) { println!(\"[log] {s}\"); } }\n\
         #[macro_export]\nmacro_rules! note { ($($a:tt)*) => { $crate::Logger::record(&format!($($a)*)) } }\n",
        "fn main() { tinylog::note!(\"hello {}\", 42); }\n",
    );

    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("vendor with macro should succeed");

    let mut out = String::new();
    use std::fmt::Write as _;
    write!(&mut out, "{}", pkg.user_source).unwrap();
    for d in &pkg.vendored {
        writeln!(&mut out).unwrap();
        writeln!(&mut out, "mod {} {{", d.name).unwrap();
        write!(&mut out, "{}", d.source).unwrap();
        writeln!(&mut out, "\n}}").unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("flat.rs");
    fs::write(&flat, &out).unwrap();
    let res = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-type=bin",
            "-A",
            "warnings",
            "-o",
        ])
        .arg(tmp.path().join("bin"))
        .arg(&flat)
        .output()
        .unwrap();
    if !res.status.success() {
        let stderr = String::from_utf8_lossy(&res.stderr);
        panic!("rustc rejected V3 vendored output:\n{stderr}\n--- output ---\n{out}");
    }

    // Run the binary; verify the macro actually expanded and printed.
    let run = std::process::Command::new(tmp.path().join("bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("[log] hello 42"),
        "expected log output; got: {stdout}"
    );
}

#[test]
fn vendor_does_not_rewrite_dollar_crate_with_whitespace() {
    // `$ crate` (with space) is NOT the special $crate token — it's a Punct
    // followed by an Ident. Our rewriter must not touch it (we check
    // Spacing::Joint on the `$`).
    let dir = make_user_with_path_dep(
        "ws_dep",
        // `$ crate` would be a parse error in real macro bodies; use it as
        // a benign token sequence inside a passthrough macro.
        "macro_rules! pass { ($($t:tt)*) => { $($t)* } }\npub fn x() { pass!(let _ = 1;); }\n",
        "fn main() {}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .expect("benign passthrough macro");
    let s = pkg.vendored[0].source.to_string();
    // Sanity: no spurious ::ws_dep insertions.
    assert!(!s.contains("crate :: ws_dep"));
    assert!(!s.contains("crate::ws_dep"));
}

// ---------------------------------------------------------------------------
// --external + --vendor-extras (see EXTERNAL.md)
// ---------------------------------------------------------------------------

/// Build a workspace with two path-deps where `outer` re-exports / uses
/// `shared`, and `other` also uses `shared`. The user crate uses both
/// outer and other. Returns the user-crate root.
///
/// Layout:
///   shared/         (a bare path-dep with one fn)
///   outer/          (depends on shared, exposes a wrapper)
///   other/          (depends on shared, exposes a different wrapper)
///   user/           (depends on outer + other)
fn make_diamond_workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    fs::create_dir_all(dir.path().join("shared/src")).unwrap();
    fs::write(
        dir.path().join("shared/Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("shared/src/lib.rs"),
        "pub fn ping() -> &'static str { \"pong\" }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("outer/src")).unwrap();
    fs::write(
        dir.path().join("outer/Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("outer/src/lib.rs"),
        "pub fn from_outer() -> &'static str { shared::ping() }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("other/src")).unwrap();
    fs::write(
        dir.path().join("other/Cargo.toml"),
        "[package]\nname = \"other\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("other/src/lib.rs"),
        "pub fn from_other() -> &'static str { shared::ping() }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\n\
         outer = { path = \"../outer\" }\n\
         other = { path = \"../other\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() {\n    \
            println!(\"{} / {}\", outer::from_outer(), other::from_other());\n\
         }\n",
    )
    .unwrap();

    dir
}

#[test]
fn external_keeps_dep_out_of_vendored_set() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n",
        "fn main() { let _ = pure_dep::x; }\n",
    );
    let opts = VendorOptions {
        external: ["pure_dep".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts)
        .expect("vendor with --external should succeed");
    assert!(pkg.vendored.is_empty(), "no deps should be vendored");
    assert_eq!(pkg.external.len(), 1);
    assert_eq!(pkg.external[0].name, "pure_dep");
    assert!(matches!(pkg.external[0].reason, ExternalReason::UserExcluded));
}

#[test]
fn external_does_not_emit_mod_block_for_skipped_dep() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n",
        "fn main() { let _ = pure_dep::x; }\n",
    );
    let opts = VendorOptions {
        external: ["pure_dep".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();
    let user_src = pkg.user_source.to_string();
    // The user's main remains; no `mod pure_dep { ... }` is added since
    // we don't vendor it. (main.rs handles the actual mod-emission per dep.)
    assert!(user_src.contains("pure_dep::x"));
    assert_eq!(pkg.vendored.len(), 0);
}

#[test]
fn external_overrides_unvendorable_refusal() {
    // Build a fixture with a build script that links a native lib
    // (always → Unvendorable under the holistic build-script policy),
    // then exclude it. Vendor should succeed without a strict-mode
    // refusal. Pre-policy any build script was unvendorable; now only
    // build scripts whose effects we can't replay (link-lib, link-arg,
    // rustc-env, …) block.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("buildy/src")).unwrap();
    fs::write(
        dir.path().join("buildy/Cargo.toml"),
        "[package]\nname = \"buildy\"\nversion = \"0.0.1\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("buildy/build.rs"),
        "fn main() { println!(\"cargo::rustc-link-lib=foo_unvendorable\"); }\n",
    )
    .unwrap();
    fs::write(dir.path().join("buildy/src/lib.rs"), "pub fn x() {}\n").unwrap();

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nbuildy = { path = \"../buildy\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { let _ = buildy::x; }\n",
    )
    .unwrap();

    // Without --external, vendoring refuses (strict + buildy is Unvendorable).
    let strict = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &VendorOptions::default(),
    );
    assert!(strict.is_err(), "expected strict refusal without --external");

    // With --external buildy, succeeds.
    let opts = VendorOptions {
        external: ["buildy".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts)
        .expect("--external should bypass refusal");
    assert!(matches!(
        pkg.external.iter().find(|d| d.name == "buildy").unwrap().reason,
        ExternalReason::UserExcluded
    ));
}

#[test]
fn external_orphan_dep_is_dropped_from_output() {
    // user → outer → shared. Pass --external outer. shared has no other
    // referrers, so it should be dropped entirely (not vendored, not in
    // external list).
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("shared/src")).unwrap();
    fs::write(
        dir.path().join("shared/Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("shared/src/lib.rs"), "pub fn x() {}\n").unwrap();

    fs::create_dir_all(dir.path().join("outer/src")).unwrap();
    fs::write(
        dir.path().join("outer/Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    )
    .unwrap();
    fs::write(dir.path().join("outer/src/lib.rs"), "pub fn x() { shared::x() }\n").unwrap();

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nouter = { path = \"../outer\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { outer::x() }\n",
    )
    .unwrap();

    let opts = VendorOptions {
        external: ["outer".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();
    assert!(pkg.vendored.is_empty(), "no vendored deps expected");
    let external_names: Vec<&str> = pkg.external.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(external_names, vec!["outer"], "shared should be dropped, not promoted");
}

#[test]
fn external_diamond_vendors_orphan_through_other_path() {
    // user → outer → shared
    // user → other → shared
    // --external outer. shared is reachable via other (a non-external),
    // so the BFS still pulls it into `to_vendor`. Both other and shared
    // get vendored; only outer ends up external.
    let dir = make_diamond_workspace();
    let opts = VendorOptions {
        external: ["outer".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();

    let mut vendored_names: Vec<&str> = pkg.vendored.iter().map(|d| d.name.as_str()).collect();
    vendored_names.sort();
    assert_eq!(vendored_names, vec!["other", "shared"]);

    let outer = pkg.external.iter().find(|d| d.name == "outer").expect("outer external");
    assert!(matches!(outer.reason, ExternalReason::UserExcluded));
    assert_eq!(pkg.external.len(), 1, "only outer should be external; got {:?}", pkg.external);
}

#[test]
fn external_unknown_name_does_not_break() {
    // Spelling a non-existent dep should not error or change behavior.
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n",
        "fn main() { let _ = pure_dep::x; }\n",
    );
    let opts = VendorOptions {
        external: ["nonexistent_typo".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();
    // pure_dep still got vendored normally.
    assert_eq!(pkg.vendored.len(), 1);
    assert_eq!(pkg.vendored[0].name, "pure_dep");
}

#[test]
fn external_file_via_cli_combines_with_inline_flag() {
    // Synthesize a workspace with two deps. Pass one via --external and
    // one via --external-file; verify both end up excluded.
    let dir = tempfile::tempdir().unwrap();

    for name in ["dep_a", "dep_b"] {
        fs::create_dir_all(dir.path().join(name).join("src")).unwrap();
        fs::write(
            dir.path().join(name).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        fs::write(dir.path().join(name).join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    }

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\ndep_a = { path = \"../dep_a\" }\ndep_b = { path = \"../dep_b\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { dep_a::x(); dep_b::x(); }\n",
    )
    .unwrap();

    // Write the externals file: comments + the dep_b name.
    let externals_file = dir.path().join("externals.txt");
    fs::write(
        &externals_file,
        "# externals shared across the team\n\
         dep_b   # b is huge, keep external\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(dir.path().join("user"))
        .args([
            "--vendor",
            "--external",
            "dep_a",
            "--external-file",
        ])
        .arg(&externals_file)
        .args(["--stdout", "--no-banner"])
        .output()
        .expect("spawn flatten");
    assert!(
        output.status.success(),
        "flatten failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let out = String::from_utf8(output.stdout).unwrap();
    // Neither dep should be vendored — no `mod dep_a {` or `mod dep_b {` block.
    assert!(!out.contains("mod dep_a {"), "dep_a should be external; got:\n{out}");
    assert!(!out.contains("mod dep_b {"), "dep_b should be external; got:\n{out}");
    // User code's references survive.
    assert!(out.contains("dep_a::x()"));
    assert!(out.contains("dep_b::x()"));
}

#[test]
fn external_file_missing_path_errors_clearly() {
    let dir = make_user_with_path_dep("pure_dep", "pub fn x() {}\n", "fn main() {}\n");
    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(dir.path().join("user"))
        .args([
            "--vendor",
            "--external-file",
            "/nonexistent/externals.txt",
            "--stdout",
        ])
        .output()
        .expect("spawn flatten");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--external-file") && stderr.contains("nonexistent"),
        "expected helpful path-missing error; got: {stderr}"
    );
}

#[test]
fn external_via_cli_compiles_with_cargo() {
    // End-to-end: synthesize user + path-dep, vendor with --external,
    // drop the flat output as src/main.rs in a NEW crate whose Cargo.toml
    // lists pure_dep as a path-dep, run `cargo build`, verify it compiles.
    if std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    let src_workspace = make_user_with_path_dep(
        "pure_dep",
        "pub fn double(x: i32) -> i32 { x * 2 }\n",
        "fn main() { println!(\"{}\", pure_dep::double(21)); }\n",
    );

    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(src_workspace.path().join("user"))
        .args(["--vendor", "--external", "pure_dep", "--stdout", "--no-banner"])
        .output()
        .expect("spawn flatten");
    assert!(
        output.status.success(),
        "flatten failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Set up a fresh crate that includes the flat output and lists pure_dep
    // as a direct dep (path-dep into the synth workspace).
    let downstream = tempfile::tempdir().unwrap();
    fs::create_dir_all(downstream.path().join("src")).unwrap();
    let dep_path = src_workspace.path().join("pure_dep");
    fs::write(
        downstream.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"downstream\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\npure_dep = {{ path = \"{}\" }}\n",
            dep_path.display()
        ),
    )
    .unwrap();
    fs::write(downstream.path().join("src/main.rs"), &output.stdout).unwrap();

    let cargo_build = std::process::Command::new("cargo")
        .arg("build")
        .current_dir(downstream.path())
        .output()
        .expect("spawn cargo build");
    if !cargo_build.status.success() {
        let stderr = String::from_utf8_lossy(&cargo_build.stderr);
        let main_rs = String::from_utf8_lossy(&output.stdout);
        panic!("cargo build failed:\n{stderr}\n--- src/main.rs ---\n{main_rs}");
    }
}

// ---------------------------------------------------------------------------
// --external-deep: externalise the transitive cone, auto-promote
// vendored crates' shared transitives to "Required" Cargo.toml entries.
// ---------------------------------------------------------------------------

/// Build a 4-crate workspace where C_core is reachable both directly
/// (A → C_core) and transitively (A → B → C → C_core). Returns the
/// workspace temp dir; user crate is at `<tmp>/user`.
///
/// This is the canonical fixture for testing --external-deep: passing
/// `--external C` alone vendors C_core (because the BFS reaches it via
/// A's direct dep), but `--external C --external-deep` cuts both C and
/// C_core, leaving A's reference to C_core needing to be resolved via
/// the user's Cargo.toml (auto-promoted to Required).
fn make_diamond_with_shared_leaf() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    fs::create_dir_all(dir.path().join("c_core/src")).unwrap();
    fs::write(
        dir.path().join("c_core/Cargo.toml"),
        "[package]\nname = \"c_core\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("c_core/src/lib.rs"),
        "pub fn shared() -> i32 { 7 }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("c/src")).unwrap();
    fs::write(
        dir.path().join("c/Cargo.toml"),
        "[package]\nname = \"c\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nc_core = { path = \"../c_core\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("c/src/lib.rs"),
        "pub fn from_c() -> i32 { c_core::shared() * 10 }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("b/src")).unwrap();
    fs::write(
        dir.path().join("b/Cargo.toml"),
        "[package]\nname = \"b\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nc = { path = \"../c\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("b/src/lib.rs"),
        "pub fn from_b() -> i32 { c::from_c() }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("a/src")).unwrap();
    fs::write(
        dir.path().join("a/Cargo.toml"),
        "[package]\nname = \"a\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nb = { path = \"../b\" }\nc_core = { path = \"../c_core\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("a/src/lib.rs"),
        "pub fn from_a() -> i32 { b::from_b() + c_core::shared() }\n",
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\na = { path = \"../a\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { println!(\"{}\", a::from_a()); }\n",
    )
    .unwrap();

    dir
}

#[test]
fn external_without_deep_vendors_shared_transitive() {
    // Baseline: --external c (without deep). c_core is reachable via A
    // directly, so it gets vendored. This is the case --external-deep
    // exists to fix.
    let dir = make_diamond_with_shared_leaf();
    let opts = VendorOptions {
        external: ["c".to_string()].into_iter().collect(),
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();

    let mut vendored: Vec<&str> = pkg.vendored.iter().map(|d| d.name.as_str()).collect();
    vendored.sort();
    assert_eq!(
        vendored,
        vec!["a", "b", "c_core"],
        "without --external-deep, c_core gets vendored: got {vendored:?}"
    );
    let externals: Vec<&str> = pkg.external.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(externals, vec!["c"]);
}

#[test]
fn external_deep_externalises_transitive_cone() {
    // --external c --external-deep-aggressive: every transitive dep
    // of every external becomes external, including c_core (reachable
    // from vendored A directly). Vendored A's `use c_core::*` now
    // needs c_core in extern prelude → auto-promoted to Required.
    //
    // The default `--external-deep` (without aggressive) preserves
    // c_core as vendored — see external_deep_keeps_dual_path_dep_vendored.
    let dir = make_diamond_with_shared_leaf();
    let opts = VendorOptions {
        external: ["c".to_string()].into_iter().collect(),
        external_deep: true,
        external_deep_aggressive: true,
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();

    let mut vendored: Vec<&str> = pkg.vendored.iter().map(|d| d.name.as_str()).collect();
    vendored.sort();
    assert_eq!(vendored, vec!["a", "b"], "c_core should NOT be vendored");

    let c = pkg.external.iter().find(|d| d.name == "c").expect("c external");
    assert!(matches!(c.reason, ExternalReason::UserExcluded));

    let c_core = pkg
        .external
        .iter()
        .find(|d| d.name == "c_core")
        .expect("c_core auto-promoted");
    let ExternalReason::Required { because } = &c_core.reason else {
        panic!("expected Required, got {:?}", c_core.reason);
    };
    assert!(
        because.iter().any(|n| n == "a"),
        "c_core should cite vendored `a` as the reason: got {because:?}"
    );
}

#[test]
fn external_deep_drops_unreferenced_transitives_silently() {
    // Linear chain: user → a → b → c → c_core, no other paths, no other
    // references. --external a --external-deep cuts everything below a.
    // The transitives are dropped entirely (not promoted to Required)
    // because no vendored crate references them.
    let dir = tempfile::tempdir().unwrap();

    for (name, deps_section, body) in [
        ("c_core", "", "pub fn x() -> i32 { 1 }\n"),
        ("c", "[dependencies]\nc_core = { path = \"../c_core\" }\n", "pub fn x() -> i32 { c_core::x() }\n"),
        ("b", "[dependencies]\nc = { path = \"../c\" }\n", "pub fn x() -> i32 { c::x() }\n"),
        ("a", "[dependencies]\nb = { path = \"../b\" }\n", "pub fn x() -> i32 { b::x() }\n"),
    ] {
        fs::create_dir_all(dir.path().join(name).join("src")).unwrap();
        fs::write(
            dir.path().join(name).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n{deps_section}"
            ),
        )
        .unwrap();
        fs::write(dir.path().join(name).join("src/lib.rs"), body).unwrap();
    }

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\na = { path = \"../a\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { let _ = a::x(); }\n",
    )
    .unwrap();

    let opts = VendorOptions {
        external: ["a".to_string()].into_iter().collect(),
        external_deep: true,
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();

    assert!(
        pkg.vendored.is_empty(),
        "everything reachable only via `a` should be cut: got {:?}",
        pkg.vendored.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
    let externals: Vec<&str> = pkg.external.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(externals, vec!["a"], "only the explicit external should appear");
}

#[test]
fn external_deep_no_op_without_external() {
    // Sanity: external_deep alone (no --external) is a no-op — the
    // expansion has nothing to expand from. (The CLI enforces this via
    // clap `requires = "external"`, but the library API allows it.)
    let dir = make_user_with_path_dep("pure_dep", "pub fn x() {}\n", "fn main() {}\n");
    let opts = VendorOptions {
        external_deep: true, // but no `external`
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();
    assert_eq!(pkg.vendored.len(), 1);
    assert_eq!(pkg.vendored[0].name, "pure_dep");
    assert!(pkg.external.is_empty());
}

#[test]
fn external_deep_with_multiple_explicit_externals_unions_their_cones() {
    // Two explicit externals, each with their own transitive cone.
    // --external-deep should externalise both cones.
    let dir = tempfile::tempdir().unwrap();

    for (name, deps_section, body) in [
        ("leaf_x", "", "pub fn x() {}\n"),
        ("leaf_y", "", "pub fn y() {}\n"),
        ("mid_x", "[dependencies]\nleaf_x = { path = \"../leaf_x\" }\n", "pub fn mx() { leaf_x::x() }\n"),
        ("mid_y", "[dependencies]\nleaf_y = { path = \"../leaf_y\" }\n", "pub fn my() { leaf_y::y() }\n"),
    ] {
        fs::create_dir_all(dir.path().join(name).join("src")).unwrap();
        fs::write(
            dir.path().join(name).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n{deps_section}"
            ),
        )
        .unwrap();
        fs::write(dir.path().join(name).join("src/lib.rs"), body).unwrap();
    }

    fs::create_dir_all(dir.path().join("user/src")).unwrap();
    fs::write(
        dir.path().join("user/Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nmid_x = { path = \"../mid_x\" }\nmid_y = { path = \"../mid_y\" }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("user/src/main.rs"),
        "fn main() { mid_x::mx(); mid_y::my(); }\n",
    )
    .unwrap();

    let opts = VendorOptions {
        external: ["mid_x".to_string(), "mid_y".to_string()].into_iter().collect(),
        external_deep: true,
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();

    assert!(pkg.vendored.is_empty(), "everything cut");
    let mut external_names: Vec<&str> = pkg.external.iter().map(|d| d.name.as_str()).collect();
    external_names.sort();
    // Only the explicit externals appear: the leaves are silently dropped
    // because nothing vendored references them, so the user doesn't need
    // them in their Cargo.toml — cargo brings them in transitively when
    // the user lists mid_x and mid_y.
    assert_eq!(external_names, vec!["mid_x", "mid_y"]);
}

#[test]
fn external_deep_via_cli_compiles_with_cargo() {
    // End-to-end: synthesize the diamond, vendor with --external c
    // --external-deep-aggressive, drop into a fresh crate that has
    // BOTH c and c_core in its Cargo.toml, run `cargo build`, verify
    // it compiles. (Aggressive mode is required to push c_core to
    // the user's Cargo.toml; the default refined mode keeps c_core
    // vendored — see external_deep_keeps_dual_path_dep_vendored.)
    if std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: cargo not on PATH");
        return;
    }

    let src_workspace = make_diamond_with_shared_leaf();

    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(src_workspace.path().join("user"))
        .args([
            "--vendor",
            "--external",
            "c",
            "--external-deep",
            "--external-deep-aggressive",
            "--stdout",
            "--no-banner",
        ])
        .output()
        .expect("spawn flatten");
    assert!(
        output.status.success(),
        "flatten failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let flat = String::from_utf8(output.stdout).unwrap();
    // Sanity: c_core is NOT vendored (aggressive mode pushed it out)
    assert!(
        !flat.contains("mod c_core {"),
        "c_core should NOT be vendored; got:\n{flat}"
    );

    let downstream = tempfile::tempdir().unwrap();
    fs::create_dir_all(downstream.path().join("src")).unwrap();
    let c_path = src_workspace.path().join("c");
    let c_core_path = src_workspace.path().join("c_core");
    fs::write(
        downstream.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"downstream\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             c = {{ path = \"{}\" }}\n\
             c_core = {{ path = \"{}\" }}\n",
            c_path.display(),
            c_core_path.display()
        ),
    )
    .unwrap();
    fs::write(downstream.path().join("src/main.rs"), &flat).unwrap();

    let cargo_build = std::process::Command::new("cargo")
        .arg("build")
        .current_dir(downstream.path())
        .output()
        .expect("spawn cargo build");
    if !cargo_build.status.success() {
        let stderr = String::from_utf8_lossy(&cargo_build.stderr);
        panic!("cargo build failed:\n{stderr}\n--- src/main.rs ---\n{flat}");
    }
}

#[test]
fn external_deep_banner_lists_required_section() {
    // Verify the banner emits the "Required by vendored deps" section
    // when auto-promotion happens. Aggressive mode is required to
    // trigger the auto-promotion in this fixture (refined mode keeps
    // c_core vendored — see external_deep_keeps_dual_path_dep_vendored).
    let dir = make_diamond_with_shared_leaf();
    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(dir.path().join("user"))
        .args([
            "--vendor",
            "--external",
            "c",
            "--external-deep",
            "--external-deep-aggressive",
            "--stdout",
        ])
        .output()
        .expect("spawn flatten");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let s = String::from_utf8(output.stdout).unwrap();
    assert!(
        s.contains("Required by vendored deps"),
        "banner missing Required section; got:\n{s}"
    );
    assert!(s.contains("c_core"), "Required section should name c_core");
    assert!(
        s.contains("used by vendored: a"),
        "Required entry should cite vendored `a`"
    );
}

#[test]
fn external_deep_keeps_dual_path_dep_vendored() {
    // Default `--external-deep` (refined mode) keeps c_core vendored
    // even when c is external, because the user reaches c_core
    // directly via vendored A. The aggressive mode is opt-in via
    // `--external-deep-aggressive` for users who actually want
    // every cut transitive pushed to their Cargo.toml.
    let dir = make_diamond_with_shared_leaf();
    let opts = VendorOptions {
        external: ["c".to_string()].into_iter().collect(),
        external_deep: true,
        ..VendorOptions::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts).unwrap();

    let mut vendored: Vec<&str> = pkg.vendored.iter().map(|d| d.name.as_str()).collect();
    vendored.sort();
    assert_eq!(
        vendored,
        vec!["a", "b", "c_core"],
        "refined mode keeps c_core vendored (reachable from A directly)"
    );
    assert!(
        !pkg.external.iter().any(|d| d.name == "c_core"),
        "c_core should not be in external set under refined mode"
    );
}

// ---------------------------------------------------------------------------
// Real-world vendoring tests against regex, clap, and rand. These exercise
// the V1 use-crate-D injection (sibling-vendored-mod path resolution),
// V2 cfg evaluation, V3 $crate rewriting, and the V3 macro_export
// handling — all in one go on real dep graphs.
//
// They use crates.io registry versions (already in cargo's on-disk cache
// for our dev envs) rather than the cloned test-crates/* repos, because
// the cloned repos are dev-versions that have unstable, churning dep
// trees (rand main pulls in wit-bindgen/prettyplease etc.). The clones
// remain in test-crates/ for manual exploration.
//
// Each test silently skips if cargo isn't on PATH or if cargo metadata
// can't resolve the deps (e.g. they aren't in the registry cache).
// ---------------------------------------------------------------------------

fn cargo_available() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Synthesise a downstream user crate at a tempdir with the given
/// `[dependencies]` section and a `src/main.rs` from `main_body`.
fn synth_user_with_deps(deps_section: &str, main_body: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"user_under_test\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
             [dependencies]\n{deps_section}"
        ),
    )
    .unwrap();
    fs::write(dir.path().join("src/main.rs"), main_body).unwrap();
    dir
}

/// Run flatten via the binary harness and capture stdout. Returns
/// `None` if the run fails (caller can decide whether that's a test
/// failure or just a "skip this scenario" signal).
fn run_flatten_capture(user_dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_flatten"))
        .arg(user_dir)
        .args(args)
        .output()
        .expect("spawn flatten")
}

/// Compile `flat_src` as the `src/main.rs` of a fresh downstream crate
/// with the given `[dependencies]` section, via `cargo build`. Returns
/// Ok on success, the stderr on failure.
fn cargo_build_downstream(flat_src: &str, deps_section: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"downstream\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n{deps_section}"
        ),
    )
    .unwrap();
    fs::write(dir.path().join("src/main.rs"), flat_src).unwrap();
    let out = std::process::Command::new("cargo")
        .arg("build")
        .current_dir(dir.path())
        .output()
        .expect("spawn cargo build");
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

#[test]
fn real_vendor_nalgebra_with_expand_deep_compiles() {
    // Exercises the chain-walk + tainted-macro_rules detection: simba's
    // `complex_trait_methods!` is a macro_rules whose body invokes
    // `paste::item!{...}`. Without tainted-detection the trait body and
    // blanket impl would never get the simd_* methods, leaving 1300+
    // method-not-found errors. With it: 0 errors, zero externals.
    if !cargo_available() || expander_binary().is_none() || !rustc_available() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "nalgebra = \"0.34\"\n",
        "use nalgebra::Vector3;\n\
         fn main() {\n    \
             let v = Vector3::new(1.0_f32, 2.0, 3.0);\n    \
             println!(\"{}\", v.norm());\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &["--vendor", "--expand", "--expand-deep", "--stdout", "--no-banner"],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat nalgebra output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_glam_with_expand_deep_compiles() {
    if !cargo_available() || expander_binary().is_none() || !rustc_available() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "glam = \"0.30\"\n",
        "use glam::Vec3;\n\
         fn main() {\n    \
             let v = Vec3::new(1.0, 2.0, 3.0);\n    \
             println!(\"{}\", v.length());\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &["--vendor", "--expand", "--expand-deep", "--stdout", "--no-banner"],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat glam output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_rayon_with_expand_deep_compiles() {
    // rayon's `into_par_iter` plus the work-stealing scheduler
    // exercises `crossbeam-utils`/`crossbeam-deque`/`crossbeam-epoch`
    // sibling vendoring. Pre-holistic-build-script-policy this also
    // required `--external crossbeam-utils --external rayon-core`
    // because both have build scripts (and rayon-core has a fake
    // `links = "rayon-core"`). Under the new policy crossbeam-utils'
    // build script emits no link directives → vendorable; rayon-core's
    // `links` is uniqueness-only → vendorable. So the test runs with
    // no required externals.
    if !cargo_available() || expander_binary().is_none() || !rustc_available() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "rayon = \"1\"\n",
        "use rayon::prelude::*;\n\
         fn main() {\n    \
             let s: i64 = (0..100i64).into_par_iter().sum();\n    \
             println!(\"sum = {s}\");\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &["--vendor", "--expand", "--expand-deep", "--stdout", "--no-banner"],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat rayon output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_regex_with_expand_deep_compiles() {
    if !cargo_available() || expander_binary().is_none() || !rustc_available() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "regex = \"1\"\n",
        "use regex::Regex;\n\
         fn main() {\n    \
             let re = Regex::new(r\"\\b(\\w+) (\\w+)\\b\").unwrap();\n    \
             let caps = re.captures(\"hello world\").unwrap();\n    \
             println!(\"{} {}\", &caps[1], &caps[2]);\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &["--vendor", "--expand", "--expand-deep", "--stdout", "--no-banner"],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat regex output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_rapier2d_with_expand_deep_compiles() {
    // Full rapier2d → parry2d → nalgebra → simba dep tree flattened
    // into one .rs file with --expand-deep. Exercises:
    //   - Attr macro `#[profiling::function]` inlining on submodule items
    //   - Derive macros `Zero`/`Unsigned`/`FromPrimitive` from num_derive
    //   - Tainted-macro_rules expansion: simba's `complex_trait_methods!`
    //     calling `paste::item!{}` inside a macro_rules body
    //   - Build-script OUT_DIR capture for thiserror's `private.rs`
    //   - Helper-attr stripping for `#[error(...)]` on submodule enums
    //   - Re-export scrubbing for `pub use crate::nalgebra::vector;`
    //     where `vector` is a stripped nalgebra-macros proc-macro
    //   - `extern crate num_traits as _num_traits;` rewriting for
    //     num_derive's emitted const-block scope
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let user = synth_user_with_deps(
        "rapier2d = \"0.27\"\n",
        "use rapier2d::prelude::*;\n\
         fn main() {\n    \
             let bodies = RigidBodySet::new();\n    \
             println!(\"rapier2d ok: {} bodies\", bodies.len());\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on rapier2d failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    assert!(
        !flat.contains("#[profiling::function]")
            && !flat.contains("#[crate::profiling::function]"),
        "expected profiling::function Attr macros to be inlined"
    );
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat rapier2d output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_bytemuck_with_expand_deep_compiles() {
    // bytemuck's #[derive(Pod, Zeroable)] expansion includes a runtime
    // padding-check that calls `panic!()`. The `panic!()` macro_rules
    // expands to `::std::rt::begin_panic("…")` (gated behind
    // `#![feature(libstd_sys_internals)]`). The wrapper's
    // rewrite_unstable_panic_calls post-pass converts that back to
    // `panic!("…")` so the flat output compiles on stable rustc.
    if !cargo_available() || expander_binary().is_none() || !rustc_available() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "bytemuck = { version = \"1\", features = [\"derive\"] }\n",
        "use bytemuck::{Pod, Zeroable};\n\
         #[repr(C)]\n\
         #[derive(Copy, Clone, Pod, Zeroable)]\n\
         struct V3 { x: f32, y: f32, z: f32 }\n\
         fn main() {\n    \
             let v = V3 { x: 1.0, y: 2.0, z: 3.0 };\n    \
             let bytes: &[u8] = bytemuck::bytes_of(&v);\n    \
             println!(\"bytemuck ok: {} bytes\", bytes.len());\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &["--vendor", "--expand", "--expand-deep", "--stdout", "--no-banner"],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on bytemuck failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    assert!(
        !flat.contains("::std::rt::begin_panic"),
        "expected ::std::rt::begin_panic to be rewritten to panic!()"
    );
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat bytemuck output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_tokio_with_expand_deep_strips_attr_macro() {
    // tokio's `#[tokio::main]` Attr macro transforms `async fn main()`
    // into a sync `fn main()` that builds a runtime and `block_on`s
    // the body. Pre-fix the wrapper would fragment the transformed
    // function into individual expression snippets at the host span
    // — the `fn main` keyword/signature was lost. Fixed via
    // detect_attr_macro_transformation in expand/src/main.rs.
    //
    // Doesn't compile end-to-end yet: tokio's heavy use of custom
    // cfg-wrapper macros (cfg_io_driver! etc.) wraps `mod NAME;`
    // declarations our scanner can't see, leaving file-not-found
    // errors at downstream cargo-build time. Tracked separately in
    // ROADMAP. This test verifies the Attr-fragmentation fix
    // specifically: the flat output contains a recognisable
    // `fn main() {` line where #[tokio::main] used to be.
    if !cargo_available() || expander_binary().is_none() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "tokio = { version = \"1\", features = [\"full\"] }\n",
        "use tokio::time::{sleep, Duration};\n\
         #[tokio::main]\n\
         async fn main() {\n    \
             println!(\"starting\");\n    \
             sleep(Duration::from_millis(1)).await;\n    \
             println!(\"done\");\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--external-preset", "infra",
            "--external-deep",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on tokio failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    // Attr-fragmentation: pre-fix the user's `fn main()` was
    // entirely missing from the output (replaced by fragmented
    // expression text). Post-fix, the transformed `fn main()`
    // should be present with the Builder-block_on shape.
    assert!(
        flat.contains("fn main()"),
        "expected transformed fn main() to be in the output (Attr-fragmentation regression?); first 40 lines:\n{}",
        flat.lines().take(40).collect::<Vec<_>>().join("\n")
    );
    assert!(
        flat.contains("Builder"),
        "expected tokio::main expansion to reference Builder"
    );
    // mod-in-cfg-macro inlining: tokio uses cfg_io_driver! and friends
    // wrapping `mod NAME;` declarations our scanner can't see at
    // syn-AST level. The inline_mods_inside_macros pass should
    // resolve the files and inline them with `// === inlined-from-
    // macro: ... ===` markers.
    assert!(
        flat.contains("// === inlined-from-macro:"),
        "expected mod-in-macro inlining to fire (cfg_io_driver! / cfg_os_poll! / etc.)"
    );
}

#[test]
fn real_vendor_axum_with_expand_deep_vendors_clean() {
    // axum uses #[async_trait] from the async_trait proc-macro on
    // its FromRequestParts/FromRequest impls. Pre-fix the wrapper's
    // detect_attr_macro_transformation didn't recurse into assoc
    // items, so the if-let __ret type-coercion code emitted INSIDE
    // each async fn body was treated as standalone Attr expansion
    // and stuffed into bang_groups at the impl block's host span,
    // fragmenting the impl. Fixed by recursing into assoc items in
    // the Scanner.
    //
    // Doesn't compile end-to-end yet — same `mod NAME;` wrapped in
    // cfg-macros issue that blocks tokio. Vendoring + syn parse of
    // the dump succeed cleanly, which is the new bar this test
    // locks in.
    if !cargo_available() || expander_binary().is_none() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "axum = \"0.7\"\ntokio = { version = \"1\", features = [\"full\"] }\n",
        "use axum::{routing::get, Router};\n\
         #[tokio::main]\n\
         async fn main() {\n    \
             let _app: Router = Router::new().route(\"/\", get(|| async { \"hi\" }));\n    \
             println!(\"axum compiles\");\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--external-preset", "infra",
            "--external-deep",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on axum failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    // The `if let __ret = None::<...>` async_trait fragment should
    // NOT appear bare at item position; it should be wrapped in a
    // proper `impl ... { fn ... { ... } }` that survives because we
    // emit the whole transformed impl block at once.
    assert!(
        flat.contains("pub mod axum"),
        "expected axum to be vendored"
    );
    // The Extension struct should still be present (pre-fix the
    // surrounding impl block fragmentation broke surrounding code).
    assert!(
        flat.contains("pub struct Extension"),
        "expected Extension struct preserved (axum_core fragmentation regression?)"
    );
}

#[test]
fn real_vendor_ratatui_with_expand_deep_vendors_clean() {
    // ratatui pulls in a deep stack including cassowary (edition 2015)
    // that uses `try!()`. Pre-fix, vendoring failed with syn parse
    // errors because `try` is reserved in edition 2018+ and
    // cassowary's `try!()` invocations couldn't be parsed when
    // wrapped in the outer 2021 user crate. Fixed via
    // vendor::rewrite_try_macro for edition-2015 deps.
    //
    // Doesn't compile end-to-end yet: cfg-skipped `mod backend` in
    // rustix cascades into many missing-symbol errors, and crossterm
    // depends on crossterm_winapi which isn't currently vendorable.
    // This test verifies the vendor pipeline runs cleanly to
    // completion (proves cassowary's try!() rewrite landed without
    // breaking the chain).
    if !cargo_available() || expander_binary().is_none() {
        eprintln!("skipping: prerequisites missing");
        return;
    }
    let user = synth_user_with_deps(
        "ratatui = \"0.28\"\n",
        "use ratatui::widgets::{Block, Borders};\n\
         fn main() {\n    \
             let _b = Block::default().borders(Borders::ALL).title(\"hi\");\n    \
             println!(\"ratatui compiles\");\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--external-preset", "infra",
            "--external-deep",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on ratatui failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    // try!() was rewritten — no surviving try!( in cassowary's source.
    assert!(
        !flat.contains("try!("),
        "expected try!() to be rewritten to (...)?"
    );
    // cassowary IS in the vendored output (it would be missing if
    // try!() rewrite had failed and the vendor refused).
    assert!(
        flat.contains("pub mod cassowary"),
        "expected cassowary to be vendored"
    );
}

#[test]
fn real_vendor_rand_with_expand_deep_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    // rand with --expand-deep: drops zerocopy_derive (proc-macro) and the
    // proc-macro helper crates (proc-macro2, quote). libc and zerocopy
    // remain external because both have build scripts.
    let user = synth_user_with_deps(
        "rand = \"0.8\"\n",
        "use rand::Rng;\n\
         fn main() {\n    \
             let mut rng = rand::thread_rng();\n    \
             let n: u32 = rng.gen();\n    \
             println!(\"{}\", n % 100);\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--external", "libc",
            "--external", "zerocopy",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on rand failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    if let Err(stderr) = cargo_build_downstream(
        &flat,
        "libc = \"0.2\"\nzerocopy = \"0.8\"\n",
    ) {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat rand output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_serde_with_derive_and_expand_deep_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    // Pin to pre-serde_core split. Exercises:
    // - both Serialize and Deserialize derives stripped from one #[derive()]
    //   list (adjacent strips that both want to swallow the same comma)
    // - #[serde(rename_all = "...")] helper attr stripped
    // - serde_json's `#[cfg(fast_arithmetic = "64")]` evaluated against
    //   the build-script cfg the user crate's serde_json emitted
    // - serde itself stays external (build script forces it)
    let user = synth_user_with_deps(
        "serde = { version = \"=1.0.219\", features = [\"derive\"] }\n\
         serde_json = \"=1.0.140\"\n",
        "use serde::{Serialize, Deserialize};\n\
         #[derive(Debug, Serialize, Deserialize)]\n\
         #[serde(rename_all = \"kebab-case\")]\n\
         struct Greeting { user_name: String, message_text: String }\n\
         fn main() {\n    \
             let g = Greeting { user_name: \"world\".into(), message_text: \"hi\".into() };\n    \
             let j = serde_json::to_string(&g).unwrap();\n    \
             println!(\"{j}\");\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--external", "serde",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: --expand-deep on serde failed: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    if let Err(stderr) = cargo_build_downstream(
        &flat,
        "serde = { version = \"=1.0.219\", features = [\"derive\"] }\n",
    ) {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!("flat serde+derive output failed to compile:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_clap_with_derive_and_expand_deep_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    // Exercises the full `--expand-deep` chain on a real proc-macro-using
    // crate: clap derive (clap_derive), helper attrs (`#[arg(...)]`),
    // absolute proc-macro path output (`::clap_builder::...`), windows-sys
    // include!() resolution, multi-file dep tree, vendoring with no
    // externals.
    let user = synth_user_with_deps(
        "clap = { version = \"4\", features = [\"derive\"] }\n",
        "use clap::Parser;\n\
         #[derive(Parser, Debug)]\n\
         struct Args { #[arg(short, long)] name: String }\n\
         fn main() {\n    \
             let a = Args::parse_from([\"app\", \"--name\", \"world\"]);\n    \
             println!(\"hello, {}\", a.name);\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--expand",
            "--expand-deep",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!(
            "skipping: --expand-deep on clap failed (likely registry not cached or expander missing): {stderr}"
        );
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();

    // No --extern flags should be required to compile the flat output.
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(40).collect::<Vec<_>>().join("\n");
        panic!(
            "flat clap+derive output failed to compile:\n{stderr}\n--- head ---\n{head}"
        );
    }
}

#[test]
fn real_vendor_clap_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    let user = synth_user_with_deps(
        "clap = { version = \"4\", default-features = false, features = [\"std\", \"help\", \"usage\"] }\n",
        "fn main() {\n    \
            let m = clap::Command::new(\"test\")\n        \
                .arg(clap::Arg::new(\"name\").long(\"name\"))\n        \
                .get_matches_from(vec![\"test\", \"--name\", \"world\"]);\n    \
            println!(\"name={:?}\", m.get_one::<String>(\"name\"));\n\
         }\n",
    );

    let out = run_flatten_capture(user.path(), &["--vendor", "--stdout", "--no-banner"]);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor clap (likely registry not cached): {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();

    // The use-crate-D fix is exercised here: clap depends on clap_builder,
    // and clap_builder is a sibling vendored mod. `mod clap` should open
    // with `use crate::clap_builder;`.
    assert!(
        flat.contains("use crate::clap_builder"),
        "expected `use crate::clap_builder` for vendored sibling; first 800 chars:\n{}",
        &flat[..flat.len().min(800)]
    );

    // Bottom-line check: this whole vendored output compiles.
    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected vendored clap output:\n{stderr}\n--- head of output ---\n{head}");
    }
}

#[test]
fn real_vendor_regex_compiles_with_externals() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    // Smoke test for user-elected externalization: aho-corasick and
    // regex-automata WOULD vendor cleanly, but the user explicitly cuts
    // them with `--external`. Verifies that --external on a vendorable
    // dep is respected (drops it + its orphans), and that the partial
    // vendoring (regex + regex-syntax + memchr) still produces a
    // compilable flat output when downstream Cargo.toml lists the
    // user-chosen externals. Also exercises the sibling-mod use-crate
    // fix (regex → `use crate::regex_syntax`).
    let user = synth_user_with_deps(
        "regex = \"1\"\n",
        "fn main() {\n    \
            let re = regex::Regex::new(r\"^\\d+$\").unwrap();\n    \
            assert!(re.is_match(\"12345\"));\n    \
            assert!(!re.is_match(\"abc\"));\n    \
            println!(\"ok\");\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--external", "aho-corasick",
            "--external", "regex-automata",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor regex: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();

    // regex (vendored) uses regex-syntax (sibling vendored mod) → should
    // get a `use crate::regex_syntax;` at the top of `mod regex`.
    assert!(
        flat.contains("use crate::regex_syntax"),
        "expected `use crate::regex_syntax`; first 1500 chars:\n{}",
        &flat[..flat.len().min(1500)]
    );

    // Build with both externals listed in downstream Cargo.toml.
    let result = cargo_build_downstream(
        &flat,
        "aho-corasick = \"1\"\nregex-automata = \"0.4\"\n",
    );
    if let Err(stderr) = result {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected vendored regex output:\n{stderr}\n--- head of output ---\n{head}");
    }
}

#[test]
fn real_vendor_regex_external_deep_promotes_required() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    // The diamond test on real crates: --external regex-automata
    // --external-deep cuts regex-automata + its transitives (regex-syntax,
    // memchr). regex (still vendored) directly uses regex-syntax + memchr,
    // so they get auto-promoted to Required.
    let user = synth_user_with_deps(
        "regex = \"1\"\n",
        "fn main() {\n    \
            let re = regex::Regex::new(\"a+\").unwrap();\n    \
            assert!(re.is_match(\"aaa\"));\n    \
            println!(\"ok\");\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--external", "aho-corasick",
            "--external", "regex-automata",
            "--external-deep",
            "--external-deep-aggressive",
            "--stdout",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor regex with deep: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();

    // Aggressive mode pushes regex-syntax + memchr (transitives of
    // the externals) to the user's Cargo.toml even though regex still
    // references them directly. The banner reports them as Required.
    assert!(
        flat.contains("Required by vendored deps"),
        "expected Required section in banner; banner:\n{}",
        flat.lines().take(20).collect::<Vec<_>>().join("\n")
    );
    assert!(
        flat.contains("regex-syntax"),
        "expected regex-syntax in Required section"
    );
    assert!(
        flat.contains("memchr"),
        "expected memchr in Required section"
    );

    // Should compile when downstream lists all the externals (originally
    // user-named + auto-promoted).
    let result = cargo_build_downstream(
        &flat,
        "aho-corasick = \"1\"\nregex-automata = \"0.4\"\nregex-syntax = \"0.8\"\nmemchr = \"2\"\n",
    );
    if let Err(stderr) = result {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected --external-deep regex output:\n{stderr}\n--- head of output ---\n{head}");
    }
}

#[test]
fn real_vendor_rand_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    // rand 0.8 has the diamond dep `rand_core` (used by both `rand` and
    // `rand_chacha`) plus heavy use of `cfg_if!` (in getrandom + ppv-lite86)
    // and `#[macro_export]` macros from cfg-gated mods. Exercises:
    //  - cfg_if expansion (getrandom's `mod imp;` decls inside cfg_if!)
    //  - cfg-aware macro_export lifting (ppv-lite86's `dispatch` macro
    //    in cfg-gated `mod x86_64`)
    //  - cfg evaluation inside macro bodies (rand's `Float` trait gated
    //    by `#[cfg(not(feature = "std"))]` plus a macro that references it)
    let user = synth_user_with_deps(
        "rand = \"0.8\"\n",
        "use rand::Rng;\n\
         fn main() {\n    \
             let mut rng = rand::thread_rng();\n    \
             let n: u32 = rng.gen();\n    \
             println!(\"{}\", n % 100);\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            // libc, proc-macro2, quote, zerocopy* are unvendorable
            // (build scripts / proc-macros). Externalise them.
            "--external", "libc",
            "--external", "proc-macro2",
            "--external", "quote",
            "--external", "zerocopy",
            "--external", "zerocopy-derive",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor rand: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();

    // Sibling-mod cross-references: rand and rand_chacha both refer
    // back into rand_core, which gets rewritten as `crate::rand_core`.
    assert!(
        flat.contains("crate::rand_core"),
        "expected `crate::rand_core` references for sibling-vendored rand_core"
    );

    let result = cargo_build_downstream(
        &flat,
        "libc = \"0.2\"\nzerocopy = \"0.8\"\n",
    );
    if let Err(stderr) = result {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected vendored rand output:\n{stderr}\n--- head of output ---\n{head}");
    }
}

#[test]
fn real_vendor_rand_preserves_target_cfgs() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    // rand uses #[cfg(target_arch = "x86_64")], #[cfg(target_pointer_width)]
    // for SIMD/perf paths. V2's evaluator should leave these as Unknown
    // (preserve the attribute), since target_* isn't a feature predicate.
    let user = synth_user_with_deps(
        "rand = \"0.8\"\n",
        "fn main() { let _: u32 = rand::random(); }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--external", "libc",
            "--external", "proc-macro2",
            "--external", "quote",
            "--external", "zerocopy",
            "--external", "zerocopy-derive",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        eprintln!("skipping: flatten couldn't vendor rand");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();

    // Heuristic: at least one target-flavoured cfg should survive in the
    // vendored output. `target_arch`, `target_pointer_width`, or
    // `target_feature` are all common in rand.
    let has_target_cfg = flat.contains("cfg(target_arch")
        || flat.contains("cfg(target_pointer_width")
        || flat.contains("cfg(target_feature")
        || flat.contains("cfg(any(target_");
    assert!(
        has_target_cfg,
        "expected at least one cfg(target_*) attr to survive V2's evaluator; \
         first 5000 chars of output:\n{}",
        &flat[..flat.len().min(5000)]
    );
}

// ---------------------------------------------------------------------------
// Physics / math ecosystem: glam, nalgebra, rapier2d.
//
// These exercise generics-heavy and macro-heavy code paths very
// different from the regex/clap/rand surface. nalgebra in particular
// stress-tests the `extern crate FOO as BAR;` alias rewrite, the
// `#[macro_use] extern crate approx;` macro propagation, and the
// stdlib-alias removal (deprecated `pub use base as core;`).
// ---------------------------------------------------------------------------

#[test]
fn real_vendor_glam_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    // glam is a SIMD-heavy math library with extensive
    // `#[cfg(target_arch = ...)]` / `#[cfg(target_feature = ...)]`
    // gating but no proc-macro deps and no external runtime crates —
    // should vendor cleanly with zero --external flags.
    let user = synth_user_with_deps(
        "glam = \"0.30\"\n",
        "use glam::Vec3;\n\
         fn main() {\n    \
             let a = Vec3::new(1.0, 2.0, 3.0);\n    \
             let b = Vec3::new(4.0, 5.0, 6.0);\n    \
             println!(\"{:?}\", a + b);\n\
         }\n",
    );

    let out = run_flatten_capture(user.path(), &["--vendor", "--stdout", "--no-banner"]);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor glam: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    assert!(flat.contains("pub mod glam"), "expected `pub mod glam` wrapper");

    if let Err(stderr) = cargo_build_downstream(&flat, "") {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected vendored glam output:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_nalgebra_compiles() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    // nalgebra exercises:
    //  - `extern crate num_traits as num;` alias rewrite (use num::Zero
    //    references substituted to num_traits::Zero across all submods)
    //  - `#[macro_use] extern crate approx;` propagation (assert_relative_eq!
    //    callable from any submod via the per-file `use approx::*;` injection)
    //  - deprecated `pub use base as core;` removal so submods don't
    //    misroute `use core::cmp::Ordering` via the alias
    let user = synth_user_with_deps(
        "nalgebra = \"0.34\"\n",
        "use nalgebra::Vector3;\n\
         fn main() {\n    \
             let a = Vector3::new(1.0_f32, 2.0, 3.0);\n    \
             let b = Vector3::new(4.0_f32, 5.0, 6.0);\n    \
             println!(\"{}\", (a + b).norm());\n\
         }\n",
    );

    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            "--external", "matrixmultiply",
            "--external", "nalgebra-macros",
            "--external", "num-traits",
            "--external", "paste",
            "--external", "proc-macro2",
            "--external", "quote",
            "--external", "num-bigint",
            "--external", "approx",
            "--external", "typenum",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor nalgebra: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    assert!(
        flat.contains("pub mod nalgebra"),
        "expected `pub mod nalgebra` wrapper"
    );
    // Per-file injection: the `use approx::*;` should appear MANY times
    // (one per nalgebra submodule that contains code).
    let approx_use_count = flat.matches("use approx::*;").count();
    assert!(
        approx_use_count > 5,
        "expected `use approx::*;` injected at top of every nalgebra file (got {approx_use_count})"
    );

    if let Err(stderr) = cargo_build_downstream(
        &flat,
        "matrixmultiply = \"0.3\"\n\
         nalgebra-macros = \"0.3\"\n\
         num-traits = \"0.2\"\n\
         paste = \"1\"\n\
         num-bigint = \"0.4\"\n\
         approx = \"0.5\"\n\
         typenum = \"1\"\n",
    ) {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected vendored nalgebra output:\n{stderr}\n--- head ---\n{head}");
    }
}

#[test]
fn real_vendor_rapier2d_compiles() {
    // The full rapier2d → parry2d → nalgebra chain. rapier2d is the
    // most complex real-world test we have: ~44 transitive deps,
    // diamond dep on nalgebra, `pub extern crate FOO as parry;` with
    // four cfg-gated alternatives picked by feature, `local_inner_macros`
    // recursive macro expansion (downcast-rs), `extern crate FOO;`
    // 2015-style, partial cfg eval (`feature = "alloc"` plus `not(no_global_oom_handling)`),
    // and stdlib-alias re-exports (`pub extern crate alloc as __alloc`).
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH");
        return;
    }
    let user = synth_user_with_deps(
        "rapier2d = \"0.27\"\n",
        "use rapier2d::prelude::*;\n\
         fn main() {\n    \
             let mut bodies = RigidBodySet::new();\n    \
             let mut colliders = ColliderSet::new();\n    \
             let body = RigidBodyBuilder::dynamic()\n        \
                 .translation(vector![0.0, 10.0])\n        \
                 .build();\n    \
             let handle = bodies.insert(body);\n    \
             colliders.insert_with_parent(\n        \
                 ColliderBuilder::cuboid(1.0, 1.0).build(),\n        \
                 handle,\n        \
                 &mut bodies,\n    \
             );\n    \
             println!(\"rapier2d ok: {} bodies\", bodies.len());\n\
         }\n",
    );
    let out = run_flatten_capture(
        user.path(),
        &[
            "--vendor",
            // The remaining externals are all proc-macro related —
            // either the proc-macro crate itself or a runtime crate
            // a proc-macro derive expands paths into. Pre-expansion
            // (V5) would unblock these. Categories A in EXTERNAL.md.
            "--external", "nalgebra-macros",
            "--external", "num-derive",
            "--external", "num-traits",
            "--external", "paste",
            "--external", "proc-macro2",
            "--external", "profiling-procmacros",
            "--external", "quote",
            "--external", "thiserror",
            "--external", "thiserror-impl",
            "--stdout",
            "--no-banner",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("skipping: flatten couldn't vendor rapier2d: {stderr}");
        return;
    }
    let flat = String::from_utf8(out.stdout).unwrap();
    assert!(flat.contains("pub mod rapier2d"), "expected `pub mod rapier2d`");
    assert!(flat.contains("pub mod parry2d"), "expected `pub mod parry2d`");
    assert!(flat.contains("pub mod nalgebra"), "expected `pub mod nalgebra`");

    let result = cargo_build_downstream(
        &flat,
        "nalgebra-macros = \"0.2\"\n\
         num-derive = \"0.4\"\n\
         num-traits = \"0.2\"\n\
         paste = \"1\"\n\
         thiserror = \"2\"\n\
         profiling-procmacros = \"1\"\n",
    );
    if let Err(stderr) = result {
        let head: String = flat.lines().take(80).collect::<Vec<_>>().join("\n");
        panic!("cargo build rejected vendored rapier2d output:\n{stderr}\n--- head ---\n{head}");
    }
}

// ---------------------------------------------------------------------------
// --minify end-to-end: vendor a real dep, minify, verify size shrinks
// and the result still compiles.
// ---------------------------------------------------------------------------

#[test]
fn minify_via_cli_shrinks_vendored_output_and_still_compiles() {
    if !rustc_available() {
        eprintln!("skipping: rustc not on PATH");
        return;
    }
    let fixture = project_dir().join("test-crates/script-with-deps");
    if !fixture.join("Cargo.toml").is_file() {
        eprintln!("skipping: test-crates/script-with-deps not present");
        return;
    }

    let bin = env!("CARGO_BIN_EXE_flatten");

    // Plain vendored output
    let plain = std::process::Command::new(bin)
        .arg(&fixture)
        .args(["--vendor", "--stdout", "--no-banner"])
        .output()
        .expect("spawn flatten plain");
    assert!(plain.status.success(), "plain failed: {}", String::from_utf8_lossy(&plain.stderr));

    // Minified vendored output
    let mini = std::process::Command::new(bin)
        .arg(&fixture)
        .args(["--vendor", "--stdout", "--no-banner", "--minify"])
        .output()
        .expect("spawn flatten minified");
    assert!(mini.status.success(), "minified failed: {}", String::from_utf8_lossy(&mini.stderr));

    let plain_len = plain.stdout.len();
    let mini_len = mini.stdout.len();
    assert!(
        mini_len < plain_len,
        "minified ({mini_len} bytes) should be smaller than plain ({plain_len})"
    );
    // Reasonable expectation: at least 15% shrinkage on a comment-heavy real dep.
    let ratio = mini_len as f64 / plain_len as f64;
    assert!(
        ratio < 0.85,
        "expected meaningful shrinkage, got ratio {ratio:.2} ({mini_len} / {plain_len})"
    );

    // Minified output should still compile and run correctly.
    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("mini.rs");
    fs::write(&flat, &mini.stdout).unwrap();
    let res = std::process::Command::new("rustc")
        .args(["--edition=2021", "--crate-type=bin", "-A", "warnings", "-o"])
        .arg(tmp.path().join("bin"))
        .arg(&flat)
        .output()
        .unwrap();
    if !res.status.success() {
        let stderr = String::from_utf8_lossy(&res.stderr);
        let head = String::from_utf8_lossy(&mini.stdout[..mini.stdout.len().min(2000)]);
        panic!("rustc rejected minified output:\n{stderr}\n--- output head ---\n{head}");
    }

    let run = std::process::Command::new(tmp.path().join("bin"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hi, 42"),
        "expected `hi, 42`, got: {stdout}"
    );
}

#[test]
fn minify_strips_comments_in_user_code_too() {
    let dir = make_user_with_path_dep(
        "pure_dep",
        "pub fn x() {}\n",
        // User code has comments that minify should strip
        "// this is the entry point\n/// docs for main\nfn main() {\n    // call into the dep\n    pure_dep::x();\n}\n",
    );
    let pkg = vendor::vendor_package(
        dir.path().join("user"),
        &TargetSelector::Auto,
        &flatten::vendor::VendorOptions::default(),
    )
    .unwrap();

    let mut assembled = String::new();
    use std::fmt::Write as _;
    write!(&mut assembled, "{}", pkg.user_source).unwrap();
    for d in &pkg.vendored {
        writeln!(&mut assembled).unwrap();
        writeln!(&mut assembled, "mod {} {{", d.name).unwrap();
        write!(&mut assembled, "{}", d.source).unwrap();
        writeln!(&mut assembled, "\n}}").unwrap();
    }

    let minified = flatten::minify::minify(&assembled);
    assert!(!minified.contains("entry point"));
    assert!(!minified.contains("docs for main"));
    assert!(!minified.contains("call into the dep"));
    assert!(minified.contains("pure_dep::x()"));
    assert!(minified.contains("fn main()"));
}

// ---------------------------------------------------------------------------
// Self-flatten: flatten flattens its own lib successfully.
// ---------------------------------------------------------------------------

#[test]
fn tree_summary_reflects_inlined_structure() {
    let dir = make_crate(&[
        ("src/lib.rs", "mod a;\nmod b;\n"),
        ("src/a.rs", "pub fn aa() {}\n"),
        ("src/b.rs", "pub mod c;\n"),
        ("src/b/c.rs", "pub fn cc() {}\n"),
    ]);
    let pkg = parse_package(dir.path()).unwrap();
    let tree = pkg.source.tree(pkg.entry_path.display().to_string());

    assert_eq!(tree.display_path, "src/lib.rs");
    assert_eq!(tree.children.len(), 2);
    assert_eq!(tree.children[0].display_path, "src/a.rs");
    assert_eq!(tree.children[0].children.len(), 0);
    assert_eq!(tree.children[1].display_path, "src/b.rs");
    assert_eq!(tree.children[1].children.len(), 1);
    assert_eq!(tree.children[1].children[0].display_path, "src/b/c.rs");

    assert_eq!(tree.total_files(), 4);
    assert!(tree.total_bytes() > 0);
}

// ---------------------------------------------------------------------------
// Defense in depth: recursion limit + rustfmt pipe
// ---------------------------------------------------------------------------

#[test]
fn errors_on_excessive_module_depth() {
    // Build a chain `lib.rs → src/a0.rs → src/a0/a1.rs → … → src/a0/a1/.../aN.rs`
    // deeper than MAX_DEPTH (128). The recursion guard should reject it.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub mod a0;\n").unwrap();

    let depth = 130;
    for i in 0..depth {
        let mut p = dir.path().join("src");
        for j in 0..i {
            p.push(format!("a{j}"));
        }
        fs::create_dir_all(&p).unwrap();
        let p_rs = p.join(format!("a{i}.rs"));
        let content = if i + 1 < depth {
            format!("pub mod a{};\n", i + 1)
        } else {
            "// leaf\n".to_string()
        };
        fs::write(&p_rs, content).unwrap();
    }

    let err = parse_package(dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("depth"),
        "expected depth-exceeded error, got: {msg}"
    );
}

#[test]
fn moderate_depth_below_limit_succeeds() {
    // Sanity check: 10 levels deep should sail through without issue.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub mod a0;\n").unwrap();
    let depth = 10;
    for i in 0..depth {
        let mut p = dir.path().join("src");
        for j in 0..i {
            p.push(format!("a{j}"));
        }
        fs::create_dir_all(&p).unwrap();
        let p_rs = p.join(format!("a{i}.rs"));
        let content = if i + 1 < depth {
            format!("pub mod a{};\n", i + 1)
        } else {
            "pub fn deep() {}\n".to_string()
        };
        fs::write(&p_rs, content).unwrap();
    }
    let pkg = parse_package(dir.path()).expect("10 levels should be fine");
    assert!(pkg.source.to_string().contains("pub fn deep()"));
}

fn rustfmt_available() -> bool {
    std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn fmt_flag_runs_rustfmt_and_output_is_fmt_stable() {
    if !rustfmt_available() {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }

    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("fmt_test")),
        ("src/lib.rs", "mod foo;\n\npub fn x() {}\n"),
        ("src/foo.rs", "pub fn   bar()   {}\n"),
    ]);

    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(dir.path())
        .args(["--lib", "--fmt", "--stdout", "--no-banner"])
        .output()
        .expect("spawn flatten");
    assert!(
        output.status.success(),
        "flatten failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The formatted output should pass `rustfmt --check` — i.e. be
    // idempotent under another rustfmt pass.
    let mut check = std::process::Command::new("rustfmt")
        .args(["--check", "--edition=2024"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn rustfmt --check");
    use std::io::Write;
    check
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&output.stdout)
        .unwrap();
    let status = check.wait().unwrap();
    assert!(
        status.success(),
        "rustfmt --check rejected the formatted output"
    );
}

#[test]
fn fmt_does_not_deadlock_on_large_input() {
    // Regression test for REVIEW A4: write_all-then-wait_with_output
    // could deadlock on multi-MB input because rustfmt's stdout pipe
    // buffer (~64KB on Linux/macOS) fills before stdin is consumed.
    // We synthesise ~2 MB of valid Rust (5000 small functions) and
    // assert that --fmt completes within 60s. Pre-fix it would hang
    // indefinitely.
    if !rustfmt_available() {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }
    let mut src = String::with_capacity(2 * 1024 * 1024);
    src.push_str("// Auto-generated bulk test source\n");
    for i in 0..5000 {
        // Each fn is ~400 bytes — roughly 2MB total. Comments
        // intentionally verbose so rustfmt has plenty to write.
        src.push_str(&format!(
            "/// Function number {i}. Used to generate enough\n\
             /// stdout pressure that the rustfmt pipe buffer fills\n\
             /// before stdin is fully consumed.\n\
             pub fn func_{i}(x: u32, y: u32, z: u32) -> u32 {{\n    \
                 let a = x.wrapping_add(y);\n    \
                 let b = a.wrapping_mul(z);\n    \
                 b.wrapping_sub(x)\n\
             }}\n\n",
        ));
    }

    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("big_fmt_test")),
        ("src/lib.rs", &src),
    ]);

    let bin = env!("CARGO_BIN_EXE_flatten");
    let mut child = std::process::Command::new(bin)
        .arg(dir.path())
        .args(["--lib", "--fmt", "--stdout", "--no-banner"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn flatten");

    // Drain stdout in a thread (the very deadlock pattern this test
    // exists to prevent) and bound the wait so a regression manifests
    // as a timeout rather than blocking the whole suite.
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);
        buf
    });

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    panic!(
                        "--fmt on 2MB input did not complete within 60s — \
                         likely deadlock regression"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => panic!("try_wait failed: {e}"),
        }
    };
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    assert!(
        status.success(),
        "flatten --fmt failed on large input"
    );
}

#[test]
fn fmt_without_rustfmt_errors_clearly() {
    if rustfmt_available() {
        eprintln!("skipping: rustfmt IS on PATH; this test only meaningful when it isn't");
        return;
    }

    let dir = make_crate(&[
        ("Cargo.toml", &minimal_manifest("fmt_test")),
        ("src/lib.rs", "pub fn x() {}\n"),
    ]);
    let bin = env!("CARGO_BIN_EXE_flatten");
    let output = std::process::Command::new(bin)
        .arg(dir.path())
        .args(["--lib", "--fmt", "--stdout"])
        .output()
        .expect("spawn flatten");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rustfmt") && stderr.contains("PATH"),
        "expected helpful rustfmt-missing error, got: {stderr}"
    );
}

#[test]
fn flattens_own_lib() {
    let pkg = parse_target(project_dir(), &TargetSelector::Lib).expect("parse own lib");
    assert_eq!(pkg.kind, PackageType::Lib);
    let out = pkg.source.to_string();

    // Each non-test source the lib depends on must have been inlined and
    // tagged with our separator marker.
    for sub in ["src/scanner.rs", "src/source_file.rs"] {
        assert!(
            out.contains(&format!("// === {sub} ===")),
            "missing inline marker for `{sub}` in self-flattened output"
        );
    }
    // Sanity: known identifiers from each module made it through.
    assert!(out.contains("scan_external_mods"), "scanner content missing");
    assert!(out.contains("ExternalModule"), "source_file content missing");
    assert!(out.contains("TargetSelector"), "lib top-level content missing");
}

// ---------------------------------------------------------------------------
// --expand: third-party proc-macro inlining via flatten_expand.
// These tests path-dep the `derive_hello` fixture in expand/tests/fixtures
// and require the expander binary at expand/target/debug/flatten_expand
// (skip otherwise — that binary needs nightly + rustc-dev to build).
// ---------------------------------------------------------------------------

fn expander_binary() -> Option<PathBuf> {
    let p = project_dir().join("expand/target/debug/flatten_expand");
    p.is_file().then_some(p)
}

fn derive_hello_fixture_path() -> PathBuf {
    project_dir().join("expand/tests/fixtures/derive_hello")
}

fn make_user_with_proc_macro_dep(user_main: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).unwrap();
    let dep_path = derive_hello_fixture_path();
    fs::write(
        dir.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
             [dependencies]\nderive_hello = {{ path = \"{}\" }}\n",
            dep_path.display()
        ),
    )
    .unwrap();
    fs::write(dir.path().join("src/main.rs"), user_main).unwrap();
    dir
}

#[test]
fn vendor_expand_strips_third_party_derive_and_appends_impl() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::Hello;\n\
         #[derive(Debug, Clone, Hello)]\n\
         struct G { x: i32 }\n\
         fn main() { let _ = G { x: 1 }; }\n",
    );
    let opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &opts).expect("vendor");
    let s = pkg.user_source.to_string();

    assert!(
        s.contains("#[derive(Debug, Clone)]"),
        "stdlib derives kept; got:\n{s}"
    );
    assert!(
        !s.contains("Hello"),
        "third-party derive name and use stripped; got:\n{s}"
    );
    assert!(
        s.contains("impl G"),
        "expanded impl appended; got:\n{s}"
    );
    assert!(
        s.contains("hello from G"),
        "expansion content present; got:\n{s}"
    );
}

#[test]
fn vendor_expand_strips_use_of_proc_macro_crate() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::Hello;\n\
         #[derive(Hello)]\n\
         struct G { y: i32 }\n\
         fn main() { let _ = G { y: 0 }; }\n",
    );
    let opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &opts).expect("vendor");
    let s = pkg.user_source.to_string();
    assert!(
        !s.contains("use derive_hello"),
        "`use derive_hello::...` should be stripped; got:\n{s}"
    );
}

#[test]
fn vendor_expand_succeeds_in_strict_mode_with_proc_macro_dep() {
    // Without --expand, strict mode would refuse because derive_hello is
    // a proc-macro (Unvendorable). With --expand, the proc-macro is auto
    // -externalized and strict mode is happy.
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::Hello;\n\
         #[derive(Hello)]\n\
         struct G { z: i32 }\n\
         fn main() {}\n",
    );

    // Sanity: without --expand, strict vendoring fails (or marks derive_hello
    // as Unvendorable).
    let plain_opts = VendorOptions::default();
    let plain_result =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &plain_opts);
    assert!(
        plain_result.is_err(),
        "without --expand, strict mode should refuse on proc-macro dep"
    );

    // With --expand, vendoring succeeds.
    let expand_opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg = vendor::vendor_package(dir.path(), &TargetSelector::Auto, &expand_opts)
        .expect("vendor with --expand should succeed");
    assert!(
        !pkg.vendored.iter().any(|v| v.name == "derive_hello"),
        "proc-macro crate must not be vendored"
    );
}

#[test]
fn vendor_expand_strips_helper_attrs_when_owning_derive_inlined() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::Hello;\n\
         #[derive(Debug, Hello)]\n\
         #[hello(version = \"1.0\")]\n\
         struct G { x: i32 }\n\
         fn main() { println!(\"{}\", G::hello()); }\n",
    );
    let opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &opts).expect("vendor");
    let s = pkg.user_source.to_string();
    assert!(
        !s.contains("#[hello"),
        "helper attr should be stripped; got:\n{s}"
    );
    let out_dir = tempfile::tempdir().unwrap();
    let out_rs = out_dir.path().join("flat.rs");
    fs::write(&out_rs, &s).unwrap();
    let out_bin = out_dir.path().join("flat_bin");
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(&out_rs)
        .arg("-o")
        .arg(&out_bin)
        .status()
        .expect("spawn rustc");
    assert!(status.success(), "output failed to compile after helper-attr strip");
    let out = std::process::Command::new(&out_bin).output().expect("run");
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello from G"));
}

#[test]
fn vendor_expand_inlines_attr_macro() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::marked;\n\
         #[marked]\n\
         fn helper() -> i32 { 7 }\n\
         fn main() { println!(\"{} {}\", helper(), MARKER); }\n",
    );
    let opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &opts).expect("vendor");
    let s = pkg.user_source.to_string();
    assert!(
        s.contains("pub const MARKER: bool = true"),
        "expected Attr macro to inline; got:\n{s}"
    );
    assert!(
        !s.contains("#[marked]"),
        "Attr should be replaced; got:\n{s}"
    );

    let out_dir = tempfile::tempdir().unwrap();
    let out_rs = out_dir.path().join("flat.rs");
    fs::write(&out_rs, &s).unwrap();
    let out_bin = out_dir.path().join("flat_bin");
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(&out_rs)
        .arg("-o")
        .arg(&out_bin)
        .status()
        .expect("spawn rustc");
    assert!(status.success(), "Attr-expanded output failed to compile");
    let out = std::process::Command::new(&out_bin).output().expect("run");
    assert!(String::from_utf8_lossy(&out.stdout).contains("7 true"));
}

#[test]
fn vendor_expand_inlines_bang_macro() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::make_const;\n\
         make_const!(ANSWER, 42);\n\
         fn main() { println!(\"{}\", ANSWER); }\n",
    );
    let opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &opts).expect("vendor");
    let s = pkg.user_source.to_string();
    assert!(
        s.contains("pub const ANSWER: i32 = 42"),
        "expected Bang macro to expand inline; got:\n{s}"
    );
    assert!(
        !s.contains("make_const!"),
        "Bang call should be replaced; got:\n{s}"
    );

    let out_dir = tempfile::tempdir().unwrap();
    let out_rs = out_dir.path().join("flat.rs");
    fs::write(&out_rs, &s).unwrap();
    let out_bin = out_dir.path().join("flat_bin");
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(&out_rs)
        .arg("-o")
        .arg(&out_bin)
        .status()
        .expect("spawn rustc");
    assert!(status.success(), "Bang-expanded output failed to compile");
    let out = std::process::Command::new(&out_bin).output().expect("run");
    assert!(String::from_utf8_lossy(&out.stdout).contains("42"));
}

#[test]
fn vendor_expand_output_compiles_via_rustc() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_user_with_proc_macro_dep(
        "use derive_hello::Hello;\n\
         #[derive(Debug, Clone, Hello)]\n\
         struct G { x: i32 }\n\
         fn main() {\n\
             let g = G { x: 7 };\n\
             let v = vec![1, 2, 3];\n\
             println!(\"{:?} {:?} {}\", g, v, G::hello());\n\
         }\n",
    );
    let opts = VendorOptions {
        strict: true,
        expand: true,
        ..Default::default()
    };
    let pkg =
        vendor::vendor_package(dir.path(), &TargetSelector::Auto, &opts).expect("vendor");

    let out_dir = tempfile::tempdir().unwrap();
    let out_rs = out_dir.path().join("flat.rs");
    fs::write(&out_rs, pkg.user_source.to_string()).unwrap();
    let out_bin = out_dir.path().join("flat_bin");

    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(&out_rs)
        .arg("-o")
        .arg(&out_bin)
        .status()
        .expect("spawn rustc");
    assert!(status.success(), "flattened+expanded output failed to compile");

    let out = std::process::Command::new(&out_bin)
        .output()
        .expect("spawn flattened binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello from G") && stdout.contains("[1, 2, 3]"),
        "expected merged stdlib+third-party output; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// --expand-deep: extend proc-macro inlining to vendored deps via the
// RUSTC_WRAPPER capture path. The fixture is a 3-level chain:
//   user_crate → middle_crate (vendored, uses #[derive(Hello)])
//                              → derive_hello (proc-macro crate)
// Without --expand-deep, middle_crate's derive(Hello) survives into the
// vendored output and the user is forced to externalize derive_hello. With
// --expand-deep, the wrapper inlines middle_crate's derive expansion and
// derive_hello drops out cleanly.
// ---------------------------------------------------------------------------

/// Build a 3-crate fixture: user crate path-deps `middle`, which path-deps
/// the derive_hello proc-macro fixture and uses `#[derive(Hello)]`.
fn make_middle_dep_fixture() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let derive_hello = derive_hello_fixture_path();

    // middle_crate: vendored intermediate that uses #[derive(Hello)]
    let middle = dir.path().join("middle_crate");
    fs::create_dir_all(middle.join("src")).unwrap();
    fs::write(
        middle.join("Cargo.toml"),
        format!(
            "[package]\nname = \"middle_crate\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
             [dependencies]\nderive_hello = {{ path = \"{}\" }}\n",
            derive_hello.display()
        ),
    )
    .unwrap();
    fs::write(
        middle.join("src/lib.rs"),
        "use derive_hello::Hello;\n\
         #[derive(Debug, Hello)]\n\
         pub struct Widget { pub n: i32 }\n",
    )
    .unwrap();

    // user_crate: depends on middle
    let user = dir.path().join("user");
    fs::create_dir_all(user.join("src")).unwrap();
    fs::write(
        user.join("Cargo.toml"),
        "[package]\nname = \"user\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [dependencies]\nmiddle_crate = { path = \"../middle_crate\" }\n",
    )
    .unwrap();
    fs::write(
        user.join("src/main.rs"),
        "fn main() {\n    \
            let w = middle_crate::Widget { n: 7 };\n    \
            println!(\"{:?} {}\", w, middle_crate::Widget::hello());\n\
         }\n",
    )
    .unwrap();
    dir
}

#[test]
fn vendor_expand_deep_inlines_derive_inside_vendored_dep() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    let dir = make_middle_dep_fixture();
    let opts = VendorOptions {
        strict: true,
        expand: true,
        expand_deep: true,
        ..Default::default()
    };
    let pkg = vendor::vendor_package(dir.path().join("user"), &TargetSelector::Auto, &opts)
        .expect("vendor with --expand-deep");

    // middle_crate must be vendored (not externalized).
    assert!(
        pkg.vendored.iter().any(|v| v.name == "middle_crate"),
        "middle_crate should be vendored"
    );

    // Its source should contain the inlined Hello impl, not the derive call.
    let middle = pkg
        .vendored
        .iter()
        .find(|v| v.name == "middle_crate")
        .expect("middle_crate vendored");
    let s = middle.source.to_string();
    assert!(
        s.contains("impl Widget") && s.contains("hello from Widget"),
        "expected inlined hello impl in middle_crate; got:\n{s}"
    );
    assert!(
        !s.contains(", Hello") && !s.contains("Hello,"),
        "Hello derive path should be stripped from middle_crate; got:\n{s}"
    );
    assert!(
        !s.contains("use derive_hello"),
        "use of proc-macro crate should be stripped from middle_crate; got:\n{s}"
    );

    // derive_hello must NOT be vendored.
    assert!(
        !pkg.vendored.iter().any(|v| v.name == "derive_hello"),
        "proc-macro crate must not be vendored"
    );
}

#[test]
fn vendor_expand_deep_output_compiles_via_rustc() {
    if expander_binary().is_none() {
        eprintln!("skipping: flatten_expand not built");
        return;
    }
    if !rustc_available() {
        eprintln!("skipping: rustc not available");
        return;
    }
    let dir = make_middle_dep_fixture();
    let out = run_flatten_capture(
        &dir.path().join("user"),
        &["--vendor", "--expand", "--expand-deep", "--no-banner", "--stdout"],
    );
    assert!(
        out.status.success(),
        "flatten --expand-deep failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let flat = String::from_utf8(out.stdout).unwrap();

    // Compile the assembled flat output with stable rustc — no Cargo deps.
    let out_dir = tempfile::tempdir().unwrap();
    let out_rs = out_dir.path().join("flat.rs");
    fs::write(&out_rs, &flat).unwrap();
    let out_bin = out_dir.path().join("flat_bin");
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(&out_rs)
        .arg("-o")
        .arg(&out_bin)
        .status()
        .expect("spawn rustc");
    assert!(status.success(), "deep-expanded output failed to compile");

    let bin_out = std::process::Command::new(&out_bin)
        .output()
        .expect("spawn flat binary");
    assert!(bin_out.status.success());
    let stdout = String::from_utf8_lossy(&bin_out.stdout);
    assert!(
        stdout.contains("hello from Widget"),
        "expected expansion content; got: {stdout}"
    );
}
