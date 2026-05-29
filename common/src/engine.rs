use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::CStr,
    io::{self, BufRead, BufReader, Write},
    marker::PhantomData,
    os::raw::c_char,
    panic::AssertUnwindSafe,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use libloading::{Library, Symbol};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// The RNG type every game receives in [`Game::new`]. Concrete (not
/// `<R: Rng>` generic) so the `Game` trait stays object-safe — we
/// don't use `Box<dyn Game>` today, but a generic method would
/// permanently rule it out. Games can still pass `&mut StdRng` to
/// anything that takes `impl Rng` internally.
pub type GameRng = StdRng;

/// Re-exported so callers that build a `GameRng` (the runner, viz,
/// tests) don't have to pull `rand` into their `Cargo.toml` just
/// for the `seed_from_u64` constructor.
pub use rand::SeedableRng as GameRngSeed;

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
        matches!(
            self,
            PlayerError::Eof | PlayerError::Io(_) | PlayerError::Panic
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    #[error("player {0} failed to initialize")]
    PlayerInit(PlayerId, #[source] PlayerError),
    #[error("player {0} failed during turn")]
    PlayerPlay(PlayerId, #[source] PlayerError),
}

/// Status byte returned by every bot's `take_turn` FFI call. Same shape for
/// every game — `Ok` means `TurnResult::output` is valid; `Panic` means the
/// bot's `catch_unwind` shim intercepted a panic and `output` is placeholder
/// data that the runner must ignore.
#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum BotStatus {
    Ok = 0,
    Panic = 1,
}

/// FFI return type of every bot's `take_turn`. Generic over the per-game
/// `O` (the game's `TurnOutput`), monomorphised by cbindgen into a concrete
/// C++ struct per game.
#[repr(C)]
#[derive(Debug)]
pub struct TurnResult<O> {
    pub status: BotStatus,
    pub output: O,
}

/// Contract on each game's owned per-tick input struct (the `TurnInput` in
/// each `_defs` crate).
///
/// Pairs the owned type with two views:
///   * `Ffi<'a>` — `#[repr(C)]` mirror sent across the plugin boundary.
///   * `Ref<'a>` — borrowed view bots read inside `decide(...)`.
///
/// Supertraits `ReadFrom + WriteTo` are what the engine uses to ferry the
/// input through subprocess pipes; the trait method pair is what the FFI
/// path uses. Implementing this trait is what makes a game's `TurnInput`
/// usable end-to-end. The cross-trait `Ffi<'a>::Ref = Self::Ref<'a>` bound
/// guarantees the same borrowed type comes out of both `as_ref` paths.
pub trait WireInput: ReadFrom + WriteTo {
    /// `#[repr(C)]` FFI mirror.
    type Ffi<'a>: WireInputFfi<'a, Ref = Self::Ref<'a>>
    where
        Self: 'a;

    /// Borrowed view passed to bot `decide` functions.
    type Ref<'a>
    where
        Self: 'a;

    /// Build the FFI mirror borrowing from `self`.
    fn as_ffi(&self) -> Self::Ffi<'_>;

    /// Build the borrowed view from `self`.
    fn as_ref(&self) -> Self::Ref<'_>;
}

/// FFI-side companion to [`WireInput`]. Bots receive `Self` from the runner
/// and call `as_ref` to get the borrowed view they actually read.
pub trait WireInputFfi<'a> {
    /// Borrowed view type — typically the same `Ref<'a>` as the owning
    /// [`WireInput`]'s associated `Ref<'a>`.
    type Ref;

    /// SAFETY: the impl relies on the invariants the FFI struct documents
    /// (pointers properly aligned, lengths in range, lifetime live). The
    /// trait wraps that in a safe method because every FFI struct's only
    /// constructor (`WireInput::as_ffi`) establishes them.
    fn as_ref(&self) -> Self::Ref;
}

