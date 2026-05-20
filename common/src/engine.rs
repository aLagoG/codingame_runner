use std::{
    io::{self, BufReader, Write},
    marker::PhantomData,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use libloading::{Library, Symbol};
use tracing::warn;

use crate::{ReadFrom, WriteTo};

pub type PlayerId = u32;

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("player panicked")]
    Panic,
    #[error("player produced malformed output: {0}")]
    InvalidOutput(String),
    #[error("player timed out")]
    Timeout,
    #[error("io error talking to player: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    #[error("player {0} failed to initialize")]
    PlayerInit(PlayerId, #[source] PlayerError),
    #[error("player {0} failed during turn")]
    PlayerPlay(PlayerId, #[source] PlayerError),
}

/// A `Game` is the rules + state for a match. Generic over its I/O types so the
/// same runner can drive any CodinGame-style game.
pub trait Game: Sized {
    /// One-time per-player input sent before the match starts (e.g. world
    /// parameters). Use `()` if not needed.
    type InitialInput: ReadFrom + WriteTo;

    /// Per-turn input sent to each active player.
    type Input: ReadFrom + WriteTo;

    /// Per-turn output collected from each active player.
    type Output: ReadFrom + WriteTo;

    /// Final result of the match (winner, scores, …).
    type Outcome;

    /// Per-tick snapshot of game state for replay / visualization.
    type ReplayFrame;

    fn new(num_players: u32, seed: u64) -> Self;

    fn initial_input(&self, player: PlayerId) -> Self::InitialInput;
    fn input_for(&self, player: PlayerId) -> Self::Input;

    /// Apply this tick's outputs and advance state. `outputs[i] == None` means
    /// player `i` either wasn't active this tick or failed (eliminated /
    /// crashed / aborted). Returns `Some(outcome)` when the match has ended.
    fn step(&mut self, outputs: &[Option<Self::Output>]) -> Option<Self::Outcome>;

    /// Players who still need to submit a move this tick.
    /// Simultaneous games return many; sequential games return one.
    fn active_players(&self) -> &[PlayerId];

    /// Snapshot of state, captured by the runner after each tick.
    fn snapshot(&self) -> Self::ReplayFrame;
}

/// Anything that can act as a player for game `G`. Implemented by the FFI
/// plugin wrapper and the subprocess wrapper.
pub trait Player<G: Game> {
    fn initialize(&mut self, input: &G::InitialInput) -> Result<(), PlayerError>;
    fn take_turn(&mut self, input: &G::Input) -> Result<G::Output, PlayerError>;
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    /// If true, the first player error fails the whole match. If false, the
    /// failing player just submits `None` for that tick — the game decides
    /// what that means (elimination, no-op, etc.).
    pub abort_on_player_error: bool,
    /// If false, the runner never calls `Game::snapshot` and `MatchResult::replay`
    /// stays empty. Skip when running tournaments where only the outcome matters.
    pub record_replay: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            abort_on_player_error: false,
            record_replay: true,
        }
    }
}

// Monomorphized per `G`; `size_of` is a const expression, so when
// `G::ReplayFrame` is zero-sized the whole branch folds to `false` at compile
// time and the snapshot call is elided.
fn should_record_replay<G: Game>(config: &RunConfig) -> bool {
    config.record_replay && std::mem::size_of::<G::ReplayFrame>() > 0
}

#[derive(Debug, Default, Clone)]
pub struct PlayerStats {
    pub turn_times: Vec<Duration>,
}

impl PlayerStats {
    pub fn average(&self) -> Option<Duration> {
        if self.turn_times.is_empty() {
            None
        } else {
            Some(self.turn_times.iter().sum::<Duration>() / self.turn_times.len() as u32)
        }
    }

    pub fn max(&self) -> Option<Duration> {
        self.turn_times.iter().max().copied()
    }
}

pub struct MatchResult<G: Game> {
    pub outcome: G::Outcome,
    pub stats: Vec<PlayerStats>,
    pub replay: Vec<G::ReplayFrame>,
}

pub fn run_match<G: Game>(
    mut game: G,
    mut players: Vec<Box<dyn Player<G>>>,
    config: RunConfig,
) -> Result<MatchResult<G>, MatchError> {
    let mut stats: Vec<PlayerStats> = (0..players.len()).map(|_| PlayerStats::default()).collect();
    let mut replay: Vec<G::ReplayFrame> = Vec::new();

    // Per-player one-time init.
    for (i, player) in players.iter_mut().enumerate() {
        let initial = game.initial_input(i as PlayerId);
        if let Err(e) = player.initialize(&initial) {
            if config.abort_on_player_error {
                return Err(MatchError::PlayerInit(i as PlayerId, e));
            }
            warn!("Player {i} failed to initialize");
        }
    }

    if should_record_replay::<G>(&config) {
        replay.push(game.snapshot());
    }

    loop {
        let active = game.active_players();
        let mut outputs: Vec<Option<G::Output>> = (0..players.len()).map(|_| None).collect();

        for &p in active {
            let input = game.input_for(p);
            let start = Instant::now();
            let result = players[p as usize].take_turn(&input);
            stats[p as usize].turn_times.push(start.elapsed());

            match result {
                Ok(out) => outputs[p as usize] = Some(out),
                Err(e) => {
                    if config.abort_on_player_error {
                        return Err(MatchError::PlayerPlay(p, e));
                    }
                    // Leave outputs[p as usize] = None; the game decides.
                }
            }
        }

        let outcome = game.step(&outputs);
        if should_record_replay::<G>(&config) {
            replay.push(game.snapshot());
        }

        if let Some(outcome) = outcome {
            return Ok(MatchResult {
                outcome,
                stats,
                replay,
            });
        }
    }
}

