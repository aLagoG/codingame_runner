use std::{
    io::{self, BufRead, BufReader, Write},
    marker::PhantomData,
    panic::AssertUnwindSafe,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use libloading::{Library, Symbol};
use serde::{Deserialize, Serialize};
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
    #[error("player closed its output (eof)")]
    Eof,
    #[error("io error talking to player: {0}")]
    Io(#[from] io::Error),
}

impl PlayerError {
    /// True for failures that mean the player isn't going to recover — the
    /// engine should stop trying to call them.
    fn is_fatal(&self) -> bool {
        matches!(self, PlayerError::Eof | PlayerError::Io(_) | PlayerError::Panic)
    }
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
    /// Short stable identifier — written into replay file headers so loading
    /// the wrong game's replay errors loudly instead of decoding garbage.
    const NAME: &'static str;

    /// One-time per-player input sent before the match starts (e.g. world
    /// parameters). Use `()` if not needed.
    type InitialInput: ReadFrom + WriteTo;

    /// Per-turn input sent to each active player.
    type Input: ReadFrom + WriteTo;

    /// Per-turn output collected from each active player.
    type Output: ReadFrom + WriteTo;

    /// Final result of the match (winner, scores, …).
    type Outcome;

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
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            abort_on_player_error: false,
        }
    }
}

/// Compact recording of a finished match: the seed, the player count, and the
/// outputs each player submitted on each tick. Combined with a deterministic
/// [`Game`] implementation this is enough to reconstruct the whole match by
/// calling `Game::new(num_players, seed)` and replaying `outputs` through
/// `Game::step`.
///
/// `outputs` is indexed `[tick][player]`. The inner Vec is the same shape
/// `Game::step` consumes, with `None` for players that didn't move that tick
/// (inactive / failed).
///
/// Parameterised over the output type (not the whole `Game`) so the serde
/// bounds stay simple and the type lines up with file IO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replay<O> {
    pub seed: u64,
    pub num_players: u32,
    pub outputs: Vec<Vec<Option<O>>>,
}

/// Versioned framing for [`Replay`] files. Layout:
///
/// ```text
/// [8B magic "CGRREPLY"][4B u32 version, LE][1B name_len][name][bincode replay body]
/// ```
///
/// The header lets `read_replay` reject obviously-wrong files (the wrong
/// game's replay, or a future format we don't know how to parse) instead of
/// decoding garbage and emitting confusing errors three levels deep.
const REPLAY_MAGIC: &[u8; 8] = b"CGRREPLY";
const REPLAY_VERSION: u32 = 1;

/// Write a framed replay (magic + version + game name + bincoded body).
pub fn write_replay<G: Game>(replay: &Replay<G::Output>, w: &mut impl Write) -> anyhow::Result<()>
where
    G::Output: Serialize,
{
    use anyhow::Context;
    let name = G::NAME.as_bytes();
    anyhow::ensure!(name.len() <= u8::MAX as usize, "game name too long");

    w.write_all(REPLAY_MAGIC)?;
    w.write_all(&REPLAY_VERSION.to_le_bytes())?;
    w.write_all(&[name.len() as u8])?;
    w.write_all(name)?;
    bincode::serialize_into(w, replay).context("serializing replay body")?;
    Ok(())
}

/// Read a framed replay. Errors on bad magic, unknown version, or a header
/// game name that doesn't match `G::NAME`.
pub fn read_replay<G: Game>(r: &mut impl io::Read) -> anyhow::Result<Replay<G::Output>>
where
    G::Output: serde::de::DeserializeOwned,
{
    use anyhow::{Context, bail, ensure};

    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).context("reading replay magic")?;
    ensure!(&magic == REPLAY_MAGIC, "not a CodinGame replay file");

    let mut ver = [0u8; 4];
    r.read_exact(&mut ver).context("reading replay version")?;
    let version = u32::from_le_bytes(ver);
    ensure!(
        version == REPLAY_VERSION,
        "unsupported replay version: file is v{version}, runner reads v{REPLAY_VERSION}",
    );

    let mut name_len = [0u8; 1];
    r.read_exact(&mut name_len).context("reading game name length")?;
    let mut name_buf = vec![0u8; name_len[0] as usize];
    r.read_exact(&mut name_buf).context("reading game name")?;
    let name = std::str::from_utf8(&name_buf).context("game name not utf-8")?;
    if name != G::NAME {
        bail!("replay is for `{name}`, this binary plays `{}`", G::NAME);
    }

    bincode::deserialize_from(r).context("deserializing replay body")
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
    pub replay: Replay<G::Output>,
}

