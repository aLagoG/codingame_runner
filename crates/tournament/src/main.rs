//! CLI front-end for the tournament harness. Subcommands:
//!
//!   * `run`    — schedule matches and stream a JSONL log of results.
//!   * `report` — read a JSONL log and print summary + win-rate matrix.
//!
//! Everything stateful (schedule generation, match execution, report
//! aggregation) lives in the `tournament` library; this file is just
//! arg parsing and I/O.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcCommand, Stdio};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use common::engine::{EngineFlags, RunConfig};
use tournament::{
    BotSpec, MatchRecord, ScheduleConfig, ScheduledMatch, build_report, build_schedule,
    play_schedule,
};

#[derive(Parser)]
#[command(
    name = "tournament",
    about = "Pit multiple bots against each other across many matches and aggregate the results."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run(RunArgs),
    Report(ReportArgs),
    Compare(CompareArgs),
    /// Internal: play a schedule chunk read from stdin and write
    /// JSONL results to stdout. Not for direct use — `run --parallel
    /// N` spawns N of these. `clap`'s `hide = true` keeps it out of
    /// `--help`.
    #[command(hide = true)]
    Worker(WorkerArgs),
}

/// Flags shared by `run` and `compare` — both schedule a round-robin
/// over the same set of bots, build them under the same profile, and
/// run matches with the same per-turn budget / abort policy. Kept as a
/// flattened struct so a new shared flag lands in one place instead of
/// two.
#[derive(clap::Args)]
struct CommonRunArgs {
    /// Game to play (`tron`, `fantastic_bits`).
    #[arg(long)]
    game: String,

    /// Bot stems to enter (≥ 2). Each is auto-resolved to
    /// `games/<game>/bots/<bot>_<lang>/` via `bot.toml`. If both rs
    /// and cpp variants exist for the same stem, qualify the name
    /// with `<bot>:rs` or `<bot>:cpp` to pick one. Stems must be
    /// unique (they double as the bot's identifier in the JSONL log).
    #[arg(required = true, num_args = 2..)]
    bots: Vec<String>,

    /// Number of seeds to play per (combination × seat rotation).
    /// For `run`, `--seeds` guarantees inclusion of specific values;
    /// everything else is filled from the sequential range or
    /// `--random-seeds` up to this count.
    #[arg(long, default_value_t = 100)]
    rounds: u64,

    /// Players per match. Default 2; pass `--bots-per-match 4` for
    /// 4-player games like tron. Must be in `[2, num_bots]`.
    #[arg(long, default_value_t = 2)]
    bots_per_match: usize,

    /// Skip the bot build and trust whatever's already in
    /// `target/<profile>/`. Useful when the bots are already built
    /// and you want the fastest possible iteration.
    #[arg(long)]
    no_build: bool,

    /// Cargo build profile for the bots. Defaults to `release`.
    /// Use `profiling` (release + full debug info) when recording
    /// with samply — `cargo xtask profile` passes this through.
    /// Resolves bot binaries from `target/<profile>/`.
    #[arg(long, default_value = "release")]
    profile: String,

    /// After the run completes, append a `[[history]]` entry to every
    /// participant's `bot.toml` capturing this run's pairwise outcomes
    /// (pts vs each opponent, verdict). Opt-in to avoid noisy git
    /// churn from fast iteration loops.
    #[arg(long)]
    record_history: bool,

    /// Number of matches to run in parallel. Defaults to the number
    /// of available logical cores. Pass `--parallel 1` for clean
    /// per-turn timing baselines (the printed verdict ignores
    /// timing, but `record_history` carries it forward).
    #[arg(long, default_value_t = default_parallel())]
    parallel: usize,

    #[command(flatten)]
    engine: EngineFlags,
}

#[derive(Parser)]
#[command(
    about = "Resolve N bots by name, build them, play a round-robin, print a focused verdict.",
    long_about = "\
Wraps `run` + `report` for the most common case: \"is candidate X better than baseline Y?\". \
Bot names are stems (e.g. `v1`, `baseline`) — `compare` reads each bot's `bot.toml` to \
figure out which language variant exists and resolves the bin under `target/release/`. \
Re-runs `cargo build --release -p <crate>` for every bot (incremental, so a no-op when up-to-date). \
For N=2 prints a single verdict line + a \"need ≈ X more games\" epilogue when inconclusive; \
for N≥3 prints a ranked table + pairwise verdict block."
)]
struct CompareArgs {
    #[command(flatten)]
    common: CommonRunArgs,
}

#[derive(Parser)]
#[command(
    about = "Schedule and play matches; stream a JSONL result log.",
    long_about = "\
Schedule N bots through a round-robin, play it, stream JSONL records to --output. \
Bot names are stems (e.g. `v1`, `baseline`) — resolved via `bot.toml` to \
`games/<game>/bots/<bot>_<lang>/` and built incrementally (`cargo build --release -p <crate>`). \
Qualify as `<bot>:rs` or `<bot>:cpp` when both variants exist. \
For a focused \"is X better than Y\" answer instead of a log, use `compare` — same \
resolver, same scheduler, prints a verdict instead of writing JSONL."
)]
struct RunArgs {
    #[command(flatten)]
    common: CommonRunArgs,

    /// Comma-separated seeds that must appear in the schedule (e.g.
    /// `--seeds 0,17,42`). If fewer than `--rounds` values are
    /// listed, the remaining slots are filled from either the
    /// sequential range or freshly-generated random seeds (see
    /// `--random-seeds`). If equal to or more than `--rounds`, all
    /// listed seeds are used as-is.
    #[arg(long, value_delimiter = ',')]
    seeds: Vec<u64>,

