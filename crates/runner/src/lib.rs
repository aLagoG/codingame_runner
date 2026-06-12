//! Library surface for the runner. The binary in `main.rs` is a thin
//! CLI wrapper; programmatic users (the `tournament` crate, future
//! tooling) call into here directly so they don't have to re-exec the
//! binary per match.
//!
//! Single source of truth for which games this build knows about
//! lives in [`for_each_game!`] below. The runner's CLI dispatch and
//! the tournament's `run_match_named` both expand the macro instead
//! of carrying parallel match arms — adding a new game is one line
//! here, and `xtask new-game` patches exactly that line.

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use common::engine::{Game, Player};

/// Build a `Player<G>` from a bot binary path. Spawns the binary as a
/// child process; the engine talks to it over stdin/stdout in the
/// wire format defined by `G::Input` / `G::Output`.
pub fn make_player<G: Game>(path: &Path) -> Result<Player<G>> {
    Player::<G>::spawn(&mut Command::new(path))
        .with_context(|| format!("spawning subprocess {}", path.display()))
}

/// Invoke `$cb!("<name>", <Game type>)` once per game compiled into
/// this build. Callers use it like a `match` table — see
/// `crates/runner/src/main.rs` and `crates/tournament/src/lib.rs`
/// for the pattern.
///
/// `__games::*` re-exports each game crate's `Game` type so callers
/// don't have to add per-game deps themselves; runner already has
/// them as direct deps.
#[macro_export]
macro_rules! for_each_game {
    ($cb:ident) => {
        $cb!("tron", $crate::__games::TronGame);
        $cb!("fantastic_bits", $crate::__games::FantasticBitsGame);
        $cb!("spider_attack", $crate::__games::SpiderAttackGame);
    };
}

#[doc(hidden)]
pub mod __games {
    pub use fantastic_bits_game::FantasticBitsGame;
    pub use tron_game::TronGame;
    pub use spider_attack_game::SpiderAttackGame;
}
