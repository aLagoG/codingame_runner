# Bot Submission — CodinGame Walkthrough

How to turn a Rust or C++ bot in this repo into a single source file paste-ready for CodinGame's IDE.

## TL;DR

```sh
# Rust bot
cargo xtask bundle <game> <bot> --lang rust --vendor
# → target/codingame/<game>_<bot>_bot.rs

# C++ bot
cargo xtask bundle <game> <bot> --lang cpp
# → target/codingame/<game>_<bot>_bot.cpp
```

Copy the file's contents, paste into CodinGame's editor, submit.

## What CodinGame expects

A CG submission is **one source file**, compiled with a single `rustc` (Rust) or `c++` (C++) invocation. CG provides a fixed allowlist of Rust crates it can `--extern`; everything else must be inlined into the submitted file. Today's allowlist (mirrored in `crates/flatten/presets/codingame.txt`):

```
chrono itertools libc rand regex time
```

Notably *not* on the list: `anyhow`, `thiserror`, `serde`. Anything a Rust bot uses outside the allowlist has to be vendored into the bundle as a `pub mod`. C++ bots have no such allowlist — the bundle is one flattened source.

## Where bots live

```
games/<game>/
  defs/                # wire-format types, shared by engine + bots
  bots/<bot>_rs/
    Cargo.toml
    src/lib.rs         # `decide(&TurnInput) -> TurnOutput` (+ optional `on_init`)
    src/main.rs        # stdio loop calling `decide`
  bots/<bot>_cpp/
    Cargo.toml
    build.rs           # `cgio_build::build()` — compiles main.cpp via cc
    main.cpp           # stdio loop (the file cpp_flatten bundles for CG)
    strategy.h         # canonical per-turn logic (on_init + decide)
    src/main.rs        # tiny Rust shim — links the static archive
```

Every bot is a **subprocess**: the runner spawns it and talks the game's wire format over stdin/stdout. There is no in-process FFI variant — all transport is stdio. C++ bots wear a Rust shim only because cargo's `[[bin]]` target expects Rust as the entry-point language; the shim does nothing but link the C++ static archive and `extern "C"` into it.

## Writing a Rust bot

Minimum viable bot — `games/<game>/bots/<bot>_rs/src/lib.rs`:

```rust
use <game>_defs::{TurnInput, TurnOutput};

pub fn decide(turn: &TurnInput) -> TurnOutput {
    // your logic
    TurnOutput::default()
}
```

And the stdio loop — `src/main.rs`:

```rust
use std::io::{self, Write};

use bot_common::{ReadFrom, WriteTo};
use <game>_defs::TurnInput;
use <bot>_rs::decide;

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let turn = TurnInput::read_from(&mut input)?;
        decide(&turn).write_to(&mut output)?;
        output.flush()?;
    }
}
```

If the game has per-match init (e.g. `fantastic_bits` ships `my_team_id` before turn 1):

```rust
// src/lib.rs
use std::sync::OnceLock;
use <game>_defs::{InitialInput, TurnInput, TurnOutput};

static INIT: OnceLock<i32> = OnceLock::new();

pub fn on_init(init: &InitialInput) {
    let _ = INIT.set(init.my_team_id);
}

pub fn decide(turn: &TurnInput) -> TurnOutput {
    let team = INIT.get().copied().unwrap_or(0);
    // your logic
    TurnOutput::default()
}

// src/main.rs (one extra read + on_init call before the loop)
let init = InitialInput::read_from(&mut input)?;
on_init(&init);
loop { /* same as above */ }
```

The two baselines (`games/tron/bots/baseline_rs`, `games/fantastic_bits/bots/baseline_rs`) are working templates — `xtask new-bot --lang rust` scaffolds in the same shape.

### Dep rules for Rust bot crates

Bot `Cargo.toml`:

```toml
[dependencies]
<game>_defs.workspace = true
bot_common.workspace = true        # NOT `common.workspace = true`
anyhow.workspace = true            # optional but typical
```

Specifically:
- Use **`bot_common`**, never the heavier engine-side `common`. `common` pulls in `tracing`, `serde`, `thiserror` — all unvendorable.
- Defs crates (`<game>_defs`) likewise depend only on `bot_common` + `anyhow`, no serde derives. The wire format is text; serde isn't involved.
- Any crate the bot reaches transitively must either vendor cleanly (no proc-macros, no build-script link directives, no `links =` C deps) or be in the codingame preset. Run `cargo flatten vendor-report` for the matrix.

## How the Rust bundle is built

`cargo xtask bundle <game> <bot> --lang rust --vendor` invokes the `flatten` CLI with:

```
flatten <crate_dir> \
  --vendor \
  --external-preset codingame \
  --bin <crate_name> \
  --output target/codingame/<game>_<bot>_bot.rs
```

What flatten does:

