// Hand-written stdio wire-format helpers for tic-tac-toe — paired with
// the cbindgen-generated `tictactoe_defs.h`. Stays in sync with the Rust
// impls in `tictactoe_defs/src/lib.rs` by hand; if you change the format
// in one place, change the other.
//
// Wire format (matches the Rust `Display`/`FromStr` impls):
//
//   <player_number>            ← one int on its own line
//   <row 0: 3 chars from {.,X,O}>
//   <row 1: 3 chars>
//   <row 2: 3 chars>
//
// Output:
//
//   <row> <col>                ← two ints, space-separated
//
// See `docs/wire-codegen.md` for the plan to autogenerate this header
// from a shared schema once the count of games justifies it.

#pragma once

#include "tictactoe_defs.h"

#include <cstdint>
#include <iostream>
#include <span>
#include <string>

namespace cgio {

inline char to_char(Cell c) {
    switch (c) {
        case Cell::Empty: return '.';
        case Cell::X:     return 'X';
        case Cell::O:     return 'O';
    }
    return '?';
}

inline bool from_char(char c, Cell& out) {
    switch (c) {
        case '.': out = Cell::Empty; return true;
        case 'X': out = Cell::X;     return true;
        case 'O': out = Cell::O;     return true;
        default:  return false;
    }
}

inline std::ostream& operator<<(std::ostream& out, Cell c) {
    return out << to_char(c);
}

inline std::istream& operator>>(std::istream& in, Pos& p) {
    return in >> p.row >> p.col;
}

inline std::ostream& operator<<(std::ostream& out, const Pos& p) {
    return out << p.row << ' ' << p.col;
}

inline std::ostream& operator<<(std::ostream& out, const TurnOutput& o) {
    return out << o.pos;
}

/// Borrowed view shared between the owning `TurnInput` (subprocess
/// transport) and the cbindgen-generated `::TurnInputFFI` (plugin
/// transport). Bot logic should take `const TurnRef&` so the same
/// `decide(...)` function works in both transports — mirrors the
/// `TurnRef`/`as_ref` pattern on the Rust side.
struct TurnRef {
    int32_t                          player_number;
    std::span<const Cell, BOARD_CELLS> board;
};

/// Owning C++ form of the per-tick input — the FFI-facing `TurnInputFFI`
/// is a borrowed view, which doesn't fit the subprocess transport. This
/// type is what subprocess bots actually read from stdin.
struct TurnInput {
    int32_t player_number;
    Cell    board[BOARD_CELLS];

    TurnRef as_ref() const {
        return TurnRef{player_number, std::span<const Cell, BOARD_CELLS>(board)};
    }
};

/// Borrowed view of the cbindgen FFI struct. Free function (not a method)
/// because `::TurnInputFFI` is regenerated and we can't add members to it.
inline TurnRef as_ref(const ::TurnInputFFI& ffi) {
    return TurnRef{
        ffi.player_number,
        std::span<const Cell, BOARD_CELLS>(ffi.board, BOARD_CELLS),
    };
}

inline std::istream& operator>>(std::istream& in, TurnInput& v) {
    if (!(in >> v.player_number)) return in;
    in.ignore();  // consume the newline after `player_number`

    std::string row;
    for (uintptr_t r = 0; r < BOARD_SIZE; ++r) {
        if (!std::getline(in, row)) return in;
        if (row.size() < BOARD_SIZE) {
            in.setstate(std::ios::failbit);
            return in;
        }
        for (uintptr_t c = 0; c < BOARD_SIZE; ++c) {
            if (!from_char(row[c], v.board[r * BOARD_SIZE + c])) {
                in.setstate(std::ios::failbit);
                return in;
            }
        }
    }
    return in;
}

}  // namespace cgio
