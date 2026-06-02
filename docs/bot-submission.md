# Bot Submission — CodinGame Walkthrough

How to turn a Rust bot in this repo into a single `.rs` file paste-ready for CodinGame's IDE.

## TL;DR

```sh
cargo xtask bundle <game> <bot> --lang rust --vendor
```

Output lands at `target/codingame/<game>_<bot>_bot.rs`. Copy the file's contents, paste into CodinGame's editor, submit. The bundle is a single self-contained Rust source that compiles with `rustc --edition=2024 <file>.rs`.

## What CodinGame expects

A CG Rust submission is **one source file**, compiled with a single `rustc` invocation. CG provides a fixed allowlist of crates the compiler can `--extern`; everything else must be inlined into the submitted file. Today's allowlist (mirrored in `crates/flatten/presets/codingame.txt`):

```
chrono itertools libc rand regex time
```

Notably *not* on the list: `anyhow`, `thiserror`, `serde`. Anything a bot uses outside the allowlist has to be vendored into the bundle as a `pub mod`.

## Where bots live

```
games/<game>/
  defs/                # wire-format types + ABI version, shared by engine + bots
  bots/<bot>_rs/
    Cargo.toml
    src/lib.rs         # `decide()` (+ optional `on_init`) + `ffi_bot!`
    src/main.rs        # subprocess stdio loop
```

Two cargo targets, one shared source tree:

- **`[lib]`** (`cdylib + rlib`) — used by the FFI plugin path (`runner` / `tournament` dlopen the cdylib for fast in-process play).
- **`[[bin]]`** — used by the subprocess stdio path AND as the canonical CG submission. This is the target flatten bundles.

The bin's `use <bot>_rs::decide;` ties the two halves together. Flatten auto-vendors the same-package lib so the resulting bundle is self-contained (see *How the bundle is built* below).

## Writing a bot

Minimum viable bot — `games/<game>/bots/<bot>_rs/src/lib.rs`:

```rust
use <game>_defs::{TurnRef, TurnOutput};

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    // your logic
    TurnOutput::default()
}

bot_common::ffi_bot!(<game>_defs::Ffi, decide);
```

If the game has per-match init (e.g. `fantastic_bits` ships `my_team_id` before turn 1):

```rust
pub fn on_init(init: InitialInputRef<'_>) { /* stash into a OnceLock */ }
pub fn decide(turn: TurnRef<'_>) -> TurnOutput { /* read init from the OnceLock */ }

bot_common::ffi_bot!(<game>_defs::Ffi, decide, on_init);
```

And the subprocess loop — `src/main.rs`:

```rust
use bot_common::{ReadFrom, WireInput, WriteTo};
use <game>_defs::TurnInput;
use std::io::{self, Write};

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        <bot>_rs::decide(turn.as_ref()).write_to(&mut output)?;
        output.flush()?;
    }
}
```

(The two existing baselines — `games/tron/bots/baseline_rs` and `games/fantastic_bits/bots/baseline_rs` — are working templates.)

### Dep rules for bot crates

Bot `Cargo.toml`:

```toml
[dependencies]
<game>_defs.workspace = true
bot_common.workspace = true        # NOT `common.workspace = true`
anyhow.workspace = true            # optional but typical
```

Specifically:
- Use **`bot_common`**, never the heavier engine-side `common`. `common` pulls in `libloading`, `tracing`, `serde`, `thiserror` — all unvendorable.
- Defs crates (`<game>_defs`) likewise depend only on `bot_common` + `anyhow`, no serde derives. The wire format is text; serde isn't involved.
- Any crate the bot reaches transitively must either vendor cleanly (no proc-macros, no build-script link directives, no `links =` C deps) or be in the codingame preset. Run `cargo flatten vendor-report` for the matrix.

## How the bundle is built

`cargo xtask bundle <game> <bot> --lang rust --vendor` invokes the `flatten` CLI with:

```
flatten <crate_dir> \
  --vendor \
  --external-preset codingame \
  --bin <bot_name> \
  --output target/codingame/<game>_<bot>_bot.rs
```

What flatten does:

