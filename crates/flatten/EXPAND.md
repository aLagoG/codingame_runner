# Pre-expansion as a path to proc-macro support — deep dive

Read-only design doc. Records the analysis of the user-suggested
"expand-then-vendor" approach for proc-macro crates, the tradeoffs,
and a phased implementation plan.

## TL;DR

The idea: instead of vendoring proc-macro crates (impossible — they
compile to dylibs that rustc loads at compile time), pre-expand
proc-macro invocations to the regular Rust they emit, then vendor
that. The proc-macro crate itself can be dropped from the dep graph;
only the **runtime crate** the expansion references (e.g. `serde`
after `serde_derive` runs) needs to stay.

**Verdict: feasible but expensive.** It works empirically on small
test crates today using `rustc -Zunpretty=expanded`. It would unlock
huge swaths of the Rust ecosystem (anything using `#[derive(Serialize)]`,
`tokio::main`, `clap::Parser`, `thiserror`, `tracing::instrument`, etc.).
But it carries six non-trivial constraints — nightly toolchain
requirement, performance hit, code-size blow-up, debugging difficulty,
nightly-only attributes in the output, and the need to expand each
vendored dep separately. Worth doing in two phases: V5a expands only
the user crate (single high-leverage win, ~1 week), V5b expands
vendored deps too (closes the loop, ~2-3 weeks).

## Why proc macros block vendoring today

Quick recap of the constraint from `VENDORING.md`: a `[lib].proc-macro = true`
crate compiles to `*.dylib`/`*.so` that rustc loads at compile time
and invokes during macro expansion. Its source can't be inlined into
the consuming crate — it has to exist as a separately-compiled
artifact running on the host platform.

Concrete examples in the wild:

| user types | proc-macro crate | runtime crate |
|---|---|---|
| `#[derive(Serialize)]` | `serde_derive` | `serde` |
| `#[tokio::main]` | `tokio-macros` | `tokio` |
| `#[derive(Parser)]` | `clap_derive` | `clap_builder` |
| `#[derive(Error)]` | `thiserror-impl` | `thiserror` |
| `#[derive(FromBytes)]` | `zerocopy-derive` | `zerocopy` |
| `tracing::instrument` | `tracing-attributes` | `tracing` |

Every entry in the left column makes today's `cargo flatten --vendor`
either refuse (strict mode) or list the crate as Required external
(non-strict). If the user is sharing a single `.rs` gist, "you also
need these 5 lines in your Cargo.toml" undermines the value prop.

## What `cargo expand` actually produces

Empirical data from running `rustc -Zunpretty=expanded` on a minimal
serde+thiserror test crate:

### Test 1: `#[derive(Serialize, Deserialize)]` for a 2-field struct

```rust
// input (8 lines)
use serde::{Serialize, Deserialize};
#[derive(Serialize, Deserialize)]
struct Foo { a: i32, b: String }
fn main() { /* ... */ }
```

Expansion (~150 lines) headers:

```rust
#![feature(prelude_import)]
extern crate std;
#[prelude_import]
use std::prelude::rust_2021::*;
use serde::{Serialize, Deserialize};

struct Foo { a: i32, b: String }

#[doc(hidden)]
#[allow(non_upper_case_globals, ...)]
const _: () = {
    #[allow(unused_extern_crates, ...)]
    extern crate serde as _serde;
    #[automatically_derived]
    impl _serde::Serialize for Foo {
        fn serialize<__S>(&self, __serializer: __S)
            -> _serde::__private228::Result<__S::Ok, __S::Error>
        where __S: _serde::Serializer { /* … */ }
    }
};
// + similar Deserialize block (~80 lines)

fn main() {
    /* println! expanded to */
    { ::std::io::_print(format_args!("{0}\n", /* ... */)); };
}
```

### Test 2: `#[derive(thiserror::Error)]` on a 2-variant enum

Compact-ish expansion (~50 lines): manual `Display`, `Debug`, and
`std::error::Error` impls. References `thiserror::__private::AsDisplay`
in the runtime crate.

### Five things to notice in the output

1. **Nightly-only attributes in the head.** `#![feature(prelude_import)]`
   and `#[prelude_import]` are nightly. Rustc emits them because
   `-Zunpretty=expanded` is itself nightly and produces nightly-form
   output. Both must be **stripped** for the flat file to compile on
   stable. (Replacing with `extern crate std;` is enough — that's
   already the legacy 2015-style equivalent.)
2. **Runtime crate is referenced as `extern crate _serde`.** Critical:
   the expansion still depends on `serde` (the runtime crate) for the
   trait definitions and `__private` helpers. The expansion only
   removes the dep on `serde_derive` (the proc-macro). We can vendor
   `serde` and drop `serde_derive` from the graph.
