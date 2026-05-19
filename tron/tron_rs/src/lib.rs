use tron_defs::{TurnInputFFI, TurnOutput};

#[unsafe(no_mangle)]
pub extern "C" fn take_turn(input: TurnInputFFI) -> TurnOutput {
    eprintln!("{}", input);
    TurnOutput {
        direction: tron_defs::Direction::Down,
    }
}
