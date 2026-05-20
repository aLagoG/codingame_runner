# Code Review — Action Items

Outcome of the workspace review. Each item has a stable number (1–18) for cross-reference. Within each item, fix options are labelled with letters (A, B, C…) so we can say "do 7A+7C", "skip 17", etc.

Severity tiers:
- **P0** (#1–#7): correctness / soundness. Real bugs or UB risks.
- **P1** (#8–#13): design problems that will hurt within a few more features.
- **P2** (#14–#18): coupling, duplication, dead weight.

If you want a starting set: **#1A, #3A, #7A** are the three "silent failure" fixes — things that look fine until they aren't.

---

## P0 — Correctness and soundness

### 1. `SubprocessPlayer` leaks zombies

**File:** `common/src/engine.rs:221-228`

**What's wrong:** `Drop` calls `child.kill()` but never `wait()`. On Unix, the child becomes a zombie that lives until the runner process exits.

**Why it matters:** A tournament server running 10k matches accumulates ~20k zombies. PID exhaustion or kernel limits eventually bite. The current comment claims `wait()` would risk blocking on a hung bot, but `wait()` *after* `SIGKILL` returns essentially immediately.

**Options:**
- **A.** Add `let _ = self.child.wait();` after `kill()` in `Drop`. One line, fixes it. Risk: if `kill()` failed (unlikely) and the child is genuinely hung, we'd block. Acceptable: `kill` only fails if the child is already gone, in which case `wait` returns immediately.
- **B.** Spawn a dedicated reaper thread that holds a registry of pending children and calls `wait` on them. Robust but over-engineered for our scale.
- **C.** Use a `try_wait` loop with a short timeout, force-kill on timeout. Solves the (paranoid) hung-after-kill case at the cost of complexity.

**Recommendation:** **1A.** Simplest correct fix.

---

### 2. No per-turn timeout

**File:** `common/src/engine.rs:155-174`

**What's wrong:** `PlayerError::Timeout` is defined but never produced. A bot that infinite-loops on its first move hangs `run_match` forever.

**Why it matters:** CodinGame normally enforces ~100ms/turn. Without this we can't run untrusted bots, can't run tournaments unattended, and even friendly bugs in our own bots wedge the runner. Subprocess case is fixable; FFI case is fundamentally harder because the call is on our thread.

**Options:**
- **A.** Subprocess: spawn a per-call reader thread, use `crossbeam_channel::recv_timeout` or `std::sync::mpsc::recv_timeout`. On timeout, kill the child and return `Timeout`. FFI plugins: leave as "trusted code"; document.
- **B.** Always-subprocess: drop `PluginPlayer`, run every bot as a subprocess. Uniform timeout enforcement. Costs the ~10000× FFI speedup we measured.
- **C.** Spawn FFI calls on a worker thread; on timeout, abandon the thread (leak it). Works but accumulates leaked threads across timeouts.
- **D.** Status quo: document the limitation, defer.

**Recommendation:** **2A.** Subprocess gets a real timeout; FFI plugins remain trusted (panic-across-FFI is already UB, so plugins were never "untrusted" anyway). If we ever want untrusted FFI, escalate to **2B**.

---

### 3. Crashed subprocess is indistinguishable from a bot printing garbage

**Files:** `common/src/engine.rs:237-242`, `common/src/lib.rs:11-25`

**What's wrong:** `SubprocessPlayer::take_turn` maps every `io::Error` (including `BrokenPipe` / EOF) and every parse failure to a single `PlayerError::InvalidOutput`. The blanket `ReadFrom for FromStr + SingleLine` impl returns `Ok("")` on `read_line` `Ok(0)` (EOF), which then parses to an empty-string error.

**Why it matters:** With default `RunConfig::abort_on_player_error = false`, the engine calls `take_turn` again next tick, gets another failure on the closed pipe, marks output `None`, and repeats forever. A crashed bot looks alive but uncooperative. Worse, the timing stats keep recording nonsense.

**Options:**
- **A.** Add `PlayerError::Eof` and `PlayerError::Io` (distinct from `InvalidOutput`). Engine tracks a `dead: Vec<bool>`; on `Eof` it stops calling that player and treats them as permanently absent.
- **B.** Don't change error types; add a separate `try_wait`-style liveness check the engine runs each tick. More plumbing, less type-driven.
- **C.** Push the decision to the game via `Game::step` (status quo); ignore the symptom because games "can decide what `None` means". Doesn't actually fix the wasted IO calls on a dead pipe.

**Recommendation:** **3A.** Distinguishing crash from parse error is information the engine genuinely needs.

---

### 4. Plugin panic unwinding across `extern "C"` is UB

**Files:** `common/src/engine.rs:321-325` (call site), `tron/tron_rs/src/lib.rs:15-28` (current shim)

**What's wrong:** The `catch_unwind` lives inside each bot's `take_turn` shim. If a bot author forgets the shim — and it's pure boilerplate they're encouraged to copy — a panic crosses the FFI boundary. UB.

**Why it matters:** It's easy to forget, especially for a new game template. The current `xtask` scaffolder doesn't even produce the shim. We should make it impossible to get wrong.

**Options:**
- **A.** Ship a `#[macro_export] macro_rules! ffi_bot { ($decide:expr) => { … } }` in the `common` (or a new `ffi_bot`) crate that generates the `catch_unwind` shim. Per-game `_rs` crates write `ffi_bot!(decide);` and that's it.
- **B.** Document the requirement; rely on copy-paste. Status quo.
- **C.** Belt-and-suspenders: also wrap the runner-side call in `catch_unwind`. Catches missing-shim cases for Rust panics. Doesn't help C++ exceptions, but those are an even bigger config error.
- **D.** Set `panic = "abort"` for plugin crates. Eliminates the unwinding entirely (panicking aborts the whole runner, which is bad UX but not UB). Requires bot authors to configure their `Cargo.toml`.

**Recommendation:** **4A + 4C.** A removes the footgun by codegen; C is the defense-in-depth that catches missing-shim cases. They compose cleanly.

---

### 5. `PluginPlayer::load` doesn't verify the plugin's ABI

**File:** `common/src/engine.rs:289-309`

**What's wrong:** libloading resolves a symbol by name and `transmute`s the pointer to your declared `Symbol` type. If the bot was built against an older `tron_defs` whose `TurnInputFFI` had a different layout, the load succeeds and the first call is UB.

**Why it matters:** Wire types change. Versioning by name isn't enough; the layout has to match. Without a check, the failure mode is "weird crashes in production" instead of "refuses to load with a clear error".

**Options:**
- **A.** Add a `pub const ABI_VERSION: u32` in each game's `_defs` crate, exported by both sides. Plugin exports a `tron_abi_version` function/symbol that returns the constant. Runner reads it on `load()` and refuses mismatches. Manually bump the constant on every wire-type change.
- **B.** Auto-derive a layout hash (e.g. SipHash of struct sizes / field offsets) at compile time. More automatic but fragile: same hash for incompatibly-named-but-same-shaped types, breakage when adding fields that didn't matter for FFI.
- **C.** Have plugins build against a versioned `abi` crate published from the workspace; rely on Cargo to refuse mismatched versions. Doesn't help if the plugin is built out-of-tree.

**Recommendation:** **5A.** Cheapest and most explicit. Manual bumps are fine — wire types change rarely.

---

### 6. The `take_turn` stub in `*_defs` exports a real symbol

**Files:** `tron/tron_defs/src/lib.rs:90-93`, `tictactoe/tictactoe_defs/src/lib.rs:91-93`

**What's wrong:** `#[no_mangle] pub extern "C" fn take_turn(_: TurnInputFFI<'_>) -> TurnResult { unreachable!() }` exists solely so cbindgen emits the prototype in the header. But it's a real exported symbol. When `*_rs` (cdylib) links against `_defs`, the linker sees two `take_turn` definitions.

**Why it matters:** macOS errors at link time; Linux silently picks one based on visibility. Any binary that links `_defs` but not `_rs` and dlsyms `take_turn` calls `unreachable!()`.

**Options:**
- **A.** Gate the stub behind `#[cfg(feature = "cbindgen-stub")]`. `_defs/build.rs` enables it when running cbindgen; `_rs` links without the feature so there's no duplicate symbol.
- **B.** Hand-write the C++ header. Drop cbindgen entirely. Header is ~30 lines, rarely changes.
- **C.** Move the stub to a separate `_abi` crate that only cbindgen sees. Pure indirection, no real win.
- **D.** Use cbindgen's config to declare external functions without needing a Rust stub (the `[export]` config section). Cleaner long-term but requires reading cbindgen docs.

**Recommendation:** **6A.** Minimum change, keeps cbindgen automation, kills the duplicate-symbol risk. Look at **6D** if we add a third game and the stub pattern starts feeling like cargo cult.

---

### 7. Determinism is assumed, not enforced — and `TronGame` ignores its seed

**Files:** `viz/src/lib.rs:121`, `tron/tron_game/src/lib.rs:67`

**What's wrong:** `viz::run` reconstructs the game via `G::new(num_players, seed)` and replays outputs. `TronGame::new` takes `_seed: u64` and discards it. Today the starts are hardcoded, so this happens to work. The moment we implement real randomized starts, every existing replay becomes invalid and the visualizer silently diverges from what the runner saw.

**Why it matters:** Determinism is the whole foundation of the seed+outputs replay format. The current Game trait gives no way for the engine to *enforce* it — a game can read wall-clock time during `step` and we'd never know.

**Options:**
- **A.** Change `Game::new` (and possibly `Game::step`) to take `&mut impl Rng` (or `&mut SmallRng`) constructed from `seed`. Makes it impossible to accidentally use system RNG because there's no other RNG in scope.
- **B.** Document determinism as a trait contract; trust game authors. Status quo. Catches nothing.
- **C.** Add a debug-mode self-check in `viz::run`: every N ticks, reconstruct from scratch and `assert_eq!` against the live game. Requires `Game: PartialEq`. Doesn't prevent the bug, but catches it loudly during development.
- **D.** Store the initial game state inside `Replay` so viz doesn't have to call `Game::new`. Sidesteps `new`'s non-determinism but not `step`'s.

**Recommendation:** **7A + 7C.** A forces game authors into the determinism contract by the type system; C is a runtime backstop for things A can't cover (e.g. hash-map iteration order — though we should also `BTreeMap` over `HashMap` for state). C requires adding `PartialEq` to game state, which is essentially free for the games we have.

---

## P1 — Design problems

### 8. Hardcoded runner dispatch + hand-rolled arg parser

**File:** `runner/src/main.rs:25-69`

**What's wrong:** Game dispatch is a `match` on a string literal that needs editing every time a game is added. The arg parser is hand-rolled with subtle bugs (`--game --save-replay foo bot1` parses game name as `--save-replay`, silently runs Tron with `foo` and `bot1` as bots). `clap` is already a workspace dep, unused.

**Why it matters:** Two flavors of pain: clap-style is a no-brainer cleanup; dispatch is the more interesting design question.

**Options:**
- **A.** Switch arg parsing to `#[derive(clap::Parser)]`. Keep the game `match`. Cheap incremental win.
- **B.** A + game registry via `linkme` / `inventory`: each `_game` crate calls `register_game!("tron", run_for_game::<TronGame>);` at link time, runner enumerates the registry. Adding a game = one line in the game's own crate.
- **C.** A + per-game binaries (one binary per game) built from a thin `runner_lib`. No dispatch needed — Cargo picks the binary. Most code, most overhead, but each game is fully independent.
- **D.** Status quo + tests on the arg parser.

**Recommendation:** **8A** now (~30 minutes). Revisit **8B** when adding a 3rd game; the cost/benefit only flips at 3+.

---

### 9. `viz::run` never exits

**File:** `viz/src/lib.rs:115-141`

**What's wrong:** The main loop has no break path. macroquad swallows `Cmd+Q` as `exit(0)`, so destructors don't run. The `anyhow::Result<()>` return type is decorative.

**Why it matters:** Can't embed viz in larger programs. Can't run viz as part of a test that needs to close cleanly. `exit(0)` skipping destructors is fine for our pure-Rust viz today but will hurt if we ever hold OS resources (temp files, subprocesses).

**Options:**
- **A.** Poll `macroquad::window::is_quit_requested()` at the top of each loop iteration; `return Ok(())` if true.
- **B.** Add an optional `should_exit: impl Fn() -> bool` callback parameter for caller control.
- **C.** Status quo.

**Recommendation:** **9A.** One line, idiomatic.

---

### 10. `_viz` binaries duplicate ~50 lines of bootstrap each

**Files:** `tron/tron_viz/src/main.rs`, `tictactoe/tictactoe_viz/src/main.rs`

**What's wrong:** Both crates duplicate: `PALETTE` (different colors but identical role), `color_chip` (identical), `load_or_demo` (identical except for the game type), and the `#[macroquad::main] async fn main()` boilerplate. `load_or_demo` panics with `.expect("read replay file")` on a typo.

**Why it matters:** Every new game pays the tax. The error UX is bad.

**Options:**
- **A.** Move `PALETTE`, `color_chip`, `load_replay_from_argv::<G>()`, and a `run_viz!(MyViz)` macro into `viz` crate. Per-game `_viz/src/main.rs` becomes the `impl Visualize` plus `run_viz!(MyViz);`.
- **B.** Same as A but a separate `viz_helpers` crate. Cleaner separation between "core engine" and "ergonomics", at the cost of one more crate.
- **C.** Status quo, factor as game #3 is added.

**Recommendation:** **10A.** No reason to split into a second crate yet.

---

### 11. Replay file format has no header; generic bounds are fragile

**File:** `common/src/engine.rs:99-108`

**What's wrong:** Two related issues:
1. `Replay<G>` is bincode-serialized with no magic, no version, no game name. Loading a tic-tac-toe replay into `tron_viz` deserializes garbage (bincode is unframed) — possibly without erroring.
2. The serde `bound(...)` attribute only mentions `G::Output`. Adding any other associated-type field later silently demands new bounds with cryptic errors. And `G` itself never appears in any field — pure phantom.

**Why it matters:** Wrong-game-loaded is a real user-error mode. Brittle bounds cost time when the format evolves.

**Options:**
- **A.** Add a `Header { magic: [u8; 8], version: u32, game_name: String }` prefix. Game name comes from a new `Game::NAME: &'static str` const. Runner writes header; viz checks magic + version + name on load.
- **B.** Reparameterize `Replay<O>` over just the output type. Cleaner generics, but loses the type-level connection to `Game`.
- **C.** **A + B.** Header for runtime safety, simpler generics for code clarity.
- **D.** Switch from bincode to a self-describing format (postcard, ciborium) with type tags. More robust, larger files.

**Recommendation:** **11C.** Magic + version + game name catches the common error mode; reparameterizing tidies the generics. Magic header is ~8 lines; reparameterization is mechanical.

---

### 12. `is_plugin` is filename-extension based

**File:** `runner/src/main.rs:127-132`

**What's wrong:** `mybot.so.bak` loads as a plugin and segfaults. A non-`.exe` executable on macOS/Linux gets spawned as subprocess regardless of whether it's actually a dynamic library.

**Why it matters:** Fragile, silent failures.

**Options:**
- **A.** Try `Library::new(path)` first; on `Err`, fall back to `Command::new(path).spawn()`. DWIM.
- **B.** Require explicit `--plugin path` / `--subprocess path` flags. Verbose but unambiguous.
- **C.** Detect via file magic (ELF/Mach-O/PE header). Robust but adds a parser.
- **D.** Status quo.

**Recommendation:** **12A.** Free DWIM; `Library::new` is cheap to attempt.

---

### 13. `xtask` scaffolder is broken and out of date

**File:** `xtask/src/main.rs:47-58` and templates

**What's wrong:** Templates only emit `_defs`/`_rs`/`_cpp`. They never emit `_game` or `_viz` (the two crates that actually do anything), never set `crate-type = ["cdylib", "rlib"]` on the bot crate, never wire `FfiGame`, never register the game with `runner/src/main.rs`. Running `cargo xtask new-game foo` today produces a non-compiling skeleton.

**Why it matters:** Useless tool that pretends to be useful.

**Options:**
- **A.** Rewrite templates to mirror current 5-crate structure per game; update `add_workspace_member` to add all five.
- **B.** Drop `xtask` entirely. Document the 5-crate pattern in a `docs/new-game.md`. Less code to maintain.
- **C.** **A + verification step:** after scaffolding, print a checklist of manual edits still needed (e.g. "add `\"foo\" => run_for_game::<FooGame>(...)` to `runner/src/main.rs:30`"). Could even edit it via `toml_edit`/`syn`.

**Recommendation:** **13C.** The scaffolder is genuinely useful for the rote 5-crate setup; the verification/auto-edit step closes the gap with the runner dispatch.

---

## P2 — Coupling, dead weight, smaller cleanups

### 14. Hardcoded grid constants are duplicated

**Files:** `tron/tron_game/src/lib.rs:6-7` ↔ `tron/tron_viz/src/main.rs:18`; `tictactoe_defs:18` ↔ `tictactoe_viz`

**What's wrong:** `WIDTH = 30` / `HEIGHT = 20` in the game crate, but `grid_size()` returns `(30, 20)` literally in viz. `tictactoe_defs` has `pub const BOARD_SIZE: usize = 3` but `tictactoe_viz::grid_size()` returns `(3, 3)` hand-coded.

**Why it matters:** Two sources of truth invite drift. Visualizer renders 30×20 grid for a game that's now 40×25 → silent visual mismatch.

**Options:**
- **A.** Make the constants `pub const` in the game crate, reference them from viz. Local fix.
- **B.** Add an associated const to the `Game` trait (e.g. `const GRID_SIZE: Option<(u32, u32)>`). Invasive — not every future game is grid-shaped.
- **C.** Status quo.

**Recommendation:** **14A.** Single source of truth without polluting the trait.

---

### 15. `TurnInputFFI::as_ref` is safe but exposes UB if hand-constructed; tic-tac-toe has no length field

**Files:** `tron/tron_defs/src/lib.rs:117-137`, `tictactoe/tictactoe_defs/src/lib.rs:117-137`

**What's wrong:** Two related issues:
1. The structs document that "the only safe constructor is `as_ffi`", but `as_ref` is a *safe* method that dereferences raw pointers. A C++ bot can hand-build a `TurnInputFFI` and pass it to `take_turn`; our `as_ref` then runs `from_raw_parts` on whatever pointer it gets.
2. Tic-tac-toe's FFI has no length field — it casts the pointer to `*const [Cell; BOARD_CELLS]` and dereferences. If a bot was built with a different `BOARD_CELLS`, this reads past the buffer.

**Why it matters:** Issue #1 is a safety-doc lie; issue #2 is a concrete read-past-buffer if `BOARD_CELLS` ever diverges (which #5 should prevent, but defense in depth).

**Options:**
- **A.** Mark `as_ref` as `unsafe` everywhere. Forces callers to acknowledge invariants.
- **B.** Add a `len` field to `TicTacToeFFI` matching tron's pattern; runtime-assert in `as_ref`.
- **C.** Status quo + better SAFETY comments.

**Recommendation:** **15A + 15B.** A is honest; B brings the two `_defs` crates to parity so future games can copy the pattern without picking the worse one.

---

### 16. Dead code that's still wired in

**Files:** `macros/src/lib.rs` (0 bytes), `tron/tron_cpp/main.h` (stub), `common/src/engine.rs:23` (`PlayerError::Timeout` unused), `tron/tron_defs/include/tron_defs.h` (committed AND regenerated)

**What's wrong:** The `macros` crate is a workspace member and a runner dep, but its lib is empty — pure dead weight. `tron_cpp` is a leftover stub. `PlayerError::Timeout` is defined but never constructed (becomes used once #2 lands). The cbindgen-generated header is both committed and regenerated by `build.rs` on every build, leaving `git status` perpetually dirty.

**Why it matters:** Confusion, slow builds, noise.

**Options:**
- **A.** Delete `macros/`, delete `tron/tron_cpp/`, drop `PlayerError::Timeout` unless we wire it up via #2, add `tron/tron_defs/include/` to `.gitignore`.
- **B.** Implement the macros crate (e.g. `ffi_bot!` from #4 lives there). Keep `tron_cpp` if we ever want a C++ example. Convert PlayerError::Timeout into a real path via #2.
- **C.** Status quo + comments explaining.

**Recommendation:** **16A**, except keep `Timeout` if doing **#2 (2A)** in the same batch; it'll get used immediately.

---

### 17. `Visualize` marker-type pattern is an orphan-rule workaround

**Files:** `viz/src/lib.rs:24-47`, per-game `_viz/src/main.rs`

**What's wrong:** `struct TronViz;` exists only to give the per-game crate a local type to `impl Visualize for …`. The trait has no `&self` methods — it's really a namespace. Feels indirect.

**Why it matters:** Mostly aesthetic / "is this idiomatic?" question. The marker pattern is a legitimate Rust idiom for exactly this problem; the discomfort is cargo-cult risk for future games.

**Options:**
- **A.** Keep as-is. Marker types are the canonical orphan-rule workaround.
- **B.** Drop the trait; have `viz::run` accept a `DrawCallbacks<G> { draw, status, side_panel, bottom_panel }` struct of closures/fn pointers. Loses default-method ergonomics; gains directness.
- **C.** Move `Visualize` trait *into the game crate* so the impl is on the game type directly. Forces game crate to depend on viz (heavy: pulls macroquad into the game's build).
- **D.** Macro that hides the marker: `viz_for!(TronGame { fn grid_size() -> (u32, u32) { (30, 20) } … })`. Generates the marker + impl behind the scenes.

**Recommendation:** **17A** (keep). The marker is fine; "I see a marker type" is one second of explanation per future contributor. Don't change unless game #3 makes it actively painful.

---

### 18. `Line::FromStr` parses by byte arithmetic

**File:** `tron/tron_defs/src/lib.rs:208-224`

**What's wrong:** Uses `s.match_indices(' ').nth(1).map(|(i, _)| i)` + `split_at` to find the second space, then re-parses each half as a `Pos`. Works *accidentally* for 4-token strings because `Pos::FromStr` calls `trim`. `"1 2 3"` (3 tokens) errors at the second `parse`; `"1 2 3 4 5"` (5 tokens) silently misparses.

**Why it matters:** It's not a bug today (only `Line::Display` produces input), but it's a fragile parser that will silently break if `Pos`'s parse rules change.

**Options:**
- **A.** Rewrite using `s.split_whitespace().collect::<Vec<_>>()`, length-check 4, parse each.
- **B.** Use a parser combinator (`nom`/`winnow`). Overkill.
- **C.** Status quo + stricter tests covering malformed input.

**Recommendation:** **18A.** Five-line rewrite, removes the trap.

---

## Suggested ordering

If we work through these piecewise, here's a sensible order — adjacent items share scope and can be done in one sitting:

1. **Pass 1 — kill the silent bugs**: 1A, 3A, 7A+7C
2. **Pass 2 — UB hardening**: 4A+4C, 5A, 15A+15B, 6A
3. **Pass 3 — ergonomics**: 8A, 9A, 10A, 12A
4. **Pass 4 — format evolution**: 11C, 5A integration with 11C (header includes ABI version)
5. **Pass 5 — quality of life**: 14A, 16A, 18A, 13C
6. **Pass 6 — fancy hardening** (only if needed): 2A (subprocess timeouts)

We've explicitly *not* recommended: **17** (keep marker pattern), **2D** / **3C** / **4B** / **6B** etc. (status-quo options listed for completeness).
