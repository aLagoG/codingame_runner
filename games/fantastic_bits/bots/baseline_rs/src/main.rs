use std::io::{self, Write};

use common::{ReadFrom, WireInput, WriteTo};
use fantastic_bits_defs::{InitialInput, TurnInput};
use fantastic_bits_baseline_rs::{decide, on_init};

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    // One-off init read — matches the CodinGame protocol: a single
    // integer `my_team_id` on its own line before the per-turn loop.
    let init = InitialInput::read_from(&mut input)?;
    on_init(init.as_ref());
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(turn.as_ref()).write_to(&mut output)?;
        output.flush()?;
    }
}
