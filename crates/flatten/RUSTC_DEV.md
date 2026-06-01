# V5 (revisited): selective proc-macro expansion via `rustc-dev`

Read-only design doc. Records the design for the rustc-dev based
custom expander, replacing the cargo-expand approach in `EXPAND.md`
(which over-expanded stdlib macros).

## TL;DR

Use the `rustc-dev` rustup component to load rustc as a library.
Drive a partial compilation of the user crate through name resolution
and macro expansion, then walk the post-expansion AST and roll back
any expansions that came from stdlib macros (`println!`, `vec!`,
built-in derives, etc.) to their original source-level calls. Third-party
proc-macro expansions stay; they're now baked into the user source.
Resulting flat file compiles on stable Rust because no internal stdlib
APIs leak through.

**Cost:** ~3-5 weeks for the integration; cargo-flatten binary
becomes nightly-only; per-rustc-release brittleness is the main
ongoing maintenance.

## Why this beats cargo expand

| | `cargo expand` (V5a draft) | rustc-dev (this doc) |
|---|---|---|
| Stdlib macro expansion | Always expanded → flat output uses `::std::io::_print` etc. | Rolled back → flat output keeps `println!` / `vec!` / etc. |
| Flat output toolchain | Nightly only (internal stdlib APIs) | **Stable** (we strip nightly-only forms) |
| Cargo-flatten binary | Stable | **Nightly** (links rustc internals) |
| Selective per-macro control | None (rustc decides) | Full (we walk the expansion graph) |
| Output readability | Bad (every `println!` becomes 5 lines) | Good (only proc-macro expansions are inlined) |

The cost shifts: with cargo expand, the USER needs nightly to
compile the flat output. With rustc-dev, only WE need nightly to
build cargo-flatten. The flat file works on stable.

## API surface (`rustc-dev`)

`rustc-dev` is a rustup component that exposes rustc's internal
crates with `extern crate` access:

```sh
rustup component add rustc-dev llvm-tools-preview
```

Crates we'll use:
- `rustc_driver` — process-level entry point. Wraps the full
  compiler with a `Callbacks` trait we override.
- `rustc_interface::Config` — input source, options, file-loader
  override.
- `rustc_interface::Queries` (on the compiler) — staged access:
  parse, expansion, name resolution, HIR.
- `rustc_ast::ast` — pre-expansion + post-expansion AST.
- `rustc_ast_pretty::pprust` — print AST back to source.
- `rustc_span::Span` + `rustc_span::ExpnData` + `rustc_span::SyntaxContext`
  — macro hygiene + expansion provenance.
- `rustc_resolve` (read-only via Resolver) — for "is this macro path
  built-in or from a specific crate?".

Setup boilerplate per `rustc-dev` examples (e.g. Clippy, dylint,
`expandr`):

```rust
extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_ast;
extern crate rustc_ast_pretty;
extern crate rustc_span;
extern crate rustc_session;
// ...

struct ExpanderCallbacks { /* state */ }

impl rustc_driver::Callbacks for ExpanderCallbacks {
    fn after_expansion<'tcx>(
        &mut self,
        compiler: &rustc_interface::interface::Compiler,
        queries: &'tcx rustc_interface::Queries<'tcx>,
    ) -> rustc_driver::Compilation {
        queries.global_ctxt().unwrap().enter(|tcx| {
            let krate = tcx.crate_for_resolver(()).borrow();
            self.process_expanded_ast(&krate.0, /* resolver */);
        });
        rustc_driver::Compilation::Stop
    }
}

fn run(crate_root: &Path) -> String {
    let config = rustc_interface::Config {
        opts: rustc_session::config::Options {
            edition: rustc_span::edition::Edition::Edition2021,
            // ... per user crate
        },
        input: Input::File(crate_root.join("src/main.rs")),
        // ...
    };
    let mut callbacks = ExpanderCallbacks::new();
    rustc_interface::run_compiler(config, |compiler| {
        // drive through expansion
    });
    callbacks.into_output()
}
```

## Pipeline

