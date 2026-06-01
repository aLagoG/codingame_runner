// C++ subprocess bot for tron: reads `TurnInput` from stdin and writes
// a `TurnOutput` line to stdout, looping until EOF.
//
// Two ways this file gets compiled:
//   * Local cargo build: `build.rs` defines `CGIO_RUST_SHIM`, so the
//     entry point is `extern "C" int cgio_main()`. The Rust binary in
//     `src/main.rs` links the static lib and calls it.
//   * CodinGame paste: no define, so the entry point is `int main()`
//     — paste this file (plus the contents of `tron_defs.h` and
//     `tron_defs_io.h`, since CodinGame is single-file) and the
//     submission compiles straight to a standalone binary.
//
// Strategy matches `bot.cpp` (the FFI variant) so both transports of
// the C++ bot behave identically — useful for the integration tests
// that pit them against each other.

// `_io.h` transitively includes `tron_defs.h`, which lacks include
// guards (cbindgen output) — don't include it twice or `TurnInputFFI`
// gets redefined.
#include "../../defs/include/tron_defs_io.h"

#include <iostream>

// Bring `cgio::operator<<` / `cgio::operator>>` into scope for the
// stream calls below. ADL can't find them on its own — `TurnInput`,
// `TurnOutput`, etc. are in the global namespace (cbindgen output)
// while the operators live in `cgio`.
using namespace cgio;

static TurnOutput decide(const TurnRef& /*turn*/) {
    return TurnOutput{};  // Default direction; bot.cpp does the same.
}

#ifdef CGIO_RUST_SHIM
extern "C" int cgio_main()
#else
int main()
#endif
{
    std::ios_base::sync_with_stdio(false);
    TurnInput input;
    while (std::cin >> input) {
        std::cout << decide(input.as_ref()) << std::endl;
    }
    return 0;
}