3. **Internal API surface (`__private228`).** Serde's internal helpers
   are versioned with a numeric suffix (`__private228` for serde 1.0.228).
   Stable across patch versions of the same minor; brittle across major
   upgrades. Vendoring the matching serde version sidesteps this.
4. **Helper attributes survive.** `#[error("io: {0}")]` from thiserror
   stays on the enum variant after expansion — it's an inert helper
   attribute that the proc-macro consumed but didn't strip. On stable,
   `#[error]` is unknown and rustc emits a warning (or an error if the
   user has `#![deny(unknown_attributes)]`). Need to either strip these
   or emit `#![allow(unknown_attributes)]` at the file top.
5. **Pretty-printer artifacts.** Spaces around `::` in attribute paths
   (`clippy :: absolute_paths`), all comments dropped, original layout
   destroyed. Compiles fine; debugging is painful.

## The runtime-crate insight

This is the lever that makes the whole approach worthwhile. After
expansion:

| crate | role | vendor-able? | needed after expansion? |
|---|---|---|---|
| `serde` | runtime traits + `__private` | yes (regular lib) | yes |
| `serde_derive` | proc-macro | no | **no** |
| `thiserror` | runtime helpers | yes | yes |
| `thiserror-impl` | proc-macro | no | **no** |
| `clap_builder` | runtime API | yes | yes |
| `clap_derive` | proc-macro | no | **no** |

The dep graph splits cleanly along the proc-macro / runtime line
that crate authors already maintain (because compile-time vs. runtime
is the same architectural boundary they care about for build time and
binary size). Pre-expansion lets us vendor the runtime half and drop
the proc-macro half.

## Architecture

Two distinct phases, each with its own implementation cost.

### Phase V5a: pre-expand the user crate only

**Scope**: handles user-direct proc-macro use (`#[derive]` on user
types, `#[tokio::main]` on user `fn main`, etc.). Does NOT handle
proc-macros used INSIDE vendored deps.

**CLI**: `--expand` flag. Default off (preserves current behavior;
explicit opt-in for the nightly requirement).

**Pipeline change**:

```
  current:                     with --expand:
  ─────────                    ─────────────
  read user lib.rs/main.rs     run `cargo +nightly rustc --
  → scanner inlines mods            -Zunpretty=expanded`
  → vendoring rewrites         strip #![feature(prelude_import)]
  → write flat output          strip #[prelude_import]
                               strip leftover helper attrs
                               → use as scanner input  (NB: no mod
                                   inlining needed, expansion already
                                   produced one big file)
                               → vendoring rewrites (proc-macro deps
                                   now dropped from graph)
                               → write flat output
```

**Plumbing details**:

- Detect `cargo` toolchain. If no nightly available, fail with
  actionable error: "install nightly: `rustup install nightly`"
  rather than a cryptic `-Zunpretty: unknown flag`.
- Spawn `cargo +nightly rustc --bin <target> -- -Zunpretty=expanded`,
  capture stdout. Bin vs. lib target selection mirrors existing
  target-resolution logic.
- The expanded source replaces the scanner's role for user code. Our
  existing `mod NAME;` scanner becomes a no-op for user code (the
  expander already inlined every mod). We still run it for vendored
  deps in case `--expand` isn't propagated there.
- `extern crate _serde;` and friends inside `const _: () = { ... }`
  blocks: leave alone. They're scoped to the const block and don't
  conflict with our existing `extern crate` removal pass (which works
  on item-level extern crates, not const-block ones).
- Drop proc-macro deps from the resolver graph before BFS-with-cut-points
  runs. They've already been "consumed" by expansion.

**What works after V5a**:

- `serde` + `serde_derive` (user code uses derive, vendored serde
  provides runtime). User has zero deps in their downstream Cargo.toml.
- `thiserror` (same pattern).
- `tokio::main` style entry points (vendored tokio runtime).
- Most `tracing::instrument`, `clap::Parser`, etc.

**What still doesn't**:

- Vendored deps that internally use proc macros. e.g. `clap_builder`
  has `#[derive(Parser)]` calls in its own source. Those still need
  `clap_derive` at vendoring time — the `--external clap_derive`
  workaround we use today doesn't go away.

**Estimated cost**: ~1 week. Mostly subprocess plumbing + nightly-attr
stripping + a handful of tests against real crates.

### Phase V5b: pre-expand vendored deps too

**Scope**: closes the loop. After V5b, `serde`-using crates can be
vendored without `serde_derive` in the user's Cargo.toml, AND
`clap_builder` (which itself uses `clap_derive` internally) can be
vendored without `clap_derive` either.

