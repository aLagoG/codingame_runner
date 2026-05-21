fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap();
    let output_path = format!("include/{pkg_name}.h");

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_language(cbindgen::Language::Cxx)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(&output_path);

    println!("cargo::rerun-if-changed=src/lib.rs");
}