```
  user source (lib.rs / main.rs)
        │
        ▼
  rustc parse (rustc_parse)
        │  produces ast::Crate (pre-expansion, MacCall nodes intact)
        ▼
  rustc macro expansion (rustc_expand)
        │  produces expanded ast::Crate
        │  - MacCall nodes replaced by their expansion
        │  - each expanded node's Span has a SyntaxContext pointing
        │    at the ExpnData for the macro that produced it
        ▼
  walk the expanded AST
        │  for each region with non-empty SyntaxContext:
        │    look up ExpnData via tcx.expn_data(ctxt.outer_expn())
        │    if ExpnData.kind says "Macro(BuiltIn or Std)" → mark for rollback
        │    otherwise (proc-macro from a third-party crate) → keep expanded
        ▼
  rollback stdlib expansions
        │  for each marked region:
        │    find the original MacCall span (ExpnData.call_site)
        │    read original source bytes for that span
        │    replace the expanded AST subtree with a fresh MacCall built
        │    from those bytes
        ▼
  pretty-print (rustc_ast_pretty::pprust::print_crate)
        │
        ▼
  flattened source (compiles on stable)
```

## The hard parts

**1. Identifying "stdlib" expansions.** Each `ExpnData` has a
`MacroKind` and a `def_id` (definition site). We classify:
- Built-in derives (`Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`,
  `Hash`, `Default`, `Ord`, `PartialOrd`): `def_id` resolves to
  `core::*` traits. Roll back.
- Built-in macros (`println!`, `vec!`, `format!`, `panic!`,
  `assert*!`, `dbg!`, `include_str!`, `concat!`, `cfg!`, `env!`,
  `option_env!`, `format_args!`, `write!`, `writeln!`, `eprint*!`,
  `print*!`, `compile_error!`, …): `def_id` resolves to `core::macros`
  or `std::macros`. Roll back.
- macro_rules! macros from `std`/`core`/`alloc`: same as above.
- macro_rules! macros from third-party crates: roll back if the
  crate is being vendored (we'd otherwise lose the macro_rules
  definition); keep expanded if not vendored.
- Proc-macros from third-party crates: keep expanded (the whole
  point of this exercise).

The classifier is a function `(ExpnData, deps_set) → KeepOrRollback`.

**2. Span-to-source rollback.** Once we know "this expanded subtree
came from `println!("hello {}", x)` at byte range [N..M] in the
original source," we want to reconstruct a fresh `MacCall` AST node
representing that source. Two implementations:
- **Source-text replacement** (simpler): defer decision until
  pretty-printing. Keep a list of `(expanded_span, original_source_text)`
  patches. When pprust outputs the expanded code, splice in the
  original text instead. Doesn't require rebuilding AST nodes.
- **AST-node reconstruction** (cleaner): re-parse the original source
  range as a `MacCall` and substitute it for the expanded subtree.
  Requires walking the AST with a mutable visitor.

We'll go with source-text replacement first; it sidesteps a class of
"how do I rebuild the right AST node here?" headaches.

**3. Item placement.** Built-in derive expansions become `impl`
blocks placed near the original `#[derive(...)]` item. Their spans
typically point AT the derive attribute. Recovering them is just
"strip everything in this AST region whose ExpnData is a built-in
derive."

Proc-macro derive expansions also become impl blocks but the spans
point at the `#[derive(...)]` site too. Different ExpnData (third-party
def_id) — we keep these.

So in pretty-printing the final source, we emit:
- All non-expanded items as-is (with their original `#[derive(...)]`
  attrs, minus the third-party derives that we expanded).
- The proc-macro-derived impls (kept from rustc's expansion).
- We do NOT emit the built-in-derive impls (they'd duplicate what
  rustc would generate from `#[derive(Debug, Clone)]` left in the
  source).

**4. Hygiene preservation.** Macro expansions can introduce
identifiers in different syntax contexts. When we PRETTY-PRINT the
expanded AST back to source text, those hygiene contexts collapse —
all identifiers become bare names. This works for most code but can
produce name collisions if a proc-macro generated `__a` and the
user has a `let __a` nearby. Rare; documenting as a known limitation.

**5. Rustc version pinning.** rustc-dev's API is unstable.
cargo-flatten will need a `rust-toolchain.toml` pinning the exact
nightly. Upgrading the toolchain becomes a quarterly task.

## Distribution model

cargo-flatten currently builds on stable. Adopting rustc-dev means:

- The **binary** must be built with a specific nightly toolchain.
- Users who install via `cargo install cargo-flatten` will need that
  same nightly available; the binary won't run otherwise (loads the
  rustc dylib at runtime).
- Pre-built releases on GitHub: pin to a specific nightly per
  release.
- CI: matrix-build per supported nightly.

Three options for ergonomics:
1. **Single-nightly support.** We pin to one nightly per
   cargo-flatten release. Users install that nightly. Simplest to
   maintain but rigid.
