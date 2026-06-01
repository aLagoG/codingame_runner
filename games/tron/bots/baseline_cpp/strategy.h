// Tron bot strategy — baseline.
//
// Single source of truth for the per-turn logic. Both `bot.cpp` (FFI
// transport) and `main.cpp` (stdio transport, also the file
// `cpp_flatten` bundles for CodinGame) include this header. Edit the
// strategy here and both transports — plus your next paste-ready
// bundle — pick it up.

#pragma once

#include "../../defs/include/tron_defs_io.h"

namespace tron_baseline_cpp {

// Match-start hook. Tron uses `NoInitialInput`, so this is a no-op;
// kept invariant with the template so a future upgrade to a real
// `InitialInput` only requires editing `tron_defs_io.h` and the body
// here (signature stays the same).
inline void on_init(const cgio::InitialInputRef& /*init*/) {}

// Trivial baseline: default direction every tick. The C++ baseline
// exists as a smoke test for the FFI / stdio / cpp_flatten paths;
// stronger strategies live in `v2/tron.cpp`.
inline TurnOutput decide(const cgio::TurnRef& /*turn*/) {
    return TurnOutput{};
}

}  // namespace tron_baseline_cpp
