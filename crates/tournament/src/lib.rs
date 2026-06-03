//! Tournament harness. The CLI in `main.rs` calls into here; this
//! crate has no I/O code beyond JSONL serialization so it stays
//! easy to unit-test.
//!
//! Architecture (matches `docs/tournament.md`):
//!
//!   * [`BotSpec`] — a name + path for one entrant.
//!   * [`ScheduledMatch`] — a generated pairing: ordered bot indices
//!     + seed.
//!   * [`Schedule`] — `Vec<ScheduledMatch>` produced by
//!     [`build_schedule`] from the user's settings.
//!   * [`MatchRecord`] — the result of running one match, written
//!     one-per-line to the JSONL output and read back by the report.
//!   * [`run_match_named`] — runs one match by game name + paths,
//!     returning a `MatchRecord`.
//!   * [`report`] — reads a JSONL log, prints summary + win-rate
//!     matrix.
//!
//! The game-name dispatch in [`run_match_named`] expands the
//! `codingame_runner::for_each_game!` macro — adding a new game
//! is a one-line edit to that macro, not parallel edits here and
//! in the runner.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use codingame_runner::{for_each_game, make_player};
use common::engine::{Game, MatchResult, Player, PlayerStats, RunConfig, run_match};
use serde::{Deserialize, Serialize};

pub mod pairwise_stats;

// ============================================================
//  Inputs
// ============================================================

/// One entrant in the tournament.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotSpec {
    pub name: String,
    pub path: PathBuf,
}

/// One generated match: indices into the bot pool in seat order, plus
/// the seed to play it at. The schedule is a deterministic enumeration
/// of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMatch {
    /// Indices into the bot pool. `bot_idx[0]` is player 0, etc.
    pub bot_idx: Vec<usize>,
    pub seed: u64,
}

/// Settings that determine the matches we run. The lib is dumb
/// about seed selection — the caller (CLI) hands over the final
/// list it wants to play, including any random/sequential filler.
#[derive(Debug, Clone)]
pub struct ScheduleConfig {
    /// Players per match. Default 2.
    pub bots_per_match: usize,
    /// Seeds to play. Each seed is played once per (combination ×
    /// seat rotation). Empty means an empty schedule — the lib does
    /// no rounding/defaulting.
    pub seeds: Vec<u64>,
    /// If true (default), every combination is played in every cyclic
    /// seat rotation (N rotations for N bots). If false, only the
    /// order produced by the combination generator is used.
    pub rotate_seats: bool,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            bots_per_match: 2,
            seeds: Vec::new(),
            rotate_seats: true,
        }
    }
}

// ============================================================
//  Schedule generation
// ============================================================

/// Generate every match the tournament should run.
pub fn build_schedule(num_bots: usize, cfg: &ScheduleConfig) -> Result<Vec<ScheduledMatch>> {
    if cfg.bots_per_match < 2 {
        bail!(
            "--bots-per-match must be at least 2 (got {})",
            cfg.bots_per_match
        );
    }
    if cfg.bots_per_match > num_bots {
        bail!(
            "--bots-per-match {} exceeds bot pool size {}",
            cfg.bots_per_match,
            num_bots,
        );
    }

    let combos = combinations(num_bots, cfg.bots_per_match);
    let mut out = Vec::new();
    for combo in &combos {
        let assignments: Vec<Vec<usize>> = if cfg.rotate_seats {
            cyclic_rotations(combo)
        } else {
            vec![combo.clone()]
        };
        for assignment in &assignments {
            for &seed in &cfg.seeds {
                out.push(ScheduledMatch {
                    bot_idx: assignment.clone(),
                    seed,
                });
            }
        }
    }
    Ok(out)
}

/// All `k`-element combinations of `0..n`, in lexicographic order.
fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    fn rec(n: usize, k: usize, start: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        for i in start..n {
            cur.push(i);
            rec(n, k, i + 1, cur, out);
            cur.pop();
        }
    }
    let mut out = Vec::new();
    let mut cur = Vec::with_capacity(k);
    rec(n, k, 0, &mut cur, &mut out);
    out
}

