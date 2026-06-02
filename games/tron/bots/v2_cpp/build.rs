// One-liner around `cgio_build`. Compiles `bot.cpp` into the cdylib's
// force-loaded inner archive (with FFI exports) and `main.cpp` into
// the stdio bin's inner archive (with the `CGIO_RUST_SHIM` define).

fn main() {
    cgio_build::build("tron", "tron_v2_cpp");
}