**The hard part**: cargo expand operates on a *cargo package*, not a
source tree. To expand a single vendored dep, we'd need to either:

1. **Per-dep tempdir**: copy the dep's source to a tempdir, write a
   minimal Cargo.toml that mirrors only the `[dependencies]` section
   relevant to building this dep at the resolved feature set, run
   cargo expand there, capture output. ~30s per dep on first run
   (cold cache), faster after.
2. **Workspace patch**: temporarily add a `[patch.crates-io]` entry
   pointing to a copy of the dep with proc-macro deps removed, then
   expand. More fragile; cargo's patch resolution can fail.

Option 1 is more robust. Cost: per-dep cargo build time, ~10-60
seconds each, single-digit minutes for a 30-dep tree. cargo-flatten
should cache expansions keyed by `(crate, version, features)` so
rerunning `--vendor` is fast.

**Pipeline addition**:

```
for each vendored dep D:
    if D depends on any proc-macro crate:
        set up tempdir with D's manifest + minimal deps
        run cargo expand on D
        use expansion as D's source for vendoring rewrites
    else:
        use original source (current behavior)
```

**What works after V5b**: every popular ecosystem crate that today
needs proc-macro externals: serde, clap, tokio, tracing, thiserror,
zerocopy, axum (which pulls in tokio macros transitively), etc.

**What still doesn't**:

- Crates using proc-macros that emit nightly-only output. Rare;
  custom-derive-heavy crates targeted at nightly.
- Crates whose proc-macros need build-script outputs (e.g.,
  generated-file `include!` calls). Tied to the build-script issue,
  not the proc-macro issue per se.
- Crates that monkey-patch their own internals through proc-macro
  paths (rare).

**Estimated cost**: ~2-3 weeks. Subprocess orchestration, cache
management, error recovery, plus a meaningful test matrix against
real crates with proc-macro internals.

## Constraints to design around

### 1. Nightly-only `-Zunpretty=expanded`

The `unpretty` flag has been nightly-only for a decade. There's no
sign of stabilization. Options:

- **Require nightly when `--expand` is used.** Honest, simple. Most
  Rust devs have nightly installed via `rustup install nightly`.
  Single one-time UX cost.
- **Vendor nightly rustc.** Massive UX cost. Rejected.
- **Implement our own expander.** rustc's expander is ~10K LoC of
  delicate compiler code. There's no third-party syn-only expander
  for proc macros (because proc macros are arbitrary Rust code that
  needs a real compiler to run). Rejected.
- **Use `dtolnay/watt`.** Watt runs proc-macros as WebAssembly. Lets
  us "vendor" a proc-macro by including its WASM blob. Requires the
  proc-macro to be compiled to WASM, which most aren't. Niche.

**Recommendation**: require nightly behind `--expand`. Document it as
"opt in to broader vendoring at the cost of needing nightly".

### 2. Output bloat

Empirical: a 8-line user file that derives `Serialize` + `Deserialize`
expands to ~150 lines. A real-world web service might 5-10x in size.

Mitigations:
- `--minify` already exists; pair with `--expand` and the output is
  still smaller than the original sources transitively.
- For the "share as one gist" goal, total file size matters less
  than dep-tree complexity. Users picking `--expand` accept the
  tradeoff.

### 3. Helper attributes survive expansion

`#[error(...)]` and friends stay on items after the proc-macro
ran. On stable rustc with `#![allow(unknown_attributes)]` (which we
inject anyway as part of lint stripping), they're warnings, not
errors. With strict downstream lints, they fail.

Approaches:
- **Whitelist + strip.** Maintain a list of known proc-macro helper
  attrs (`#[error]`, `#[serde]`, `#[clap]`, `#[arg]`, `#[command]`,
  `#[from]`, etc.). Strip them post-expansion. Brittle; won't catch
  obscure crates' helpers.
- **Auto-detect.** Walk the expanded source for attributes whose path
  (a) isn't a known builtin, (b) isn't `#[cfg]`/`#[derive]`/etc.,
  (c) doesn't resolve to any item in our final dep set. Strip
  matches. Heuristic but covers the common case.
- **Inject `#![allow(unknown_attributes)]`** at the file top and
  ignore. Simple; downstream users with strict lints have to
  override. Lowest-effort; recommended for V5a.

### 4. Internal API stability (`__private228`)

Serde-style numeric versioning (`__private228` = serde 1.0.228) is
opt-in stability tracking. Vendoring matches the version, so the
expansion's `__private228` references resolve to the vendored
serde's `__private228` module. Unchanged across patch versions.

Risk: user upgrades serde minor version in the future, expansion
references stale internals. Mitigation: re-run `cargo flatten` after
dep upgrades. Same constraint as today.

### 5. Hygiene / span loss

