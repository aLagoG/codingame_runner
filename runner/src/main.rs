use std::{env, ffi::OsString, fmt::Debug, path::PathBuf, process, process::Command};

use anyhow::{Context, Result};
use common::engine::{
    FfiGame, MatchResult, Player, PluginPlayer, RunConfig, SubprocessPlayer, run_match,
};
use serde::Serialize;
use tictactoe_game::TicTacToeGame;
use tron_game::TronGame;

struct Args {
    game: String,
    save_replay: Option<PathBuf>,
    bots: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = parse_args();
    match args.game.as_str() {
        "tron" => run_for_game::<TronGame>(args.bots, args.save_replay),
        "tictactoe" => run_for_game::<TicTacToeGame>(args.bots, args.save_replay),
        other => fail(&format!("unknown game: {other}")),
    }
}

fn parse_args() -> Args {
    let mut argv: Vec<OsString> = env::args_os().skip(1).collect();
    let mut game = "tron".to_string();
    let mut save_replay: Option<PathBuf> = None;

    while let Some(first) = argv.first() {
        if first == "--game" {
            argv.remove(0);
            game = argv
                .first()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| fail("--game needs a value"));
            argv.remove(0);
        } else if first == "--save-replay" {
            argv.remove(0);
            save_replay = Some(PathBuf::from(
                argv.first().cloned().unwrap_or_else(|| fail("--save-replay needs a path")),
            ));
            argv.remove(0);
        } else {
            break;
        }
    }

    let bots: Vec<PathBuf> = argv.into_iter().map(PathBuf::from).collect();
    if bots.is_empty() {
        fail("no bots given");
    }
    Args { game, save_replay, bots }
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    eprintln!(
        "usage: codingame_runner [--game tron|tictactoe] [--save-replay <path>] <bot1> [bot2 ...]"
    );
    eprintln!("  bot is a dynamic library (.so/.dylib/.dll) or a subprocess binary");
    process::exit(2);
}

fn run_for_game<G: FfiGame + 'static>(
    paths: Vec<PathBuf>,
    save_replay: Option<PathBuf>,
) -> Result<()>
where
    G::Outcome: Debug,
    G::Output: Serialize,
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
        let bytes = bincode::serialize(&replay).context("serializing replay")?;
        std::fs::write(&path, &bytes)
            .with_context(|| format!("writing replay to {}", path.display()))?;
        println!("saved replay ({} bytes) to {}", bytes.len(), path.display());
    }

    Ok(())
}

fn is_plugin(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("so") | Some("dylib") | Some("dll")
    )
}
