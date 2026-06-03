// Stdio wire-format helpers for tron. Paired with `tron_defs.h` (the
// type definitions). Stays in sync with the Rust impls in
// `tron_defs/src/lib.rs` by hand — change the format in one place,
// change the other.
//
// Wire format:
//
//   <number_of_players> <player_number>     ← header line
//   <x1> <y1> <x2> <y2>                     ← one line per player, N total
//   ...
//
// Output:
//
//   <UP|DOWN|LEFT|RIGHT>                    ← single token on its own line

#pragma once

#include "tron_defs.h"

#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

namespace cgio {

// ---- InitialInput (empty for tron — no per-match init payload) ----
//
// Empty struct + no-op operator>> so main.cpp can read it uniformly
// with games that DO have an init step (fantastic_bits).

struct InitialInput {};

inline std::istream& operator>>(std::istream& in, InitialInput&) { return in; }

// ---- Pos / Line / Direction / TurnOutput ----

inline std::istream& operator>>(std::istream& in, Pos& p) {
    return in >> p.x >> p.y;
}

inline std::ostream& operator<<(std::ostream& out, const Pos& p) {
    return out << p.x << ' ' << p.y;
}

inline std::istream& operator>>(std::istream& in, Line& l) {
    return in >> l.start >> l.end;
}

inline std::ostream& operator<<(std::ostream& out, const Line& l) {
    return out << l.start << ' ' << l.end;
}

inline const char* to_str(Direction d) {
    switch (d) {
        case Direction::Up:    return "UP";
        case Direction::Down:  return "DOWN";
        case Direction::Left:  return "LEFT";
        case Direction::Right: return "RIGHT";
    }
    return "?";
}

inline bool from_str(const std::string& s, Direction& out) {
    if (s == "UP")    { out = Direction::Up;    return true; }
    if (s == "DOWN")  { out = Direction::Down;  return true; }
    if (s == "LEFT")  { out = Direction::Left;  return true; }
    if (s == "RIGHT") { out = Direction::Right; return true; }
    return false;
}

inline std::ostream& operator<<(std::ostream& out, Direction d) {
    return out << to_str(d);
}

inline std::istream& operator>>(std::istream& in, Direction& d) {
    std::string tok;
    if (!(in >> tok)) return in;
    if (!from_str(tok, d)) in.setstate(std::ios::failbit);
    return in;
}

inline std::ostream& operator<<(std::ostream& out, const TurnOutput& o) {
    return out << o.direction;
}

// ---- TurnInput (owning) ----

struct TurnInput {
    int32_t           number_of_players;
    int32_t           player_number;
    std::vector<Line> player_lines;
};

inline std::istream& operator>>(std::istream& in, TurnInput& v) {
    if (!(in >> v.number_of_players >> v.player_number)) return in;
    v.player_lines.resize(static_cast<size_t>(v.number_of_players));
    for (auto& line : v.player_lines) {
        if (!(in >> line)) return in;
    }
    return in;
}

}  // namespace cgio
