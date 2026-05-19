#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

enum class Direction : uint8_t {
  Up,
  Down,
  Left,
  Right,
};

struct TurnOutput {
  Direction direction;
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

TurnOutput take_turn(TurnInputFFI);

}  // extern "C"