pub fn run_match<G: Game>(
    num_players: u32,
    seed: u64,
    mut players: Vec<Box<dyn Player<G>>>,
    config: RunConfig,
) -> Result<MatchResult<G>, MatchError> {
    let mut game = G::new(num_players, seed);
    let mut stats: Vec<PlayerStats> =
        (0..players.len()).map(|_| PlayerStats::default()).collect();
    let mut outputs_per_tick: Vec<Vec<Option<G::Output>>> = Vec::new();
    // Players that hit a fatal error (panic, EOF, IO). We stop calling them
    // entirely — otherwise we keep banging on a closed pipe / crashed plugin
    // and the timing stats fill with garbage.
    let mut dead: Vec<bool> = vec![false; players.len()];

    // Per-player one-time init.
    for (i, player) in players.iter_mut().enumerate() {
        let initial = game.initial_input(i as PlayerId);
        if let Err(e) = player.initialize(&initial) {
            if config.abort_on_player_error {
                return Err(MatchError::PlayerInit(i as PlayerId, e));
            }
            if e.is_fatal() {
                warn!("player {i} init failed fatally ({e}) — marking dead");
                dead[i] = true;
            } else {
                warn!("player {i} failed to initialize: {e}");
            }
        }
    }

    loop {
        let active = game.active_players();
        let mut outputs: Vec<Option<G::Output>> = (0..players.len()).map(|_| None).collect();

        for &p in active {
            if dead[p as usize] {
                continue;
            }
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
                    if e.is_fatal() {
                        warn!("player {p} {e} — marking dead");
                        dead[p as usize] = true;
                    }
                    // Leave outputs[p as usize] = None; the game decides.
                }
            }
        }

        let outcome = game.step(&outputs);
        outputs_per_tick.push(outputs);

        if let Some(outcome) = outcome {
            return Ok(MatchResult {
                outcome,
                stats,
                replay: Replay {
                    seed,
                    num_players,
                    outputs: outputs_per_tick,
                },
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
        // SIGKILL first, then reap. Without `wait()` the child becomes a zombie
        // and a long-running runner leaks PIDs. After `kill()` the wait should
        // be ~immediate; if it isn't, something's wrong and we log it.
        let _ = self.child.kill();
        let start = Instant::now();
        let _ = self.child.wait();
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            warn!("waiting for killed child took {elapsed:?}");
        }
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
        // Peek at the read buffer to distinguish EOF (child crashed / closed
        // its stdout) from a parse error on actual bytes. Without this both
        // collapse into `InvalidOutput` and the engine keeps calling a dead
        // bot forever.
        let buf = self.stdout.fill_buf()?;
        if buf.is_empty() {
            return Err(PlayerError::Eof);
        }
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

    /// The version of this game's FFI ABI the runner was built against. The
    /// plugin exports its own value through the `abi_version` symbol; load
    /// fails fast on mismatch. Bump in `_defs::ABI_VERSION` whenever a wire
    /// type changes shape.
    const ABI_VERSION: u32;

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
        use anyhow::{Context, bail};

        let lib = unsafe { Library::new(path) }?;

        // ABI handshake before we touch anything else: loading a plugin built
        // against an incompatible `_defs` and then calling its `take_turn` is
        // straight UB. Refusing here turns that into a friendly error.
        let abi: Symbol<unsafe extern "C" fn() -> u32> = unsafe { lib.get(b"abi_version") }
            .context("plugin missing `abi_version` symbol — was it built with the `*_bot!` macro?")?;
        let plugin_abi = unsafe { abi() };
        if plugin_abi != G::ABI_VERSION {
            bail!(
                "ABI mismatch loading {}: plugin reports v{plugin_abi}, runner expects v{}",
                path.display(),
                G::ABI_VERSION,
            );
        }

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
        let Some(sym) = self.init_sym else {
            return Ok(());
        };
        // SAFETY: `init_sym` came from `PluginPlayer::load`, whose contract
        // matches `FfiGame::call_init`'s preconditions.
        catch_into_player_err(AssertUnwindSafe(|| unsafe { G::call_init(sym, input) }))
    }

    fn take_turn(&mut self, input: &G::Input) -> Result<G::Output, PlayerError> {
        // SAFETY: `self.sym` was obtained by `PluginPlayer::load`, whose
        // safety contract is exactly what `FfiGame::call` requires.
        catch_into_player_err(AssertUnwindSafe(|| unsafe { G::call(self.sym, input) }))
    }
}

/// Defense in depth around the FFI call: bots SHOULD wrap their own panics
/// (the `*_bot!` macros do), but if one slips through and the Rust runtime
/// propagates it back to us, catching here turns it into a graceful
/// `PlayerError::Panic` instead of cross-FFI unwinding UB. Zero measurable
/// overhead on the no-panic path — see `docs/code-review.md` §4C.
fn catch_into_player_err<T>(
    f: AssertUnwindSafe<impl FnOnce() -> Result<T, PlayerError>>,
) -> Result<T, PlayerError> {
    match std::panic::catch_unwind(f) {
        Ok(r) => r,
        Err(_) => Err(PlayerError::Panic),
    }
}
