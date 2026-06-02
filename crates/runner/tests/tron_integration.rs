//! End-to-end smoke tests for tron: build the baselines (Rust + C++,
//! both stdio and FFI), spawn each in turn against a fixed seed, and
//! assert on the wire-format outputs to confirm the runner's
//! plugin/subprocess plumbing still works.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;

static BUILD: Once = Once::new();

fn ensure_bots_built() {
    BUILD.call_once(|| {
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-p", "tron_baseline_rs", "-p", "tron_baseline_cpp"]);
        if !cfg!(debug_assertions) {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("invoke cargo build for bot artifacts");
        assert!(status.success(), "cargo build of bot artifacts failed");
    });
}

fn artifact_dir() -> PathBuf {
    let workspace_target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // <ws>/crates
        .and_then(std::path::Path::parent) // <ws>
        .expect("runner crate is at <ws>/crates/runner")
        .join("target");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    workspace_target.join(profile)
}

#[cfg(target_os = "macos")]
fn plugin_filename(stem: &str) -> String {
    format!("lib{stem}.dylib")
}
#[cfg(target_os = "linux")]
fn plugin_filename(stem: &str) -> String {
    format!("lib{stem}.so")
}
#[cfg(target_os = "windows")]
fn plugin_filename(stem: &str) -> String {
    format!("{stem}.dll")
}

#[derive(Copy, Clone, Debug)]
enum Bot {
    RustStdio,
    RustFfi,
    CppStdio,
    CppFfi,
}

impl Bot {
    fn path(self) -> PathBuf {
        let d = artifact_dir();
        match self {
            Bot::RustStdio => d.join("tron_baseline_rs"),
            Bot::CppStdio => d.join("tron_baseline_cpp_stdio"),
            Bot::RustFfi => d.join(plugin_filename("tron_baseline_rs")),
            Bot::CppFfi => d.join(plugin_filename("tron_baseline_cpp")),
        }
    }
}

fn run_match(p0: Bot, p1: Bot) {
    ensure_bots_built();

    let p0_path = p0.path();
    let p1_path = p1.path();
    assert!(
        p0_path.exists(),
        "missing bot artifact: {}",
        p0_path.display()
    );
    assert!(
        p1_path.exists(),
        "missing bot artifact: {}",
        p1_path.display()
    );

    let runner = env!("CARGO_BIN_EXE_codingame_runner");
    let out = Command::new(runner)
        .args(["--game", "tron"])
        .arg(&p0_path)
        .arg(&p1_path)
        .output()
        .expect("spawn runner");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "runner failed ({:?} vs {:?})\nstdout:\n{}\nstderr:\n{}",
        p0,
        p1,
        stdout,
        stderr,
    );
    assert!(
        stdout.contains("outcome:"),
        "no outcome line in runner output ({:?} vs {:?})\nstdout:\n{}",
        p0,
        p1,
        stdout,
    );
}

#[test]
fn rust_ffi_vs_rust_stdio() {
    run_match(Bot::RustFfi, Bot::RustStdio);
}

#[test]
fn cpp_ffi_vs_cpp_stdio() {
    run_match(Bot::CppFfi, Bot::CppStdio);
}

#[test]
fn rust_ffi_vs_cpp_ffi() {
    run_match(Bot::RustFfi, Bot::CppFfi);
}

#[test]
fn rust_stdio_vs_cpp_stdio() {
    run_match(Bot::RustStdio, Bot::CppStdio);
}

#[test]
fn rust_ffi_vs_cpp_stdio() {
    run_match(Bot::RustFfi, Bot::CppStdio);
}

#[test]
fn rust_stdio_vs_cpp_ffi() {
    run_match(Bot::RustStdio, Bot::CppFfi);
}
