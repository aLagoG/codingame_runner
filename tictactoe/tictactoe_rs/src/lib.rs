use tictactoe_defs::{
    BOARD_SIZE, BotStatus, Cell, Pos, TurnInputFFI, TurnOutput, TurnRef, TurnResult,
};

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

// FFI shim. A panic unwinding across `extern "C"` would be UB, so we catch
// it and report it through `TurnResult::status`.
#[unsafe(no_mangle)]
pub extern "C" fn take_turn(input: TurnInputFFI<'_>) -> TurnResult {
    match std::panic::catch_unwind(|| decide(input.as_ref())) {
        Ok(output) => TurnResult {
            status: BotStatus::Ok,
            output,
        },
        Err(_) => TurnResult {
            status: BotStatus::Panic,
            // Placeholder — runner must ignore `output` when status != Ok.
            output: TurnOutput::default(),
        },
    }
}
