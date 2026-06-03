// Hand-written C++ mirror of games/tron/defs/src/lib.rs. Both sides
// are now independent (no cbindgen between them) — when you change
// the wire types here, change them there too.

#pragma once

#include <cstdint>

enum class Direction : uint8_t {
    Up,
    Down,
    Left,
    Right,
};

struct Pos {
    int32_t x;
    int32_t y;
};

struct Line {
    Pos start;
    Pos end;
};

struct TurnOutput {
    Direction direction;
};
