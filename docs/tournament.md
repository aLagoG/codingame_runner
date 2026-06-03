# Tournament Harness — Design Note

**Status**: implemented; this doc is the original design rationale (Option A / round-robin landed; Elo + Swiss were intentionally deferred and remain deferred). For current usage see `cargo run -p tournament -- --help`. The CLI example below and the dlopen-caching section have been updated/redacted to match the post-FFI implementation; everything else is the original design text.

## Why this exists

`cargo run -p codingame_runner -- --game tron <bot1> <bot2>` plays one match. That's enough for debugging but useless for ranking. We need to answer:

- "Is v2 actually stronger than v1, or did we get lucky on one seed?"
- "What's the win-rate matrix when I bring three bots into the same arena?"
- "Are decision times stable under load, or does v2 blow its budget when paired against a CPU-hungry opponent?"

All of those are statistical questions over many matches. The tournament harness exists to run those matches and aggregate the results.

---

## Structural options

### Option A — Round-robin

Every bot plays every other bot, `K` matches per pair. For pairs of size 2 (most games), alternate seat assignment to remove first-player bias.

```
N bots, K rounds → N·(N-1)/2 · K matches
4 bots, 50 rounds → 300 matches
```

**Pros**
- Conceptually simple and statistically complete: every pairing gets the same number of games.
- Per-pair win rates are directly comparable.
- Easy to parallelize (every match is independent).

**Cons**
- Quadratic in bot count. Fine at 4-10 bots; awkward at 50.
- "Strong vs weak" matchups are uninformative noise — strong bot wins most.

**Best for**: small-to-medium bot pool, validation-style runs (v1 vs v2 vs cpp_v1 vs cpp_v2).

### Option B — Swiss

`M` rounds; each round pairs bots with similar running scores. After M rounds, total points = ranking.

**Pros**
- Sub-quadratic; works at larger bot counts.
- Spends matchup budget where it disambiguates: top half vs top half.

**Cons**
- Pairings are sequential — harder to parallelize across rounds.
- Less informative per-pair (a strong vs weak matchup may never happen).
- Bot count needs to be ≥ ~2^M for clean Swiss pairings.

**Best for**: ladder-style "rank these 30 candidates" workflows.

### Option C — Elo-rated continuous tournament

Maintain an Elo rating per bot. Schedule matches forever; each result updates Elo via the standard formula. Stop when ratings stabilize.

**Pros**
- Cheap per-match; ranking converges as you spend more compute.
- Natural for "kept running" CI workflows.

**Cons**
- Match scheduling needs care to avoid pathological imbalance.
- More complex; harder to reason about "did v2 beat v1 here".

**Best for**: long-lived bot pools where you keep adding entrants.

### Option D — Single/double-elimination bracket

Pros: dramatic. Cons: nearly useless for actual comparison — one loss eliminates a bot whose true skill is high.

**Best for**: showmanship. Not what we need.

### Recommendation

Start with **A (round-robin)** for the size of pool we have today (≤ 10 bots). Compute Elo from the round-robin results as a derived ranking metric — that gets us both per-pair data *and* a single number per bot. If/when the pool grows past ~20 bots, switch to **B (Swiss)** for scheduling but keep the same result format so downstream tooling doesn't change.

---

## Match format options

How does each pairing decide a winner?

