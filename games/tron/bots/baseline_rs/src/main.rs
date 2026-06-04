use std::io::{self, Write};

use bot_common::{ReadFrom, WriteTo};
use tron_baseline_rs::{decide, on_init};
use tron_defs::{InitialInput, TurnInput};

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    #[allow(clippy::let_unit_value)]
    let init = InitialInput::read_from(&mut input)?;
    on_init(&init);
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(&turn).write_to(&mut output)?;
        output.flush()?;
    }
}