/// Sentinel `InitialInput` for games that don't ferry per-player data at
/// match start. Real games either use this (typical) or define their own
/// `WireInput`-implementing struct. One padding byte makes the type
/// non-zero-sized — without it, the `improper_ctypes` lint would fire on
/// the per-game extern block because zero-sized types aren't FFI-safe.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoInitialInput {
    _padding: u8,
}

/// FFI mirror of [`NoInitialInput`]. Same one-byte layout.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoInitialInputFfi<'a> {
    _padding: u8,
    _marker: PhantomData<&'a NoInitialInput>,
}

/// Borrowed view of [`NoInitialInput`] — handed to bot init handlers.
/// Carries no semantic data; bots typically ignore the argument.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoInitialInputRef<'a> {
    _marker: PhantomData<&'a NoInitialInput>,
}

impl WireInput for NoInitialInput {
    type Ffi<'a> = NoInitialInputFfi<'a>;
    type Ref<'a> = NoInitialInputRef<'a>;

    fn as_ffi(&self) -> NoInitialInputFfi<'_> {
        NoInitialInputFfi {
            _padding: 0,
            _marker: PhantomData,
        }
    }

    fn as_ref(&self) -> NoInitialInputRef<'_> {
        NoInitialInputRef {
            _marker: PhantomData,
        }
    }
}

impl<'a> WireInputFfi<'a> for NoInitialInputFfi<'a> {
    type Ref = NoInitialInputRef<'a>;

    fn as_ref(&self) -> NoInitialInputRef<'a> {
        NoInitialInputRef {
            _marker: PhantomData,
        }
    }
}

impl ReadFrom for NoInitialInput {
    fn read_from(_r: &mut impl BufRead) -> anyhow::Result<Self> {
        Ok(NoInitialInput::default())
    }
}

impl WriteTo for NoInitialInput {
    fn write_to(&self, _w: &mut impl Write) -> io::Result<()> {
        Ok(())
    }
}

/// Bundled contract on each game's `TurnOutput`. Implementing this on your
/// `TurnOutput` is the single line that asserts — at the `_defs` crate site
/// — that the type satisfies every requirement the rest of the system will
/// eventually need:
///
///   * `ReadFrom + WriteTo` — stdio between runner and subprocess bots.
///   * `Default` — placeholder the `ffi_bot!` macro stores on the panic path.
///   * `Serialize + DeserializeOwned` — `Replay<TurnOutput>` round-trips.
///   * `'static` — implied by `DeserializeOwned`, made explicit for clarity.
///
/// No blanket impl: write `impl WireOutput for TurnOutput {}` explicitly so
/// missing pieces fail at *this* line instead of three crates downstream.
pub trait WireOutput:
    ReadFrom + WriteTo + Default + Serialize + serde::de::DeserializeOwned + 'static
{
}

/// Crate-level contract for a `_defs` crate: implement this on a unit
/// marker type (conventionally `Ffi`) to ratify that the crate exposes a
/// complete and consistent FFI surface.
///
/// ```ignore
/// // tron_defs/src/lib.rs
/// pub struct Ffi;
/// impl common::Defs for Ffi {
///     type Input = TurnInput;
///     type Output = TurnOutput;
///     const ABI_VERSION: u32 = ABI_VERSION;
/// }
/// ```
///
/// The single `impl Defs` line forces the compiler to check every required
/// trait (`WireInput`, `WireOutput`, transitively `WireInputFfi` via the
/// GAT) at this exact site — not three crates downstream. The bot-side
/// `ffi_bot!` macro and the runner-side `FfiGame` reach all the other types
/// they need by projecting through these associated types.
pub trait Defs {
    /// One-time per-player input sent before the match starts. Use `()`
    /// (which has a blanket `WireInput` impl) for games that don't need
    /// init data.
    type InitialInput: WireInput;

    /// Owned per-tick input type. Pulls in `WireInput` (which itself
    /// requires `ReadFrom + WriteTo`), and through that the FFI mirror
    /// and the borrowed view via GATs.
    type Input: WireInput;

    /// Per-tick output type. `WireOutput` bundles `ReadFrom + WriteTo +
    /// Default + Serialize + DeserializeOwned + 'static`.
    type Output: WireOutput;