    /// Fill schedule slots with random u64 seeds instead of the
    /// sequential range. Any seeds passed via `--seeds` are kept
    /// verbatim; the rest are random. Generated values are printed
    /// on stderr and embedded in every match's JSONL record so the
    /// run is reproducible via `--seeds <list>`.
    #[arg(long)]
    random_seeds: bool,

    /// By default every combination is played in all N cyclic seat
    /// rotations. Pass `--no-rotate-seats` to skip rotation entirely
    /// (only the combination's natural order is used).
    #[arg(long)]
    no_rotate_seats: bool,

    /// Where to write the JSONL match log. Parent dirs are created.
    #[arg(short, long)]
    output: PathBuf,

    /// Stop early once every bot-pair's two-sided p-value drops
    /// below `--alpha`. `--rounds` becomes the cap (max matches to
    /// play if confidence is never reached). Forces `--parallel 1`
    /// because the wave-by-wave check needs deterministic ordering;
    /// document this if you need fast adaptive runs.
    #[arg(long)]
    until_confident: bool,

    /// Family-wise significance threshold used by `--until-confident`.
    /// The per-wave check applies a Bonferroni correction
    /// (`alpha / max_waves`) so peeking after every wave doesn't
    /// inflate the false-positive rate to nominal alpha.
    #[arg(long, default_value_t = 0.05)]
    alpha: f64,

    /// `--until-confident` plays matches in waves of this size and
    /// re-checks pairwise CIs after each wave. Larger waves = fewer
    /// stat checks (cheaper) but coarser early-stop granularity.
    #[arg(long, default_value_t = 50)]
    wave_size: usize,
}

#[derive(Parser)]
struct WorkerArgs {
    #[arg(long)]
    game: String,
    #[arg(long = "bot", value_parser = parse_bot_spec, required = true)]
    bots: Vec<BotSpec>,
    /// Mirrors of the parent's engine-tuning flags — forwarded through
    /// the worker spawn via `EngineFlags::to_argv()` so per-turn budget
    /// + abort behavior match the parent's.
    #[command(flatten)]
    engine: EngineFlags,
}

fn default_parallel() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[derive(Parser)]
#[command(about = "Read a JSONL match log and print per-bot summary + win-rate matrix.")]
struct ReportArgs {
    input: PathBuf,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run(args) => cmd_run(args),
        Command::Report(args) => cmd_report(args),
        Command::Compare(args) => cmd_compare(args),
        Command::Worker(args) => cmd_worker(args),
    }
}

// ============================================================
//  run
// ============================================================

fn cmd_run(args: RunArgs) -> Result<()> {
    let RunArgs {
        common,
        seeds: explicit_seeds,
        random_seeds,
        no_rotate_seats,
        output,
        until_confident,
        alpha,
        wave_size,
    } = args;
    let CommonRunArgs {
        game,
        bots,
        rounds,
        bots_per_match,
        no_build,
        profile,
        record_history: record_history_flag,
        parallel: parallel_request,
        engine,
    } = common;

    let resolved = resolve_and_build(&game, &bots, no_build, &profile)?;
    let bot_specs: Vec<BotSpec> = resolved.iter().map(|r| r.to_spec()).collect();

    let seeds = assemble_seeds(&explicit_seeds, rounds as usize, random_seeds);
    let cfg = ScheduleConfig {
        bots_per_match,
        seeds,
        rotate_seats: !no_rotate_seats,
    };
    let schedule = build_schedule(bot_specs.len(), &cfg)?;
    let total = schedule.len();
    if total == 0 {
        bail!("schedule is empty — check --bots-per-match / --seeds / --rounds");
    }

    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = File::create(&output).with_context(|| format!("creating {}", output.display()))?;
    let out = BufWriter::new(file);

    let config: RunConfig = engine.into();

    // Clamp `--parallel` to something useful: at most one worker
    // per match, and at least 1.
    let parallel = parallel_request.clamp(1, total).max(1);
    if parallel > 1 && !until_confident {
        eprintln!(
            "⚠ Running with --parallel {parallel}. Per-turn decision-time \
             numbers will be affected by CPU contention; use --parallel 1 \
             for clean timing baselines."
        );
    }

    let actually_played: usize = if until_confident {
        if parallel > 1 {
            eprintln!(
                "⚠ --until-confident currently runs sequentially (parallel mode \
                 doesn't compose with wave-by-wave checks); ignoring --parallel {parallel}."
            );
        }
        anyhow::ensure!(
            wave_size >= 1,
            "--wave-size must be ≥ 1 (got {})",
            wave_size
        );
        anyhow::ensure!(
            alpha > 0.0 && alpha < 1.0,
            "--alpha must be in (0, 1) (got {})",
            alpha
        );
        eprintln!(
            "Running up to {total} matches of {game} (adaptive: wave_size={wave_size}, α={alpha})…"
        );
        run_adaptive(&game, &bot_specs, &schedule, wave_size, alpha, config, out)?
    } else {
        eprintln!("Running {total} matches of {game} (--parallel {parallel})…");
        if parallel == 1 {
            run_sequential(&game, &bot_specs, &schedule, config, out, total)?;
        } else {
            run_parallel(&game, &bot_specs, schedule, engine, out, parallel)?;
        }
        total
    };

    eprintln!("Wrote {} → {}", actually_played, output.display());

    // Re-read the JSONL we just wrote so we can print the report (and
    // record history, when requested). Cheap — the file was just
    // flushed and its pages are hot in the page cache. Avoids forking
    // the sequential/parallel/adaptive code paths just to thread
    // records back through.
    let records = read_jsonl_records(&output)?;
    let report = build_report(&records);
    println!();
    print_report(&report);

    if record_history_flag {
        let participants: Vec<(String, String)> = resolved
            .iter()
            .map(|r| (r.name.clone(), r.lang.clone()))
            .collect();
        record_history(&game, &participants, &report)?;
    }
    Ok(())
}

