use std::io::{self, Write};

use bot_common::{BotResult, ReadFrom, WriteTo};
use fantastic_bits_baseline_rs::{decide, on_init};
use fantastic_bits_defs::{InitialInput, TurnInput};

fn main() -> BotResult<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());

    // Signal readiness to the runner so it can stop sleeping and start
    // measuring turn-1 latency from a clean baseline. Emitted *after*
    // the stdin/stdout lock acquisition so that setup cost is absorbed
    // into the spawn-time measurement rather than billed to turn 1.
    // Any line on stderr counts; the runner drops the content and
    // forwards subsequent eprintln!s as usual.
    eprintln!("READY");
    let init = InitialInput::read_from(&mut input)?;
    on_init(&init);
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(&turn).write_to(&mut output)?;
        output.flush()?;
    }
}
