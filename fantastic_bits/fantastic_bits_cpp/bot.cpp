// C++ bot for fantastic_bits. Build + run via cargo (from workspace root):
//
//   cargo build -p fantastic_bits_cpp
//   cargo run -p codingame_runner -- --game fantastic_bits \
//       target/debug/libfantastic_bits_cpp.dylib \      # .so on Linux, .dll on Windows
//       target/debug/libfantastic_bits_cpp.dylib
//
// The crate's `build.rs` invokes `cc-rs` to compile this file, then
// force-loads its symbols into the cdylib so the runner can `dlsym` them.
//
// The three `extern "C"` exports below are the FFI contract — every bot
// must define all of them. Their signatures and the required type/constant
// definitions come from the cbindgen-generated header.

#include "../fantastic_bits_defs/include/fantastic_bits_defs.h"

// ---- Counter callback (optional, registered by the runner when
// `tournament --counters` is set).

static void (*g_emit_counter)(const char*, double) = nullptr;

extern "C" {

// Runner-injected counter callback. Bots store the pointer; the runner
// only registers a non-null callback when `--counters` is enabled.
void set_counter_callback(void (*cb)(const char*, double)) {
    g_emit_counter = cb;
}

// Called once per player at match start. The default `NoInitialInput` type
// carries no data — leave this empty. Games that ferry real init data
// would stash the input in a `static` here for `take_turn` to read.
void initialize(NoInitialInputFfi /*input*/) {}

// Phase-1 placeholder: idle both wizards at (0,0) thrust 0. Real strategy
// lands in a later phase. The Rust baseline bot in `fantastic_bits_rs` is
// the reference for what a minimal active bot looks like.
TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    (void)input;
    WizardAction idle{
        /* kind      = */ ActionKind::Move,
        /* x         = */ 0,
        /* y         = */ 0,
        /* power     = */ 0,
        /* target_id = */ 0,
    };
    return TurnResult<TurnOutput>{
        /* status = */ BotStatus::Ok,
        /* output = */ TurnOutput{idle, idle},
    };
}

// Must return the ABI version the bot was built against — the runner
// checks this on load and refuses mismatched plugins.
uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
