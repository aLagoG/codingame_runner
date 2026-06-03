// Tron bot strategy — baseline.
//
// Single source of truth for the per-turn logic. `main.cpp` includes
// this header; `cpp_flatten` bundles the same file for CodinGame
// submission. Edit the strategy here and both your local cargo
// build and the next paste-ready bundle pick it up.

#pragma once

#include "../../defs/include/tron_defs_io.h"

namespace tron_baseline_cpp {

// Match-start hook. Tron has no per-match init payload, so this is
// a no-op; kept so the bot template's signature stays uniform with
// games that DO ship init data (fantastic_bits).
inline void on_init(const cgio::InitialInput& /*init*/) {}

// Trivial baseline: default direction every tick. Smoke test for the
// stdio + cpp_flatten paths; stronger strategies live in v1_cpp and
// v2_cpp.
inline TurnOutput decide(const cgio::TurnInput& /*turn*/) {
    return TurnOutput{};
}

}  // namespace tron_baseline_cpp
