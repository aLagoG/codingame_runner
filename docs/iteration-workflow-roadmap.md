# Iteration Workflow — Roadmap

What's still missing from the bot-iteration loop, ordered by my read of value-per-effort. Pick up by jumping straight into any section — each entry stands alone.

## Status snapshot (what already shipped)

For grounding when you return to this:

- **Lineage primitives**: `bot_manifest` crate with `BotManifest` schema (`name`, `lang`, `parent`, `description`, `champion`, `codingame_league`/`standing`, `[[history]]`); `remove_workspace_member` helper; backfilled bot.toml for all existing bots.
- **Verbs**: `cargo xtask new-bot` (with `--from-existing`), `retire` (with `--force`, champion + has-children safety), `promote` (with `--archive`, `--cleanup-siblings`, champion propagation), `bundle` (defaults to champion when bot omitted), `champion`, `history`, `compact-history`, `doctor`.
- **Tournament**: `tournament compare <bot> <bot> ...` (focused 2-bot verdict OR N-bot ranking + pairwise table); `tournament run --until-confident` (Bonferroni-corrected wave-by-wave); `tournament report` + `compare` print Wilson 95% CI + LOS + two-sided p-value; `--record-history` on both `run` and `compare`.
- **Tests**: 9 in xtask, 25 in tournament lib, 9 in pairwise_stats, 3 in bot_manifest. Read-only lineage helpers refactored to take `bots_dir: &Path` for tempdir testability.

## Tier A — `cargo xtask iterate` (the headline missing verb)

**The pitch**: collapse the daily loop (`new-bot --from-existing` → edit → `cargo build` → `tournament compare` → `xtask promote`/`retire`) into one verb the user invokes per idea.

```sh
cargo xtask iterate --game G --from <baseline> --name <candidate> \
    [--rounds 200] [--against <other>...] [--no-edit] [--continue]
```

**Steps the verb owns:**

1. Clone via `new_bot --from-existing` under the candidate name.
2. Either open `$EDITOR` on `strategy.h`/`lib.rs` (default) or print the path and exit (with `--no-edit` — user edits separately, comes back with `--continue`).
3. `cargo build --release -p <candidate_crate>` + the baseline crate(s) in parallel.
4. Shell out to `tournament compare --record-history --game G <baseline> <candidate>` (+ any `--against` opponents).
5. Parse the verdict; prompt `[p]romote / [r]etire / [k]eep / [e]dit-again`.
6. Dispatch to `promote` / `retire`, or leave the candidate in place.

**Design questions to resolve before coding** (these came up the first time and we deferred):

- **Editor handoff vs `--continue`**: blocking `$EDITOR` is awkward in Cursor / over SSH where the user might want to edit in a separate window. Default to `--no-edit` mode? Or detect TTY and pick?
- **Default `--rounds`**: 200 is enough for a noisy signal; 500 is enough to actually decide. Lean toward 200 + the `compare` epilogue (`"need ≈ N more games to resolve"`) for the user to escalate.
- **Where the JSONL log lives**: `target/iterate/<candidate>/<ts>.jsonl` (per-bot history) is probably what the user wants for grep-back. Pure design choice.
- **Auto-promote threshold**: `--auto-promote-at 0.55` (or similar) would skip the prompt when the verdict is unambiguous. Mild scope creep but pairs naturally with `--no-edit` for scripted runs.
- **Non-2-bot mode**: if the user passes `--against v0 --against v2`, the candidate plays a 3-way round-robin. Promote-or-retire prompt still operates on the candidate alone.

**Cost estimate**: 200-300 lines. Mostly orchestration on top of code that already exists. One small chunk: a `prompt()` helper for the keystroke selection (use `std::io::stdin().read_line` + accept `p`/`r`/`k`/`e`).

**Dependencies**: nothing new. The verbs it composes are stable.

## Tier B — Other deferred verbs / features

### B1. `cargo xtask sweep`
Cartesian fan-out over `// @sweep: 1, 2, 3` markers in `strategy.h` / `lib.rs`. Generates N variant bots, runs one N-way round-robin, optionally promotes the winner.

```sh
cargo xtask sweep --game G --from <baseline> [--max-variants 16] [--rounds 50] \
    [--auto-promote-winner-as <name>]
```

**Marker syntax** to settle on: `// @sweep: 1, 2, 3` is the simplest. Sub-options: `// @sweep: range(1, 10, step=2)` for ranges; type-aware (int vs float vs bool) parsing.