    /// Plugin ABI version. Bumped on any wire-type change. The runner
    /// reads this at load time through the plugin's `abi_version()`
    /// symbol and refuses mismatches before any UB-prone call.
    const ABI_VERSION: u32;
}

/// A `Game` is the rules + state for a match. Generic over its I/O types so the
/// same runner can drive any CodinGame-style game.
pub trait Game: Sized {
    /// Short stable identifier — written into replay file headers so loading
    /// the wrong game's replay errors loudly instead of decoding garbage.
    const NAME: &'static str;

    /// One-time per-player input sent before the match starts (e.g. world
    /// parameters). Use `()` if not needed — `()` impls `WireInput`
    /// trivially.
    type InitialInput: WireInput;

    /// Per-turn input sent to each active player.
    type Input: WireInput;

    /// Per-turn output collected from each active player.
    type Output: WireOutput;

    /// Final result of the match (winner, scores, …).
    type Outcome;

    /// Build a fresh game. The runner builds the `rng` from a `u64`
    /// seed (see [`run_match`]) and persists that seed in the
    /// [`Replay`], so re-running a replay reconstructs the same
    /// RNG and produces the same game stream. Games consume the
    /// RNG however they want — store it as a field, draw eagerly,
    /// or ignore it entirely.
    fn new(num_players: u32, rng: &mut GameRng) -> Self;

    fn initial_input(&self, player: PlayerId) -> Self::InitialInput;
    fn input_for(&self, player: PlayerId) -> Self::Input;

    /// Apply this tick's outputs and advance state. `outputs[i] == None` means
    /// player `i` either wasn't active this tick or failed (eliminated /
    /// crashed / aborted). Returns `Some(outcome)` when the match has ended.
    fn step(&mut self, outputs: &[Option<Self::Output>]) -> Option<Self::Outcome>;

    /// Players who still need to submit a move this tick.
    /// Simultaneous games return many; sequential games return one.
    fn active_players(&self) -> &[PlayerId];

    /// Final rank of each player in a finished match, 1-indexed.
    /// `standings[i]` is player `i`'s rank — 1 is the winner,
    /// larger numbers are worse. Tied players share a rank using
    /// *competition ranking* (1, 1, 3 — not 1, 1, 2), so the rank
    /// gaps make the standings-pairwise Elo decomposition come
    /// out right. Returned vector has length `num_players` (one
    /// entry per player, in player-id order).
    ///
    /// For binary win/loss games (e.g. tic-tac-toe), the natural
    /// mapping is winner = rank 1, loser = rank 2, draw = all
    /// rank 1. For tron, the implementation tracks death tick and
    /// ranks survivors first, then dead players in reverse death
    /// order (later death = better rank, ties allowed).
    fn standings(outcome: &Self::Outcome) -> Vec<u32>;

    /// The unique winner of a finished match, or `None` if no single
    /// player came first (draw, tied survivors, all-mutual-death).
    /// Default impl derives from [`standings`] — games rarely need
    /// to override.
    fn winner(outcome: &Self::Outcome) -> Option<PlayerId> {
        let p = Self::standings(outcome);
        let mut firsts = p
            .iter()
            .enumerate()
            .filter(|&(_, &r)| r == 1)
            .map(|(i, _)| i as PlayerId);
        let first = firsts.next()?;
        if firsts.next().is_some() {
            None
        } else {
            Some(first)
        }
    }

    /// Per-player numeric scores from a finished match, in player-id
    /// order. `None` if score isn't a meaningful concept for this
    /// game (tic-tac-toe is binary — winner / not). `Some` for
    /// games that track a continuous metric: trail length in tron,
    /// points in a scored game, etc.
    ///
    /// Scores are *informational*; standings is the authoritative
    /// ranking. They exist so tournaments can surface tiebreakers
    /// like "both bots tied for 1st, but A averaged 80 cells claimed
    /// vs B's 40" — useful for tracking improvement across
    /// otherwise-identical outcomes.
    ///
    /// Default returns `None` so games opt in.
    fn scores(_outcome: &Self::Outcome) -> Option<Vec<f64>> {
        None
    }
}

