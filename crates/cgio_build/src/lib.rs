//! Shared build-script helpers for C++ bot crates.
//!
//! A C++ bot crate has two compilation paths:
//!
//! * `bot.cpp` → static archive force-loaded into the crate's `cdylib`
//!   so the runner's `PluginPlayer` can `dlsym` the FFI exports.
//! * `main.cpp` → static archive linked into the crate's `[[bin]]`
//!   target, which a tiny `src/main.rs` shim wraps so `cargo run`
//!   works for the stdio subprocess transport.
//!
//! The build dance for the cdylib is non-trivial: rustc's empty
//! export list would otherwise strip the FFI symbols, and the linker
//! has to be told to force-load the static archive's objects before
//! it can keep them. This module centralises that dance so each bot
//! crate's `build.rs` collapses to a one-liner:
//!
//! ```ignore
//! fn main() {
//!     cgio_build::build("tron", "tron_baseline_cpp");
//! }
//! ```
//!
//! Layout convention (matches the workspace): the bot crate lives at
//! `<workspace>/games/<game>/bots/<bot_name>_<lang>/`, and the game's
//! generated C++ header lives at `<workspace>/games/<game>/defs/include/`.
//! `cgio_build::build` derives both from `CARGO_MANIFEST_DIR`.

use std::env;
use std::path::{Path, PathBuf};

/// The four FFI symbols every bot may export. `set_counter_callback`
/// is optional from the runner's POV (looked up with `dlsym`, silently
/// ignored if absent) but listing it here is what gets it onto the
/// cdylib's export table for bots that *do* define it.
const EXPORTED_SYMBOLS: &[&str] = &[
    "initialize",
    "take_turn",
    "abi_version",
    "set_counter_callback",
];

/// Run the cpp bot build pipeline.
///
/// * `game` — the game directory name (e.g. `"tron"`). Used in error
///   messages; the defs crate's include directory is always at
///   `<workspace>/games/<game>/defs/include/` by layout convention.
/// * `crate_name` — the bot crate's name (e.g. `"tron_baseline_cpp"`).
///   Used for naming the inner static libs and the stdio bin's lib.
///
/// File-presence convention:
/// * `bot.cpp` is required.
/// * `main.cpp` is optional; if present, the stdio bin is compiled and
///   its inner static lib (named `<crate_name>_stdio_inner`) is
///   emitted for the crate's `src/main.rs` shim to link against.
pub fn build(game: &str, crate_name: &str) {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let header_dir = locate_header_dir(&manifest_dir, game);

    compile_ffi_plugin(&manifest_dir, &header_dir, crate_name);
    if let Some(main_cpp) = optional_main_cpp(&manifest_dir) {
        compile_stdio_bot(&main_cpp, &header_dir, crate_name);
    }

    emit_rerun_directives(&manifest_dir, &header_dir);
}

/// Locate the generated header directory. The expected layout is
/// `<workspace>/games/<game>/bots/<bot_crate>/`, so the defs crate is
/// two directories up from `CARGO_MANIFEST_DIR`. We panic with a
/// clear message if the path doesn't exist so misconfiguration fails
/// fast instead of producing a cryptic `#include` error downstream.
fn locate_header_dir(manifest_dir: &Path, game: &str) -> PathBuf {
    let workspace_game_dir = manifest_dir
        .parent() // .../<game>/bots
        .and_then(Path::parent) // .../<game>
        .unwrap_or_else(|| {
            panic!(
                "cgio_build::build: expected manifest under <ws>/{game}/bots/<bot_crate>/, \
                 got CARGO_MANIFEST_DIR={}",
                manifest_dir.display(),
            )
        });
    let header_dir = workspace_game_dir.join("defs").join("include");
    if !header_dir.exists() {
        panic!(
            "cgio_build::build: header dir not found at {} \
             (does <ws>/{game}/defs/include/ exist?)",
            header_dir.display(),
        );
    }
    header_dir
}

