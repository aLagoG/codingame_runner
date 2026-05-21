#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

/// Bumped on any wire-type change. Plugins built against an older `tron_defs`
/// export an older value; `PluginPlayer::load` reads it through `abi_version()`
/// and refuses mismatches before any UB-prone call lands.
constexpr static const uint32_t ABI_VERSION = 1;

enum class Direction : uint8_t {
  Up,
  Down,
  Left,
  Right,
};

/// Status byte returned by every bot's `take_turn` FFI call. Same shape for
/// every game — `Ok` means `TurnResult::output` is valid; `Panic` means the
/// bot's `catch_unwind` shim intercepted a panic and `output` is placeholder
/// data that the runner must ignore.
enum class BotStatus : uint8_t {
  Ok = 0,
  Panic = 1,
};

struct TurnOutput {
  Direction direction;
};

/// FFI return type of every bot's `take_turn`. Generic over the per-game
/// `O` (the game's `TurnOutput`), monomorphised by cbindgen into a concrete
/// C++ struct per game.
template<typename O>
struct TurnResult {
  BotStatus status;
  O output;
};

struct Pos {
  int32_t x;
  int32_t y;
};

struct Line {
  Pos start;
  Pos end;
};

struct TurnInputFFI {
  int32_t number_of_players;
  int32_t player_number;
  const Line *player_lines;
};

extern "C" {

extern TurnResult<TurnOutput> take_turn(TurnInputFFI input);

extern uint32_t abi_version();

}  // extern "C"
