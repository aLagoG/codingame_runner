// C++ FFI bot for tron (v2). See ../baseline_cpp/bot.cpp for the
// transport contract; only the strategy namespace changes per bot.

#include "strategy.h"

#include <cstdint>

namespace {

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
    tron_v2_cpp::on_init(cgio::as_ref(input));
}

TurnResult<TurnOutput> take_turn(TurnInputFFI input) {
    ++g_turn_count;
    emit_counter("turn_idx", static_cast<double>(g_turn_count));
    emit_counter("players_alive", static_cast<double>(input.number_of_players));

    TurnOutput output = tron_v2_cpp::decide(cgio::as_ref(input));
    emit_counter("nodes_searched", static_cast<double>(tron_v2_cpp::nodes_searched));
    return TurnResult<TurnOutput>{BotStatus::Ok, output};
}

uint32_t abi_version() { return ABI_VERSION; }

}  // extern "C"
