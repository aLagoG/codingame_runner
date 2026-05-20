use std::{env, path::PathBuf, process, process::Command};

use anyhow::{Context, Result};
use common::engine::{
    run_match, Game, MatchResult, Player, PluginPlayer, RunConfig, SubprocessPlayer,
};
use tron_game::TronGame;

fn main() -> Result<()> {
    let paths: Vec<PathBuf> = env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: codingame_runner <bot1> [bot2 ...]");
        eprintln!("  bot is a dynamic library (.so/.dylib/.dll) or a subprocess binary");
        process::exit(2);
    }

    let num_players = paths.len() as u32;
    let game = TronGame::new(num_players, 0);

    let mut players: Vec<Box<dyn Player<TronGame>>> = Vec::with_capacity(paths.len());
    for path in &paths {
        let player: Box<dyn Player<TronGame>> = if is_plugin(path) {
            Box::new(
                unsafe { PluginPlayer::<TronGame>::load(path) }
                    .with_context(|| format!("loading plugin {}", path.display()))?,
            )
        } else {
            Box::new(
                SubprocessPlayer::<TronGame>::spawn(&mut Command::new(path))
                    .with_context(|| format!("spawning subprocess {}", path.display()))?,
            )
        };
        players.push(player);
    }

    let MatchResult {
        outcome,
        stats,
        replay,
    } = run_match(game, players, RunConfig::default())?;

    println!("outcome: {outcome:?}");
    println!("ticks: {}", replay.len().saturating_sub(1));
    for (i, s) in stats.iter().enumerate() {
        println!(
            "player {i}: {} turns, avg {:?}, max {:?}",
            s.turn_times.len(),
            s.average(),
            s.max()
        );
    }

    Ok(())
}

fn is_plugin(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("so") | Some("dylib") | Some("dll")
    )
}
