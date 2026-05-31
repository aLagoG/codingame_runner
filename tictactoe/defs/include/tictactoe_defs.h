#if defined(__clang__) || defined(__GNUC__)
#pragma GCC diagnostic ignored "-Wreturn-type-c-linkage"
#endif

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

/// Status byte returned by every bot's `take_turn` FFI call. Same shape for
/// every game — `Ok` means `TurnResult::output` is valid; `Panic` means the
/// bot's `catch_unwind` shim intercepted a panic and `output` is placeholder
/// data that the runner must ignore.
enum class BotStatus : uint8_t {
  Ok = 0,
  Panic = 1,
};

enum class Cell : uint8_t {
  Empty = 0,
  X = 1,
  O = 2,
};

/// FFI mirror of [`NoInitialInput`]. Same one-byte layout.
struct NoInitialInputFfi {
  uint8_t _padding;
};

struct Pos {
  int32_t row;
  int32_t col;
};

struct TurnOutput {
  Pos pos;
};

/// FFI return type of every bot's `take_turn`. Generic over the per-game
/// `O` (the game's `TurnOutput`), monomorphised by cbindgen into a concrete
/// C++ struct per game.
template<typename O>
struct TurnResult {
  BotStatus status;
  O output;
};

struct TurnInputFFI {
  int32_t player_number;
  const Cell *board;
};

extern "C" {

extern void initialize(NoInitialInputFfi input);

extern TurnResult<TurnOutput> take_turn(TurnInputFFI input);

extern uint32_t abi_version();

}  // extern "C"
