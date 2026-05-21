#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

constexpr static const uintptr_t BOARD_SIZE = 3;

constexpr static const uintptr_t BOARD_CELLS = (BOARD_SIZE * BOARD_SIZE);

/// Bumped on any wire-type change. Plugins built against an older
/// `tictactoe_defs` export an older value; `PluginPlayer::load` reads it
/// through `abi_version()` and refuses mismatches before any UB-prone call
/// lands.
constexpr static const uint32_t ABI_VERSION = 1;

enum class BotStatus : uint8_t {
  Ok = 0,
  Panic = 1,
};

enum class Cell : uint8_t {
  Empty = 0,
  X = 1,
  O = 2,
};

struct Pos {
  int32_t row;
  int32_t col;
};

struct TurnOutput {
  Pos pos;
};

struct TurnResult {
  BotStatus status;
  TurnOutput output;
};

struct TurnInputFFI {
  int32_t player_number;
  const Cell *board;
};

extern "C" {

extern TurnResult take_turn(TurnInputFFI input);

extern uint32_t abi_version();

}  // extern "C"
