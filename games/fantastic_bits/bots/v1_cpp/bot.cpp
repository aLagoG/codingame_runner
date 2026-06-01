// C++ FFI bot for fantastic_bits (v1). Build + run via cargo (from workspace root):
//
//   cargo build -p fantastic_bits_v1_cpp
//   cargo run -p codingame_runner -- --game fantastic_bits \
//       target/debug/libfantastic_bits_v1_cpp.dylib \   # .so on Linux, .dll on Windows
//       target/debug/libfantastic_bits_v1_cpp.dylib
//
// Strategy lives in `strategy.h` — shared with `main.cpp` (the stdio
// transport, and the file cpp_flatten bundles for CodinGame). This
// file is *only* the FFI plumbing: borrow `TurnInputFFI` as a
// `cgio::TurnRef`, hand it to `strategy.h::decide`, wrap the answer
// in `TurnResult<TurnOutput>`. Tune the strategy in `strategy.h`;
// both the FFI build and the CG bundle pick it up.

#include "strategy.h"

#include <cstdint>

namespace {

// Counter callback (optional; runner attaches under `--counters`).
void (*g_emit_counter)(const char*, double) = nullptr;

}  // namespace

extern "C" {

void set_counter_callback(void (*cb)(const char*, double)) {
    g_emit_counter = cb;
}

void initialize(cgio::InitialInputFfi input) {
    fantastic_bits_v1::on_init(cgio::as_ref(input));
}

TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    TurnOutput output = fantastic_bits_v1::decide(cgio::as_ref(input));
    return TurnResult<TurnOutput>{BotStatus::Ok, output};
}

uint32_t abi_version() {
    return ABI_VERSION;
}

}  // extern "C"
