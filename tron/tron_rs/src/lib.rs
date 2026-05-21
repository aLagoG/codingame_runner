use tron_defs::{TurnOutput, TurnRef};

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    eprintln!(
        "players={} me={} lines={}",
        turn.number_of_players,
        turn.player_number,
        turn.player_lines.len()
    );
    TurnOutput::default()
}

common::ffi_bot!(tron_defs::Ffi, decide);
