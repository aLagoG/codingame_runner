use std::{fmt::Debug, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use clap::Parser;
use common::engine::{
    FfiGame, MatchResult, Player, PluginPlayer, RunConfig, SubprocessPlayer, run_match,
    write_replay,
};
use tictactoe_game::TicTacToeGame;
use tron_game::TronGame;

#[derive(Parser)]
#[command(
    about = "Run a CodinGame-style match between two or more bots.",
    long_about = "Bots can be either dynamic libraries (.so/.dylib/.dll, \
                  loaded via FFI) or standalone binaries (spawned as a \
                  subprocess that talks over stdin/stdout)."
)]
struct Args {
    /// Which game to run.
    #[arg(long, default_value = "tron")]
    game: String,

    /// Write a compact replay (seed + per-tick outputs) to this path.
    #[arg(long, value_name = "PATH")]
    save_replay: Option<PathBuf>,

    /// Bot binaries or dynamic libraries. One per player.
    #[arg(required = true)]
    bots: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.game.as_str() {
        "tron" => run_for_game::<TronGame>(args.bots, args.save_replay),
        "tictactoe" => run_for_game::<TicTacToeGame>(args.bots, args.save_replay),
        other => bail!("unknown game: {other} (expected `tron` or `tictactoe`)"),
    }
}

fn run_for_game<G: FfiGame + 'static>(
    paths: Vec<PathBuf>,
    save_replay: Option<PathBuf>,
) -> Result<()>
where
    G::Outcome: Debug,
{
    let num_players = paths.len() as u32;
    let seed: u64 = 0;

    let mut players: Vec<Box<dyn Player<G>>> = Vec::with_capacity(paths.len());
    for path in &paths {
        let player: Box<dyn Player<G>> = if is_plugin(path) {
            Box::new(
                unsafe { PluginPlayer::<G>::load(path) }
                    .with_context(|| format!("loading plugin {}", path.display()))?,
            )
        } else {
            Box::new(
                SubprocessPlayer::<G>::spawn(&mut Command::new(path))
                    .with_context(|| format!("spawning subprocess {}", path.display()))?,
            )
        };
        players.push(player);
    }

    let MatchResult {
        outcome,
        stats,
        replay,
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

fn is_plugin(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("so") | Some("dylib") | Some("dll")
    )
}
