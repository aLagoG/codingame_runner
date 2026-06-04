use tron_defs::{InitialInput, TurnInput, TurnOutput};

#[derive(Default)]
pub struct GameState {}

// Tron has no per-match init payload (`InitialInput = ()`); this is
// a no-op kept for shape symmetry with init-shipping games like
// fantastic_bits.
pub fn on_init(_init: &InitialInput, _state: &mut GameState) {}

pub fn decide(turn: &TurnInput, _state: &mut GameState) -> TurnOutput {
    eprintln!(
        "players={} me={} lines={}",
        turn.number_of_players,
        turn.player_number,
        turn.player_lines.len()
    );
    TurnOutput::default()
}
