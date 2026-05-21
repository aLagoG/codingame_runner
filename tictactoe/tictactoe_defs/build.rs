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
        .with_language(cbindgen::Language::Cxx)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(&output_path);

    println!("cargo::rerun-if-changed=src/lib.rs");
}
