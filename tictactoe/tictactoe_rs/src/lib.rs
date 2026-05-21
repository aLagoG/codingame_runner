use tictactoe_defs::{BOARD_SIZE, Cell, Pos, TurnOutput, TurnRef};

/// Trivial bot: take the center if free, otherwise the first empty cell.
pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    let center = (BOARD_SIZE * BOARD_SIZE) / 2;
    let idx = if turn.board[center] == Cell::Empty {
        center
    } else {
        turn.board
            .iter()
            .position(|&c| c == Cell::Empty)
            .unwrap_or(0)
    };
    TurnOutput {
        pos: Pos {
            row: (idx / BOARD_SIZE) as i32,
            col: (idx % BOARD_SIZE) as i32,
        },
    }
}

common::ffi_bot!(tictactoe_defs::Ffi, decide);
