use std::io::{self, Write};

use common::{ReadFrom, WriteTo};
use tron_defs::TurnInput;
use tron_rs::decide;

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(turn.as_ref()).write_to(&mut output)?;
        output.flush()?;
    }
}
