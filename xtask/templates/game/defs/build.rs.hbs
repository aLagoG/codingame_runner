fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap();
    let output_path = format!("include/{pkg_name}.h");

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        // Walk into `common` so the shared `BotStatus` + `TurnResult<O>` types
        // (reached via the `extern "C"` block in lib.rs) land in the header.
        .with_parse_deps(true)
        .with_parse_include(&["common".to_string()])
        // Suppress Clang/GCC's noisy `-Wreturn-type-c-linkage` on the
        // `extern TurnResult<TurnOutput> take_turn(...)` declaration. The
        // template-instantiation-in-extern-C warning is spurious here:
        // both runner and bot are C++ compilers seeing the same header.
        .with_header(
            "#if defined(__clang__) || defined(__GNUC__)\n\
             #pragma GCC diagnostic ignored \"-Wreturn-type-c-linkage\"\n\
             #endif",
        )
        .with_language(cbindgen::Language::Cxx)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(&output_path);

    println!("cargo::rerun-if-changed=src/lib.rs");
}