1. **Parse the bin** — walks `src/main.rs`, inlines every local `mod` declaration into one source tree.
2. **Vendor the dep graph** — `cargo metadata` enumerates dependencies; everything in the codingame preset (`anyhow` is NOT in it; only `chrono itertools libc rand regex time`) stays as `use foo::…` references, the rest gets inlined as `pub mod foo { … }` blocks. Today's bot bundle vendors `anyhow + bot_common + <game>_defs + <bot>_rs` (the self-lib).
3. **Inline the same-package lib** — the bin's `use <bot>_rs::decide;` refers to the package's `[lib]` target. Cargo metadata treats bin+lib as one package (no edge to traverse), so flatten synthesizes a `DepEntry` for the lib and vendors it alongside other deps. The bundle contains `pub mod <bot>_rs { … decide … ffi_bot! … }` at the end.
4. **Rewrite for vendoring** — relevant passes for bot bundles:
   - `pub use NAME as ALIAS;` of `#[macro_export]` macros gets demoted to `pub(crate)` (anyhow's `pub use anyhow as format_err;` cross-file case).
   - Edition-2024 default-binding-mode fix: `match &mut EXPR { Variant { mut field } => … }` gets the explicit `&mut` reference pattern prepended (anyhow's `Chain::next_back`).
   - `extern crate alloc;` / `extern crate std;` injected at the bundle top so vendored 2018-era source resolves.

The header of every bundle lists what got vendored and what flatten cut as external. If the cut set is non-empty, that means something the bot transitively reaches couldn't be inlined — usually a proc-macro or a build-script link. Fix the upstream dep (drop the derive, swap for a vendor-clean crate) rather than papering over it.

## Verifying a bundle locally

Sanity-check the output before pasting:

```sh
rustc --edition=2024 target/codingame/<game>_<bot>_bot.rs -o /tmp/bot
```

A clean compile is the same compile CG will do. Warnings about `unused macro definition: anyhow` and `#![no_std] / #![doc(html_root_url)]` at non-crate-root are cosmetic (flatten leaves anyhow's lib.rs inner attrs intact inside the vendored mod) — they don't break submission.

Round-trip with real wire input:

```sh
# tron — 2 players, you're player 0, both at (0,0)→(5,5)
printf '2 0\n0 0 5 5\n10 10 15 15\n' | /tmp/bot
# → DOWN

# fantastic_bits — team 0, 2 wizards, no snaffles
printf '0\n0 0\n0 0\n2\n0 WIZARD 1000 3750 0 0 0\n1 WIZARD 2000 3750 0 0 0\n' | /tmp/bot
# → MOVE 8000 3750 0 (×2)
```

If the bundle round-trips here, CG will accept it.

## Gotchas

- **`#[derive(Serialize, Deserialize)]` anywhere reachable from the bot pulls in `serde_derive`**, which is a proc-macro and can't be vendored. The defs crates deliberately don't derive serde — wire format is hand-rolled `ReadFrom`/`WriteTo`. Don't add serde derives "for tooling convenience" unless you've thought through the vendor impact.
- **`thiserror::Error`** has the same proc-macro problem. Hand-roll errors in bot-reachable code, or use `anyhow::Error`.
- **`tracing` macros** are partly proc-macro; avoid in bot code. Use `eprintln!` for debug output (CG shows stderr per turn).
- **`build.rs` on bot crates** — bot Cargo.tomls don't declare `build = "build.rs"` and don't have a `build.rs`. If you add one that emits `cargo:rustc-link-lib=…`, flatten will refuse to vendor the bot's lib (link directives can't be replicated in a single rustc invocation).
- **Edition-2024 features in bot source** — `let-chains` (`if let X && let Y`), `gen` blocks, etc. all work in the bundle (it compiles at `--edition=2024`). `#[unsafe(no_mangle)]` syntax via `ffi_bot!` is fine across editions.
- **The bundle includes the ffi_bot! extern fns** even though the subprocess binary never calls them. They sit as `#[no_mangle] pub extern "C" fn initialize/take_turn/abi_version/set_counter_callback` — dead symbols, a few hundred wasted bytes. Harmless.

## Iterating

The CG paste loop is fast: edit `decide`, re-run `cargo xtask bundle`, paste, submit. For local sanity:

```sh
# Build the cdylib for FFI playtesting via the runner.
cargo build -p <bot>_rs --release
cargo run -p codingame_runner --release -- --game <game> \
  target/release/lib<bot>_rs.dylib \
  target/release/lib<other_bot>_rs.dylib
```

Or compete in the tournament harness — see `docs/tournament.md`.

## C++ bots

C++ bots ship the same way but use `cpp_flatten` (inlines `#include "..."` recursively) instead of the Rust flatten:

```sh
cargo xtask bundle <game> <bot> --lang cpp
# → target/codingame/<game>_<bot>_bot.cpp
```

Layout:

```
games/<game>/bots/<bot>_cpp/
  Cargo.toml          # [lib] cdylib + [[bin]]
  build.rs            # cgio_build::build("<game>", "<crate_name>")
  strategy.h          # canonical per-turn logic (on_init + decide)
  bot.cpp             # FFI plumbing — forwards to strategy.h
  main.cpp            # stdio + cpp_flatten entry — also forwards to strategy.h
  src/lib.rs          # rust shim — empty cdylib body
  src/main.rs         # rust shim — links static lib, calls cgio_main
```

**One file owns the strategy.** Both `bot.cpp` (FFI) and `main.cpp` (stdio) just `#include "strategy.h"` and call `on_init` / `decide`. Edit `strategy.h`; both transports plus the next paste-ready bundle pick up the change. Drift between local-play and CG-submission strategy is structurally impossible.

**Invariant `InitialInput` shape.** Every game's `<game>_defs_io.h` declares `cgio::InitialInput`, `cgio::InitialInputRef`, and `cgio::InitialInputFfi` (a `using` alias to whatever cbindgen named the FFI struct). For games without per-player init (`NoInitialInput` on the Rust side), the structs are empty and `operator>>` is a no-op. Bot files always look like:

```cpp
// strategy.h
inline void on_init(const cgio::InitialInputRef& /*init*/) {}
inline TurnOutput decide(const cgio::TurnRef&) { return {}; }

// bot.cpp
void initialize(cgio::InitialInputFfi input) { bot::on_init(cgio::as_ref(input)); }

// main.cpp
cgio::InitialInput init;
if (!(std::cin >> init)) return 0;
bot::on_init(cgio::as_ref(init));
```

When a game grows a real `InitialInput`, **only `<game>_defs_io.h` + the Rust defs change** (give the structs real fields, update `operator>>`, point the typedef at the new cbindgen-emitted name). All three bot files stay untouched. If you forget to update the typedef, the compiler catches it immediately ("no type named `NoInitialInputFfi` in the global namespace; did you mean `InitialInputFFI`?") — no silent runtime ABI mismatch.

**fantastic_bits's `v1_cpp` is the worked example with a non-empty init**; tron + tic-tac-toe baselines are the empty-init shape. `cargo xtask new-bot --lang cpp` scaffolds bots in the same shape as the existing baselines.
