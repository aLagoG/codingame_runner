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

// FFI shim. Panics in `decide` unwinding across `extern "C"` would be UB, so we
// catch and fall back to a default move. SAFETY contract on the input pointer
// is documented on `TurnInputFFI::as_ref`.
#[unsafe(no_mangle)]
pub extern "C" fn take_turn(input: TurnInputFFI) -> TurnOutput {
    std::panic::catch_unwind(|| decide(unsafe { input.as_ref() })).unwrap_or(TurnOutput {
        direction: Direction::Down,
    })
}
