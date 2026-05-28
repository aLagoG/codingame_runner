//! CLI front-end for the tournament harness. Subcommands:
//!
//!   * `run`    — schedule matches and stream a JSONL log of results.
//!   * `report` — read a JSONL log and print summary + win-rate matrix.
//!
//! Everything stateful (schedule generation, match execution, report
//! aggregation) lives in the `tournament` library; this file is just
//! arg parsing and I/O.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Command as ProcCommand, Stdio};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
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
    /// Internal: play a schedule chunk read from stdin and write
    /// JSONL results to stdout. Not for direct use — `run --parallel
    /// N` spawns N of these. `clap`'s `hide = true` keeps it out of
    /// `--help`.
    #[command(hide = true)]
    Worker(WorkerArgs),
}

#[derive(Parser)]
#[command(about = "Schedule and play matches; stream a JSONL result log.")]
struct RunArgs {
    /// Game to play (`tictactoe`, `tron`).
    #[arg(long)]
    game: String,

    /// Entrant in the form `name=path/to/bot`. Pass `--bot` multiple
    /// times; names must be unique. Plugins are loaded via FFI
    /// (`.so` / `.dylib` / `.dll`); everything else is spawned as a
    /// subprocess.
    #[arg(long = "bot", value_parser = parse_bot_spec, required = true)]
    bots: Vec<BotSpec>,

    /// Players per match. 2 by default. Must be in `[2, num_bots]`.
    #[arg(long, default_value_t = 2)]
    bots_per_match: usize,

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

    /// Number of seeds to play per (combination × seat rotation).
    /// `--seeds` guarantees inclusion of specific values; everything
    /// else is filled from the sequential range or `--random-seeds`
    /// up to this count.
    #[arg(long, default_value_t = 100)]
    rounds: u64,

    /// By default every combination is played in all N cyclic seat
    /// rotations. Pass `--no-rotate-seats` to skip rotation entirely
    /// (only the combination's natural order is used).
    #[arg(long)]
    no_rotate_seats: bool,

    /// Where to write the JSONL match log. Parent dirs are created.
    #[arg(short, long)]
    output: PathBuf,

    /// Number of matches to run in parallel. Defaults to the number
    /// of available logical cores. Set to 1 for clean per-turn
    /// decision-time measurements; higher values trade timing
    /// fidelity for wall-clock speedup. When >1, the matches run in
    /// separate worker processes so FFI plugins (which have shared
    /// global state) stay safely isolated.
    #[arg(long, default_value_t = default_parallel())]
    parallel: usize,

    /// Enable FFI counter capture. Subprocess bots ignore this;
    /// plugin bots that export `set_counter_callback` get a
    /// callback registered and their per-tick counter emissions
    /// are aggregated into the JSONL log + report.
    #[arg(long)]
    counters: bool,
}

#[derive(Parser)]
struct WorkerArgs {
    #[arg(long)]
    game: String,
    #[arg(long = "bot", value_parser = parse_bot_spec, required = true)]
    bots: Vec<BotSpec>,
    /// Mirror of the parent's `--counters` flag — propagated through
    /// the worker spawn so plugin players in each worker register
    /// the callback independently.
    #[arg(long)]
    counters: bool,
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
        Command::Worker(args) => cmd_worker(args),
    }
}

// ============================================================
//  run
// ============================================================

fn cmd_run(args: RunArgs) -> Result<()> {
    let mut seen = BTreeSet::new();
    for b in &args.bots {
        if !seen.insert(b.name.clone()) {
            bail!("duplicate bot name: {}", b.name);
        }
    }

    let seeds = assemble_seeds(&args.seeds, args.rounds as usize, args.random_seeds);
    let cfg = ScheduleConfig {
        bots_per_match: args.bots_per_match,
        seeds,
        rotate_seats: !args.no_rotate_seats,
    };
    let schedule = build_schedule(args.bots.len(), &cfg)?;
    let total = schedule.len();
    if total == 0 {
        bail!("schedule is empty — check --bots-per-match / --seeds / --rounds");
    }

    if let Some(parent) = args.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = File::create(&args.output)
        .with_context(|| format!("creating {}", args.output.display()))?;
    let out = BufWriter::new(file);

    // Clamp `--parallel` to something useful: at most one worker
    // per match, and at least 1.
    let parallel = args.parallel.clamp(1, total).max(1);
    if parallel > 1 {
        eprintln!(
            "⚠ Running with --parallel {parallel}. Per-turn decision-time \
             numbers will be affected by CPU contention; use --parallel 1 \
             for clean timing baselines."
        );
    }

    eprintln!("Running {total} matches of {} (--parallel {parallel})…", args.game);
    if parallel == 1 {
        run_sequential(&args.game, &args.bots, &schedule, args.counters, out, total)?;
    } else {
        run_parallel(&args.game, &args.bots, schedule, args.counters, out, parallel)?;
    }

    eprintln!("Wrote {} → {}", total, args.output.display());
    Ok(())
}

