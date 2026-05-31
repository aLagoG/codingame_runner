//! End-to-end smoke tests: spawn the runner against every combination
//! of {Rust, C++} × {stdio, FFI} bot transports and verify the match
//! completes with a winner. Catches wire-format drift, ABI mismatches,
//! and force-load/export regressions in the C++ plugin link.
//!
//! Bot artifacts come from sibling workspace crates that aren't direct
//! dependencies of this test (`tictactoe_cpp` is cdylib-only and can't
//! be a dev-dep), so the first test to run calls `cargo build` on them
//! via `Once`. The runner binary itself is supplied through the
//! `CARGO_BIN_EXE_codingame_runner` env var that cargo sets for tests.
//!
//! Outcome assertions are intentionally loose — `outcome: ...` on
//! stdout is enough to prove the loop ran end-to-end. Bot strategies
//! aren't part of the contract under test.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;

static BUILD: Once = Once::new();

fn ensure_bots_built() {
    BUILD.call_once(|| {
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-p", "tictactoe_baseline_rs", "-p", "tictactoe_baseline_cpp"]);
        // Match the profile this test binary was built with so the
        // artifacts land in the directory `artifact_dir()` looks in.
        if !cfg!(debug_assertions) {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("invoke cargo build for bot artifacts");
        assert!(status.success(), "cargo build of bot artifacts failed");
    });
}

fn artifact_dir() -> PathBuf {
    let workspace_target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("runner crate has a parent")
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
            Bot::RustStdio => d.join("tictactoe_baseline_rs"),
            Bot::CppStdio => d.join("tictactoe_baseline_cpp_stdio"),
            Bot::RustFfi => d.join(plugin_filename("tictactoe_baseline_rs")),
            Bot::CppFfi => d.join(plugin_filename("tictactoe_baseline_cpp")),
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
        .args(["--game", "tictactoe"])
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
