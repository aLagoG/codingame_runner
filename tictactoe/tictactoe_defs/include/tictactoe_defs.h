#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

constexpr static const uintptr_t BOARD_SIZE = 3;

constexpr static const uintptr_t BOARD_CELLS = (BOARD_SIZE * BOARD_SIZE);

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

TurnResult take_turn(TurnInputFFI);

}  // extern "C"
