// Tic-tac-toe bot strategy — baseline.
//
// Single source of truth for the per-turn logic. Both `bot.cpp` (FFI
// transport) and `main.cpp` (stdio transport, also the file
// `cpp_flatten` bundles for CodinGame) include this header. Edit the
// strategy here and both transports — plus your next paste-ready
// bundle — pick it up.

#pragma once

#include "../../defs/include/tictactoe_defs_io.h"

namespace tictactoe_baseline_cpp {

// Match-start hook. Tic-tac-toe uses `NoInitialInput`, so this is a
// no-op; kept invariant with the template so a future upgrade to a
// real `InitialInput` only requires editing `tictactoe_defs_io.h`
// and the body here (signature stays the same).
inline void on_init(const cgio::InitialInputRef& /*init*/) {}

// Trivial baseline: play the first empty cell, row-major. The C++
// baseline exists as a smoke test for the FFI / stdio / cpp_flatten
// paths.
inline TurnOutput decide(const cgio::TurnRef& turn) {
    for (size_t i = 0; i < turn.board.size(); ++i) {
        if (turn.board[i] == Cell::Empty) {
            return TurnOutput{
                Pos{
                    static_cast<int32_t>(i / BOARD_SIZE),
                    static_cast<int32_t>(i % BOARD_SIZE),
                },
            };
        }
    }
    return TurnOutput{};
}

}  // namespace tictactoe_baseline_cpp
