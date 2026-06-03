use std::{fmt::Debug, path::PathBuf};

use anyhow::{Result, bail};
use clap::Parser;
use codingame_runner::{for_each_game, make_player};
use common::engine::{Game, MatchResult, Player, RunConfig, run_match, write_replay};

#[derive(Parser)]
#[command(
    about = "Run a CodinGame-style match between two or more bots.",
    long_about = "Each bot is a standalone binary; the runner spawns it as a \
                  subprocess and talks the game's wire format over stdin/stdout."
)]
struct Args {
    /// Which game to run.
    #[arg(long, default_value = "tron")]
    game: String,

    /// Write a compact replay (seed + per-tick outputs) to this path.
    #[arg(long, value_name = "PATH")]
    save_replay: Option<PathBuf>,

    /// Bot binaries — one per player.
    #[arg(required = true)]
    bots: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let game = args.game.as_str();
    macro_rules! dispatch {
        ($name:literal, $ty:ty) => {
            if game == $name {
                return run_for_game::<$ty>(args.bots, args.save_replay);
            }
        };
    }
    for_each_game!(dispatch);
    bail!("unknown game: {game}");
}

fn run_for_game<G: Game>(paths: Vec<PathBuf>, save_replay: Option<PathBuf>) -> Result<()>
where
    G::Outcome: Debug,
{
    use anyhow::Context;

    let num_players = paths.len() as u32;
    let seed: u64 = 0;

    let mut players: Vec<Player<G>> = Vec::with_capacity(paths.len());
    for path in &paths {
        players.push(make_player::<G>(path)?);
    }

    let MatchResult {
        outcome,
        stats,
        replay,
        ..
    } = run_match::<G>(num_players, seed, players, RunConfig::default())?;

    println!("outcome: {outcome:?}");
    println!("ticks: {}", replay.outputs.len());
    for (i, s) in stats.iter().enumerate() {
        println!(
            "player {i}: {} turns, avg {:?}, max {:?}",
            s.turn_times.len(),
            s.average(),
            s.max()
        );
    }

    if let Some(path) = save_replay {
        let mut file = std::fs::File::create(&path)
            .with_context(|| format!("creating replay file {}", path.display()))?;
        write_replay::<G>(&replay, &mut file)
            .with_context(|| format!("writing replay to {}", path.display()))?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        println!("saved replay ({size} bytes) to {}", path.display());
    }

    Ok(())
}
