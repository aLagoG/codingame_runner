#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

/// Bumped on any wire-type change. Plugins built against an older `tron_defs`
/// export an older value; `PluginPlayer::load` reads it through `abi_version()`
/// and refuses mismatches before any UB-prone call lands.
constexpr static const uint32_t ABI_VERSION = 1;

enum class BotStatus : uint8_t {
  Ok = 0,
  Panic = 1,
};

enum class Direction : uint8_t {
  Up,
  Down,
  Left,
  Right,
};

struct TurnOutput {
  Direction direction;
};

struct TurnResult {
  BotStatus status;
  TurnOutput output;
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

extern TurnResult take_turn(TurnInputFFI input);

extern uint32_t abi_version();

}  // extern "C"
