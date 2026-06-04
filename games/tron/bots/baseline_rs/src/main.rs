use std::io::{self, Write};

use bot_common::{ReadFrom, WriteTo};
use tron_baseline_rs::{GameState, decide, on_init};
use tron_defs::{InitialInput, TurnInput};

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());

    let mut state = GameState::default();
    on_init(&InitialInput::read_from(&mut input)?, &mut state);
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(&turn, &mut state).write_to(&mut output)?;
        output.flush()?;
    }
}
