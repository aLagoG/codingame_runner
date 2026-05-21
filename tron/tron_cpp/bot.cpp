// C++ bot for tron. Build + run via cargo (from workspace root):
//
//   cargo build -p tron_cpp
//   cargo run -p codingame_runner -- --game tron \
//       target/debug/libtron_cpp.dylib \         # .so on Linux, .dll on Windows
//       target/debug/libtron_cpp.dylib
//
// The crate's `build.rs` invokes `cc-rs` to compile this file, then
// force-loads its symbols into the cdylib so the runner can `dlsym` them.
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