fn run_sequential(
    game: &str,
    bots: &[BotSpec],
    schedule: &[ScheduledMatch],
    config: RunConfig,
    mut out: BufWriter<File>,
    total: usize,
) -> Result<()> {
    for (i, m) in schedule.iter().enumerate() {
        let entries: Vec<BotSpec> = m.bot_idx.iter().map(|&j| bots[j].clone()).collect();
        let names: Vec<&str> = entries.iter().map(|b| b.name.as_str()).collect();
        eprintln!(
            "  [{:>4}/{}] seed={} {}",
            i + 1,
            total,
            m.seed,
            names.join(" vs ")
        );
        let rec = tournament::run_match_named(game, &entries, m.seed, config.clone())
            .with_context(|| format!("match {} ({})", i + 1, names.join(" vs ")))?;
        serde_json::to_writer(&mut out, &rec)?;
        writeln!(out)?;
        out.flush()?;
    }
    Ok(())
}

/// `--until-confident` driver: play in waves of `wave_size`, after
/// each wave check that every bot-pair's two-sided p-value drops
/// below a Bonferroni-corrected per-look alpha
/// (`alpha / max_waves`). Returns the number of matches actually
/// played (≤ schedule.len()) so the caller can report it accurately.
///
/// Bonferroni is the simplest peeking correction — strict but
/// honest. Tighter alpha-spending boundaries (Pocock,
/// O'Brien-Fleming) would let us stop a touch earlier but add
/// real complexity; the user can opt into `gauntlet --sprt` later
/// for proper SPRT semantics.
fn run_adaptive(
    game: &str,
    bots: &[BotSpec],
    schedule: &[ScheduledMatch],
    wave_size: usize,
    alpha: f64,
    config: RunConfig,
    mut out: BufWriter<File>,
) -> Result<usize> {
    let total = schedule.len();
    let max_waves = total.div_ceil(wave_size).max(1);
    let per_look_alpha = alpha / max_waves as f64;
    eprintln!("  Bonferroni-corrected per-wave α = {alpha:.4} / {max_waves} = {per_look_alpha:.6}");

    let mut records: Vec<MatchRecord> = Vec::with_capacity(total);
    let mut played = 0usize;

    for (wave_idx, wave) in schedule.chunks(wave_size).enumerate() {
        for (j, m) in wave.iter().enumerate() {
            let entries: Vec<BotSpec> = m.bot_idx.iter().map(|&k| bots[k].clone()).collect();
            let names: Vec<&str> = entries.iter().map(|b| b.name.as_str()).collect();
            eprintln!(
                "  [w{:>2} {:>3}/{}] seed={} {}",
                wave_idx + 1,
                j + 1,
                wave.len(),
                m.seed,
                names.join(" vs ")
            );
            let rec = tournament::run_match_named(game, &entries, m.seed, config.clone())
                .with_context(|| {
                    format!("match in wave {} ({})", wave_idx + 1, names.join(" vs "))
                })?;
            serde_json::to_writer(&mut out, &rec)?;
            writeln!(out)?;
            out.flush()?;
            records.push(rec);
            played += 1;
        }
        // End-of-wave check.
        let report = build_report(&records);
        let bot_names: Vec<&str> = bots.iter().map(|b| b.name.as_str()).collect();
        let mut all_significant = true;
        'pairs: for i in 0..bot_names.len() {
            for j in (i + 1)..bot_names.len() {
                match report.pair_stats(bot_names[i], bot_names[j]) {
                    Some(s) if s.p_value < per_look_alpha => {}
                    _ => {
                        all_significant = false;
                        break 'pairs;
                    }
                }
            }
        }
        if all_significant {
            eprintln!(
                "  ✓ all pairs significant at p<{per_look_alpha:.6} after wave {} ({} matches); stopping early.",
                wave_idx + 1,
                played,
            );
            return Ok(played);
        }
    }
    eprintln!(
        "  ⚠ ran out of scheduled matches before reaching significance ({} played, cap was {}).",
        played, total,
    );
    Ok(played)
}

/// Spawn `parallel` copies of the tournament binary in `worker`
/// mode, hand each a partition of the schedule on stdin, collect
/// JSONL records from each stdout via a per-worker reader thread,
/// and write them to the output file as they arrive.
///
/// Partitioning is round-robin (`schedule[i] → worker[i % N]`)
/// rather than contiguous chunks — adjacent schedule entries share
/// the same combo + rotation, so round-robin balances both bot
/// strength and game-length variance across workers more evenly.
fn run_parallel(
    game: &str,
    bots: &[BotSpec],
    schedule: Vec<ScheduledMatch>,
    engine: EngineFlags,
    mut out: BufWriter<File>,
    parallel: usize,
) -> Result<()> {
    play_schedule_parallel(game, bots, schedule, engine, parallel, |line| {
        writeln!(out, "{line}")?;
        out.flush()?;
        Ok(())
    })
}

