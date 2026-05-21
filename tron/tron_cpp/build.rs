// Build script: compile `bot.cpp` via cc-rs and force its symbols into
// the cdylib's export table. See the matching script in `tictactoe_cpp`
// for the longer explanation of why both the force-load and the explicit
// `-exported_symbol` flags are necessary.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bot_cpp = manifest_dir.join("bot.cpp");

    let header_dir = manifest_dir
        .parent()
        .expect("crate has a parent dir")
        .join("tron_defs")
        .join("include");
    let header = header_dir.join("tron_defs.h");

    cc::Build::new()
        .file(&bot_cpp)
        .include(&header_dir)
        .cpp(true)
        .flag_if_supported("-std=c++17")
        .compile("tron_cpp_bot_inner");

    let out_dir = env::var("OUT_DIR").unwrap();
    let archive = format!("{out_dir}/libtron_cpp_bot_inner.a");
    force_load(&archive);

    println!("cargo::rerun-if-changed={}", bot_cpp.display());
    println!("cargo::rerun-if-changed={}", header.display());
}

const EXPORTED_SYMBOLS: &[&str] = &["initialize", "take_turn", "abi_version"];

fn force_load(archive: &str) {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match target_os.as_str() {
        "macos" | "ios" => {
            println!("cargo::rustc-link-arg=-Wl,-force_load,{archive}");
            for sym in EXPORTED_SYMBOLS {
                println!("cargo::rustc-link-arg=-Wl,-exported_symbol,_{sym}");
            }
        }
        "linux" | "android" | "freebsd" => {
            println!("cargo::rustc-link-arg=-Wl,--whole-archive");
            println!("cargo::rustc-link-arg={archive}");
            println!("cargo::rustc-link-arg=-Wl,--no-whole-archive");
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
