# Bot Performance Analysis — Design Note

**Status**: design only. Pick one or more of the approaches below, then implement.

## Why this is hard for bots specifically

Two transport modes (FFI plugin, subprocess) × two languages (Rust, C++) × two cost models (per-decision wall time vs per-decision *work*) means the tooling that lights up easily for one cell of the matrix often doesn't for another. The runner already gives us **per-decision wall time** via `PlayerStats` (`runner/src/main.rs`); that's our baseline. Everything below extends it.

Specific things we want to measure:

1. **Wall time per decision.** Already have it (avg, max). Want: p50/p95/p99, full histogram.
2. **Work per decision.** Nodes searched, leaves visited, BFS calls — proxies for "how hard is the bot actually working". Lets us distinguish "fast bot" from "lucky bot" and tells us whether a regression is in the algorithm or the implementation.
3. **Hot-path attribution.** Where in the bot is the time going — heuristic? Move generation? TT probing? I/O?
4. **Cross-bot fairness.** When we benchmark v1 vs v2, we need confidence we're measuring decision quality / speed, not noise from the runner / OS scheduler / cargo build profile.

---

## Design A — Lightweight counters over stderr

**Idea**: every bot prints a structured one-line summary to stderr at the end of each tick, e.g.
```
@CGR cnt nodes=12453 leaves=3201 tt_hits=8722 ms=78 depth=6
```
The runner already captures subprocess stderr (it gets logged); for FFI bots we'd add a counters-emitting callback. A small parser in the runner pulls counter lines out, attaches them to the matching tick in the replay.

**Pros**
- Trivially portable across Rust and C++; nothing more than `eprintln!` / `cerr <<`.
- Zero infrastructure — works today with the subprocess transport.
- Counters live with the bot's logic, so adding "TT collisions" or "nodes pruned" is a one-line change in the bot.

**Cons**
- String parsing is fragile; a runaway `eprintln!` from debug code drowns the counters.
- No timing breakdown *within* a decision (e.g. "30ms in BFS, 40ms in AB") without lots of counters.

**Effort**: ~half a day. The parser is ~30 lines of Rust; bot side is one line per counter.

---

## Design B — Tracing spans + Perfetto export