/// Spawn `parallel` worker processes, partition the schedule round-
/// robin across them, and invoke `on_line` for each JSONL record they
/// stream back. Used by both `run --parallel N` (callback writes to
/// the output file) and `compare --parallel N` (callback deserialises
/// into an in-memory `Vec<MatchRecord>`). Worker-per-process keeps
/// each match's bot-subprocess spawns independent across workers.
fn play_schedule_parallel<F>(
    game: &str,
    bots: &[BotSpec],
    schedule: Vec<ScheduledMatch>,
    engine: EngineFlags,
    parallel: usize,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(String) -> Result<()>,
{
    let total = schedule.len();
    let exe = std::env::current_exe().context("locating tournament binary")?;

    // Round-robin partition the schedule across `parallel` workers.
    let mut partitions: Vec<Vec<ScheduledMatch>> = (0..parallel).map(|_| Vec::new()).collect();
    for (i, m) in schedule.into_iter().enumerate() {
        partitions[i % parallel].push(m);
    }

    // Spawn all workers + reader threads first, *then* write to
    // their stdins. Writing first risks deadlock: workers can
    // block on output backpressure if their stdouts have nowhere
    // to drain to yet.
    let (tx, rx) = mpsc::channel::<String>();
    let mut workers = Vec::with_capacity(parallel);
    let mut readers = Vec::with_capacity(parallel);
    for _ in 0..parallel {
        let mut cmd = ProcCommand::new(&exe);
        cmd.args(["worker", "--game", game]);
        // Forward engine flags so the worker's `play_schedule` call
        // applies the same per-turn budget + abort behavior as the
        // parent.
        cmd.args(engine.to_argv());
        for bot in bots {
            cmd.arg("--bot")
                .arg(format!("{}={}", bot.name, bot.path.display()));
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().context("spawning tournament worker")?;

        let stdout = child.stdout.take().expect("piped");
        let tx = tx.clone();
        let handle = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) if l.trim().is_empty() => continue,
                    Ok(l) => {
                        // The receiver is gone only if main bailed;
                        // in that case dropping the message is fine.
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        readers.push(handle);
        workers.push(child);
    }
    drop(tx); // rx ends once every cloned tx (one per reader) drops.

    // Write each partition into its worker's stdin. Spawn a thread
    // per writer so a slow stdin pipe doesn't block scheduling of
    // the next partition.
    let mut writers = Vec::with_capacity(parallel);
    for (worker, partition) in workers.iter_mut().zip(partitions) {
        let stdin = worker.stdin.take().expect("piped");
        writers.push(thread::spawn(move || -> std::io::Result<()> {
            let mut stdin = BufWriter::new(stdin);
            serde_json::to_writer(&mut stdin, &partition).map_err(std::io::Error::other)?;
            stdin.flush()?;
            // Dropping `stdin` here closes the pipe → worker sees
            // EOF on its end and starts playing.
            Ok(())
        }));
    }

    // Stream records to the caller as they arrive. Order is by
    // completion time, not by schedule index — neither caller cares.
    let mut done = 0usize;
    for line in rx {
        on_line(line)?;
        done += 1;
        eprintln!("  [{done:>4}/{total}] match completed");
    }

    // Join all background threads + reap workers, surfacing the
    // first non-success exit if any.
    for w in writers {
        w.join()
            .map_err(|_| anyhow::anyhow!("writer thread panicked"))?
            .context("writing partition to worker stdin")?;
    }
    for r in readers {
        r.join()
            .map_err(|_| anyhow::anyhow!("reader thread panicked"))?;
    }
    for (i, mut w) in workers.into_iter().enumerate() {
        let status = w.wait().context("waiting for worker")?;
        if !status.success() {
            bail!("worker {i} exited with {status}");
        }
    }
    Ok(())
}

fn cmd_worker(args: WorkerArgs) -> Result<()> {
    // Read the partition handed over by main on stdin.
    let stdin = std::io::stdin();
    let schedule: Vec<ScheduledMatch> =
        serde_json::from_reader(stdin.lock()).context("parsing schedule chunk from stdin")?;

    // Play it and stream JSONL on stdout. Each line is one
    // MatchRecord; main's reader thread picks them up by line.
    let stdout = std::io::stdout();
    play_schedule(
        &args.game,
        &args.bots,
        &schedule,
        args.engine.into(),
        stdout.lock(),
    )
}

/// Build the final seed list for the scheduler. `explicit` is the
/// user's `--seeds` list (an inclusion guarantee). `target` is
/// `--rounds`. If `explicit` is short of `target`, fill the rest
/// with either the sequential range `0, 1, 2, …` (skipping values
/// already in `explicit`) or freshly-generated random u64s.
/// If `explicit.len() >= target`, the explicit list is returned
/// as-is — the user asked for more than `target`, respect them.
///
/// Random-mode filler is printed on stderr so the user can
/// re-run with `--seeds <list>` for an exact replay.
fn assemble_seeds(explicit: &[u64], target: usize, random: bool) -> Vec<u64> {
    let mut out: Vec<u64> = explicit.to_vec();
    if out.len() >= target {
        return out;
    }
    let needed = target - out.len();
    let filler: Vec<u64> = if random {
        let s = random_seeds(needed);
        eprintln!(
            "Generated {needed} random seed(s): pass --seeds {} to reproduce.",
            s.iter().map(u64::to_string).collect::<Vec<_>>().join(","),
        );
        s
    } else {
        // Sequential, skipping any value already in `explicit`. We
        // expect `explicit` to be small, so a linear `.contains()`
        // check per candidate is fine.
        let mut filler = Vec::with_capacity(needed);
        let mut next: u64 = 0;
        while filler.len() < needed {
            if !out.contains(&next) {
                filler.push(next);
            }
            next += 1;
        }
        filler
    };
    out.extend(filler);
    out
}

/// Generate `n` u64 seeds from OS entropy. `rand::rng()` returns
/// the thread-local CSPRNG (ChaCha-based, seeded from `getrandom`
/// on most platforms), so per-call uniqueness and statistical
/// quality both come for free.
fn random_seeds(n: usize) -> Vec<u64> {
    // `RngExt::random` (not `Rng::random`) is the one returning a
    // typed sample — `Rng` only has the lower-level helpers in 0.10.
    use rand::RngExt;
    let mut rng = rand::rng();
    (0..n).map(|_| rng.random()).collect()
}

fn parse_bot_spec(s: &str) -> Result<BotSpec, String> {
    let (name, path) = s
        .split_once('=')
        .ok_or_else(|| format!("expected `name=path`, got `{s}`"))?;
    if name.is_empty() {
        return Err("bot name must be non-empty".into());
    }
    Ok(BotSpec {
        name: name.to_string(),
        path: PathBuf::from(path),
    })
}

// ============================================================
//  report
// ============================================================

fn cmd_report(args: ReportArgs) -> Result<()> {
    let records = read_jsonl_records(&args.input)?;
    if records.is_empty() {
        bail!("no records in {}", args.input.display());
    }

    let report = build_report(&records);
    print_report(&report);
    Ok(())
}

/// Read a JSONL match log into `Vec<MatchRecord>`. Used by `report`
/// to load a log written by a previous `run`, and by `run` itself to
/// re-read the log it just wrote (so the post-play report-printing
/// and optional history-recording paths share one source).
fn read_jsonl_records(path: &Path) -> Result<Vec<MatchRecord>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut records = Vec::new();
    for (lineno, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: MatchRecord = serde_json::from_str(&line)
            .with_context(|| format!("parsing line {} of {}", lineno + 1, path.display()))?;
        records.push(rec);
    }
    Ok(records)
}

/// Per-bot summary table + win-rate matrix + pairwise verdicts.
/// Shared between `report`, `run` (post-play), and `compare`
/// (post-play, before the focused verdict line).
fn print_report(report: &tournament::Report) {
    print_summary(report);
    println!();
    print_matrix(report);
    println!();
    print_pairwise_verdicts(report);
}

/// Print a "Pairwise verdicts" block — one row per unordered bot
/// pair, ranking each row by the LEFT bot's effective win-rate so
/// the strongest-vs-weakest comparisons sit at the top. For each
/// pair: win-rate ± Wilson 95% CI, LOS, two-sided p-value, and a
/// verdict ("significant (BETTER)" / "(WORSE)" / "inconclusive").
fn print_pairwise_verdicts(report: &tournament::Report) {
    use tournament::pairwise_stats::{PairStats, Verdict};

    let names: Vec<String> = report.per_bot.keys().cloned().collect();
    if names.len() < 2 {
        return;
    }

    // Pre-compute every unordered pair's stats (orient A so its
    // effective win-rate ≥ B's, so each row reads naturally as "A is
    // (or isn't) better than B").
    let mut rows: Vec<(String, String, PairStats)> = Vec::new();
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            let (a, b) = (&names[i], &names[j]);
            let Some(stats) = report.pair_stats(a, b) else {
                continue;
            };
            if stats.a_win_rate >= 0.5 {
                rows.push((a.clone(), b.clone(), stats));
            } else {
                // Flip orientation so A is the stronger side.
                rows.push((b.clone(), a.clone(), report.pair_stats(b, a).unwrap()));
            }
        }
    }
    // Sort: largest win-rate first, then by p (most-significant first).
    rows.sort_by(|x, y| {
        y.2.a_win_rate
            .partial_cmp(&x.2.a_win_rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                x.2.p_value
                    .partial_cmp(&y.2.p_value)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    println!("Pairwise verdicts (95% CI, no Elo):");
    let name_w = names.iter().map(|n| n.len()).max().unwrap_or(4);
    for (a, b, s) in &rows {
        let lo_pct = s.a_ci_95.0 * 100.0;
        let hi_pct = s.a_ci_95.1 * 100.0;
        // Half-width of the CI — what users actually read as "±".
        let half = (hi_pct - lo_pct) / 2.0;
        let verdict_glyph = match s.verdict {
            Verdict::Better => "→ significant",
            Verdict::Worse => "→ significant (WORSE)",
            Verdict::Inconclusive => "→ inconclusive",
        };
        println!(
            "  {a:<aw$} vs {b:<bw$}  {wr:>5.1}% ± {half:>4.1}%  \
             LOS {los:>5.1}%  p={p:>6.3}  {verdict}",
            a = a,
            b = b,
            aw = name_w,
            bw = name_w,
            wr = s.a_win_rate * 100.0,
            half = half,
            los = s.los * 100.0,
            p = s.p_value,
            verdict = verdict_glyph,
        );
    }
}

fn print_summary(report: &tournament::Report) {
    let names: Vec<&String> = report.per_bot.keys().collect();
    let name_w = names.iter().map(|n| n.len()).max().unwrap_or(4).max(4);

    let max_rank = report
        .per_bot
        .values()
        .map(|s| s.standing_counts.len())
        .max()
        .unwrap_or(0);
    let any_scores = report.per_bot.values().any(|s| s.score_summary.is_some());

    let mut header = format!(
        "{:<width$}  {:>5}  {:>5}  {:>6}  {:>5}  {:>5}  {:>6}  {:>6}",
        "bot",
        "games",
        "wins",
        "losses",
        "draws",
        "win%",
        "pts",
        "avgpl",
        width = name_w,
    );
    for r in 1..=max_rank {
        header.push_str(&format!("  {:>4}", format!("{}{}", r, ordinal_suffix(r))));
    }
    if any_scores {
        header.push_str(&format!(
            "  {:>8}  {:>7}  {:>7}",
            "avg sc", "min sc", "max sc"
        ));
    }
    header.push_str(&format!(
        "  {:>7}  {:>7}  {:>7}",
        "avg ms", "p95 ms", "max ms"
    ));
    println!("{header}");
    println!("{}", "-".repeat(header.len()));

    for (name, s) in &report.per_bot {
        let total = (s.wins + s.losses + s.draws).max(1);
        let win_pct = 100.0 * s.wins as f64 / total as f64;
        // `s.pts` is pairwise-normalised in `build_report` — each
        // match contributes at most 1, so the column is comparable
        // across 2-player and 4-player matches. See `BotSummary.pts`.
        let mut row = format!(
            "{:<width$}  {:>5}  {:>5}  {:>6}  {:>5}  {:>4.0}%  {:>6.1}  {:>6.2}",
            name,
            s.games,
            s.wins,
            s.losses,
            s.draws,
            win_pct,
            s.pts,
            s.avg_standing,
            width = name_w,
        );
        for r in 0..max_rank {
            let n = s.standing_counts.get(r).copied().unwrap_or(0);
            row.push_str(&format!("  {:>4}", n));
        }
        if any_scores {
            match &s.score_summary {
                Some(sc) => row.push_str(&format!(
                    "  {:>8.2}  {:>7.2}  {:>7.2}",
                    sc.avg, sc.min, sc.max
                )),
                None => row.push_str(&format!("  {:>8}  {:>7}  {:>7}", "-", "-", "-")),
            }
        }
        row.push_str(&format!(
            "  {:>7.2}  {:>7.2}  {:>7.2}",
            s.time_summary.avg_of_avg_ms, s.time_summary.avg_of_p95_ms, s.time_summary.worst_max_ms,
        ));
        println!("{row}");
    }
}

fn ordinal_suffix(n: usize) -> &'static str {
    match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    }
}