- **Single match per pairing**: noisy on stochastic games, fine on deterministic ones (tron with a fixed seed is deterministic for both players' decisions). With `seed = 0` always, two pure-functional bots play the same game every time — no information from repeating.
- **K matches with seed sweep**: vary `seed ∈ 0..K`. Recommended default.
- **K matches × 2 seats**: also alternate which bot is player 0 vs player 1 to cancel first-player advantage. Recommended default for 2-player games.
- **Best-of-K**: for binary "who's better"; not useful for tournaments where we want continuous data.

**Recommended**: `K matches × 2 seats × seed sweep` — that's `2K` matches per pair, balanced across seats and seeds.

---

## What we record

For each match (one JSON line in a results file):
```json
{
  "pair": ["tron_rs_v1", "tron_cpp_v2"],
  "seats": [0, 1],
  "seed": 17,
  "outcome": {"winner": 1, "ticks": 84},
  "stats": [
    {"avg_ms": 12.3, "max_ms": 41.0, "p95_ms": 28.0, "p99_ms": 39.0, "turns": 42},
    {"avg_ms": 56.1, "max_ms": 92.5, "p95_ms": 84.0, "p99_ms": 91.0, "turns": 42}
  ],
  "errors": []
}
```

Aggregated per-bot (CSV or JSON, derived from the per-match log):
- Matches played, wins, losses, draws, win-rate, win-rate 95% CI
- Avg / max / p50 / p95 / p99 decision time
- Avg ticks (longer = closer game; shorter loss = blew up early)
- Elo rating (derived from the match log via standard Elo update)
- Counts of `PlayerError::Timeout`, `Eof`, `Panic`, `InvalidOutput` — bots that win by timing the opponent out vs bots that win cleanly

**Why bot stats matter (this is where `PlayerStats` shines):** a bot that wins 60% of matches *and* averages 30ms per move is strictly better than one that wins 60% averaging 85ms — the latter is one slow opponent away from timing out and forfeiting. The tournament's job is to surface both axes.

---

## Parallelism

Every match in a round-robin is independent. The harness should run them in a thread pool sized to `--parallel` (default: physical core count).

Caveats:
- The runner currently does no resource isolation between bot subprocesses; running 8 matches concurrently means 16 bot processes contending for the same CPU. Time-budget assertions become noisy. Two options:
  - Run **sequentially** for time-sensitive runs; parallel only for outcome-only runs.
  - Or run with `taskset --cpu-list` / `nice` so each match owns its own core. (Adds platform-specific glue.)
- TT memory: v2's TT is 3 MB per bot process. 16 processes × 3 MB = 48 MB. Fine.

**Recommended default**: `--parallel 1` for runs where decision-time data matters; user can crank it up explicitly for outcome-only mass runs.

---

## CLI design

```sh
cargo run -p tournament -- run \
    --game tron \
    --rounds 50 \
    --parallel 1 \
    --output results/2026-05-22-tron.jsonl \
    baseline v1 v2
```

(As-built today: positional bot stems are resolved via `bot.toml` to `games/<game>/bots/<bot>_<lang>/` and built via `cargo build --release -p <crate>`. Use `<bot>:rs` / `<bot>:cpp` to pick a lang when both exist.)

After a run:

```sh
cargo run -p tournament -- report results/2026-05-22-tron.jsonl
```

prints the per-bot summary table:

```
bot         games  wins  losses  draws  win%   elo    p50ms  p95ms  errs
rust_v1     100     32      67      1   32%   -98    11.2   28.0     0
cpp_v1      100     40      59      1   40%   -32    14.3   31.0     0
cpp_v2      100     78      21      1   78%  +130    35.4   68.0     0
```

…plus the win-rate matrix:

```
              rust_v1  cpp_v1  cpp_v2
rust_v1            -    44%     12%
cpp_v1            56%     -     20%
cpp_v2            88%   80%       -
```

---

## Crate layout

New workspace crate `tournament/`:

```
tournament/
├── Cargo.toml          — depends on common, runner (lib), serde, clap
├── src/
│   ├── main.rs         — CLI dispatch: `run`, `report`
│   ├── schedule.rs     — round-robin / swiss pairing generators
│   ├── runner.rs       — spawns matches by calling into runner-as-library
│   ├── elo.rs          — standard Elo update
│   └── report.rs       — JSONL → summary table + matrix
└── tests/
    └── tournament.rs   — schedules N bots, runs K rounds, checks counts + invariants
```

**Important prerequisite**: extract `runner/src/main.rs`'s match-running logic into `runner/src/lib.rs` so the tournament can call it programmatically (today the runner is a binary that spawns child processes via `Command`; calling into it as a library avoids re-launching the runner binary per match).

---

## Recommended first PR

Smallest useful slice:

1. Refactor `runner` into `lib.rs` + a thin `main.rs` that calls into it.
2. Add `tournament` crate with `Schedule::round_robin`, the runner glue, and JSONL output.
3. Add `tournament report` for the summary table.
4. Leave Elo, Swiss, the matrix view, and parallelism as follow-ups — round-robin sequential single-table is already a huge step up from "run the runner by hand".

Once that lands, "is v2 stronger than v1" becomes:

```sh
# Today's CLI:
cargo run -p tournament -- compare --game tron --rounds 100 v1 v2
# (compare wraps run + report and prints a focused verdict line.)
```

Followed (if perf-analysis counters from `docs/perf-analysis.md` are in) by per-decision histograms in the same run — and we have a clean story about both *who wins* and *how hard each bot worked to get there*.

---

## Deferred — fork+exec cost per match

The FFI variant of this doc planned an optional `reset_for_match` symbol so the runner could cache a loaded dylib and reuse it across matches, dodging the dlopen+relocations+ctors per match. That entire optimization disappeared with FFI itself — there are no dylibs to cache.

The subprocess equivalent is `fork+exec` cost per match. `SubprocessPlayer::spawn` (`crates/common/src/engine.rs`) already absorbs the bulk of it via `SUBPROCESS_WARMUP_DEFAULT` (~100 ms blocking sleep). Real spawn cost on macOS is ~5–20 ms for Rust bots and ~10–50 ms for C++ bots (libcxx static init). For ≥ 100 ms matches it's noise; for sub-10 ms matches it dominates wall time.

When the workload demands it, the obvious next move is to keep one long-lived bot process across multiple matches and reset its state via a wire-level `RESET\n` line — same shape as the `READY\n` handshake idea referenced in `engine.rs`, just played per match instead of per spawn. Deferred until a real workload hits the threshold.