2. **Multi-nightly cargo install.** We publish a cargo subcommand
   wrapper that detects the user's nightly and downloads/builds the
   right cargo-flatten binary. Complex.
3. **Subprocess isolation.** cargo-flatten itself stays stable;
   spawn a separate `cargo-flatten-expand` binary built against
   nightly via `cargo-flatten install-expand`. Cleanest separation.
   Doubles the binary count.

Option 3 is the cleanest split and what I'd recommend.

## Implementation phases

### Phase 1 — Hello rustc-dev (1-2 days)

Validate that we can:
- Add `rustc-dev` to the user's toolchain.
- Add `extern crate rustc_*;` to a test binary.
- Drive a no-op compilation via `rustc_driver::RunCompiler`.
- Print the parsed AST.

Deliverable: minimal example that takes a `.rs` file path and prints
"parsed N items." Shows rustc-dev integration works on our dev
machines and CI.

### Phase 2 — Capture post-expansion AST (3-5 days)

Build the `Callbacks` integration to intercept after `after_expansion`.
Walk the AST and print every macro invocation we find with its
ExpnData. Verify we can distinguish stdlib from third-party.

Deliverable: tool that prints "expansions found:" with one line per
expansion identifying its origin (`std::macros::println` vs
`serde_derive::Serialize`).

### Phase 3 — Source-text rollback for stdlib macros (1-2 weeks)

Build the source patcher:
- Take the original source + post-expansion AST.
- Emit pretty-printed AST.
- For each region whose ExpnData is stdlib, substitute the original
  source text for that span instead.

Deliverable: tool that takes a user crate and outputs source with
proc-macro derives expanded but `println!`, `vec!`, etc. preserved.

### Phase 4 — Vendoring integration (3-5 days)

Wire into `vendor_package`. The expanded source replaces
`user_pkg.source`. Auto-externalize proc-macro deps. Add
`#![feature(...)]` opt-ins for any internal APIs that DO leak through
proc-macro expansions (some proc-macros emit
`::core::fmt::rt::Argument::*` etc.).

Deliverable: `--expand` flag on `cargo-flatten --vendor` that produces
flat output compilable on stable Rust.

### Phase 5 — Distribution + docs (1 week)

Decide on the distribution model (option 3 from above), write
`INSTALL.md`, set up CI to build per-nightly artifacts, document the
limitations and known failure modes.

## Risks and unknowns

**Risk: rustc-dev API changes.** Mitigation: integration tests that
compile a fixed set of fixtures against the pinned nightly. Caught
on toolchain upgrade.

**Risk: pretty-printer fidelity.** rustc's `pprust` emits valid Rust
but not necessarily readable Rust. The output will be uglier than the
input. Pair with `--fmt` to compensate.

**Risk: hygiene collisions.** Some proc-macros emit `__a`, `__b`,
etc., that could collide with user-defined names after pretty-printing
strips hygiene. Documenting as a known limitation; if it becomes a
real problem, add a hygiene-preserving renaming pass.

**Risk: incremental build cache.** If we drive rustc on the user's
crate, the user's `target/` cache might fill up with our intermediate
state. Run in a tempdir or use `target-dir`-override.

**Unknown: proc-macros that read `OUT_DIR`.** Some proc-macros
(serde_derive's `__private<N>`) read files generated by their
parent crate's build script. Even via rustc-dev, vendoring serde
itself remains blocked by build-script-file-generation (a B1
limitation, separate from the V5 expansion work).

**Unknown: order of operations.** Does rustc-dev let us drive *just*
expansion without also running borrow-check / type-check? If type-check
runs, it might fail on intermediate states. Phase 1 will tell us.

## What we're explicitly NOT doing

- **macro_rules! expansion** for non-stdlib macros: keep them
  un-expanded. The vendoring pipeline already handles macro_rules!
  in vendored crates via our existing `$crate` rewrites.
- **Vendored-dep expansion**: V5b territory. After V5a (this doc)
  ships, deciding whether to extend to vendored deps is a separate
  question.
- **macros 2.0 (`pub macro foo`)**: experimental syntax; out of
  scope.
- **Inline assembly macros**: out of scope.

## Recommendation

Phase 1 first as a 1-2 day spike. If it works smoothly, commit to
phases 2-4. Phase 5 (distribution) can lag — internal use can be
"build from source on nightly" while we figure out shipping.