fn print_matrix(report: &tournament::Report) {
    let names: Vec<String> = report.per_bot.keys().cloned().collect();
    if names.len() < 2 {
        return;
    }
    let cell_w = names.iter().map(|n| n.len()).max().unwrap_or(4).max(6) + 2;

    println!("Win-rate matrix (row vs column):");
    print!("{:>width$}", "", width = cell_w);
    for col in &names {
        print!("{:>width$}", col, width = cell_w);
    }
    println!();

    for row in &names {
        print!("{:>width$}", row, width = cell_w);
        for col in &names {
            if row == col {
                print!("{:>width$}", "-", width = cell_w);
                continue;
            }
            let games = report
                .pair_games
                .get(&(row.clone(), col.clone()))
                .copied()
                .unwrap_or(0);
            if games == 0 {
                print!("{:>width$}", "·", width = cell_w);
            } else {
                let wins = report
                    .pair_wins
                    .get(&(row.clone(), col.clone()))
                    .copied()
                    .unwrap_or(0);
                let pct = 100.0 * wins as f64 / games as f64;
                let cell = format!("{:.0}%", pct);
                print!("{:>width$}", cell, width = cell_w);
            }
        }
        println!();
    }
}

// ============================================================
//  `compare` — focused N-bot A/B with verdicts
// ============================================================

