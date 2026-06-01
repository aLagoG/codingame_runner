//! Pure-flattening behaviour tests. Each test builds a tiny on-disk
//! fixture in a tempdir, flattens it, and asserts on the output. No
//! compiler involved — see `e2e_compile.rs` for the "does the result
//! actually compile and run" test.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Spread a `path → contents` map across a fresh tempdir and return the
/// directory handle (keeps the temp alive) plus the absolute path of
/// the entry file. Subdirectories are created as needed.
fn fixture(files: &[(&str, &str)], entry: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("create tempdir");
    for (rel, body) in files {
        let abs = dir.path().join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&abs, body).expect("write fixture");
    }
    let entry_path = dir.path().join(entry);
    (dir, entry_path)
}

fn flatten(files: &[(&str, &str)], entry: &str) -> String {
    let (_dir, entry_path) = fixture(files, entry);
    cpp_flatten::flatten(&entry_path).expect("flatten")
}

#[test]
fn no_includes_is_identity() {
    let out = flatten(&[("main.cpp", "int main() { return 0; }\n")], "main.cpp");
    assert_eq!(out, "int main() { return 0; }\n");
}

#[test]
fn single_local_include_is_inlined() {
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\"\nint main(){}\n"),
            ("a.h", "struct A {};\n"),
        ],
        "main.cpp",
    );
    assert_eq!(out, "struct A {};\nint main(){}\n");
}

#[test]
fn system_include_is_left_alone() {
    let out = flatten(
        &[("main.cpp", "#include <iostream>\nint main(){}\n")],
        "main.cpp",
    );
    assert_eq!(out, "#include <iostream>\nint main(){}\n");
}

#[test]
fn nested_includes_are_recursed() {
    // main → a → b
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\"\nM();\n"),
            ("a.h", "#include \"b.h\"\nA();\n"),
            ("b.h", "B();\n"),
        ],
        "main.cpp",
    );
    assert_eq!(out, "B();\nA();\nM();\n");
}

#[test]
fn shared_header_is_emitted_only_once() {
    // main includes both a.h and b.h; both include shared.h.
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\"\n#include \"b.h\"\nM();\n"),
            ("a.h", "#include \"shared.h\"\nA();\n"),
            ("b.h", "#include \"shared.h\"\nB();\n"),
            ("shared.h", "SHARED();\n"),
        ],
        "main.cpp",
    );
    // shared.h is inlined where a.h first requests it, then the second
    // request from b.h is a no-op.
    assert_eq!(out, "SHARED();\nA();\nB();\nM();\n");
}

#[test]
fn cbindgen_style_guardless_header_is_safe() {
    // main.cpp pulls in BOTH defs.h and io.h; io.h ALSO pulls in defs.h.
    // Without dedup, `struct S {};` would be defined twice and break.
    let out = flatten(
        &[
            (
                "main.cpp",
                "#include \"defs.h\"\n#include \"io.h\"\nMAIN();\n",
            ),
            ("defs.h", "struct S {};\n"),
            ("io.h", "#include \"defs.h\"\nIO();\n"),
        ],
        "main.cpp",
    );
    assert_eq!(out, "struct S {};\nIO();\nMAIN();\n");
}

#[test]
fn relative_path_traversal_works() {
    let out = flatten(
        &[
            ("src/main.cpp", "#include \"../include/a.h\"\nM();\n"),
            ("include/a.h", "A();\n"),
        ],
        "src/main.cpp",
    );
    assert_eq!(out, "A();\nM();\n");
}

#[test]
fn missing_local_include_errors_with_context() {
    let (_dir, entry) = fixture(&[("main.cpp", "#include \"nope.h\"\n")], "main.cpp");
    let err = cpp_flatten::flatten(&entry).expect_err("should fail");
    let chain: String = err
        .chain()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        chain.contains("nope.h"),
        "error chain didn't mention nope.h: {chain}"
    );
}

#[test]
fn comment_after_include_is_preserved() {
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\" // pull in A\nM();\n"),
            ("a.h", "A();\n"),
        ],
        "main.cpp",
    );
    // The whole `#include` line (comment and all) is replaced by the
    // inlined contents — the comment was attached to the directive, so
    // dropping it is the right call.
    assert_eq!(out, "A();\nM();\n");
}

#[test]
fn cycle_terminates_thanks_to_dedup() {
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\"\nM();\n"),
            ("a.h", "#include \"b.h\"\nA();\n"),
            ("b.h", "#include \"a.h\"\nB();\n"),
        ],
        "main.cpp",
    );
    // When b.h re-requests a.h, the dedup skips it; a.h's body emits
    // only once.
    assert_eq!(out, "B();\nA();\nM();\n");
}

#[test]
fn pragma_once_is_stripped_to_avoid_main_file_warning() {
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\"\nM();\n"),
            ("a.h", "#pragma once\nA();\n"),
        ],
        "main.cpp",
    );
    assert!(
        !out.contains("#pragma once"),
        "pragma once should be dropped: {out}",
    );
    assert!(out.contains("A();"));
    assert!(out.contains("M();"));
}

#[test]
fn file_without_trailing_newline_does_not_glue() {
    let out = flatten(
        &[
            ("main.cpp", "#include \"a.h\"\nM();\n"),
            ("a.h", "A();"), // no trailing newline
        ],
        "main.cpp",
    );
    assert!(
        out.contains("A();\nM();"),
        "trailing newline was not synthesised: {out:?}",
    );
}

/// Smoke check: the binary itself runs and produces the same output as
/// the library API. Keeps `src/main.rs` honest.
#[test]
fn binary_matches_library_output() {
    let (_dir, entry) = fixture(
        &[
            ("main.cpp", "#include \"a.h\"\nMAIN();\n"),
            ("a.h", "A();\n"),
        ],
        "main.cpp",
    );

    let lib_out = cpp_flatten::flatten(&entry).expect("lib flatten");

    let bin = env!("CARGO_BIN_EXE_cpp_flatten");
    let proc_out = std::process::Command::new(bin)
        .arg(&entry)
        .output()
        .expect("spawn cpp_flatten binary");
    assert!(
        proc_out.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&proc_out.stderr),
    );
    assert_eq!(String::from_utf8(proc_out.stdout).unwrap(), lib_out);
}

#[test]
fn output_file_flag_writes_to_disk() {
    let (dir, entry) = fixture(&[("main.cpp", "M();\n")], "main.cpp");
    let out_path = dir.path().join("flat.cpp");

    let bin = env!("CARGO_BIN_EXE_cpp_flatten");
    let status = std::process::Command::new(bin)
        .arg(&entry)
        .args(["-o".as_ref(), out_path.as_os_str()])
        .status()
        .expect("spawn cpp_flatten binary");
    assert!(status.success());

    let written = std::fs::read_to_string(&out_path).expect("read output");
    assert_eq!(written, "M();\n");
}

// Reference unused param for clippy-cleanliness in tests that don't use
// the returned PathBuf separately.
#[allow(dead_code)]
fn _types_referenced(_p: &Path) {}
