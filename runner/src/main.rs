use std::{env, io::{self, Write}, path::PathBuf, process};

use anyhow::{Context, Result};
use codingame_runner::Bot;
use common::{ReadFrom, WriteTo};
use tron_defs::TurnInput;

fn main() -> Result<()> {
    let bot_path = match env::args_os().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: codingame_runner <path-to-bot.{{so,dylib,dll}}>");
            process::exit(2);
        }
    };

    let bot = unsafe { Bot::load(&bot_path) }
        .with_context(|| format!("loading bot from {}", bot_path.display()))?;

    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        bot.run_turn(&turn).write_to(&mut output)?;
        output.flush()?;
    }
}
