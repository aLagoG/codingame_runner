// C++ bot for tic-tac-toe. Build + run via cargo (from workspace root):
//
//   cargo build -p tictactoe_cpp
//   cargo run -p codingame_runner -- --game tictactoe \
//       target/debug/libtictactoe_cpp.dylib \      # .so on Linux, .dll on Windows
//       target/debug/libtictactoe_cpp.dylib
//
// The crate's `build.rs` invokes `cc-rs` to compile this file, then
// force-loads its symbols into the cdylib so the runner can `dlsym` them.
//
// The three `extern "C"` exports below are the FFI contract — every bot
// must define all of them. Their signatures and the required type/constant
// definitions come from the cbindgen-generated header.

#include "../../defs/include/tictactoe_defs.h"

// Counter-callback stub. Bots that want to emit performance counters
// can override the body to store `cb` in a global and call it from
// take_turn. Even the no-op form must exist because the cdylib's
// exported-symbols list (see `build.rs`) names it — the runner
// `dlsym`s it when `tournament --counters` is set.
extern "C" void set_counter_callback(void (* /*cb*/)(const char*, double)) {}

extern "C" {

// Called once per player at match start. Tic-tac-toe's `InitialInput` is
// `NoInitialInput` (no per-player data to ferry), so this is a no-op.
void initialize(NoInitialInputFfi /*input*/) {}

// Trivial strategy: play the first empty cell, row-major.
TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    for (uintptr_t i = 0; i < BOARD_CELLS; ++i) {
        if (input.board[i] == Cell::Empty) {
            return TurnResult<TurnOutput>{
                /* status = */ BotStatus::Ok,
                /* output = */ TurnOutput{
                    Pos{
                        /* row = */ static_cast<int32_t>(i / BOARD_SIZE),
                        /* col = */ static_cast<int32_t>(i % BOARD_SIZE),
                    },
                },
            };
        }
    }

    // Board is full — should never happen (the game ends at 9 plays), but
    // satisfy the compiler with a sentinel result.
    return TurnResult<TurnOutput>{ BotStatus::Ok, TurnOutput{} };
}

// Must return the ABI version the bot was built against — the runner
// checks this on load and refuses mismatched plugins.
uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
