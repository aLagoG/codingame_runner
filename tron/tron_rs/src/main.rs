use tron_defs::{TurnInput};
use common::{ReadFrom, WriteTo};
use tron_rs::take_turn;
use std::io::{self, Write};

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop{
        let turn = TurnInput::read_from(&mut input)?; 
        let decision = take_turn(turn.as_ffi());       
        decision.write_to(&mut output)?;
        output.flush()?;
    }
}