**Idea**: instrument the hot paths with structured spans. Rust already has `tracing` in the workspace; C++ gets a small inline equivalent (`namespace tracing { struct span { ... }; }`) that emits the same JSON shape. Each match produces a `trace.json` in [Chrome Trace Event format](https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU). View in [Perfetto](https://ui.perfetto.dev) or `chrome://tracing`.

```rust
let _s = trace::span("ab", &[("depth", &depth)]);
```
```cpp
auto _s = trace::span("ab", {{"depth", depth}});
```

**Pros**
- Visual timeline shows *where* the time goes per decision.
- Cross-language: both Rust and C++ emit the same event shape.
- Existing tracing crate experience in the codebase carries over.
- Trace files round-trip with replays — instrument once, debug forever.

**Cons**
- Real overhead. Even with sampling, span creation per AB node would dominate. Must be coarse-grained ("per decision", "per BFS call") rather than per-node.
- C++ side is custom — Perfetto's C++ SDK is heavy; a homegrown emitter is fine but is one more thing to maintain.
- Trace files can be large (10s of MB per match) — fine for dev, awkward for CI.

**Effort**: 1–2 days. Most of that is the C++ tracing shim and the JSON writer.

---

## Design C — External profilers (no code changes)

**Idea**: use the platform's profiler against a normal bot binary.

| OS | Tool | Notes |
|---|---|---|
| macOS | Instruments (Time Profiler, CPU Counters) | Best UI; samples both Rust and C++ symbols, works with the subprocess bots out of the box. |
| Linux | `perf record` + `cargo flamegraph` / `flamegraph.pl` | Standard. Needs frame pointers (`-Cforce-frame-pointers=yes` on Rust, `-fno-omit-frame-pointer` on C++) for clean stacks. |
| Any | `samply` (cross-platform) | Newer; produces Firefox-profiler-compatible output, works with Rust + C++. |

To make these useful you need the bot to do *a lot* of work; the easiest is to put it in a loop replaying a saved match a few hundred times.

**Pros**
- Zero changes to the bot or runner; production binaries are profilable as-is.
- Best signal for finding actual hot functions (e.g. "60% of time is in `bfs_owner` write").
- Free flamegraphs.

**Cons**
- Off-band — results don't live with replays or tournaments.
- macOS Instruments doesn't script well; Linux `perf` does but the workflow is ad-hoc.
- Subprocess bots launched by the runner are awkward to attach to (you'd run the bot binary directly under the profiler against a recorded input stream).
- Tells you "where time goes", not "how much work the bot did".

**Effort**: zero to start (just run the profiler). A day to write a small `scripts/profile-bot.sh` that replays N matches under `samply` and saves the output next to the replay.

---

## Design D — Criterion micro-benches

**Idea**: each game's `_defs` and `_rs` crates get a `benches/` directory with [criterion](https://github.com/bheisler/criterion.rs) benchmarks against canned inputs.

```rust
fn bench_decide(c: &mut Criterion) {
    let turn = synthetic_midgame();
    c.bench_function("tron_decide_midgame", |b| {
        b.iter(|| tron_rs::decide(turn.as_ref()))
    });
}
```

For C++ bots we'd add `_cpp/benches/` with Google Benchmark or a homegrown timing loop.

**Pros**
- Stable, low-noise measurement of one function in isolation.
- CI-friendly: criterion can flag a >5% regression automatically.
- Same toolchain across all games.

**Cons**
- Only measures what you point it at. Picking representative inputs is hard (early vs mid vs endgame all behave very differently).
- Doesn't catch full-match issues (timing budget, allocator pressure, …).
- C++ side needs its own bench framework.

**Effort**: ~half a day per game once the harness is built; the harness itself is ~1 day.

---

## Design E — Replay-driven re-execution

**Idea**: the runner already writes replays (`Replay<O>` in `common::engine`). Add a `cargo run --bin replay-bench` that:

1. Loads a replay.
2. Walks tick-by-tick, presenting each `Game::input_for(player)` to a *target* bot (NOT the one that played originally).
3. Times each `decide()` call and records what the target bot would have played vs what the replay recorded.

This is essentially "replay the same scenarios through a candidate bot to compare without the noise of full match outcomes".

**Pros**
- Apples-to-apples comparisons: every bot under test sees the *same* sequence of positions.
- Cheap — N matches' worth of inputs become a fixed corpus.
- Composes with everything else (run it under a profiler, emit counters during it, etc.).

**Cons**
- Only measures bots' decisions on positions the *original* bot wandered into. A bot that would have played differently never sees the positions it'd actually face — so this is benchmarking, not gameplay evaluation. (For gameplay evaluation, see `docs/tournament.md`.)

**Effort**: ~1 day. Replay infrastructure already exists.

---

## Design F — Per-decision histograms in the replay

**Idea**: extend `PlayerStats` to record the *full* timing list (already does) plus a sidecar dict-per-tick: `{"nodes": …, "depth_reached": …, "tt_hit_rate": …}` if the bot emits them. The runner writes this beside the replay file. A `cargo run --bin replay-report` walks the sidecar and prints / plots histograms.

**Pros**
- Combines (A) and (E): rich counters, queryable later.
- Falls out of work we've already done — `PlayerStats` is most of the way there.

**Cons**
- Only useful if bots actually emit counters.

**Effort**: ~half a day on top of (A).

---

## Recommendation

A pragmatic combination, picked for low cost-to-first-signal:

1. **Now**: implement **A** (counters over stderr). Cheap, instantly useful, makes v1 vs v2 comparable.
2. **Next**: add **F** (sidecar histograms in replays) so we can look back at runs.
3. **As needed**: pull in **C** (Instruments / samply) when a counter regression points at "the bot is slow but I don't know where".
4. **Later, gated on need**: **D** (criterion) for the hot paths that show up in C, and **B** (tracing spans + Perfetto) if we want richer per-decision breakdowns than counters provide.

(E) is mostly subsumed by the tournament harness (`docs/tournament.md`); if we end up wanting per-position comparisons specifically, it's a small addition.

### Concrete first PR

```
common/src/counters.rs                          — Counter struct, stderr parser
runner/src/main.rs                              — wire parser into subprocess transport, attach to PlayerStats
games/tron/bots/baseline_rs/src/lib.rs          — bump counters around decide()
games/tron/bots/v2_cpp/strategy.h               — write `@CGR cnt …` on stderr at end of each turn
docs/perf-analysis.md                           — mark Design A as implemented
```

Once that lands, v1 vs v2 head-to-head with counters is a one-command experiment.
