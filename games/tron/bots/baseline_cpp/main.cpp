// C++ stdio entry point for tron baseline. Reads `InitialInput` once,
// then `TurnInput` per tick from stdin and writes a `TurnOutput` to
// stdout until EOF.
//
// Two compilation contexts:
//   * Local cargo build: `build.rs` (via `cgio_build`) defines
//     `CGIO_RUST_SHIM`, so the entry point is renamed
//     `extern "C" int cgio_main()`. The Rust binary in `src/main.rs`
//     links the static archive and calls it.
//   * CodinGame paste: no define, so the entry point is `int main()` —
//     `cargo xtask bundle tron --lang cpp` runs cpp_flatten on this
//     file, inlining `strategy.h` and the transitive `_defs_io.h` /
//     `_defs.h` headers into a single paste-ready source.

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

    // Signal readiness to the runner so it can stop sleeping and start
    // measuring turn-1 latency from a clean baseline. `std::endl`
    // flushes; `std::cerr` is unbuffered anyway but the explicit flush
    // is defensive.
    std::cerr << "READY" << std::endl;

    InitialInput init;
    if (!(std::cin >> init)) return 0;
    tron_baseline_cpp::on_init(init);

    TurnInput input;
    while (std::cin >> input) {
        std::cout << tron_baseline_cpp::decide(input) << std::endl;
    }
    return 0;
}
