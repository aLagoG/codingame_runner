//! Library surface for the runner. The binary in `main.rs` is a thin
//! CLI wrapper; programmatic users (the `tournament` crate, future
//! tooling) call into here directly so they don't have to re-exec the
//! binary per match.

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use common::engine::{FfiGame, Player, PluginPlayer, SubprocessPlayer};

/// True if `path` looks like a dynamic library we can `dlopen` as an
/// FFI plugin (`.so` / `.dylib` / `.dll`). Anything else is treated
/// as a subprocess bot.
pub fn is_plugin(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("so") | Some("dylib") | Some("dll")
    )
}

/// Build a `Player<G>` from a filesystem path, picking the FFI plugin
/// loader or the subprocess spawner based on the extension. Used by
/// both the runner binary and the tournament harness.
///
/// # Safety
/// On the plugin path this calls `PluginPlayer::load`, which is
/// `unsafe` because it `dlopen`s a foreign library; the
/// ABI-version handshake inside `load` is what makes it sound to call
/// from safe code here. See `PluginPlayer::load` for the contract.
pub fn make_player<G: FfiGame + 'static>(path: &Path) -> Result<Box<dyn Player<G>>> {
    if is_plugin(path) {
        let player = unsafe { PluginPlayer::<G>::load(path) }
            .with_context(|| format!("loading plugin {}", path.display()))?;
        Ok(Box::new(player))
    } else {
        let player = SubprocessPlayer::<G>::spawn(&mut Command::new(path))
            .with_context(|| format!("spawning subprocess {}", path.display()))?;
        Ok(Box::new(player))
    }
}