Pretty-printed output loses macro hygiene: invisible spans collapse
to plain idents. This is fine if (a) the original macro was
hygienic-by-construction (most are) and (b) the pretty-printer
doesn't emit colliding names.

Risk areas:
- Macros that emit `__a` / `__b` / `__field0`: rare collisions if
  multiple macros emit the same name in the same scope. Empirically
  rare.
- Macros that capture surrounding lifetimes/types: should still work
  because expansion resolves names at expansion time.

### 6. Per-dep expansion cost (V5b only)

Each vendored dep needs its own cargo invocation. For a 30-dep tree:
- First run: ~5-15 minutes total (cold cargo cache for each tempdir).
- Cached: ~30s-2min total.
- With on-disk expansion cache: near-instant on rerun.

Should ship with disk caching keyed by `(crate, version, features-hash,
rustc-version)` from the start. Otherwise the dev loop is unbearable.

## Alternatives considered

### Watt (WASM-based proc-macro execution)

dtolnay's [watt](https://github.com/dtolnay/watt) lets a host crate
load a proc-macro at compile time as WebAssembly bytecode rather than
a native dylib. The proc-macro has to be precompiled to WASM and
shipped as a `.wasm` blob.

For our flat-file goal:
- Pro: works on stable Rust.
- Con: requires every relevant proc-macro to ship a WASM build.
  Almost none do.
- Con: would embed a multi-MB WASM blob in the flat output.
- Con: still requires `wasmer` or similar at compile time on the
  user's machine.

**Verdict**: niche. Skip.

### Manual expansion of common derives

Implement our own expander for ~10 popular derives (`Debug`, `Clone`,
`Serialize`, `Deserialize`, `Error`, `Default`, `Hash`, `PartialEq`).

- Pro: stable Rust. Faster than cargo expand.
- Con: 10 popular derives is a small fraction of the proc-macro
  ecosystem. Would punt on `clap::Parser`, `tokio::main`, `tracing::instrument`,
  custom derives, declarative procedural macros (function-like).
- Con: each derive is its own miniature implementation that has to
  track upstream changes. Maintenance nightmare.

**Verdict**: low ceiling, high cost. Skip.

### Source-only proc macros via syn directly

Some proc-macros are simple enough to be syn-driven AST transforms
that we could replicate. Detect `#[derive(SimpleX)]` patterns,
generate the impl directly.

- Pro: stable, fast.
- Con: requires per-macro implementation. Same scaling problem as
  manual expansion.
- Con: doesn't generalize to user-defined proc-macros at all.

**Verdict**: skip; same reasons as manual expansion.

### Skip the problem (current behavior)

Continue listing proc-macro crates as Required externals; user adds
them to their Cargo.toml. Status quo.

- Pro: zero implementation cost.
- Con: undermines the "share as one file" promise for any code that
  uses serde, tokio, clap, thiserror, tracing, ... in other words,
  most real-world Rust code.

**Verdict**: this is what we ship today. The user has correctly
identified that the promise is incomplete without proc-macro support.

## Recommendation

Implement V5a (user-crate expansion) as the next major feature after
V4 stabilizes. It unlocks the most common ecosystem crates with the
lowest implementation cost and keeps the nightly requirement opt-in.

V5b (vendored-dep expansion) follows as a separate phase once V5a is
proven and we've collected real-world failure modes from V5a users.
Don't ship V5a + V5b together — they have different failure modes
and the per-dep expansion machinery in V5b is a meaningful added
surface.

Add a single integration test for V5a that exercises the canonical
case (`#[derive(Serialize, Deserialize)]` on user types) and asserts
the flat output compiles without `serde_derive` in the downstream
Cargo.toml. That's the smallest-possible end-to-end proof the
approach works.

## Open questions / unknowns

- How does cargo expand handle `#[cfg(test)]` items? Need to verify
  expansion respects target cfgs and we don't accidentally include
  test-only code in release output.
- What's the failure mode when the user crate uses an UNRESOLVED
  proc-macro (typo, missing dep)? cargo expand will probably error
  out; need to surface that gracefully.
- Workspace inheritance: `version.workspace = true` in dep specs.
  cargo expand resolves these at the workspace root; our tempdir-per-dep
  approach in V5b needs to copy enough manifest context to resolve
  them.
- Crate patches and Git deps: cargo expand follows the resolver, so
  these should "just work" in V5a (user crate). In V5b, we'd need to
  forward patch info into the per-dep tempdir.
- Procedural-macro-only attributes used by the user code that don't
  trigger expansion (e.g. `#[serde(rename = ...)]` in the absence of
  a `#[derive(Serialize)]`): expansion can't strip these. Whitelist
  approach in §3 above handles them.