/// One resolved bot — what its `bot.toml` says + where its bin lives.
struct ResolvedBot {
    /// Stem the user passed (e.g. `v1`), used to look up the crate +
    /// bot.toml. Note: when the same stem appears multiple times in
    /// one invocation (a self-vs-self comparison), `display_name`
    /// gets a `#N` suffix while `name` keeps the bare stem.
    name: String,
    /// What to call this bot in the schedule + report. Equal to `name`
    /// unless disambiguation kicked in.
    display_name: String,
    /// `rs` or `cpp`.
    lang: String,
    /// Cargo crate name, e.g. `fantastic_bits_v1_5_cpp`.
    crate_name: String,
    /// Path to the bot binary `cargo build --release` produces. Both
    /// Rust and C++ bots produce a default-discovered bin named after
    /// the crate (`target/<profile>/<crate_name>`).
    bin_path: PathBuf,
}

impl ResolvedBot {
    fn to_spec(&self) -> BotSpec {
        BotSpec {
            name: self.display_name.clone(),
            path: self.bin_path.clone(),
        }
    }
}

fn cmd_compare(args: CompareArgs) -> Result<()> {
    let CommonRunArgs {
        game,
        bots,
        rounds,
        bots_per_match,
        no_build,
        profile,
        record_history: record_history_flag,
        parallel: parallel_request,
        engine,
    } = args.common;

    anyhow::ensure!(bots.len() >= 2, "compare needs at least 2 bots");

    let resolved = resolve_and_build(&game, &bots, no_build, &profile)?;
    let bot_specs: Vec<BotSpec> = resolved.iter().map(|r| r.to_spec()).collect();

    // Build the schedule. Reuse the `tournament run` logic — same
    // seeds-and-rotations rules so `compare --rounds N` matches what
    // `run --rounds N` would have done.
    let seeds = assemble_seeds(&[], rounds as usize, false);
    let cfg = ScheduleConfig {
        bots_per_match,
        seeds,
        rotate_seats: true,
    };
    let schedule = build_schedule(bot_specs.len(), &cfg)?;
    anyhow::ensure!(
        !schedule.is_empty(),
        "empty schedule — check --bots-per-match (got {}) vs bot count ({})",
        bots_per_match,
        bot_specs.len(),
    );

    eprintln!(
        "Playing {} matches of {game} ({} bots × {bots_per_match}-per-match)…",
        schedule.len(),
        bot_specs.len(),
    );

    let parallel = parallel_request.clamp(1, schedule.len()).max(1);
    eprintln!("  (--parallel {parallel})");
    let config: RunConfig = engine.into();
    let mut records: Vec<MatchRecord> = Vec::with_capacity(schedule.len());
    if parallel == 1 {
        // Sequential path keeps the per-match "[i/N] seed=... a vs b"
        // breadcrumb line on stderr — handy for watching short
        // compares scroll by. The parallel path's by-completion-time
        // ordering would scramble it.
        for (i, m) in schedule.iter().enumerate() {
            let entries: Vec<BotSpec> = m.bot_idx.iter().map(|&j| bot_specs[j].clone()).collect();
            let names: Vec<&str> = entries.iter().map(|b| b.name.as_str()).collect();
            eprintln!(
                "  [{:>4}/{}] seed={} {}",
                i + 1,
                schedule.len(),
                m.seed,
                names.join(" vs "),
            );
            let rec = tournament::run_match_named(&game, &entries, m.seed, config.clone())
                .with_context(|| format!("match {} ({})", i + 1, names.join(" vs ")))?;
            records.push(rec);
        }
    } else {
        play_schedule_parallel(&game, &bot_specs, schedule, engine, parallel, |line| {
            let rec: MatchRecord = serde_json::from_str(&line)
                .with_context(|| format!("parsing worker record `{line}`"))?;
            records.push(rec);
            Ok(())
        })?;
    }
    let report = build_report(&records);

    println!();
    print_report(&report);
    // Focused verdict goes last so it's the last thing on screen
    // when the tables push the per-pair line off-frame. For N≥3
    // `print_pairwise_verdicts` already covered every pair inside
    // `print_report`; the ranking adds the pts-sorted leaderboard
    // on top.
    println!();
    if bot_specs.len() == 2 {
        print_compare_focused(&report, &bot_specs);
    } else {
        print_compare_ranking(&report, &bot_specs);
    }

    if record_history_flag {
        let participants: Vec<(String, String)> = resolved
            .iter()
            .map(|r| (r.name.clone(), r.lang.clone()))
            .collect();
        record_history(&game, &participants, &report)?;
    }
    Ok(())
}