1. **Parse the bin** — walks `src/main.rs`, inlines every local `mod` declaration into one source tree.
2. **Vendor the dep graph** — `cargo metadata` enumerates dependencies; everything in the codingame preset (`chrono itertools libc rand regex time`) stays as `use foo::…` references, the rest gets inlined as `pub mod foo { … }` blocks. Today's typical bot bundle vendors `anyhow + bot_common + <game>_defs + <bot>_rs` (the self-lib).
3. **Inline the same-package lib** — the bin's `use <bot>_rs::decide;` refers to the package's `[lib]` target. Cargo metadata treats bin+lib as one package (no edge to traverse), so flatten synthesizes a `DepEntry` for the lib and vendors it alongside other deps. The bundle contains `pub mod <bot>_rs { … decide … }` at the end.
4. **Rewrite for vendoring** — relevant passes for bot bundles:
   - `pub use NAME as ALIAS;` of `#[macro_export]` macros gets demoted to `pub(crate)` (anyhow's `pub use anyhow as format_err;` cross-file case).
   - Edition-2024 default-binding-mode fix: `match &mut EXPR { Variant { mut field } => … }` gets the explicit `&mut` reference pattern prepended (anyhow's `Chain::next_back`).
   - `extern crate alloc;` / `extern crate std;` injected at the bundle top so vendored 2018-era source resolves.

The header of every bundle lists what got vendored and what flatten cut as external. If the cut set is non-empty, that means something the bot transitively reaches couldn't be inlined — usually a proc-macro or a build-script link. Fix the upstream dep (drop the derive, swap for a vendor-clean crate) rather than papering over it.

## Verifying a Rust bundle locally

Sanity-check the output before pasting:

```sh
rustc --edition=2024 target/codingame/<game>_<bot>_bot.rs -o /tmp/bot
```

A clean compile is the same compile CG will do. Warnings about `unused macro definition: anyhow` and `#![no_std] / #![doc(html_root_url)]` at non-crate-root are cosmetic (flatten leaves anyhow's lib.rs inner attrs intact inside the vendored mod) — they don't break submission.

Round-trip with real wire input:

```sh
# tron — 2 players, you're player 0
printf '2 0\n0 0 5 5\n10 10 15 15\n' | /tmp/bot
# → DOWN

# fantastic_bits — team 0, 2 wizards, no snaffles
printf '0\n0 0\n0 0\n2\n0 WIZARD 1000 3750 0 0 0\n1 WIZARD 2000 3750 0 0 0\n' | /tmp/bot
# → MOVE 8000 3750 0 (×2)
```

If the bundle round-trips here, CG will accept it.

## Gotchas (Rust)

- **`#[derive(Serialize, Deserialize)]` anywhere reachable from the bot pulls in `serde_derive`**, which is a proc-macro and can't be vendored. The defs crates deliberately don't derive serde — wire format is hand-rolled `ReadFrom`/`WriteTo`.
- **`thiserror::Error`** has the same proc-macro problem. Hand-roll errors in bot-reachable code, or use `anyhow::Error`.
- **`tracing` macros** are partly proc-macro; avoid in bot code. Use `eprintln!` for debug output (CG shows stderr per turn).
- **`build.rs` on Rust bot crates** — Rust bot crates don't have one (only C++ bot crates do, for the cc-rs invocation). If you add a Rust-side `build.rs` that emits `cargo:rustc-link-lib=…`, flatten will refuse to vendor the bot's lib (link directives can't be replicated in a single rustc invocation).
- **Edition-2024 features in bot source** — `let-chains` (`if let X && let Y`), `gen` blocks, etc. all work in the bundle (it compiles at `--edition=2024`).

## Iterating

The CG paste loop is fast: edit `decide`, re-run `cargo xtask bundle`, paste, submit. For local sanity:

```sh
# Build the bot, then play a match via the runner.
cargo build -p <bot>_rs --release
cargo run -p codingame_runner --release -- --game <game> \
  target/release/<bot>_rs target/release/<other_bot>_rs
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
  Cargo.toml          # [build-dependencies] cgio_build; no [lib]; default-discovered [[bin]]
  build.rs            # one line: cgio_build::build();
  strategy.h          # canonical per-turn logic (on_init + decide)
  main.cpp            # stdio loop — the file cpp_flatten bundles for CG
  src/main.rs         # tiny Rust shim — links the static archive cgio_build produces
```

**One file owns the strategy.** `main.cpp` `#include`s `strategy.h` and calls `on_init` / `decide`. Edit `strategy.h`; both local cargo builds and the next paste-ready bundle pick up the change.

`main.cpp` has a tiny `#ifdef CGIO_RUST_SHIM` branch at its entry point — cgio_build sets that define during local cargo builds (entry point is `extern "C" int cgio_main()` so the Rust shim can call it), and leaves it unset when `cpp_flatten` bundles for CG (entry point reverts to a plain `int main()`). Bot authors don't normally touch that scaffolding.

The strategy's `decide` takes a `const cgio::TurnInput&` and returns a `TurnOutput`. For games with init data, `on_init` takes a `const cgio::InitialInput&`. For games without (tron), `InitialInput` is an empty struct and the function body is empty:

```cpp
namespace <bot>_cpp {
inline void on_init(const cgio::InitialInput& /*init*/) {}

inline TurnOutput decide(const cgio::TurnInput& turn) {
    // your logic
    return TurnOutput{};
}
}
```

Both `<game>_defs.h` (types) and `<game>_defs_io.h` (`operator>>` / `operator<<` per type) live at `games/<game>/defs/include/`. Both are hand-written; keep them in sync with the corresponding Rust defs by hand. (See `docs/wire-codegen.md` for the future schema-driven story; deferred.)

`fantastic_bits/bots/v1_cpp` is the worked example with a non-empty `InitialInput`; tron's three bot crates are the empty-init shape. `cargo xtask new-bot --lang cpp` scaffolds bots in the same shape as the existing baselines.
