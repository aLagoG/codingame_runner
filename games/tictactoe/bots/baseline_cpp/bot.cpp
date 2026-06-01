// C++ FFI bot for tic-tac-toe (baseline). Build + run via cargo (from workspace root):
//
//   cargo build -p tictactoe_baseline_cpp
//   cargo run -p codingame_runner -- --game tictactoe \
//       target/debug/libtictactoe_baseline_cpp.dylib \   # .so on Linux, .dll on Windows
//       target/debug/libtictactoe_baseline_cpp.dylib
//
// Strategy lives in `strategy.h` — shared with `main.cpp` (the stdio
// transport, and the file cpp_flatten bundles for CodinGame). This
// file is *only* the FFI plumbing.

#include "strategy.h"

#include <cstdint>

extern "C" {

// Counter-callback stub. Bots that want to emit performance counters
// can override the body to store `cb` in a global and call it from
// take_turn. Even the no-op form must exist because the cdylib's
// exported-symbols list (see `build.rs`) names it — the runner
// `dlsym`s it when `tournament --counters` is set.
void set_counter_callback(void (* /*cb*/)(const char*, double)) {}

void initialize(cgio::InitialInputFfi input) {
    tictactoe_baseline_cpp::on_init(cgio::as_ref(input));
}

TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    TurnOutput output = tictactoe_baseline_cpp::decide(cgio::as_ref(input));
    return TurnResult<TurnOutput>{BotStatus::Ok, output};
}

uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
