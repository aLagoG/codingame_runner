// One-liner around `cgio_build`. Compiles `bot.cpp` into the cdylib's
// force-loaded inner archive (with FFI exports) and `main.cpp` into
// the stdio bin's inner archive (with the `CGIO_RUST_SHIM` define).
// See `cgio_build/src/lib.rs` for the linker dance behind it.

fn main() {
    cgio_build::build("tron", "tron_baseline_cpp");
}
