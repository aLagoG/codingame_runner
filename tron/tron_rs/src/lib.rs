use tron_defs::{BotStatus, TurnInputFFI, TurnOutput, TurnRef, TurnResult};

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    eprintln!(
        "players={} me={} lines={}",
        turn.number_of_players,
        turn.player_number,
        turn.player_lines.len()
    );
    TurnOutput::default()
}

// FFI shim. A panic in `decide` unwinding across `extern "C"` would be UB,
// so we catch it and report it through the `TurnResult` status.
#[unsafe(no_mangle)]
pub extern "C" fn take_turn(input: TurnInputFFI<'_>) -> TurnResult {
    match std::panic::catch_unwind(|| decide(input.as_ref())) {
        Ok(output) => TurnResult {
            status: BotStatus::Ok,
            output,
        },
        Err(_) => TurnResult {
            status: BotStatus::Panic,
            // Placeholder — the runner must ignore `output` when status != Ok.
            output: TurnOutput::default(),
        },
    }
}