/// `[a, b, c, d]` → `[[a,b,c,d], [b,c,d,a], [c,d,a,b], [d,a,b,c]]`.
fn cyclic_rotations(items: &[usize]) -> Vec<Vec<usize>> {
    (0..items.len())
        .map(|i| {
            let mut v: Vec<usize> = items[i..].to_vec();
            v.extend_from_slice(&items[..i]);
            v
        })
        .collect()
}

// ============================================================
//  Records (JSONL line format)
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchRecord {
    pub game: String,
    /// Bots in seat order. `bots[0]` was player 0, etc.
    pub bots: Vec<String>,
    pub seed: u64,
    /// Index into `bots`, or `None` for a draw.
    pub winner: Option<usize>,
    /// Final rank of each bot, 1-indexed, in seat order. Tied players
    /// share a rank (competition ranking). Driven by `Game::standings`
    /// — for tron that means survivors share rank 1 and dead players
    /// are ordered by death tick. Optional for backward compatibility
    /// with logs written before standings existed; missing standings
    /// is reconstructed from `winner` at read time.
    #[serde(default)]
    pub standings: Vec<u32>,
    /// Per-bot scores in seat order, from `Game::scores`. `None` for
    /// games where score isn't meaningful (tic-tac-toe). `Some` for
    /// games that track a continuous metric (tron's trail length,
    /// etc.). The report aggregates these as a tiebreaker alongside
    /// standings.
    #[serde(default)]
    pub scores: Option<Vec<f64>>,
    pub ticks: usize,
    pub stats: Vec<BotMatchStats>,
}