/// Append a `[[history]]` entry to every participant's `bot.toml`
/// summarising this run's pairwise outcomes against each opponent.
/// One entry per (bot, opponent) pair — so a 3-bot run writes 2
/// entries to each bot's manifest (its 2 opponents). Skips bots
/// whose bot.toml is missing rather than fabricating one.
/// Append a `[[history]]` entry to every participant's bot.toml
/// summarising this run's pairwise outcomes against each opponent.
/// `participants` is `(stem, lang)` pairs; we resolve `bot.toml`
/// per `games/<game>/bots/<stem>_<lang>/`. Skips (with warning)
/// any participant whose bot.toml is missing — happens when
/// `tournament run`'s `--bot name=path` names don't match a real
/// crate in the workspace.
fn record_history(
    game: &str,
    participants: &[(String, String)],
    report: &tournament::Report,
) -> Result<()> {
    use bot_manifest::{BotManifest, HistoryEntry, now_rfc3339};
    use tournament::pairwise_stats::Verdict;

    let ran_at = now_rfc3339();
    let mut wrote = 0usize;
    for (name, lang) in participants {
        let manifest_path = BotManifest::path(game, name, lang);
        if !manifest_path.exists() {
            eprintln!(
                "⚠ skipping history record for {}_{} — no bot.toml at {}",
                name,
                lang,
                manifest_path.display(),
            );
            continue;
        }
        let mut manifest = BotManifest::read(&manifest_path)?;
        for (other_name, _) in participants {
            if other_name == name {
                continue;
            }
            let Some(stats) = report.pair_stats(name, other_name) else {
                continue;
            };
            // The HistoryEntry's pts is from THIS bot's perspective
            // (effective wins, draws split 0.5/0.5), to match the
            // user's mental model from the printed verdict line.
            let pts = stats.wins_a as f64 + 0.5 * stats.draws as f64;
            let opp_pts = stats.wins_b as f64 + 0.5 * stats.draws as f64;
            let verdict = match stats.verdict {
                Verdict::Better => "significant",
                Verdict::Worse => "worse",
                Verdict::Inconclusive => "inconclusive",
            };
            manifest.history.push(HistoryEntry {
                ran_at: ran_at.clone(),
                opponent: other_name.clone(),
                rounds: stats.n,
                pts,
                opponent_pts: opp_pts,
                verdict: verdict.to_string(),
            });
        }
        manifest.write(&manifest_path)?;
        wrote += 1;
    }
    eprintln!("Recorded history to {wrote} bot.toml file(s).");
    Ok(())
}

/// Resolve a list of bot stems (each optionally `<bot>:<lang>` qualified)
/// to fully-resolved `ResolvedBot` entries, optionally running
/// `cargo build --profile <profile>` to make sure the binaries exist.
/// Shared by `run` and `compare`. Bails on duplicate stems, missing
/// crate dirs, build failure, and post-build missing artifacts — all
/// the same checks both verbs used to do inline.
fn resolve_and_build(
    game: &str,
    stems: &[String],
    no_build: bool,
    cargo_profile: &str,
) -> Result<Vec<ResolvedBot>> {
    let mut resolved: Vec<ResolvedBot> = stems
        .iter()
        .map(|spec| resolve_bot(game, spec, cargo_profile))
        .collect::<Result<_>>()?;

    // Allow duplicate stems (self-vs-self comparison) but
    // disambiguate their display names so the report's per-bot
    // table doesn't collapse them into one row. First occurrence
    // keeps the bare stem; subsequent ones get `#2`, `#3`, ...
    // Sequence is preserved by walking once with running counters.
    use std::collections::HashMap;
    let total: HashMap<&str, usize> =
        stems
            .iter()
            .map(String::as_str)
            .fold(HashMap::new(), |mut m, s| {
                *m.entry(s).or_insert(0) += 1;
                m
            });
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (stem, r) in stems.iter().zip(resolved.iter_mut()) {
        if total.get(stem.as_str()).copied().unwrap_or(0) > 1 {
            let n = seen.entry(stem.as_str()).or_insert(0);
            *n += 1;
            r.display_name = format!("{}#{}", r.name, *n);
        }
    }

    if !no_build {
        let crates: Vec<&str> = resolved.iter().map(|r| r.crate_name.as_str()).collect();
        eprintln!("Building: {} ({})…", crates.join(", "), cargo_profile);
        let mut cmd = ProcCommand::new("cargo");
        cmd.arg("build").args(["--profile", cargo_profile]);
        for c in &crates {
            cmd.arg("-p").arg(c);
        }
        let status = cmd
            .status()
            .with_context(|| "spawning cargo build for tournament bots")?;
        anyhow::ensure!(status.success(), "cargo build failed (exit {status})");
    }
    for r in &resolved {
        anyhow::ensure!(
            r.bin_path.exists(),
            "expected bot binary not found: {} (pass --no-build to skip the build step \
             only when you know the artifact is somewhere else)",
            r.bin_path.display(),
        );
    }
    Ok(resolved)
}

