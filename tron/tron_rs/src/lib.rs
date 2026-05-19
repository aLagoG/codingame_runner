use tron_defs::{Direction, TurnInputFFI, TurnOutput, TurnRef};

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    eprintln!(
        "players={} me={} lines={}",
        turn.number_of_players,
        turn.player_number,
        turn.player_lines.len()
    );
    TurnOutput {
        direction: Direction::Down,
    }
}

// FFI shim. A panic in `decide` unwinding across `extern "C"` would be UB,
// so we catch and fall back to a default move.
#[unsafe(no_mangle)]
pub extern "C" fn take_turn(input: TurnInputFFI<'_>) -> TurnOutput {
    std::panic::catch_unwind(|| decide(input.as_ref())).unwrap_or(TurnOutput {
        direction: Direction::Down,
    })
}