fn optional_main_cpp(manifest_dir: &Path) -> Option<PathBuf> {
    let p = manifest_dir.join("main.cpp");
    p.exists().then_some(p)
}

/// Compile `bot.cpp` into a static archive named `<crate_name>_bot_inner`,
/// then force-load it into the cdylib (or else rustc would silently drop
/// the FFI exports as unused).
fn compile_ffi_plugin(manifest_dir: &Path, header_dir: &Path, crate_name: &str) {
    let bot_cpp = manifest_dir.join("bot.cpp");
    let inner_name = format!("{crate_name}_bot_inner");
    cc::Build::new()
        .file(&bot_cpp)
        .include(header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++20")
        .compile(&inner_name);

    let out_dir = env::var("OUT_DIR").unwrap();
    let archive = format!("{out_dir}/lib{inner_name}.a");
    force_load_cdylib(&archive);
}

/// Compile `main.cpp` into a static archive named
/// `<crate_name>_stdio_inner`, with the `CGIO_RUST_SHIM` define so the
/// entry point is renamed `extern "C" int cgio_main()` for the Rust
/// shim in `src/main.rs` to call.
fn compile_stdio_bot(main_cpp: &Path, header_dir: &Path, crate_name: &str) {
    let inner_name = format!("{crate_name}_stdio_inner");
    cc::Build::new()
        .file(main_cpp)
        .include(header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++20")
        .define("CGIO_RUST_SHIM", None)
        .compile(&inner_name);
}

/// Emit cargo `rerun-if-changed` directives for every cpp source file
/// and every header file in the include dir. Header changes are rare
/// but they do invalidate compiled objects, so we want cargo to notice.
fn emit_rerun_directives(manifest_dir: &Path, header_dir: &Path) {
    println!(
        "cargo::rerun-if-changed={}",
        manifest_dir.join("bot.cpp").display()
    );
    if let Some(main_cpp) = optional_main_cpp(manifest_dir) {
        println!("cargo::rerun-if-changed={}", main_cpp.display());
    }
    if let Ok(entries) = std::fs::read_dir(header_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("h") {
                println!("cargo::rerun-if-changed={}", p.display());
            }
        }
    }
}

/// Emit linker args that force every symbol from `archive` into the
/// cdylib AND name them in the cdylib's exported-symbols list. The two
/// are independent on macOS/Linux; both are needed because Rust's
/// empty export list would otherwise mask everything cc-rs brought in.
///
/// `cargo::rustc-link-arg-cdylib` (note the `-cdylib` suffix) targets
/// only the cdylib build, so the binary target doesn't pick up flags
/// it has no use for.
fn force_load_cdylib(archive: &str) {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match target_os.as_str() {
        "macos" | "ios" => {
            println!("cargo::rustc-link-arg-cdylib=-Wl,-force_load,{archive}");
            for sym in EXPORTED_SYMBOLS {
                println!("cargo::rustc-link-arg-cdylib=-Wl,-exported_symbol,_{sym}");
            }
        }
        "linux" | "android" | "freebsd" => {
            println!("cargo::rustc-link-arg-cdylib=-Wl,--whole-archive");
            println!("cargo::rustc-link-arg-cdylib={archive}");
            println!("cargo::rustc-link-arg-cdylib=-Wl,--no-whole-archive");
            let dyn_list = format!("{}/exports.txt", env::var("OUT_DIR").unwrap());
            let body = format!(
                "{{\n{}\n}};\n",
                EXPORTED_SYMBOLS
                    .iter()
                    .map(|s| format!("    {s};"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            std::fs::write(&dyn_list, body).expect("write dynamic-list");
            println!("cargo::rustc-link-arg-cdylib=-Wl,--dynamic-list={dyn_list}");
        }
        "windows" => {
            println!("cargo::rustc-link-arg-cdylib=/WHOLEARCHIVE:{archive}");
            for sym in EXPORTED_SYMBOLS {
                println!("cargo::rustc-link-arg-cdylib=/EXPORT:{sym}");
            }
        }
        other => panic!("unsupported target OS for force-load: {other}"),
    }
}