/// A `Player` backed by a spawned subprocess that talks over stdin/stdout.
pub struct SubprocessPlayer<G>
where
    G: Game,
    G::InitialInput: WriteTo,
    G::Input: WriteTo,
    G::Output: ReadFrom,
{
    // Order matters for drop: stdin/stdout drop before the child handle so the
    // pipes close cleanly first, then the child is reaped.
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    child: Child,
    _marker: PhantomData<G>,
}

impl<G: Game> SubprocessPlayer<G> {
    pub fn spawn(cmd: &mut Command) -> io::Result<Self> {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let stdin = child.stdin.take().expect("piped stdin missing");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout missing"));
        Ok(Self {
            stdin,
            stdout,
            child,
            _marker: PhantomData,
        })
    }
}

impl<G: Game> Drop for SubprocessPlayer<G> {
    fn drop(&mut self) {
        // Best-effort: tell the child to shut down. We don't `wait()` here —
        // a slow or hung bot shouldn't block us. Caller can poll `child` if
        // they need exit status.
        let _ = self.child.kill();
    }
}

impl<G: Game> Player<G> for SubprocessPlayer<G> {
    fn initialize(&mut self, input: &G::InitialInput) -> Result<(), PlayerError> {
        input.write_to(&mut self.stdin)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn take_turn(&mut self, input: &G::Input) -> Result<G::Output, PlayerError> {
        input.write_to(&mut self.stdin)?;
        self.stdin.flush()?;
        G::Output::read_from(&mut self.stdout)
            .map_err(|e| PlayerError::InvalidOutput(e.to_string()))
    }
}

/// A `Game` that can be played by a bot loaded from a dynamic library.
/// Implementations describe the FFI symbol name + signature and how to
/// convert between the game's `Input`/`Output` and the C-ABI types the bot
/// expects. With this trait, `PluginPlayer<G>` becomes fully generic.
pub trait FfiGame: Game {
    /// The `extern "C"` function pointer type the bot exports for per-turn
    /// play. Typically `for<'a> unsafe extern "C" fn(SomeInputFFI<'a>) -> SomeResult`.
    /// Must be `Copy` because libloading hands you a `Symbol<T>` that derefs
    /// to `&T`, and we cache the bare pointer in `PluginPlayer`.
    type Symbol: Copy;

    /// The `extern "C"` function pointer type for one-time initialization.
    /// Looked up optionally — if the bot doesn't export `INIT_SYMBOL_NAME`,
    /// `PluginPlayer::initialize` is a no-op. Games whose `InitialInput = ()`
    /// can declare any dummy `unsafe extern "C" fn()` here.
    type InitSymbol: Copy;

    /// Per-turn symbol name.
    const SYMBOL_NAME: &'static [u8];

    /// One-time init symbol name. Missing from the bot library is fine.
    const INIT_SYMBOL_NAME: &'static [u8];

    /// SAFETY: `sym` must be a valid pointer to the bot's exported per-turn
    /// symbol, obtained by loading `SYMBOL_NAME` from a compatible dynamic
    /// library, and the bot must uphold its UB contracts (no unwinding past
    /// the FFI boundary, no out-of-bounds reads on input pointers, …).
    unsafe fn call(sym: Self::Symbol, input: &Self::Input) -> Result<Self::Output, PlayerError>;

    /// SAFETY: `sym` must be a valid pointer to the bot's exported init
    /// symbol, obtained by loading `INIT_SYMBOL_NAME` from a compatible
    /// dynamic library, with the same UB-contract requirements as `call`.
    unsafe fn call_init(
        sym: Self::InitSymbol,
        input: &Self::InitialInput,
    ) -> Result<(), PlayerError>;
}

pub struct PluginPlayer<G: FfiGame> {
    sym: G::Symbol,
    init_sym: Option<G::InitSymbol>,
    _lib: Library,
}

impl<G: FfiGame> PluginPlayer<G> {
    /// SAFETY: `path` must point to a dynamic library exporting `G::SYMBOL_NAME`
    /// with a type matching `G::Symbol`, and that symbol must uphold the
    /// game's FFI contracts. If `G::INIT_SYMBOL_NAME` is also exported, it
    /// must match `G::InitSymbol`.
    pub unsafe fn load(path: &Path) -> anyhow::Result<Self> {
        let lib = unsafe { Library::new(path) }?;
        let symbol: Symbol<G::Symbol> = unsafe { lib.get(G::SYMBOL_NAME) }?;
        let sym = *symbol;
        // Init symbol is optional — bot doesn't have to export it.
        let init_sym = match unsafe { lib.get::<G::InitSymbol>(G::INIT_SYMBOL_NAME) } {
            Ok(s) => Some(*s),
            Err(_) => None,
        };
        Ok(PluginPlayer {
            sym,
            init_sym,
            _lib: lib,
        })
    }
}

impl<G: FfiGame> Player<G> for PluginPlayer<G> {
    fn initialize(&mut self, input: &G::InitialInput) -> Result<(), PlayerError> {
        match self.init_sym {
            // SAFETY: `init_sym` came from `PluginPlayer::load`, whose contract
            // matches `FfiGame::call_init`'s preconditions.
            Some(sym) => unsafe { G::call_init(sym, input) },
            None => Ok(()),
        }
    }

    fn take_turn(&mut self, input: &G::Input) -> Result<G::Output, PlayerError> {
        // SAFETY: `self.sym` was obtained by `PluginPlayer::load`, whose
        // safety contract is exactly what `FfiGame::call` requires.
        unsafe { G::call(self.sym, input) }
    }
}
