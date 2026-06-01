// C++ subprocess bot for tic-tac-toe (baseline): reads `InitialInput`
// once (empty no-op — uses `NoInitialInput`), then `TurnInput` per
// tick from stdin and writes a `TurnOutput` to stdout until EOF.
//
// Two ways this file gets compiled:
//   * Local cargo build: `build.rs` (via `cgio_build`) defines
//     `CGIO_RUST_SHIM`, so the entry point is `extern "C" int
//     cgio_main()`. The Rust binary in `src/main.rs` links the
//     static lib and calls it.
//   * CodinGame paste: no define, so the entry point is `int main()`
//     — `cargo xtask bundle tictactoe baseline --lang cpp` runs
//     cpp_flatten on this file, inlining `strategy.h` and the
//     transitive `tictactoe_defs_io.h` / `tictactoe_defs.h` headers
//     into a single paste-ready source.

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
    tictactoe_baseline_cpp::on_init(cgio::as_ref(init));

    TurnInput input;
    while (std::cin >> input) {
        std::cout << tictactoe_baseline_cpp::decide(input.as_ref()) << std::endl;
    }
    return 0;
}
