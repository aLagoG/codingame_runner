// Build script: compile both `bot.cpp` (the FFI plugin, linked into the
// cdylib) and `main.cpp` (the stdio bot, linked into the `[[bin]]`
// target) via cc-rs. See the matching script in `tictactoe_cpp` for
// the longer explanation of the force-load + export-list gymnastics
// the cdylib needs and why the binary doesn't.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bot_cpp = manifest_dir.join("bot.cpp");
    let main_cpp = manifest_dir.join("main.cpp");

    let header_dir = manifest_dir
        .parent()
        .expect("crate has a parent dir")
        .join("tron_defs")
        .join("include");
    let header = header_dir.join("tron_defs.h");
    let io_header = header_dir.join("tron_defs_io.h");

    // FFI plugin → cdylib.
    cc::Build::new()
        .file(&bot_cpp)
        .include(&header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++20")
        .compile("tron_cpp_bot_inner");

    // Subprocess bot → `[[bin]]`.
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
        .compile("tron_cpp_stdio_inner");

    let out_dir = env::var("OUT_DIR").unwrap();
    let archive = format!("{out_dir}/libtron_cpp_bot_inner.a");
    force_load_cdylib(&archive);

    println!("cargo::rerun-if-changed={}", bot_cpp.display());
    println!("cargo::rerun-if-changed={}", main_cpp.display());
    println!("cargo::rerun-if-changed={}", header.display());
    println!("cargo::rerun-if-changed={}", io_header.display());
}

// `set_counter_callback` is optional from the runner's POV — it's
// looked up with `dlsym` and silently ignored if missing. We list
// it here so bots that *do* define it get the symbol exposed on the
// cdylib's exported-symbol table; without this, rustc's empty
// export list would strip it.
const EXPORTED_SYMBOLS: &[&str] = &[
    "initialize",
    "take_turn",
    "abi_version",
    "set_counter_callback",
];

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
