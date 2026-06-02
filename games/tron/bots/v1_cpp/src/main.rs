// One-line Rust shim for the C++ stdio bot. The real work lives in
// `main.cpp` (compiled by `build.rs` via cc-rs); cargo just needs a
// Rust entry point for its `[[bin]]` target.

#[link(name = "tron_v1_cpp_stdio_inner", kind = "static")]
#[link(name = "c++", kind = "dylib")]
unsafe extern "C" {
    fn cgio_main() -> i32;
}

fn main() -> std::process::ExitCode {
    let code = unsafe { cgio_main() };
    std::process::ExitCode::from(code as u8)
}
