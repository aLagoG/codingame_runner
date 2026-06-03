use std::{
    io::{self, BufRead, BufReader, Write},
    marker::PhantomData,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use tracing::warn;

// Bot-facing surface — moved to `bot_common`. Re-import the names we
// need locally so the rest of this file reads the same as before.
use bot_common::{ReadFrom, WriteTo};

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

pub type PlayerId = u32;

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("player produced malformed output: {0}")]
    InvalidOutput(String),
    #[error("player exceeded the per-turn time budget ({budget_ms} ms)")]
    Timeout { budget_ms: u64 },
    #[error("player closed its output (eof)")]
    Eof,
    #[error("io error talking to player: {0}")]
    Io(#[from] io::Error),
}

impl PlayerError {
    /// True for failures that mean the player isn't going to recover —
    /// the engine should stop trying to call them. All variants count
    /// as fatal today: a bot that emits garbage, times out, hits EOF,
    /// or fails an I/O syscall doesn't get retried — it's marked dead
    /// and the game decides what that means (forfeit, no-op, etc.).
    fn is_fatal(&self) -> bool {
        // Listed explicitly (vs. `true`) so a future non-fatal variant
        // would force you to think about its retry semantics.
        matches!(
            self,
            PlayerError::InvalidOutput(_)
                | PlayerError::Timeout { .. }
                | PlayerError::Eof
                | PlayerError::Io(_)
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    #[error("player {0} failed to initialize")]
    PlayerInit(PlayerId, #[source] PlayerError),
    #[error("player {0} failed during turn")]
    PlayerPlay(PlayerId, #[source] PlayerError),
    #[error("failed to serialize per-tick outputs into replay buffer")]
    ReplayBuild(#[source] anyhow::Error),
}

/// A `Game` is the rules + state for a match. Generic over its I/O types so the
/// same runner can drive any CodinGame-style game.
pub trait Game: Sized {
    /// Short stable identifier — written into replay file headers so loading
    /// the wrong game's replay errors loudly instead of decoding garbage.
    const NAME: &'static str;

    /// Wall-clock budget for a player's first turn, in milliseconds.
    /// Mirrors CodinGame's per-game per-tick budget for tick 1 (the
    /// extra leeway some games give for one-time bot setup). Engine
    /// kills a bot that doesn't produce its first move within this
    /// window and marks it dead via `PlayerError::Timeout`.
    const INITIAL_TURN_TIMEOUT_MS: u64;

    /// Wall-clock budget for every subsequent turn, in milliseconds.
    /// Engine kills a bot that exceeds it and marks it dead.
    const TURN_TIMEOUT_MS: u64;

    /// One-time per-player input sent before the match starts (e.g. world
    /// parameters). Use `()` if not needed — `()` impls `ReadFrom`+`WriteTo`
    /// trivially.
    type InitialInput: ReadFrom + WriteTo;

    /// Per-turn input sent to each active player.
    type Input: ReadFrom + WriteTo;

    /// Per-turn output collected from each active player.
    type Output: ReadFrom + WriteTo;

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
    /// For binary win/loss games, the natural mapping is winner =
    /// rank 1, loser = rank 2, draw = all rank 1. For tron, the
    /// implementation tracks death tick and ranks survivors first,
    /// then dead players in reverse death order (later death =
    /// better rank, ties allowed).
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
    /// game. `Some` for games that track a continuous metric: trail
    /// length in tron, points in a scored game, etc.
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

#[derive(Debug, Clone)]
pub struct RunConfig {
    /// If true, the first player error fails the whole match. If false, the
    /// failing player just submits `None` for that tick — the game decides
    /// what that means (elimination, no-op, etc.).
    pub abort_on_player_error: bool,
    /// Multiplier applied to `G::INITIAL_TURN_TIMEOUT_MS` and
    /// `G::TURN_TIMEOUT_MS` when the engine enforces deadlines. The
    /// CLI's `--allow-slow-bots` flag sets this to 3.0 so weakly-tuned
    /// bots get triple time without us changing the per-game budget
    /// constants. Default 1.0 = the game's own budget unchanged.
    pub timeout_multiplier: f64,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            abort_on_player_error: false,
            timeout_multiplier: 1.0,
        }
    }
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
/// Outputs are stored as the wire-format text each bot emitted (the
/// same string that ran across the subprocess pipe). That keeps per-
/// game `TurnOutput` types free of `Serialize`/`Deserialize` derives —
/// important because those would pull `serde_derive` (a proc-macro)
/// into the bot's dependency closure and make flattened CodinGame
/// submissions unvendorable.
///
/// Reconstruct typed outputs via [`Replay::parse_outputs`] /
/// build the runner side via [`Replay::from_typed_outputs`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replay {
    pub seed: u64,
    pub num_players: u32,
    pub outputs: Vec<Vec<Option<String>>>,
}

impl Replay {
    /// Build a `Replay` from the runner's `Vec<Vec<Option<O>>>` by
    /// serialising each typed output via its `WriteTo` impl.
    pub fn from_typed_outputs<O: WriteTo>(
        seed: u64,
        num_players: u32,
        outputs: &[Vec<Option<O>>],
    ) -> anyhow::Result<Self> {
        let mut out = Vec::with_capacity(outputs.len());
        for tick in outputs {
            let mut row = Vec::with_capacity(tick.len());
            for slot in tick {
                row.push(match slot {
                    Some(o) => {
                        let mut buf = Vec::new();
                        o.write_to(&mut buf)?;
                        Some(String::from_utf8(buf)?)
                    }
                    None => None,
                });
            }
            out.push(row);
        }
        Ok(Replay {
            seed,
            num_players,
            outputs: out,
        })
    }

    /// Parse the stored wire-format outputs back to typed `O`. Used
    /// by the viz and any tooling that wants to replay forward
    /// through `Game::step`.
    pub fn parse_outputs<O: ReadFrom>(&self) -> anyhow::Result<Vec<Vec<Option<O>>>> {
        let mut out = Vec::with_capacity(self.outputs.len());
        for tick in &self.outputs {
            let mut row = Vec::with_capacity(tick.len());
            for slot in tick {
                row.push(match slot {
                    Some(s) => Some(O::read_from(&mut s.as_bytes())?),
                    None => None,
                });
            }
            out.push(row);
        }
        Ok(out)
    }
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
pub fn write_replay<G: Game>(replay: &Replay, w: &mut impl Write) -> anyhow::Result<()> {
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
pub fn read_replay<G: Game>(r: &mut impl io::Read) -> anyhow::Result<Replay> {
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
    pub replay: Replay,
}

pub fn run_match<G: Game>(
    num_players: u32,
    seed: u64,
    mut players: Vec<Player<G>>,
    config: RunConfig,
) -> Result<MatchResult<G>, MatchError> {
    // Build the RNG from the seed and hand it to the game. The
    // seed itself still rides into `Replay` below so re-runs are
    // deterministic.
    let mut rng = StdRng::seed_from_u64(seed);
    let mut game = G::new(num_players, &mut rng);
    let mut stats: Vec<PlayerStats> = (0..players.len()).map(|_| PlayerStats::default()).collect();
    let mut outputs_per_tick: Vec<Vec<Option<G::Output>>> = Vec::new();
    // Players that hit a fatal error (timeout, invalid output, EOF, IO).
    // We stop calling them entirely — otherwise we keep banging on a
    // closed/misbehaving pipe and the timing stats fill with garbage.
    let mut dead: Vec<bool> = vec![false; players.len()];
    // Tracks whether each player has played their first turn yet so
    // we can apply `INITIAL_TURN_TIMEOUT_MS` (CG's looser tick-1
    // budget) once and `TURN_TIMEOUT_MS` from then on.
    let mut first_turn: Vec<bool> = vec![true; players.len()];

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
            let base_budget = if first_turn[p as usize] {
                G::INITIAL_TURN_TIMEOUT_MS
            } else {
                G::TURN_TIMEOUT_MS
            };
            let budget_ms = (base_budget as f64 * config.timeout_multiplier) as u64;
            first_turn[p as usize] = false;
            let start = Instant::now();
            let result = players[p as usize].take_turn(&input, budget_ms);
            let elapsed = start.elapsed();
            stats[p as usize].turn_times.push(elapsed);

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
            let replay =
                Replay::from_typed_outputs::<G::Output>(seed, num_players, &outputs_per_tick)
                    .map_err(MatchError::ReplayBuild)?;
            return Ok(MatchResult {
                outcome,
                stats,
                replay,
            });
        }
    }
}

/// A bot the engine drives by talking the game's wire format over a
/// spawned subprocess's stdin/stdout. The only player transport in
/// the workspace.
pub struct Player<G: Game> {
    // Order matters for drop: stdin/stdout drop before the child handle so the
    // pipes close cleanly first, then the child is reaped.
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    child: Child,
    _marker: PhantomData<G>,
}

/// Time `Player::spawn` waits after `fork+exec` before returning.
/// Absorbs dynamic-linker / libc / C++ static-init time so it isn't
/// billed against the bot's first `take_turn` call. The alternative
/// (a `READY` handshake on stdout) would be cleaner but requires
/// per-bot cooperation; a fixed sleep works for any bot and is the
/// right default until we know we need finer control.
///
/// 100 ms is enough for our tron/fantastic_bits bots' startup on
/// macOS/Linux even with a cold filesystem cache (50 ms left rare
/// first-of-day outliers in the per-turn max). If a heavier bot ever
/// shows tick-1 outliers in its stats, override via
/// `CGR_SUBPROCESS_WARMUP_MS=<n>`.
const SUBPROCESS_WARMUP_DEFAULT: Duration = Duration::from_millis(100);

fn subprocess_warmup() -> Duration {
    std::env::var("CGR_SUBPROCESS_WARMUP_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(SUBPROCESS_WARMUP_DEFAULT)
}

impl<G: Game> Player<G> {
    pub fn spawn(cmd: &mut Command) -> io::Result<Self> {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let stdin = child.stdin.take().expect("piped stdin missing");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout missing"));
        // Wait out the bot's process-startup cost so it doesn't
        // contaminate the first `take_turn` measurement. See
        // `SUBPROCESS_WARMUP_DEFAULT` for the rationale.
        std::thread::sleep(subprocess_warmup());
        Ok(Self {
            stdin,
            stdout,
            child,
            _marker: PhantomData,
        })
    }

    pub fn initialize(&mut self, input: &G::InitialInput) -> Result<(), PlayerError> {
        input.write_to(&mut self.stdin)?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Run one turn and enforce a wall-clock budget. Writes `input`,
    /// waits up to `budget_ms` for the bot to produce a line of
    /// output, then reads + parses it. If the budget expires before
    /// any output arrives, kills the child and returns
    /// `PlayerError::Timeout` — the bot is dead for the rest of the
    /// match.
    pub fn take_turn(
        &mut self,
        input: &G::Input,
        budget_ms: u64,
    ) -> Result<G::Output, PlayerError> {
        input.write_to(&mut self.stdin)?;
        self.stdin.flush()?;

        match wait_readable(&self.stdout, budget_ms) {
            WaitOutcome::Ready => {}
            WaitOutcome::Eof => return Err(PlayerError::Eof),
            WaitOutcome::Timeout => {
                let _ = self.child.kill();
                return Err(PlayerError::Timeout { budget_ms });
            }
            WaitOutcome::Io(e) => return Err(PlayerError::Io(e)),
        }

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

enum WaitOutcome {
    Ready,
    Eof,
    Timeout,
    Io(io::Error),
}

/// Wait up to `budget_ms` for `reader`'s underlying fd to become
/// readable. Returns `Ready` (data or EOF visible to the next read),
/// `Timeout` (budget elapsed with no data), or `Io` on a syscall
/// error. Unix-only via `libc::poll`; on other platforms it returns
/// `Ready` immediately, meaning the timeout is unenforced and the
/// subsequent `fill_buf` will block.
#[cfg(unix)]
fn wait_readable(reader: &BufReader<ChildStdout>, budget_ms: u64) -> WaitOutcome {
    use std::os::unix::io::AsRawFd;

    // BufReader's internal buffer might already have data from a
    // previous `fill_buf` call; if so, poll would block waiting for
    // *new* bytes that never come. Caller checks `buffer().is_empty()`
    // via the public method to short-circuit before calling us.
    if !reader.buffer().is_empty() {
        return WaitOutcome::Ready;
    }

    let mut pfd = libc::pollfd {
        fd: reader.get_ref().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // i32-max is ~24 days; clamp to that and round up so a 1 ms
    // budget doesn't collapse to 0 (which means "no wait" to poll).
    let timeout_ms = budget_ms.min(i32::MAX as u64) as i32;
    let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if ret < 0 {
        return WaitOutcome::Io(io::Error::last_os_error());
    }
    if ret == 0 {
        return WaitOutcome::Timeout;
    }
    if pfd.revents & libc::POLLHUP != 0 && pfd.revents & libc::POLLIN == 0 {
        return WaitOutcome::Eof;
    }
    WaitOutcome::Ready
}

#[cfg(not(unix))]
fn wait_readable(_reader: &BufReader<ChildStdout>, _budget_ms: u64) -> WaitOutcome {
    // No portable std-only mechanism; fall back to "trust the read
    // will return promptly" until someone wires WaitForSingleObject
    // on Windows.
    WaitOutcome::Ready
}

impl<G: Game> Drop for Player<G> {
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