fn run_sequential(
    game: &str,
    bots: &[BotSpec],
    schedule: &[ScheduledMatch],
    enable_counters: bool,
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
        let rec = tournament::run_match_named(game, &entries, m.seed, enable_counters)
            .with_context(|| format!("match {} ({})", i + 1, names.join(" vs ")))?;
        serde_json::to_writer(&mut out, &rec)?;
        writeln!(out)?;
        out.flush()?;
    }
    Ok(())
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
    enable_counters: bool,
    mut out: BufWriter<File>,
    parallel: usize,
) -> Result<()> {
    let total = schedule.len();
    let exe = std::env::current_exe().context("locating tournament binary")?;

    // Round-robin partition the schedule across `parallel` workers.
    let mut partitions: Vec<Vec<ScheduledMatch>> =
        (0..parallel).map(|_| Vec::new()).collect();
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
        if enable_counters {
            cmd.arg("--counters");
        }
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
    for (worker, partition) in workers.iter_mut().zip(partitions.into_iter()) {
        let stdin = worker.stdin.take().expect("piped");
        writers.push(thread::spawn(move || -> std::io::Result<()> {
            let mut stdin = BufWriter::new(stdin);
            serde_json::to_writer(&mut stdin, &partition)
                .map_err(std::io::Error::other)?;
            stdin.flush()?;
            // Dropping `stdin` here closes the pipe → worker sees
            // EOF on its end and starts playing.
            Ok(())
        }));
    }

    // Stream records to the output file as they arrive. Order is
    // by completion time, not by schedule index — the report
    // doesn't care, and each record carries enough metadata
    // (bots, seed) to be self-identifying.
    let mut done = 0usize;
    for line in rx {
        writeln!(out, "{line}")?;
        out.flush()?;
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
    let schedule: Vec<ScheduledMatch> = serde_json::from_reader(stdin.lock())
        .context("parsing schedule chunk from stdin")?;

    // Play it and stream JSONL on stdout. Each line is one
    // MatchRecord; main's reader thread picks them up by line.
    let stdout = std::io::stdout();
    play_schedule(&args.game, &args.bots, &schedule, args.counters, stdout.lock())
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

/// Generate `n` u64 seeds from process entropy. No external crate:
/// `std::collections::hash_map::RandomState` is seeded from a
/// process-wide non-deterministic source (`getrandom` on most
/// platforms), and `SipHasher::write_u64(nanos) + write_usize(i)`
/// gives us per-call uniqueness without needing a real RNG.
fn random_seeds(n: usize) -> Vec<u64> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let state = RandomState::new();
    (0..n)
        .map(|i| {
            let mut h = state.build_hasher();
            h.write_u64(nanos);
            h.write_usize(i);
            h.finish()
        })
        .collect()
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
    let file = File::open(&args.input)
        .with_context(|| format!("opening {}", args.input.display()))?;
    let mut records: Vec<MatchRecord> = Vec::new();
    for (lineno, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: MatchRecord = serde_json::from_str(&line)
            .with_context(|| format!("parsing line {} of {}", lineno + 1, args.input.display()))?;
        records.push(rec);
    }
    if records.is_empty() {
        bail!("no records in {}", args.input.display());
    }

    let report = build_report(&records);
    print_summary(&report);
    println!();
    print_matrix(&report);
    Ok(())
}

fn print_summary(report: &tournament::Report) {
    let names: Vec<&String> = report.per_bot.keys().collect();
    let name_w = names.iter().map(|n| n.len()).max().unwrap_or(4).max(4);

    let max_rank = report
        .per_bot
        .values()
        .map(|s| s.placement_counts.len())
        .max()
        .unwrap_or(0);
    let any_scores = report
        .per_bot
        .values()
        .any(|s| s.score_summary.is_some());
    // Collect the union of counter names across all bots so we
    // print one column per counter, with `-` for bots that didn't
    // emit it.
    let counter_keys: std::collections::BTreeSet<String> = report
        .per_bot
        .values()
        .flat_map(|s| s.counter_summary.keys().cloned())
        .collect();

    let mut header = format!(
        "{:<width$}  {:>5}  {:>5}  {:>6}  {:>5}  {:>5}  {:>6}  {:>6}",
        "bot", "games", "wins", "losses", "draws", "win%", "elo", "avgpl",
        width = name_w,
    );
    for r in 1..=max_rank {
        header.push_str(&format!("  {:>4}", format!("{}{}", r, ordinal_suffix(r))));
    }
    if any_scores {
        header.push_str(&format!("  {:>8}  {:>7}  {:>7}", "avg sc", "min sc", "max sc"));
    }
    for key in &counter_keys {
        // Headline: average across matches of the per-match average.
        // Tight ~9-char column so a handful of counters fit on one
        // line without wrapping.
        header.push_str(&format!("  {:>9}", truncate(key, 9)));
    }
    header.push_str(&format!("  {:>7}  {:>7}  {:>7}", "avg ms", "p95 ms", "max ms"));
    println!("{header}");
    println!("{}", "-".repeat(header.len()));

    for (name, s) in &report.per_bot {
        let total = (s.wins + s.losses + s.draws).max(1);
        let win_pct = 100.0 * s.wins as f64 / total as f64;
        let mut row = format!(
            "{:<width$}  {:>5}  {:>5}  {:>6}  {:>5}  {:>4.0}%  {:>+6.0}  {:>6.2}",
            name,
            s.games,
            s.wins,
            s.losses,
            s.draws,
            win_pct,
            s.elo - 1500.0,
            s.avg_placement,
            width = name_w,
        );
        for r in 0..max_rank {
            let n = s.placement_counts.get(r).copied().unwrap_or(0);
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
        for key in &counter_keys {
            match s.counter_summary.get(key) {
                Some(c) => row.push_str(&format!("  {:>9.2}", c.avg_of_avg)),
                None => row.push_str(&format!("  {:>9}", "-")),
            }
        }
        row.push_str(&format!(
            "  {:>7.2}  {:>7.2}  {:>7.2}",
            s.time_summary.avg_of_avg_ms,
            s.time_summary.avg_of_p95_ms,
            s.time_summary.worst_max_ms,
        ));
        println!("{row}");
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
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
