// Hand-written stdio wire-format helpers for tron — paired with the
// cbindgen-generated `tron_defs.h`. Stays in sync with the Rust impls
// in `tron_defs/src/lib.rs` by hand; if you change the format in one
// place, change the other.
//
// Wire format (matches the Rust `Display`/`FromStr` impls):
//
//   <number_of_players> <player_number>     ← header line
//   <x1> <y1> <x2> <y2>                     ← one line per player, N total
//   ...
//
// Output:
//
//   <UP|DOWN|LEFT|RIGHT>                    ← single token on its own line
//
// See `docs/wire-codegen.md` for the plan to autogenerate this header
// from a shared schema once the count of games justifies it.

#pragma once

#include "tron_defs.h"

#include <cstdint>
#include <iostream>
#include <span>
#include <string>
#include <vector>

namespace cgio {

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

/// Borrowed view shared between the owning `TurnInput` (subprocess
/// transport) and the cbindgen-generated `::TurnInputFFI` (plugin
/// transport). Bot logic should take `const TurnRef&` so the same
/// `decide(...)` function works in both transports — mirrors the
/// `TurnRef`/`as_ref` pattern on the Rust side.
struct TurnRef {
    int32_t              number_of_players;
    int32_t              player_number;
    std::span<const Line> player_lines;
};

/// Owning C++ form of the per-tick input — the FFI-facing `TurnInputFFI`
/// is a borrowed view, which doesn't fit the subprocess transport. This
/// type is what subprocess bots actually read from stdin.
struct TurnInput {
    int32_t           number_of_players;
    int32_t           player_number;
    std::vector<Line> player_lines;

    TurnRef as_ref() const {
        return TurnRef{number_of_players, player_number, std::span<const Line>(player_lines)};
    }
};

/// Borrowed view of the cbindgen FFI struct. Free function (not a method)
/// because `::TurnInputFFI` is regenerated and we can't add members to it.
inline TurnRef as_ref(const ::TurnInputFFI& ffi) {
    return TurnRef{
        ffi.number_of_players,
        ffi.player_number,
        std::span<const Line>(ffi.player_lines, static_cast<size_t>(ffi.number_of_players)),
    };
}

inline std::istream& operator>>(std::istream& in, TurnInput& v) {
    if (!(in >> v.number_of_players >> v.player_number)) return in;
    v.player_lines.resize(static_cast<size_t>(v.number_of_players));
    for (auto& line : v.player_lines) {
        if (!(in >> line)) return in;
    }
    return in;
}

}  // namespace cgio
