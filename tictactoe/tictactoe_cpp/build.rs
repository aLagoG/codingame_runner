// Build script: compile `bot.cpp` via cc-rs and force its symbols into
// the cdylib's export table.
//
// Why force-load? cc-rs hands us a static archive (`libtictactoe_cpp_bot_inner.a`).
// Rustc links it into the cdylib but, by default, *only* keeps symbols the
// Rust code references. Our Rust code references nothing in bot.cpp, so
// without the link flag below, `initialize` / `take_turn` / `abi_version`
// would never appear in the final dylib's symbol table — and the runner's
// `lib.get(b"take_turn")` would fail with `symbol not found`.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bot_cpp = manifest_dir.join("bot.cpp");

    // Header lives in the sibling `_defs` crate. The `[dependencies]` entry
    // in `Cargo.toml` guarantees that crate's build.rs (which regenerates
    // the header) runs before this one.
    let header_dir = manifest_dir
        .parent()
        .expect("crate has a parent dir")
        .join("tictactoe_defs")
        .join("include");
    let header = header_dir.join("tictactoe_defs.h");

    cc::Build::new()
        .file(&bot_cpp)
        .include(&header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++17")
        .compile("tictactoe_cpp_bot_inner");

    let out_dir = env::var("OUT_DIR").unwrap();
    let archive = format!("{out_dir}/libtictactoe_cpp_bot_inner.a");
    force_load(&archive);

    println!("cargo::rerun-if-changed={}", bot_cpp.display());
    println!("cargo::rerun-if-changed={}", header.display());
}

/// The three symbols the runner expects from every FFI bot.
const EXPORTED_SYMBOLS: &[&str] = &["initialize", "take_turn", "abi_version"];

/// Emit linker args that (a) force every symbol from `archive` into the
/// final cdylib's link, and (b) add our three C++ entry points to the
/// cdylib's exported-symbols list. The second step matters because rustc
/// passes its own export list to the linker (`-Wl,-exported_symbols_list`
/// on macOS, `-Wl,--version-script` on Linux); since this crate's Rust
/// surface is empty, that list is empty too and our force-loaded symbols
/// would otherwise be stripped on dylib creation.
fn force_load(archive: &str) {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match target_os.as_str() {
        "macos" | "ios" => {
            println!("cargo::rustc-link-arg=-Wl,-force_load,{archive}");
            for sym in EXPORTED_SYMBOLS {
                // macOS prefixes C symbols with `_` at link time.
                println!("cargo::rustc-link-arg=-Wl,-exported_symbol,_{sym}");
            }
        }
        "linux" | "android" | "freebsd" => {
            println!("cargo::rustc-link-arg=-Wl,--whole-archive");
            println!("cargo::rustc-link-arg={archive}");
            println!("cargo::rustc-link-arg=-Wl,--no-whole-archive");
            // Rust's default `--version-script` hides all non-Rust-pub
            // symbols. `--dynamic-list` adds back exactly the ones we
            // name. Generate it on the fly.
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
            println!("cargo::rustc-link-arg=-Wl,--dynamic-list={dyn_list}");
        }
        "windows" => {
            println!("cargo::rustc-link-arg=/WHOLEARCHIVE:{archive}");
            for sym in EXPORTED_SYMBOLS {
                println!("cargo::rustc-link-arg=/EXPORT:{sym}");
            }
        }
        other => panic!("unsupported target OS for force-load: {other}"),
    }
}
