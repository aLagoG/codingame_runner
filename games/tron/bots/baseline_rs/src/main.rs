use std::io::{self, Write};

use bot_common::{ReadFrom, WireInput, WriteTo};
use tron_baseline_rs::decide;
use tron_defs::TurnInput;

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(turn.as_ref()).write_to(&mut output)?;
        output.flush()?;
    }
}
