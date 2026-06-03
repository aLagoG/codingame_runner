// Tiny Rust shim that links the static archive produced from
// `main.cpp` (compiled by `build.rs` via cgio_build) and calls into
// it. Cargo needs a Rust entry point for its `[[bin]]` target;
// `cgio_main` is what `main.cpp` exports under the `CGIO_RUST_SHIM`
// define cgio_build sets.

#[link(name = "tron_baseline_cpp_inner", kind = "static")]
unsafe extern "C" {
    fn cgio_main() -> i32;
}

fn main() -> std::process::ExitCode {
    let code = unsafe { cgio_main() };
    std::process::ExitCode::from(code as u8)
}