/// Find the bin for `<game>/<bot>[:<lang>]` and read its `bot.toml`
/// to recover the language. Accepts `<bot>:rs` or `<bot>:cpp` as an
/// explicit qualifier when both variants exist. `cargo_profile` is
/// the cargo build profile whose output dir we resolve from
/// (`target/<cargo_profile>/<bin>`).
fn resolve_bot(game: &str, spec: &str, cargo_profile: &str) -> Result<ResolvedBot> {
    let bots_dir = PathBuf::from("games").join(game).join("bots");
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );

    // Parse `<bot>:<lang>` qualifier if present.
    let (bot, explicit_lang): (&str, Option<&str>) = match spec.split_once(':') {
        Some((b, "rs")) => (b, Some("rs")),
        Some((b, "cpp")) => (b, Some("cpp")),
        Some((_, other)) => bail!("unknown lang qualifier `:{other}` (expected `:rs` or `:cpp`)"),
        None => (spec, None),
    };

    let rs_dir = bots_dir.join(format!("{bot}_rs"));
    let cpp_dir = bots_dir.join(format!("{bot}_cpp"));
    let lang: &str = match (explicit_lang, rs_dir.exists(), cpp_dir.exists()) {
        (Some("rs"), true, _) => "rs",
        (Some("cpp"), _, true) => "cpp",
        (Some(want), _, _) => bail!("{bot}:{want} not found at games/{game}/bots/{bot}_{want}/",),
        (None, true, false) => "rs",
        (None, false, true) => "cpp",
        (None, true, true) => {
            bail!("{bot} has both rs and cpp variants — qualify as `{bot}:rs` or `{bot}:cpp`",)
        }
        (None, false, false) => bail!("no bot at games/{game}/bots/{bot}_rs/ or _cpp/",),
    };

    let crate_name = format!("{game}_{bot}_{lang}");
    // Both rs and cpp bots produce a default-discovered bin named
    // after the crate, so the resolver doesn't need to special-case
    // lang.
    let bin_path = PathBuf::from("target")
        .join(cargo_profile)
        .join(format!("{crate_name}{}", std::env::consts::EXE_SUFFIX));
    Ok(ResolvedBot {
        name: bot.to_string(),
        display_name: bot.to_string(),
        lang: lang.to_string(),
        crate_name,
        bin_path,
    })
}

/// 2-bot single-verdict output. Resolves which bot the report
/// orients as "stronger", prints one line + a rounds-needed
/// epilogue when inconclusive.
fn print_compare_focused(report: &tournament::Report, bot_specs: &[BotSpec]) {
    use tournament::pairwise_stats::Verdict;
    let (a, b) = (bot_specs[0].name.as_str(), bot_specs[1].name.as_str());
    let Some(stats) = report.pair_stats(a, b) else {
        println!("no matches played between {a} and {b}");
        return;
    };

    // Orient so the LEFT bot is the stronger one — reads more naturally.
    let (left, right, s) = if stats.a_win_rate >= 0.5 {
        (a, b, stats)
    } else {
        (b, a, report.pair_stats(b, a).unwrap())
    };

    let lo = s.a_ci_95.0 * 100.0;
    let hi = s.a_ci_95.1 * 100.0;
    let half = (hi - lo) / 2.0;
    println!(
        "{left} vs {right}:  {wr:.1}% ± {half:.1}% (Wilson 95% CI),  LOS {los:.1}%,  p={p:.3}",
        wr = s.a_win_rate * 100.0,
        half = half,
        los = s.los * 100.0,
        p = s.p_value,
    );
    let verdict_line = match s.verdict {
        Verdict::Better => format!("VERDICT: {left} is BETTER  (significant at p<0.05)"),
        Verdict::Worse => format!("VERDICT: {left} is WORSE  (significant at p<0.05)"),
        Verdict::Inconclusive => match s.rounds_needed_for_significance() {
            Some(n) => format!(
                "VERDICT: INCONCLUSIVE — collected {} games, need ≈ {} to resolve a {:.1}% gap at p<0.05",
                s.n,
                n,
                (s.a_win_rate - 0.5).abs() * 100.0,
            ),
            None => "VERDICT: INCONCLUSIVE".to_string(),
        },
    };
    println!("{verdict_line}");
}

/// N≥3 output: pts ranking + the same pairwise verdict block the
/// `report` subcommand prints. No counters / timing / score columns
/// (those live in `report` proper for the full picture).
fn print_compare_ranking(report: &tournament::Report, bot_specs: &[BotSpec]) {
    println!("Ranking (by pts):");
    let mut sorted: Vec<(&String, &tournament::BotSummary)> = report.per_bot.iter().collect();
    sorted.sort_by(|a, b| {
        b.1.pts
            .partial_cmp(&a.1.pts)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let name_w = bot_specs.iter().map(|b| b.name.len()).max().unwrap_or(4);
    for (rank, (name, s)) in sorted.iter().enumerate() {
        let total = (s.wins + s.losses + s.draws).max(1);
        let win_pct = 100.0 * s.wins as f64 / total as f64;
        println!(
            "  {n}. {name:<width$}   pts {pts:>6.1}   {wp:>4.1}% wins overall",
            n = rank + 1,
            name = name,
            width = name_w,
            pts = s.pts,
            wp = win_pct,
        );
    }
}
