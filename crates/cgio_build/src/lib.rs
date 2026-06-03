//! Shared build-script helper for C++ bot crates.
//!
//! Compiles the crate's `main.cpp` into a static archive named
//! `<crate_name>_inner` and links it into the crate's `[[bin]]`.
//! A tiny `src/main.rs` shim wraps the binary so cargo's bin
//! discovery works.
//!
//! Each bot crate's `build.rs` collapses to a one-liner:
//!
//! ```ignore
//! fn main() {
//!     cgio_build::build();
//! }
//! ```
//!
//! Layout convention: the bot crate lives at
//! `<workspace>/games/<game>/bots/<bot>_<lang>/`, and the game's
//! C++ headers live at `<workspace>/games/<game>/defs/include/`.
//! `build()` derives both `<game>` and `<crate_name>` from cargo's
//! env vars (`CARGO_MANIFEST_DIR` for the layout, `CARGO_PKG_NAME`
//! for the crate name).

use std::env;
use std::path::{Path, PathBuf};

/// Compile `main.cpp` into an inner static lib for the crate's `[[bin]]`.
///
/// Discovers the game name + crate name from cargo's env vars:
///
/// * `CARGO_MANIFEST_DIR` → expected at `.../games/<game>/bots/<crate>/`;
///   we split out `<game>` and use it to locate the defs include dir.
/// * `CARGO_PKG_NAME` → the bot crate name; used to name the inner
///   static lib `<crate>_inner` so the Rust shim in `src/main.rs`
///   knows what to `#[link(name = "…")]`.
pub fn build() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let crate_name = env::var("CARGO_PKG_NAME").unwrap();
    let header_dir = locate_header_dir(&manifest_dir);
    let main_cpp = manifest_dir.join("main.cpp");
    // No existence check — `cc::Build::file().compile()` reports a
    // clear "file not found" with the full path if it's missing.

    let inner_name = format!("{crate_name}_inner");
    cc::Build::new()
        .file(&main_cpp)
        .include(&header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++20")
        .define("CGIO_RUST_SHIM", None)
        .compile(&inner_name);

    // Link the C++ standard library: `c++` (libc++) on macOS/clang,
    // `stdc++` (libstdc++) on Linux/gcc. Doing it here keeps the
    // per-OS choice in one place — bots' `src/main.rs` shims don't
    // need a `#[link(name = "c++")]` line and stay portable.
    println!("cargo::rustc-link-lib=dylib={}", cpp_runtime_lib());

    println!("cargo::rerun-if-changed={}", main_cpp.display());
    emit_header_rerun(&header_dir);
    emit_header_rerun(&manifest_dir);
}

/// Name of the C++ standard library to pass to `-l`, per target. We
/// drive off `CARGO_CFG_TARGET_OS` (the cargo-set env var that
/// reflects the build target, not the host) so cross-compiles to
/// Linux from macOS still pick `stdc++`.
fn cpp_runtime_lib() -> &'static str {
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        // Apple platforms ship libc++ as the system C++ runtime.
        Ok("macos") | Ok("ios") | Ok("tvos") | Ok("watchos") => "c++",
        // Linux/freebsd default to GCC's libstdc++; this is also what
        // distro packages produce.
        _ => "stdc++",
    }
}

/// Walk from `<ws>/games/<game>/bots/<crate>/` up to `<ws>/games/<game>/`
/// and return `defs/include/` under it. Panics with a clear message
/// if the manifest isn't where the convention says it should be.
fn locate_header_dir(manifest_dir: &Path) -> PathBuf {
    let workspace_game_dir = manifest_dir
        .parent() // .../<game>/bots
        .and_then(Path::parent) // .../<game>
        .unwrap_or_else(|| {
            panic!(
                "cgio_build::build: expected manifest under <ws>/games/<game>/bots/<bot_crate>/, \
                 got CARGO_MANIFEST_DIR={}",
                manifest_dir.display(),
            )
        });
    let header_dir = workspace_game_dir.join("defs").join("include");
    assert!(
        header_dir.exists(),
        "cgio_build::build: header dir not found at {} (expected by layout convention)",
        header_dir.display(),
    );
    header_dir
}

/// Emit `rerun-if-changed` for every C/C++ header file directly under
/// `dir`. Non-recursive — that's enough for both the defs include dir
/// (flat by convention) and bot dirs (typically just a `strategy.h`).
fn emit_header_rerun(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let is_header = p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "h" | "hpp" | "hh" | "hxx"));
        if is_header {
            println!("cargo::rerun-if-changed={}", p.display());
        }
    }
}
