// One-line Rust shim for the C++ stdio bot. The real work lives in
// `main.cpp` (compiled by `build.rs` via cc-rs); cargo just needs a
// Rust entry point for its `[[bin]]` target.
//
// `#[link(...)]` is here (not in `build.rs`) because cargo's
// `cargo::rustc-link-lib` from a build script applies only to the
// package's `[lib]` target — the binary wouldn't otherwise link the
// stdio object. The search path (`-L`) does flow through globally, so
// we only need to name the lib.

#[link(name = "tictactoe_cpp_stdio_inner", kind = "static")]
#[link(name = "c++", kind = "dylib")]
unsafe extern "C" {
    fn cgio_main() -> i32;
}

fn main() -> std::process::ExitCode {
    let code = unsafe { cgio_main() };
    std::process::ExitCode::from(code as u8)
}
