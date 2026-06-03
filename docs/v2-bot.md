# Tron Bot — v2 Design Note

Companion to `games/tron/bots/v2_cpp/strategy.h`. v1 is at `games/tron/bots/v1_cpp/strategy.h` for diffing.

## Goals

- **Correctness first.** Fix the v1 bugs around `dead_players` and terminal scoring before adding cleverness.
- **Stop timing out.** v1's fixed `max_depth = 5` overspends on open boards and under-spends in tight endgames. Iterative deepening within a wall-clock budget removes both failure modes.
- **Strengthen pruning.** Get more out of alpha-beta via a transposition table + move ordering. Algorithmic wins dwarf micro-optimizations at this depth.
- **Stay paste-ready.** Single C++20 file, no external deps, runs on CodinGame's editor unchanged.

## What changed

| # | Change | Type | Why |
|---|---|---|---|
| 1 | `game_over()` / `count_live()` consult `players[p].dead` | bug fix | v1's check ignored in-search deaths; recursion thought the game was still going after an opponent ran out of moves. |
| 2 | Removed `winner()`; `terminal_score()` reads live count directly | bug fix | v1's "first can_move" loop is semantically wrong under the corrected death model. |
| 3 | Root undo restores the actual previous byte | bug fix | v1 always restored to `0`. Silent today; latent footgun if `passable()` ever stops treating dead-id cells as empty. |
| 4 | `counts[0]` no longer underflows on first claim | bug fix | v1 unconditionally did `--controls[best]` even when `best == 0`. Silent because no one reads `counts[0]`, still wrong. |
| 5 | Iterative deepening + 90ms budget | algorithm | Replaces hardcoded depth 5. Time the search instead of the depth. |
| 6 | Zobrist hash + transposition table | algorithm | Cache sub-search values so we don't re-explore the same position through a different move order. |
| 7 | PV-move-first at the root; freedom-greedy at inner nodes | algorithm | Better move ordering → more alpha-beta cutoffs → smaller effective branching factor. |
| 8 | Isolation detection → switch heuristic | algorithm | Once no opponent's BFS frontier overlaps mine, the game is solitaire; the right score is region size, not voronoi delta. |
| 9 | Leaf score `my − max_opp` (was `my`) | algorithm | Distinguishes "I have 200, they have 50" from "I have 200, they have 200". |
| 10 | Direct `if/else` min/max; no `std::function` | perf | Removes a virtual dispatch per AB node. |
| 11 | `Move { int dx, dy; const char* name; }` array | perf / style | v1 kept a `std::pair<std::string, position>`; the heap string per direction was needless. |
| 12 | Debug output removed from the per-tick path | style | v1 spammed `cerr` per move; on CodinGame, stderr competes with the judge's pipe and counts against budget. |

## How the transposition table interacts with the search

A worked example, with the full mechanics in the long comment block above `init_zobrist()` in the source:

1. Search reaches a node whose hash is `H`, with depth `d` to go and window `(α, β)`.
2. We probe `tt[H & MASK]`. If the slot stores `(H, d' ≥ d, value, flag)`:
   - `EXACT` → use `value` directly.
   - `LOWER` and `value ≥ β` → use `value` (proves a beta cutoff would happen anyway).
   - `UPPER` and `value ≤ α` → use `value` (proves an alpha cutoff would happen anyway).
   - Otherwise: probe missed, search normally.
3. After searching, classify the result:
   - `best ≤ α_original` → `UPPER` bound (we never improved alpha).
   - `best ≥ β_original` → `LOWER` bound (we cut on beta).
   - else → `EXACT`.
4. Store `(H, d, best, flag)` in the slot, replacing if our `d` is ≥ the stored depth.

The hash is updated **incrementally** by every move helper (`mark_cell`, `unmark_cell`, the side and dead-flag XORs around the recursive call), so the per-node cost is a few XORs instead of a full re-hash.

## Performance expectations (qualitative)

I haven't benchmarked yet, but here's the rough shape I expect once we add the perf-analysis tooling (see `docs/perf-analysis.md`):

- **Open early boards (4 players, lots of space)**: TT hit rate low (positions diverge fast). Win comes from the time budget — we'll search depth 3–4 reliably without timing out.
- **Mid-game (2–3 players, half-full board)**: TT hits start showing up; move ordering pays off. Depth 5–7 expected at the deadline.
- **Endgame corridors (2 players, narrow)**: TT hit rate high (many transpositions in restricted topology); isolation often fires. Should easily reach depth 10+ and play near-perfectly with the region-fill score.

If the BFS leaf turns out to dominate (likely), the next iteration is bitboarding the BFS — see "Deferred" below.

## Deferred

These were on the analysis but didn't fit v2; they're separate, larger changes worth a v3:

- **Bitboard.** Pack the board into ~10 × `uint64_t`; emptiness checks become a couple of ANDs. Probably 5–10× faster than the byte grid, but rewriting the BFS and the mutation helpers safely is a project of its own.
- **Max⁻ instead of paranoid.** Paranoid assumes all opponents collude against me — pessimistic in real 3–4 player CodinGame matches. Max⁻ (each player optimizes their own score, propagated as a vector) plays less defensively. Worth measuring once the tournament harness exists.
- **History heuristic / killer moves.** Standard chess-engine moves for stronger ordering. Smaller win than the TT, harder to evaluate without the tournament harness.
- **Time-aware deeper budgets.** Use less than 90ms in the early game (when we obviously don't need it) and bank the savings.

## Validation plan

Once `docs/perf-analysis.md`'s counters and `docs/tournament.md`'s harness are live:

1. Counters on, v1 vs v2, 100 head-to-head matches per seat assignment. Win rate is the headline metric.
2. From the same matches, compare nodes/sec, TT hit rate, p50/p95 decision time, average reached depth.
3. Ablate: disable the TT, disable isolation detection, disable move ordering — each should measurably degrade win rate. If one doesn't, the change wasn't worth the complexity.
