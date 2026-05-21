// C++ bot for tic-tac-toe. Compile as a shared library:
//
//   Linux:   c++ -shared -fPIC -O2 -std=c++17 \
//                -I../tictactoe_defs/include \
//                bot.cpp -o libtictactoe_bot.so
//
//   macOS:   c++ -dynamiclib -O2 -std=c++17 \
//                -I../tictactoe_defs/include \
//                bot.cpp -o libtictactoe_bot.dylib
//
//   Windows: cl /LD /O2 /std:c++17 ^
//                /I../tictactoe_defs/include ^
//                bot.cpp /Fe:tictactoe_bot.dll
//
// Then run with the codingame runner (from the workspace root):
//
//   codingame_runner --game tictactoe \
//       tictactoe/tictactoe_cpp/libtictactoe_bot.dylib \
//       tictactoe/tictactoe_cpp/libtictactoe_bot.dylib
//
// The three `extern "C"` exports below are the FFI contract — every bot
// must define all of them. Their signatures and the required type/constant
// definitions come from the cbindgen-generated header.

#include "../tictactoe_defs/include/tictactoe_defs.h"

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
