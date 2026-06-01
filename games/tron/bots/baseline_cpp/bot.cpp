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

#include "../../defs/include/tron_defs.h"

#include <cstdint>

// ---- Counter callback (optional, registered by the runner when
// `tournament --counters` is set). Storing the pointer in a global
// is fine: the FFI plugin's globals are isolated per process by the
// process-pool design.

static void (*g_emit_counter)(const char*, double) = nullptr;
static unsigned long long g_turn_count = 0;

extern "C" {

// Runner-injected counter callback. Bots store the pointer; the
// runner only registers a non-null callback when `--counters` is
// enabled. Absent here = absent in `dlsym` = runner just won't
// capture anything, which is fine.
void set_counter_callback(void (*cb)(const char*, double)) {
    g_emit_counter = cb;
}

static inline void emit_counter(const char* key, double value) {
    if (g_emit_counter) g_emit_counter(key, value);
}

// Called once per player at match start. The default `NoInitialInput` type
// carries no data — leave this empty. Games that ferry real init data
// would stash the input in a `static` here for `take_turn` to read.
void initialize(NoInitialInputFfi /*input*/) {}

// Called once per tick — return what your bot wants to play.
TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    (void)input;  // TODO: read `input` and decide.
    // Demo counters so the tournament's --counters path has live
    // data to aggregate. Real bots would emit search nodes, TT
    // hits, depth reached, etc.
    ++g_turn_count;
    emit_counter("turn_idx", static_cast<double>(g_turn_count));
    emit_counter("players_alive", static_cast<double>(input.number_of_players));
    return TurnResult<TurnOutput>{
        /* status = */ BotStatus::Ok,
        /* output = */ TurnOutput{},
    };
}

// Must return the ABI version the bot was built against — the runner
// checks this on load and refuses mismatched plugins.
uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
