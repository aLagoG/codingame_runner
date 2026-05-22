// Build script: compile both `bot.cpp` (the FFI plugin, linked into the
// cdylib) and `main.cpp` (the stdio bot, linked into the `[[bin]]`
// target) via cc-rs. The FFI plugin needs extra linker juggling to get
// its `extern "C"` symbols exported from the cdylib; the binary does
// not, because its Rust `main` references `cgio_main` so the linker
// pulls in `main.cpp`'s object naturally.
//
// Why force-load AND explicit `-exported_symbol` for the cdylib?
// cc-rs hands us a static archive. Rustc links it in but (a) by default
// only keeps symbols the Rust code references, and (b) passes its own
// export list to the linker — and since this crate's Rust surface is
// empty, that list is empty too. Both flags are needed: force-load to
// get the objects into the link at all, and explicit per-symbol exports
// to override the empty export list.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bot_cpp = manifest_dir.join("bot.cpp");
    let main_cpp = manifest_dir.join("main.cpp");

    // Header lives in the sibling `_defs` crate. The `[dependencies]` entry
    // in `Cargo.toml` guarantees that crate's build.rs (which regenerates
    // the header) runs before this one.
    let header_dir = manifest_dir
        .parent()
        .expect("crate has a parent dir")
        .join("tictactoe_defs")
        .join("include");
    let header = header_dir.join("tictactoe_defs.h");
    let io_header = header_dir.join("tictactoe_defs_io.h");

    // FFI plugin → cdylib.
    cc::Build::new()
        .file(&bot_cpp)
        .include(&header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++20")
        .compile("tictactoe_cpp_bot_inner");

    // Subprocess bot → `[[bin]]`. cc-rs emits a `cargo::rustc-link-lib`
    // directive that applies to all targets, but the cdylib's link just
    // ignores the unreferenced `cgio_main` symbol (cdylibs have no entry
    // point, so a stray `main`/`cgio_main` is harmless).
    cc::Build::new()
        .file(&main_cpp)
        .include(&header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++20")
        // Renames the entry point from `int main()` to
        // `extern "C" int cgio_main()` so the Rust binary shim in
        // `src/main.rs` can link and call it without a duplicate-`main`
        // collision against the Rust runtime's own startup symbol.
        // CodinGame pastes the file *without* this define, leaving the
        // standalone `int main()` in place.
        .define("CGIO_RUST_SHIM", None)
        .compile("tictactoe_cpp_stdio_inner");

    let out_dir = env::var("OUT_DIR").unwrap();
    let archive = format!("{out_dir}/libtictactoe_cpp_bot_inner.a");
    force_load_cdylib(&archive);

    println!("cargo::rerun-if-changed={}", bot_cpp.display());
    println!("cargo::rerun-if-changed={}", main_cpp.display());
    println!("cargo::rerun-if-changed={}", header.display());
    println!("cargo::rerun-if-changed={}", io_header.display());
}

/// The three symbols the runner expects from every FFI bot.
const EXPORTED_SYMBOLS: &[&str] = &["initialize", "take_turn", "abi_version"];

/// Emit linker args that force every symbol from `archive` into the
/// cdylib's link AND name them in the cdylib's exported-symbols list.
/// The two are independent on macOS/Linux; both are needed because
/// Rust's empty export list would otherwise mask everything cc-rs
/// brought in. Scoped via `rustc-link-arg-cdylib` so the binary target
/// doesn't inherit them.
fn force_load_cdylib(archive: &str) {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match target_os.as_str() {
        "macos" | "ios" => {
            println!("cargo::rustc-link-arg-cdylib=-Wl,-force_load,{archive}");
            for sym in EXPORTED_SYMBOLS {
                // macOS prefixes C symbols with `_` at link time.
                println!("cargo::rustc-link-arg-cdylib=-Wl,-exported_symbol,_{sym}");
            }
        }
        "linux" | "android" | "freebsd" => {
            println!("cargo::rustc-link-arg-cdylib=-Wl,--whole-archive");
            println!("cargo::rustc-link-arg-cdylib={archive}");
            println!("cargo::rustc-link-arg-cdylib=-Wl,--no-whole-archive");
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