**Hard parts**:
- Type-aware in-source rewrite (`inline int g_petr_thresh = 3;` → substitute `3` carefully without touching unrelated `3`s). Probably regex against the specific line.
- Cartesian blow-up — guardrail with `--max-variants` cap; later add Latin-hypercube sampling.
- Naming the scratch crates so they don't collide and clean up after themselves. Probably `games/G/bots/scratch_<run_id>/<variant>_cpp` style, with a separate `scratch-clean` verb.

**Cost**: 200-300 lines including the marker parser. Compose with the iterate verb's prompt for "auto-promote winner?".

### B2. `tournament gauntlet --sprt`
Full Sequential Probability Ratio Test, chess-engine standard.

```sh
tournament gauntlet --baseline=path --candidate=path --elo0 0 --elo1 5 \
    [--alpha 0.05] [--beta 0.05] [--max-rounds 40000]
```

Per-match LLR update; early-stop on H0/H1 acceptance. Typical sample efficiency is 3-5× over the fixed-N `--until-confident` we have today.

**Subtleties**:
- Elo0/elo1 conventions: chess uses `[0, 1.75]` for STC, `[0, 0.5]` for LTC. We'd need sensible defaults per-game.
- Multi-player game adaptation: classic SPRT is 2-player. Could restrict gauntlet to `--bots-per-match 2` (clean) or use pairwise decomposition (lossy).

