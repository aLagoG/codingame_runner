#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

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

TurnResult take_turn(TurnInputFFI);

}  // extern "C"
