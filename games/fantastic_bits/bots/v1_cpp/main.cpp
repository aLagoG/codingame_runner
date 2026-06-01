// C++ subprocess bot for fantastic_bits (v1): reads `InitialInput`
// once, then `TurnInput` per tick from stdin and writes a `TurnOutput`
// (two lines, one per wizard) to stdout, looping until EOF.
//
// Two ways this file gets compiled:
//   * Local cargo build: `build.rs` (via `cgio_build`) defines
//     `CGIO_RUST_SHIM`, so the entry point is `extern "C" int
//     cgio_main()`. The Rust binary in `src/main.rs` links the
//     static lib and calls it.
//   * CodinGame paste: no define, so the entry point is `int main()`
//     — `cargo xtask bundle fantastic_bits v1 --lang cpp` runs
//     `cpp_flatten` on this file, inlining `strategy.h` and the
//     transitive `..._defs_io.h` / `..._defs.h` headers into a single
//     paste-ready source.
//
// Strategy lives in `strategy.h` — both transports call the same
// `decide(TurnRef)`. The two physics fixes in the Flipendo branch
// (symmetric wall radii + corrected bounce formula) live there, so a
// paste-ready bundle out of `main.cpp` ships those fixes.

#include "strategy.h"

#include <iostream>

using namespace cgio;

#ifdef CGIO_RUST_SHIM
extern "C" int cgio_main()
#else
int main()
#endif
{
    std::ios_base::sync_with_stdio(false);

    InitialInput init;
    if (!(std::cin >> init)) return 0;
    fantastic_bits_v1_cpp::on_init(cgio::as_ref(init));

    TurnInput input;
    while (std::cin >> input) {
        std::cout << fantastic_bits_v1_cpp::decide(input.as_ref()) << std::endl;
    }
    return 0;
}