**Cost**: 100-150 lines. The math is mechanical (Stockfish's fishtest has the canonical formula).

**My take**: only worth building when you're A/B-ing enough that the 3-5× speedup compounds (10+ tests a day). The current `--until-confident` covers most needs.

## Tier C — Polish + correctness items

These were called out in the "remaining work" discussion but never landed. Smallest first.

### C1. Drop the "no Elo" annotation
`tournament report` and `compare` both label the verdict block `"Pairwise verdicts (95% CI, no Elo):"`. The "no Elo" parenthetical is a historical artifact from when we deliberately removed Elo. Now it just adds noise. Two lines to remove.

### C2. `promote` clears `codingame_league` / `codingame_standing`
Today promote carries forward the candidate's bot.toml — its CG league/standing fields go into the promoted slot. Usually they're `None` (candidates haven't been submitted), so it's a no-op. **But** if a candidate WAS submitted before promotion, those fields now describe a different bot (the new one in the slot) — misleading.

**Fix**: in `promote_one_lang`, explicitly clear `promoted_manifest.codingame_league = None; promoted_manifest.codingame_standing = None;` before writing. ~3 lines + a test.

**Open call**: do we also clear them on the *archived* parent? Archived parent IS the bot that was submitted, so its fields stay relevant. Leave them.

### C3. `archive_timestamp` collision guard
`promote --archive` derives a dir name as `<parent>_archived_<ts>_<lang>` where `<ts>` is `YYYYMMDD_HHMMSS` (second-precision). Two promote-archives in the same second collide on directory name and the second one errors at `fs::rename`. Almost impossible in real use but trivial to fix: detect existing dir and append `_2`, `_3`, etc.

### C4. Auto `--bots-per-match` from game metadata
`tournament compare` defaults to `--bots-per-match 2`. For 4-player games (tron supports 2-4), the user has to remember to pass `--bots-per-match 4`. Engine knows; CLI doesn't.

**Fix**: expose a `Game::PLAYER_COUNT_DEFAULT: u32` const on the `Game` trait, plumb through `run_match_named` → CLI default. Compare can read it via a per-game mapping (since the dispatch already exists).

**Cost**: ~30 lines + one const per game crate.

### C5. `retire --lang both` atomicity
If `--lang both` retires lang=rs successfully then lang=cpp fails (cargo clean error, fs permission, ...), the workspace is half-mutated. Currently the safety checks are collected upfront, but execution is per-lang and can fail per-lang.

**Fix**: stage all mutations (workspace member removals, dir paths to delete, crate names to clean) into Vec<Action>, then apply atomically — or accept the current behavior and document it. Probably not worth real engineering; the failure modes are rare.

### C6. `bundle` ↔ `bot.toml` lang resolution
`bundle` resolves which language variant to use via dir-glob (`<bot>_cpp/` exists? `<bot>_rs/` exists?). With bot.toml everywhere, it could read `lang` directly. Marginal.

### C7. `tournament report --bot <name>` filter
Render only the named bot's row + its pairwise verdicts. Useful when a 5-bot log has too much noise to skim. ~30 lines.

### C8. Test coverage for `retire` / `promote` orchestrators
Today only the read-only helpers (`find_children_in`, `find_descendants_in`, `list_champions_in`, `rewrite_dir_contents`) have tests. The orchestrators themselves are tested only manually. End-to-end tests would either:
- Shell out to `cargo xtask retire` in a tempdir workspace (slow; needs cargo available).
- Refactor `retire`/`promote` to take a `workspace_root: &Path` parameter and skip the `cargo clean` step in tests.

Option 2 is the cleaner path. ~100 lines per verb to test the happy + safety-rejection paths.

## Tier D — Doctor-adjacent ideas (lineage maintenance)

### D1. `xtask doctor --fix`
Today `doctor` reports findings; doesn't fix any. Most findings have an obvious fix:
- Multiple champions → keep the most-recently-submitted (or just the first); set others to `false`.
- Dangling history entry → remove from the history list.
- Bot dir without workspace member → add the member.
- Workspace member with missing dir → remove the entry.

Each fix is benign. Could ship as `--fix` with a confirmation prompt or `--yes`.

### D2. `xtask prune-history --game G --name N [--orphans-only]`
Inverse of `compact-history`. Removes history entries whose `opponent` doesn't exist anymore (orphans-only mode). Useful after a bunch of retires.

### D3. Inter-game doctor — `xtask doctor --all`
Walk every `games/*/bots/` instead of one. Tiny extension, useful for CI integration.

## Tier E — Documentation refresh

`docs/bot-submission.md` covers the old flow (write a bot → bundle). None of the new verbs (`retire`, `promote`, `compare`, `champion`, `history`, `doctor`, `compact-history`, `tournament compare --until-confident`, `--record-history`) are documented.

Specifically missing:
- The lineage model (parent / champion / history) and what each verb does.
- The clone-and-edit loop (currently relies on the user remembering all the verbs).
- How to read a `compare` verdict (LOS, p-value, "need ≈ N more games").
- How to interpret a `doctor` report.

**Cost**: 2-3 hours of writing. Easy to defer until iterate ships — the iterate verb is the natural anchor for "here's how to actually use this."

## Suggested attack order

If picking back up:

1. **iterate** (Tier A) — the headline verb. Spend the design questions before coding; everything else flows from this.
2. **C2** (promote clears CG fields) + **C3** (timestamp collision) — both ~5 lines, real correctness wins.
3. **C1** (drop "no Elo" annotation) — trivial cleanup.
4. **C4** (auto `--bots-per-match`) — daily-use ergonomics.
5. **Tier E** docs refresh — anchors on iterate.
6. **B1** (sweep) — once you have at least one bot worth tuning.
7. **D1** (`doctor --fix`) — once the lineage tree gets big enough to need it.
8. **C8** (orchestrator tests) — pick up before adding more verbs.
9. **B2** (SPRT/gauntlet) — when sample efficiency starts hurting.
10. **C5/C6/C7** — minor polish, do opportunistically.

## What's intentionally not on this list

- Anything about the engine layer (game traits, runner, viz). The iteration workflow is a self-contained slice; engine work is its own thing.
- C++ bundling polish. `cpp_flatten` is stable and the workflow handles it adequately.
- Rust flatten work beyond what's done (`--vendor` + self-lib injection). Future improvements would be specific compatibility fixes for new vendored deps.

## Files to read first when picking back up

- `crates/xtask/src/main.rs` — every verb except compare lives here. Helpers are `find_children_in`, `find_descendants_in`, `list_champions_in`, `rewrite_dir_contents`. Verb implementations are `retire`, `promote_one_lang`, `bundle`, `champion`, `history`, `compact_history`, `doctor`.
- `crates/tournament/src/main.rs` — `cmd_compare`, `cmd_run` (now with `--until-confident` adaptive path), `record_history`, `print_pairwise_verdicts`, `lang_for_bot_in_dir`.
- `crates/tournament/src/pairwise_stats.rs` — Wilson CI / LOS / p-value math + `PairStats::rounds_needed_for_significance`.
- `crates/bot_manifest/src/lib.rs` — the schema. Add fields here as optional with `#[serde(default, skip_serializing_if = "Option::is_none")]` for forward compat.
- `docs/bot-submission.md` — current state of the workflow docs (stale relative to the new verbs).