impl MatchRecord {
    /// Returns `standings` if set, or a best-effort reconstruction
    /// from `winner` (winner = 1, others = 2; draw = all 1). Older
    /// JSONL files written before the standings field exists get a
    /// reasonable degradation.
    pub fn standings_or_derive(&self) -> Vec<u32> {
        if !self.standings.is_empty() {
            return self.standings.clone();
        }
        match self.winner {
            Some(w) => (0..self.bots.len())
                .map(|i| if i == w { 1 } else { 2 })
                .collect(),
            None => vec![1; self.bots.len()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotMatchStats {
    pub turns: usize,
    pub avg_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

impl BotMatchStats {
    fn from(stats: &PlayerStats) -> Self {
        let p = |q: f64| percentile_ms(&stats.turn_times, q);
        Self {
            turns: stats.turn_times.len(),
            avg_ms: stats.average().map(d_ms),
            max_ms: stats.max().map(d_ms),
            p50_ms: p(0.50),
            p95_ms: p(0.95),
            p99_ms: p(0.99),
        }
    }
}

fn d_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentile_ms(times: &[Duration], q: f64) -> Option<f64> {
    if times.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = times.iter().map(|d| d_ms(*d)).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
}

// ============================================================
//  Match execution
// ============================================================

/// Play `schedule` sequentially against `bots`, emitting one JSONL
/// line per match to `writer` (flushed after each). Shared by the
/// sequential run path and the parallel worker subcommand — anything
/// "play a list of matches and stream results" goes through here.
pub fn play_schedule<W: std::io::Write>(
    game: &str,
    bots: &[BotSpec],
    schedule: &[ScheduledMatch],
    mut writer: W,
) -> Result<()> {
    for m in schedule {
        let bots: Vec<BotSpec> = m.bot_idx.iter().map(|&j| bots[j].clone()).collect();
        let rec = run_match_named(game, &bots, m.seed)?;
        serde_json::to_writer(&mut writer, &rec)
            .map_err(|e| anyhow::anyhow!("serialize match record: {e}"))?;
        writeln!(writer)?;
        writer.flush()?;
    }
    Ok(())
}

/// Run one match. `game` selects the dispatch arm; `bots` lists the
/// entrants in seat order. Game registry is shared with the runner
/// via `codingame_runner::for_each_game!` — see that macro for the
/// list.
pub fn run_match_named(game: &str, bots: &[BotSpec], seed: u64) -> Result<MatchRecord> {
    macro_rules! dispatch {
        ($name:literal, $ty:ty) => {
            if game == $name {
                return run_match_typed::<$ty>(game, bots, seed);
            }
        };
    }
    for_each_game!(dispatch);
    bail!("unknown game: {game}");
}

fn run_match_typed<G: Game>(
    game_name: &str,
    bots: &[BotSpec],
    seed: u64,
) -> Result<MatchRecord> {
    let num_players = bots.len() as u32;
    let mut players: Vec<Player<G>> = Vec::with_capacity(bots.len());
    for bot in bots {
        players.push(
            make_player::<G>(&bot.path)
                .with_context(|| format!("building player for {}", bot.name))?,
        );
    }

    let MatchResult {
        outcome,
        stats,
        replay,
        ..
    } = run_match::<G>(num_players, seed, players, RunConfig::default())
        .with_context(|| format!("running match for game {game_name}"))?;

    Ok(MatchRecord {
        game: game_name.to_string(),
        bots: bots.iter().map(|b| b.name.clone()).collect(),
        seed,
        winner: G::winner(&outcome).map(|p| p as usize),
        standings: G::standings(&outcome),
        scores: G::scores(&outcome),
        ticks: replay.outputs.len(),
        stats: stats.iter().map(BotMatchStats::from).collect(),
    })
}

// ============================================================
//  Report
// ============================================================

#[derive(Debug, Clone, Default)]
pub struct BotSummary {
    pub games: u32,
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    /// `standing_counts[k]` = number of matches where this bot
    /// finished at rank `k + 1`. Length grows to fit the largest
    /// rank observed across the log; missing tail entries are
    /// implicit zeros.
    pub standing_counts: Vec<u32>,
    /// Mean standings across all matches (lower is better). Computed
    /// as `sum_of_ranks / games`. `0.0` if no games played.
    pub avg_standing: f64,
    /// Aggregated score stats — `None` if this bot's game never
    /// emitted scores. The interpretation of "score" is per-game
    /// (trail length for tron, points for a scored game, ...);
    /// the tournament treats them as opaque floats.
    pub score_summary: Option<ScoreSummary>,
    /// Aggregated decision-time summaries. We don't carry raw turn
    /// times through the JSONL, so these are "stats of per-match
    /// stats" — useful for ranking but not exact distribution
    /// percentiles. If we need true global p95 across all turns,
    /// log raw turn times into the record and re-derive at report
    /// time.
    pub time_summary: TimeSummary,
    /// Pairwise tournament points. For each match: per opponent, +1
    /// if this bot strictly out-placed them, +0.5 if tied, 0
    /// otherwise; the per-match sum is divided by `num_opponents`
    /// so each match contributes at most 1 to `pts`. Reduces to
    /// `wins + 0.5 * draws` for 2-player matches; in multiplayer it
    /// rewards mid-pack finishes proportionally (2nd in a 4-player
    /// match ≈ 0.67 pts, vs 4th = 0). Comparable across games of
    /// different player counts because the per-match cap is always 1.
    pub pts: f64,
}

impl BotSummary {
    fn record_standing(&mut self, rank: u32) {
        let idx = (rank - 1) as usize;
        if self.standing_counts.len() <= idx {
            self.standing_counts.resize(idx + 1, 0);
        }
        self.standing_counts[idx] += 1;
        // Incremental mean: `avg += (x - avg) / n`.
        let n = self.games as f64;
        self.avg_standing += (rank as f64 - self.avg_standing) / n;
    }

    fn record_score(&mut self, score: f64) {
        self.score_summary
            .get_or_insert_with(ScoreSummary::default)
            .add(score);
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScoreSummary {
    pub avg: f64,
    pub min: f64,
    pub max: f64,
    samples: u32,
}

impl ScoreSummary {
    fn add(&mut self, x: f64) {
        self.samples += 1;
        let n = self.samples as f64;
        self.avg += (x - self.avg) / n;
        if self.samples == 1 {
            self.min = x;
            self.max = x;
        } else {
            if x < self.min {
                self.min = x;
            }
            if x > self.max {
                self.max = x;
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TimeSummary {
    /// Average of per-match `avg_ms`.
    pub avg_of_avg_ms: f64,
    /// Average of per-match `p95_ms`.
    pub avg_of_p95_ms: f64,
    /// Worst per-match `max_ms` we ever observed.
    pub worst_max_ms: f64,
    /// Number of matches contributing to the averages (== games).
    samples: u32,
}

impl TimeSummary {
    fn add(&mut self, m: &BotMatchStats) {
        self.samples += 1;
        let n = self.samples as f64;
        let mix = |old: f64, new_sample: Option<f64>| -> f64 {
            match new_sample {
                Some(x) => old + (x - old) / n,
                None => old,
            }
        };
        self.avg_of_avg_ms = mix(self.avg_of_avg_ms, m.avg_ms);
        self.avg_of_p95_ms = mix(self.avg_of_p95_ms, m.p95_ms);
        if let Some(x) = m.max_ms
            && x > self.worst_max_ms
        {
            self.worst_max_ms = x;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    /// Bot name → summary.
    pub per_bot: std::collections::BTreeMap<String, BotSummary>,
    /// `pair_wins[(a, b)]` = matches in which `a` beat `b` directly
    /// (decomposing N-player matches as "winner beats each loser").
    pub pair_wins: std::collections::BTreeMap<(String, String), u32>,
    /// `pair_games[(a, b)]` = matches in which `a` and `b` appeared
    /// together (excluding draws from `pair_wins`).
    pub pair_games: std::collections::BTreeMap<(String, String), u32>,
}

impl Report {
    /// `PairStats` for the matchup `a` vs `b`, oriented A-side. Returns
    /// `None` if the pair never played together. Shared by the
    /// adaptive-stopping check, the pairwise verdict block, the
    /// focused 2-bot compare output, and history recording — anything
    /// that wants "the (wins_a, wins_b, draws, p, LOS, ...) tuple".
    pub fn pair_stats(&self, a: &str, b: &str) -> Option<pairwise_stats::PairStats> {
        let games = self
            .pair_games
            .get(&(a.to_string(), b.to_string()))
            .copied()
            .unwrap_or(0);
        if games == 0 {
            return None;
        }
        let wins_a = self
            .pair_wins
            .get(&(a.to_string(), b.to_string()))
            .copied()
            .unwrap_or(0);
        let wins_b = self
            .pair_wins
            .get(&(b.to_string(), a.to_string()))
            .copied()
            .unwrap_or(0);
        let draws = games.saturating_sub(wins_a + wins_b);
        Some(pairwise_stats::PairStats::compute(wins_a, wins_b, draws))
    }
}

/// Build a [`Report`] from a sequence of [`MatchRecord`]s. Pure: no
/// I/O; the CLI is responsible for reading the JSONL stream.
pub fn build_report(records: &[MatchRecord]) -> Report {
    use std::collections::BTreeMap;

    let mut per_bot: BTreeMap<String, BotSummary> = BTreeMap::new();
    let mut pair_wins: BTreeMap<(String, String), u32> = BTreeMap::new();
    let mut pair_games: BTreeMap<(String, String), u32> = BTreeMap::new();

    for rec in records {
        let standings = rec.standings_or_derive();

        // Track participation. `games` is incremented *before*
        // `record_standing` because the incremental-mean formula
        // expects `n` to already include the new sample.
        for name in &rec.bots {
            let s = per_bot.entry(name.clone()).or_default();
            s.games += 1;
        }
        for (i, name_i) in rec.bots.iter().enumerate() {
            let s = per_bot.get_mut(name_i).unwrap();
            s.time_summary.add(&rec.stats[i]);
            s.record_standing(standings[i]);
            if let Some(scores) = &rec.scores
                && let Some(score) = scores.get(i)
            {
                s.record_score(*score);
            }
        }

        // Wins / losses / draws are projected from rank: rank 1 wins
        // (or draws if shared); anyone not at rank 1 loses (or
        // shares the draw if everyone is at rank 1).
        let firsts: Vec<usize> = (0..rec.bots.len()).filter(|&i| standings[i] == 1).collect();
        if firsts.len() == rec.bots.len() {
            for name in &rec.bots {
                per_bot.get_mut(name).unwrap().draws += 1;
            }
        } else {
            for (i, name) in rec.bots.iter().enumerate() {
                let s = per_bot.get_mut(name).unwrap();
                if standings[i] == 1 {
                    s.wins += 1;
                } else {
                    s.losses += 1;
                }
            }
        }

        // pair_games counts every ordered (a, b) where both played
        // together. Done unconditionally so the matrix denominator
        // is "matches in which a met b".
        for a in &rec.bots {
            for b in &rec.bots {
                if a != b {
                    *pair_games.entry((a.clone(), b.clone())).or_insert(0) += 1;
                }
            }
        }

        // Placement-pairwise pair_wins + tournament points. For every
        // ordered pair (i, j) with i < j: better rank → +1 to
        // pair_wins[(i, j)]; the bot in the better-ranked seat also
        // earns 1 pair-point, ties split 0.5/0.5. Per-match pair-
        // points are divided by `n_opponents` at the end so each
        // match contributes at most 1.0 to `pts` regardless of
        // player count — comparable across 2-player and 4-player
        // games. Ties contribute to neither side's pair_wins (they
        // show up as the gap between pair_games and pair_wins in the
        // matrix).
        let n = rec.bots.len();
        let denom = (n.saturating_sub(1)).max(1) as f64;
        for i in 0..n {
            for j in (i + 1)..n {
                let (name_i, name_j) = (&rec.bots[i], &rec.bots[j]);
                let (rank_i, rank_j) = (standings[i], standings[j]);
                let (s_i, s_j) = match rank_i.cmp(&rank_j) {
                    std::cmp::Ordering::Less => {
                        *pair_wins
                            .entry((name_i.clone(), name_j.clone()))
                            .or_insert(0) += 1;
                        (1.0, 0.0)
                    }
                    std::cmp::Ordering::Greater => {
                        *pair_wins
                            .entry((name_j.clone(), name_i.clone()))
                            .or_insert(0) += 1;
                        (0.0, 1.0)
                    }
                    std::cmp::Ordering::Equal => (0.5, 0.5),
                };
                per_bot.get_mut(name_i).unwrap().pts += s_i / denom;
                per_bot.get_mut(name_j).unwrap().pts += s_j / denom;
            }
        }
    }

    Report {
        per_bot,
        pair_wins,
        pair_games,
    }
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_basic() {
        assert_eq!(
            combinations(4, 2),
            vec![
                vec![0, 1],
                vec![0, 2],
                vec![0, 3],
                vec![1, 2],
                vec![1, 3],
                vec![2, 3],
            ],
        );
        assert_eq!(combinations(3, 3), vec![vec![0, 1, 2]]);
        assert_eq!(combinations(3, 1), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn cyclic_rotations_basic() {
        assert_eq!(
            cyclic_rotations(&[0, 1, 2]),
            vec![vec![0, 1, 2], vec![1, 2, 0], vec![2, 0, 1]],
        );
    }

    #[test]
    fn schedule_pairs_with_rotation_and_seeds() {
        let cfg = ScheduleConfig {
            bots_per_match: 2,
            seeds: vec![10, 20],
            rotate_seats: true,
        };
        let sched = build_schedule(3, &cfg).unwrap();
        // 3 combos × 2 rotations × 2 seeds = 12
        assert_eq!(sched.len(), 12);
        // First combo (0, 1): both seat orders, both seeds.
        assert!(sched.contains(&ScheduledMatch {
            bot_idx: vec![0, 1],
            seed: 10
        }));
        assert!(sched.contains(&ScheduledMatch {
            bot_idx: vec![1, 0],
            seed: 10
        }));
    }

    #[test]
    fn schedule_three_player_full_pool() {
        let cfg = ScheduleConfig {
            bots_per_match: 3,
            seeds: vec![0],
            rotate_seats: true,
        };
        let sched = build_schedule(3, &cfg).unwrap();
        // 1 combo × 3 rotations × 1 seed = 3
        assert_eq!(sched.len(), 3);
    }

    #[test]
    fn schedule_rejects_oversize_match() {
        let cfg = ScheduleConfig {
            bots_per_match: 5,
            ..Default::default()
        };
        assert!(build_schedule(3, &cfg).is_err());
    }

    #[test]
    fn schedule_empty_seeds_is_empty() {
        // Lib doesn't default-fill; the CLI is responsible for
        // providing the final seed list.
        let cfg = ScheduleConfig {
            bots_per_match: 2,
            seeds: vec![],
            rotate_seats: false,
        };
        assert!(build_schedule(2, &cfg).unwrap().is_empty());
    }

    fn rec(bots: &[&str], winner: Option<usize>) -> MatchRecord {
        // 2-player rec helper; standings derived from winner.
        let standings = match winner {
            Some(w) => (0..bots.len())
                .map(|i| if i == w { 1 } else { 2 } as u32)
                .collect(),
            None => vec![1u32; bots.len()],
        };
        MatchRecord {
            game: "tron".into(),
            bots: bots.iter().map(|s| s.to_string()).collect(),
            seed: 0,
            winner,
            standings,
            scores: None,
            ticks: 5,
            stats: (0..bots.len())
                .map(|_| BotMatchStats {
                    turns: 1,
                    avg_ms: Some(1.0),
                    max_ms: Some(1.0),
                    p50_ms: Some(1.0),
                    p95_ms: Some(1.0),
                    p99_ms: Some(1.0),
                })
                .collect(),
        }
    }

    fn rec_standings(bots: &[&str], standings: Vec<u32>) -> MatchRecord {
        rec_standings_scores(bots, standings, None)
    }

    fn rec_standings_scores(
        bots: &[&str],
        standings: Vec<u32>,
        scores: Option<Vec<f64>>,
    ) -> MatchRecord {
        let firsts: Vec<usize> = (0..bots.len()).filter(|&i| standings[i] == 1).collect();
        let winner = if firsts.len() == 1 {
            Some(firsts[0])
        } else {
            None
        };
        MatchRecord {
            game: "tron".into(),
            bots: bots.iter().map(|s| s.to_string()).collect(),
            seed: 0,
            winner,
            standings,
            scores,
            ticks: 5,
            stats: (0..bots.len())
                .map(|_| BotMatchStats {
                    turns: 1,
                    avg_ms: Some(1.0),
                    max_ms: Some(1.0),
                    p50_ms: Some(1.0),
                    p95_ms: Some(1.0),
                    p99_ms: Some(1.0),
                })
                .collect(),
        }
    }

    #[test]
    fn report_counts_wins_losses_draws() {
        let records = vec![
            rec(&["a", "b"], Some(0)), // a beats b
            rec(&["a", "b"], Some(1)), // b beats a
            rec(&["a", "b"], None),    // draw
            rec(&["a", "b"], Some(0)), // a beats b
        ];
        let report = build_report(&records);
        let a = &report.per_bot["a"];
        let b = &report.per_bot["b"];
        assert_eq!((a.games, a.wins, a.losses, a.draws), (4, 2, 1, 1));
        assert_eq!((b.games, b.wins, b.losses, b.draws), (4, 1, 2, 1));
    }

    #[test]
    fn report_decomposes_multiplayer_match_into_pairs() {
        let records = vec![rec(&["a", "b", "c"], Some(0))]; // a wins
        let report = build_report(&records);
        assert_eq!(report.per_bot["a"].wins, 1);
        assert_eq!(report.per_bot["b"].losses, 1);
        assert_eq!(report.per_bot["c"].losses, 1);
        assert_eq!(report.pair_wins[&("a".into(), "b".into())], 1);
        assert_eq!(report.pair_wins[&("a".into(), "c".into())], 1);
    }

    #[test]
    fn report_pair_games_counts_all_pairs_regardless_of_outcome() {
        // Three-player match. a wins; the matrix needs pair_games to
        // include (b, c) and (c, b) too — those bots *did* meet.
        let records = vec![rec(&["a", "b", "c"], Some(0))];
        let report = build_report(&records);
        // Every ordered pair appears exactly once.
        for (a, b) in [
            ("a", "b"),
            ("a", "c"),
            ("b", "a"),
            ("b", "c"),
            ("c", "a"),
            ("c", "b"),
        ] {
            assert_eq!(
                report.pair_games[&(a.into(), b.into())],
                1,
                "pair_games[({a}, {b})] should be 1",
            );
        }
    }

    #[test]
    fn report_pair_games_counts_draws() {
        let records = vec![rec(&["a", "b"], None)];
        let report = build_report(&records);
        assert_eq!(report.pair_games[&("a".into(), "b".into())], 1);
        assert_eq!(report.pair_games[&("b".into(), "a".into())], 1);
        // No wins.
        assert_eq!(report.pair_wins.get(&("a".into(), "b".into())), None);
    }

    #[test]
    fn standings_pairwise_separates_2nd_from_4th() {
        // The motivating case: a is always 2nd, b is always 4th.
        // Both lose every match (rank > 1), but avg_standing +
        // pair_wins + pts separate them.
        let records = vec![
            rec_standings(&["w", "a", "x", "b"], vec![1, 2, 3, 4]),
            rec_standings(&["w", "a", "x", "b"], vec![1, 2, 3, 4]),
            rec_standings(&["w", "a", "x", "b"], vec![1, 2, 3, 4]),
        ];
        let report = build_report(&records);
        let a = &report.per_bot["a"];
        let b = &report.per_bot["b"];
        // Both have 0 wins, 3 losses — binary win-rate is a tie.
        assert_eq!((a.wins, a.losses, b.wins, b.losses), (0, 3, 0, 3));
        // But avg standings, the pair matrix, and pts all separate
        // them: a out-placed b in every match (a was 2nd, b was 4th).
        assert!(a.avg_standing < b.avg_standing);
        assert_eq!(report.pair_wins[&("a".into(), "b".into())], 3);
        assert_eq!(report.pair_wins.get(&("b".into(), "a".into())), None);
        // Pairwise pts: per 4-player match, a (rank 2) beats 2 of 3
        // opponents → 2/3 per match → 2.0 across 3 matches.
        // b (rank 4) beats 0 of 3 → 0 per match → 0.0 total.
        // w (rank 1) beats all 3 → 1.0 per match → 3.0 total.
        let w = &report.per_bot["w"];
        assert!((a.pts - 2.0).abs() < 1e-9, "a.pts = {}", a.pts);
        assert!((b.pts - 0.0).abs() < 1e-9, "b.pts = {}", b.pts);
        assert!((w.pts - 3.0).abs() < 1e-9, "w.pts = {}", w.pts);
        // a placed 2nd 3 times.
        assert_eq!(a.standing_counts[1], 3);
        // b placed 4th 3 times.
        assert_eq!(b.standing_counts[3], 3);
    }

    #[test]
    fn scores_aggregate_separately_from_standings() {
        // Two matches: a and b tied for 1st in both. Same standings,
        // different score profile. Aggregation should reflect that.
        let records = vec![
            rec_standings_scores(&["a", "b"], vec![1, 1], Some(vec![100.0, 50.0])),
            rec_standings_scores(&["a", "b"], vec![1, 1], Some(vec![80.0, 60.0])),
        ];
        let report = build_report(&records);
        let a = &report.per_bot["a"];
        let b = &report.per_bot["b"];
        // Same standings.
        assert_eq!(a.avg_standing, b.avg_standing);
        // Different scores.
        let a_s = a.score_summary.as_ref().unwrap();
        let b_s = b.score_summary.as_ref().unwrap();
        assert!((a_s.avg - 90.0).abs() < 1e-9);
        assert!((b_s.avg - 55.0).abs() < 1e-9);
        assert_eq!((a_s.min, a_s.max), (80.0, 100.0));
        assert_eq!((b_s.min, b_s.max), (50.0, 60.0));
    }

    #[test]
    fn missing_scores_leave_summary_none() {
        // tic-tac-toe records have scores: None — bot's score_summary
        // stays None throughout.
        let records = vec![rec(&["a", "b"], Some(0)), rec(&["a", "b"], None)];
        let report = build_report(&records);
        assert!(report.per_bot["a"].score_summary.is_none());
        assert!(report.per_bot["b"].score_summary.is_none());
    }

    #[test]
    fn standings_handle_tied_survivors() {
        // 4-player mutual death (everyone rank 1). Should count as
        // a draw for everyone (no wins/losses, no pair_wins).
        let records = vec![rec_standings(&["a", "b", "c", "d"], vec![1, 1, 1, 1])];
        let report = build_report(&records);
        for name in ["a", "b", "c", "d"] {
            let s = &report.per_bot[name];
            assert_eq!((s.wins, s.losses, s.draws), (0, 0, 1));
        }
        assert!(
            report.pair_wins.is_empty(),
            "ties should produce no pair_wins"
        );
    }
}
