//! End-to-end smoke test for tron: build both baselines (Rust + C++),
//! spawn each in turn against a fixed seed via the runner, and assert
//! on the wire-format outputs to confirm the subprocess plumbing
//! still works.

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

fn run_match(p0_stem: &str, p1_stem: &str) {
    ensure_bots_built();

    let d = artifact_dir();
    let p0_path = d.join(p0_stem);
    let p1_path = d.join(p1_stem);
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
        "runner failed ({p0_stem} vs {p1_stem})\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        stdout.contains("outcome:"),
        "no outcome line in runner output ({p0_stem} vs {p1_stem})\nstdout:\n{stdout}",
    );
}

#[test]
fn rust_vs_cpp() {
    run_match("tron_baseline_rs", "tron_baseline_cpp");
}

#[test]
fn cpp_vs_rust() {
    run_match("tron_baseline_cpp", "tron_baseline_rs");
}
