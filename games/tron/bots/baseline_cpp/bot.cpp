// C++ FFI bot for tron (baseline). Build + run via cargo (from workspace root):
//
//   cargo build -p tron_baseline_cpp
//   cargo run -p codingame_runner -- --game tron \
//       target/debug/libtron_baseline_cpp.dylib \   # .so on Linux, .dll on Windows
//       target/debug/libtron_baseline_cpp.dylib
//
// Strategy lives in `strategy.h` — shared with `main.cpp` (the stdio
// transport, and the file cpp_flatten bundles for CodinGame). This
// file is *only* the FFI plumbing: borrow `cgio::InitialInputFfi` /
// `TurnInputFFI` via `cgio::as_ref`, hand the result to `strategy.h`,
// wrap the answer in `TurnResult<TurnOutput>`.

#include "strategy.h"

#include <cstdint>

namespace {

// Counter callback (optional; runner attaches under `--counters`).
void (*g_emit_counter)(const char*, double) = nullptr;
unsigned long long g_turn_count = 0;

inline void emit_counter(const char* key, double value) {
    if (g_emit_counter) g_emit_counter(key, value);
}

}  // namespace

extern "C" {

void set_counter_callback(void (*cb)(const char*, double)) {
    g_emit_counter = cb;
}

void initialize(cgio::InitialInputFfi input) {
    tron_baseline_cpp::on_init(cgio::as_ref(input));
}

TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    // Demo counters so the tournament's --counters path has live
    // data to aggregate. Real bots would emit search nodes, TT
    // hits, depth reached, etc.
    ++g_turn_count;
    emit_counter("turn_idx", static_cast<double>(g_turn_count));
    emit_counter("players_alive", static_cast<double>(input.number_of_players));

    TurnOutput output = tron_baseline_cpp::decide(cgio::as_ref(input));
    return TurnResult<TurnOutput>{BotStatus::Ok, output};
}

uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