/// Anything that can act as a player for game `G`. Implemented by the FFI
/// plugin wrapper and the subprocess wrapper.
pub trait Player<G: Game> {
    fn initialize(&mut self, input: &G::InitialInput) -> Result<(), PlayerError>;
    fn take_turn(&mut self, input: &G::Input) -> Result<G::Output, PlayerError>;

    /// Return the counter map this player accumulated during the
    /// most recent `take_turn`, then clear its internal slot.
    /// Default is empty — subprocess players and FFI bots that
    /// don't opt into the counter callback report nothing.
    fn drain_counters(&mut self) -> HashMap<String, f64> {
        HashMap::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunConfig {
    /// If true, the first player error fails the whole match. If false, the
    /// failing player just submits `None` for that tick — the game decides
    /// what that means (elimination, no-op, etc.).
    pub abort_on_player_error: bool,
}

/// Compact recording of a finished match: the seed, the player count, and the
/// outputs each player submitted on each tick. Combined with a deterministic
/// [`Game`] implementation this is enough to reconstruct the whole match by
/// rebuilding the RNG via `StdRng::seed_from_u64(seed)`, calling
/// `Game::new(num_players, &mut rng)`, and replaying `outputs` through
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
pub fn write_replay<G: Game>(replay: &Replay<G::Output>, w: &mut impl Write) -> anyhow::Result<()> {
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
pub fn read_replay<G: Game>(r: &mut impl io::Read) -> anyhow::Result<Replay<G::Output>> {
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
    r.read_exact(&mut name_len)
        .context("reading game name length")?;
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
    /// Per-turn map of counter name → value the bot reported via the
    /// FFI counter callback. Length is parallel to `turn_times`; an
    /// empty inner map means "the bot didn't emit anything that
    /// turn" (or counters are disabled / unsupported).
    pub turn_counters: Vec<HashMap<String, f64>>,
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

// ============================================================
//  Counter accumulator (FFI-only)
// ============================================================
//
// FFI bots opt in to runtime instrumentation by exporting a
// `set_counter_callback` symbol. The runner (PluginPlayer) looks it
// up and registers `cgr_emit_counter` as the callback. The bot then
// calls the callback during `take_turn` with `(key, value)` pairs.
//
// Storage is a thread-local `RefCell<HashMap>`. Each worker process
// is single-threaded for match execution, so a thread-local is
// sufficient and cheaper than a `Mutex`. `PluginPlayer::take_turn`
// clears the accumulator before the FFI call and drains it
// immediately after — that's what binds the values to the right
// player's tick.
//
// The function's address is what bots store; it does NOT need to
// be #[no_mangle] / exported, because nothing ever looks it up by
// name. We hand the pointer over inside the process.

thread_local! {
    static COUNTER_ACCUMULATOR: RefCell<HashMap<String, f64>> =
        RefCell::new(HashMap::new());
}

/// FFI callback the runner hands plugins via `set_counter_callback`.
///
/// # Safety
/// `key` must be a valid pointer to a NUL-terminated UTF-8 string that
/// lives for the duration of the call. Bots that pass bad pointers get
/// their counter silently dropped — we never dereference past the NUL.
pub unsafe extern "C" fn cgr_emit_counter(key: *const c_char, value: f64) {
    if key.is_null() {
        return;
    }
    // SAFETY: caller's contract per above.
    let cstr = unsafe { CStr::from_ptr(key) };
    let Ok(s) = cstr.to_str() else { return };
    let owned = s.to_string();
    COUNTER_ACCUMULATOR.with(|a| {
        a.borrow_mut().insert(owned, value);
    });
}

/// Public type alias for the FFI callback bots are expected to store
/// and call. Plugins receive a `CounterFn` via their
/// `set_counter_callback(cb: CounterFn)` symbol.
pub type CounterFn = unsafe extern "C" fn(*const c_char, f64);

/// Empty the accumulator (used right before each instrumented
/// `take_turn`).
pub(crate) fn counter_take() -> HashMap<String, f64> {
    COUNTER_ACCUMULATOR.with(|a| std::mem::take(&mut *a.borrow_mut()))
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
    // Build the RNG from the seed and hand it to the game. The
    // seed itself still rides into `Replay` below so re-runs are
    // deterministic.
    let mut rng = StdRng::seed_from_u64(seed);
    let mut game = G::new(num_players, &mut rng);
    let mut stats: Vec<PlayerStats> = (0..players.len()).map(|_| PlayerStats::default()).collect();
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
            let elapsed = start.elapsed();
            let counters = players[p as usize].drain_counters();
            stats[p as usize].turn_times.push(elapsed);
            stats[p as usize].turn_counters.push(counters);

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
///
/// The implementing crate just points at its `_defs` crate's [`Defs`]
/// marker; everything else (the FFI fn-pointer shapes, the ABI version,
/// the symbol names) is derived from there. With this trait,
/// `PluginPlayer<G>` becomes fully generic.
pub trait FfiGame: Game {
    /// The `_defs` crate's marker (e.g. `tron_defs::Ffi`). The
    /// `InitialInput = Self::InitialInput, Input = Self::Input,
    /// Output = Self::Output` bound ties this game's I/O types to the FFI
    /// surface that backs them.
    type Defs: Defs<InitialInput = Self::InitialInput, Input = Self::Input, Output = Self::Output>;
}

/// The `extern "C"` `take_turn` function pointer type for a given
/// `FfiGame` — derived from the game's `Defs` so per-game `_game` crates
/// never have to spell it out themselves.
pub type FfiTakeTurn<G> =
    for<'a> unsafe extern "C" fn(
        <<<G as FfiGame>::Defs as Defs>::Input as WireInput>::Ffi<'a>,
    ) -> TurnResult<<<G as FfiGame>::Defs as Defs>::Output>;

/// The `extern "C"` `initialize` function pointer type for a given
/// `FfiGame`. Called once per player at match start with the FFI mirror of
/// `<G::Defs as Defs>::InitialInput`. Bots that don't care about init
/// (games whose `InitialInput = ()`) get a no-op stub from the default
/// `ffi_bot!` invocation.
pub type FfiInitialize<G> = for<'a> unsafe extern "C" fn(
    <<<G as FfiGame>::Defs as Defs>::InitialInput as WireInput>::Ffi<'a>,
);

/// Symbol names every `ffi_bot!`-generated plugin exports. Free constants
/// (not `FfiGame` associated consts) because the macro hardcodes them and
/// nothing per-game can vary them.
const TAKE_TURN_SYMBOL: &[u8] = b"take_turn";
const INITIALIZE_SYMBOL: &[u8] = b"initialize";
const ABI_VERSION_SYMBOL: &[u8] = b"abi_version";
/// Optional. Bots that export this take a `CounterFn` and store it
/// internally, then call it during `take_turn`. Absence is fine —
/// PluginPlayer just falls back to "no counters reported".
const SET_COUNTER_CALLBACK_SYMBOL: &[u8] = b"set_counter_callback";

/// Signature of the plugin-side hook the runner calls to wire the
/// counter callback. Plugins that don't define it simply don't
/// participate in counter capture.
type SetCounterCallback = unsafe extern "C" fn(CounterFn);

pub struct PluginPlayer<G: FfiGame> {
    init: FfiInitialize<G>,
    take_turn: FfiTakeTurn<G>,
    /// True iff `enable_counters` succeeded for this player. Drives
    /// whether `take_turn` clears + drains the thread-local
    /// accumulator into `last_counters`.
    counters_enabled: bool,
    /// Counters drained from the accumulator after the last
    /// `take_turn`. Replaced (not merged) per turn.
    last_counters: HashMap<String, f64>,
    _lib: Library,
}

impl<G: FfiGame> PluginPlayer<G> {
    /// # Safety
    /// `path` must point to a dynamic library produced by
    /// `common::ffi_bot!(<defs>::Ffi, decide)` for a `_defs` crate whose
    /// `Defs::ABI_VERSION` matches this binary's. The ABI handshake below
    /// refuses mismatches before any UB-prone call lands.
    pub unsafe fn load(path: &Path) -> anyhow::Result<Self> {
        use anyhow::{Context, bail};

        let lib = unsafe { Library::new(path) }?;

        let abi: Symbol<unsafe extern "C" fn() -> u32> = unsafe { lib.get(ABI_VERSION_SYMBOL) }
            .context("plugin missing `abi_version` symbol — was it built with `ffi_bot!`?")?;
        let plugin_abi = unsafe { abi() };
        let expected = <G::Defs as Defs>::ABI_VERSION;
        if plugin_abi != expected {
            bail!(
                "ABI mismatch loading {}: plugin reports v{plugin_abi}, runner expects v{expected}",
                path.display(),
            );
        }

        let init: Symbol<FfiInitialize<G>> = unsafe { lib.get(INITIALIZE_SYMBOL) }
            .context("plugin missing `initialize` symbol — was it built with `ffi_bot!`?")?;
        let init = *init;
        let take_turn: Symbol<FfiTakeTurn<G>> = unsafe { lib.get(TAKE_TURN_SYMBOL) }?;
        let take_turn = *take_turn;
        Ok(PluginPlayer {
            init,
            take_turn,
            counters_enabled: false,
            last_counters: HashMap::new(),
            _lib: lib,
        })
    }

    /// Register the runner's counter callback with the plugin (if
    /// the plugin exports `set_counter_callback`). Returns `true` if
    /// the plugin opted in, `false` if the symbol is absent. Either
    /// case is fine — counters are purely informational.
    pub fn enable_counters(&mut self) -> bool {
        let set_cb: Symbol<SetCounterCallback> =
            match unsafe { self._lib.get(SET_COUNTER_CALLBACK_SYMBOL) } {
                Ok(s) => s,
                Err(_) => return false,
            };
        // SAFETY: we just looked the symbol up out of the same
        // library; calling it with our own `cgr_emit_counter` is
        // ABI-safe — both sides agree on the `CounterFn` signature.
        unsafe { set_cb(cgr_emit_counter) };
        self.counters_enabled = true;
        true
    }
}

impl<G: FfiGame> Player<G> for PluginPlayer<G> {
    fn initialize(&mut self, input: &G::InitialInput) -> Result<(), PlayerError> {
        let init = self.init;
        let ffi = input.as_ffi();
        catch_into_player_err(AssertUnwindSafe(move || {
            // SAFETY: `init` was obtained by `load`, which verified the
            // plugin's ABI version. The macro-generated `initialize` on
            // the bot side catches its own panics.
            unsafe { init(ffi) };
            Ok(())
        }))
    }

    fn take_turn(&mut self, input: &G::Input) -> Result<G::Output, PlayerError> {
        let take_turn = self.take_turn;
        let ffi = input.as_ffi();
        // If counters are on, drop anything left over from a prior
        // call (defensive — should already be drained) so this
        // turn's accumulator starts clean.
        if self.counters_enabled {
            let _ = counter_take();
        }
        let result = catch_into_player_err(AssertUnwindSafe(move || {
            // SAFETY: `take_turn` was obtained by `load`, which verified
            // the plugin's ABI version. The macro-generated `take_turn`
            // on the bot side catches its own panics — so a Panic status
            // here is the bot's, not UB unwinding.
            let result = unsafe { take_turn(ffi) };
            match result.status {
                BotStatus::Ok => Ok(result.output),
                BotStatus::Panic => Err(PlayerError::Panic),
            }
        }));
        if self.counters_enabled {
            self.last_counters = counter_take();
        }
        result
    }

    fn drain_counters(&mut self) -> HashMap<String, f64> {
        std::mem::take(&mut self.last_counters)
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
