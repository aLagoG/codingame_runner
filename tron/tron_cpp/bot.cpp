// C++ bot for tron. Compile as a shared library:
//
//   Linux:   c++ -shared -fPIC -O2 -std=c++17 \
//                -I../tron_defs/include \
//                bot.cpp -o libtron_bot.so
//
//   macOS:   c++ -dynamiclib -O2 -std=c++17 \
//                -I../tron_defs/include \
//                bot.cpp -o libtron_bot.dylib
//
//   Windows: cl /LD /O2 /std:c++17 ^
//                /I../tron_defs/include ^
//                bot.cpp /Fe:tron_bot.dll
//
// Then run with the codingame runner:
//
//   codingame_runner --game tron libtron_bot.so   (or .dylib / .dll)
//
// The three `extern "C"` exports below are the FFI contract — every bot
// must define all of them. Their signatures and the required type/constant
// definitions come from the cbindgen-generated header.

#include "../tron_defs/include/tron_defs.h"

extern "C" {

// Called once per player at match start. The default `NoInitialInput` type
// carries no data — leave this empty. Games that ferry real init data
// would stash the input in a `static` here for `take_turn` to read.
void initialize(NoInitialInputFfi /*input*/) {}

// Called once per tick — return what your bot wants to play.
TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    (void)input;  // TODO: read `input` and decide.
    return TurnResult<TurnOutput>{
        /* status = */ BotStatus::Ok,
        /* output = */ TurnOutput{},
    };
}

// Must return the ABI version the bot was built against — the runner
// checks this on load and refuses mismatched plugins.
uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
