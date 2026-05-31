use tron_defs::{TurnOutput, TurnRef};

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    eprintln!(
        "players={} me={} lines={}",
        turn.number_of_players,
        turn.player_number,
        turn.player_lines.len()
    );
    // Demo counters — shows the Rust-side `emit_counter` flow.
    // When the runner enables counters (--counters), these
    // surface in the tournament report; otherwise they're a cheap
    // no-op (one atomic load + null check).
    common::emit_counter("players_alive", turn.number_of_players as f64);
    common::emit_counter("my_seat", turn.player_number as f64);
    TurnOutput::default()
}

common::ffi_bot!(tron_defs::Ffi, decide);
