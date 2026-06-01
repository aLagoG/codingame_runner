# Vendoring dependencies — design plan

Read-only design doc. Reads what cargo cached on disk under
`~/.cargo/registry/src/...` and `~/.cargo/git/checkouts/...`; does not
fetch from the network.

## TL;DR

The user-facing promise of "flatten my crate plus its deps to one .rs
file" runs into four hard walls: **proc-macro crates** (need dylib
compilation), **build scripts** (need execution at compile time),
**conditional compilation** (`#[cfg(feature = ...)]` everywhere), and
**`$crate` in macros** (refers to the defining crate). The pragmatic
approach: discover the dep graph via `cargo metadata`, classify each
dep as vendorable or not, vendor what we can by emitting one
`mod <crate_name>` per dep at the flat file's root + applying a small
set of syn rewrites + partial cfg evaluation, and either refuse or
fall back gracefully on the rest. This is multiple weeks of work, so
it should ship in 5 phases starting with a read-only "what would
happen?" report.

## Why this is genuinely hard

These are the constraints that shape every decision below.

**1. Proc-macro crates can't be vendored.** A `[lib].proc-macro = true`
crate compiles to a `.dylib`/`.so` that rustc loads at compile time.
Its source can't be inlined into the consuming crate — it has to exist
as a separately-compiled artifact. This means crates like
`serde_derive`, `tokio-macros`, `thiserror` (the proc-macro half),
`clap_derive` cannot be vendored. Worse, if the user's crate uses
`#[derive(Serialize)]`, vendoring serde alone doesn't help — the
derive needs serde_derive at compile time, which we can't inline.

**2. Build scripts can't be run.** `build.rs` runs before compilation
and can: generate `.rs` files in `OUT_DIR` that get `include!`'d, emit
`cargo:rustc-cfg=...` directives that change conditional compilation,
link C libraries via `cargo:rustc-link-lib`. None of this can happen
at flat-file compile time — there's no Cargo to drive it. Crates with
`build.rs` (or a `[package].build` or `[package].links` field) are
unvendorable.

**3. Conditional compilation is everywhere.** Real deps have
`#[cfg(feature = "X")]` peppered throughout. The flat file has no
Cargo.toml, so rustc has no idea what features are enabled — they all
default to "off". Without partial cfg evaluation at vendoring time,
vendored serde would have most of itself gated off. We evaluate
`feature = "..."` predicates against the resolved feature set
from `cargo metadata` and either drop or unconditionally-include the
gated code.

We deliberately DO NOT evaluate compiler-set predicates
(`target_os`, `target_family`, `target_arch`, `target_env`,
`target_abi`, `target_endian`, `target_pointer_width`,
`target_vendor`, `target_has_atomic`, `target_feature`, the
`unix`/`windows` shorthands, `debug_assertions`, `test`,
`proc_macro`, `panic`) — these flow through verbatim so the user's
compile picks the right branches at build time. **The flat output
is target-portable**: vendor on macOS, run on Linux. Per-target
file selection (mio's `mod selector;` with epoll/kqueue/poll
candidates) emits ALL existing candidates as separate cfg-gated
`mod NAME { contents }` blocks; the user's compile picks one.
Whole files / whole deps gated by `#![cfg(target_os = "X")]` are
inlined unconditionally — the inner cfg flows through and gates
the contents at user time. Sibling-import injections referring to
deps with such inner cfgs get cfg-gated with the same predicate.

Build-script `--cfg=NAME` directives (rustix's `apple` from build.rs
detecting macOS) ARE baked at vendor time — they're host-specific
by construction since build.rs runs on the host. This is the one
target-portability limitation: deps that rely on build-script cfgs
behave as if those cfgs match the vendoring host.

**4. `$crate` inside macros refers to the defining crate.** When
`anyhow` defines
`macro_rules! bail { ($e:expr) => { return Err($crate::Error::msg($e)); } }`,
`$crate` resolves to the `anyhow` crate at expansion. After we inline
anyhow into a `mod anyhow`, those macros think they're defined in the
**consuming** crate, so `$crate::Error` resolves to nothing useful.
This is the single most fragile part of vendoring. The only fix is to
rewrite `$crate` → `$crate :: anyhow` in every `macro_rules!` body
before inlining, walking the token stream, which is doable but easy
to get wrong (hygiene, edge cases like `$crate` used in argument
position).

There are smaller obstacles too — edition mixing, `extern crate`
declarations, `include_str!` with relative paths, version
disambiguation when two versions of the same crate are in the graph,
collisions with the user's own module names — but the four above are
what determine whether this can work at all.

## Discovery: cargo metadata

Use the `cargo_metadata` crate (already widely used, well-maintained)
to invoke `cargo metadata --format-version 1 --filter-platform <host-triple>`
against the user's crate. The output gives us:

- `packages[].manifest_path` — every crate's `Cargo.toml`, source is
  the parent directory
- `packages[].targets[]` — including which targets are `proc-macro`
- `packages[].features` — feature definitions
- `packages[].build` — `Some(...)` if there's a build script
- `packages[].links` — `Some(...)` if it links a native lib
- `resolve.nodes[]` — the actual graph: each node has `id`, `deps[]`
  (with rename info!), and `features[]` (the resolved feature set
  after unification)
- `resolve.root` — the user's crate

We don't have to know about `~/.cargo/registry/src/...` paths — cargo
metadata gives us absolute paths regardless of whether deps are from
crates.io, git, or local paths. The "use already-downloaded sources"
promise is satisfied automatically: if cargo metadata succeeds,
sources are on disk.

The host triple matters because feature unification can pull in
different deps per platform. For v1, just use the host triple.
Cross-target vendoring is a stretch goal.

## Architecture: what the flat file looks like

```rust
//! Generated by cargo-flatten --vendor
//! User crate: my_app v0.1.0 (bin)
//! Vendored: anyhow 1.0.102, itoa 1.0.18 (4.2 KB)
//! External requirements (could not vendor):
//!   serde_derive 1.0.225 — proc-macro
//!   ring 0.17.0 — has build script + links openssl
//! Total: 18.4 KB across 3 vendored files

// === User crate ===
// (existing flatten output for the user's source tree)
fn main() {
    let n = itoa::Buffer::new().format(42_u32);
    anyhow::bail!("oops: {}", n);
}

// === Vendored deps ===

mod anyhow {
    // entire anyhow source flattened, with `$crate` rewritten and
    // `crate::` paths rewritten to `crate::anyhow::`.
    ...
}

mod itoa {
    ...
}
```

We use the dep's plain crate name as the mod name. In Rust 2018+
`use` paths are absolute (rooted at the crate root), so `use anyhow::Error`
in the user's code looks for `anyhow` at the crate root, finds our
`mod anyhow`, and resolves correctly. No path rewriting in the user's
own code is needed. The same automatic resolution covers
inter-vendored references: `use serde_core::Foo` inside `mod serde`
resolves to the sibling `mod serde_core` at the crate root.

The shape that *does* need rewriting is the meaning of `crate::` and
`$crate` *inside* a vendored mod — both still point at the flat
file's actual crate root, not at the vendored mod's root. Vendored
anyhow's `use crate::Error` would otherwise resolve to nothing, and
its `bail!` macro's `$crate::Error::msg(...)` likewise. Both need a
syn `VisitMut` pass: rewrite `crate::*` → `crate::anyhow::*` and
`$crate` → `$crate :: anyhow` inside each vendored mod.

## Per-dep processing pipeline

For each dep classified as vendorable:

1. **Locate.** From `manifest_path`, find the lib target's source file
   (we already do this; `parse_target` with `TargetSelector::Lib`).
2. **Flatten.** Run our existing flattener on it. We get a
   `SourceFile` tree.
3. **Cfg-evaluate.** Walk the AST, find `#[cfg(...)]` attributes whose
   predicate is purely about features. Look up the resolved feature
   set for this dep (from cargo metadata). For each gated item: drop
   if predicate evaluates to false, strip the cfg attr if it
   evaluates to true, leave alone if the predicate touches non-feature
   cfgs.
4. **Rewrite `$crate`.** Walk every `macro_rules!` body's token
   stream. For each `$crate` token, splice in `:: <crate_name>` after
   it. Tricky details about token spacing.
5. **Rewrite `crate::` paths.** Walk the AST, find paths starting with
   the `crate` keyword. Prepend `<crate_name>::`.
6. **Strip `extern crate`.** Old-edition crates have
   `extern crate foo;` at the root; these are now meaningless and
   should be removed.
7. **Handle `#[macro_export]`.** Strip the attribute (it would
   otherwise lift the macro to the *outer* crate root, leaking it
   beside the vendored mod). Add a `pub use <macro_name>;` inside
   the mod so `<crate_name>::<macro_name>!()` still resolves.
8. **Wrap.** Emit `mod <crate_name> { /* rewritten source */ }`.

Steps 3-5 are syn `VisitMut` passes. Each is a few hundred lines but
conceptually clean.

## Classifying deps: what's vendorable

Refuse to vendor if any of the following holds:

- `targets[].kind` contains `proc-macro`
- `package.build` is `Some(...)` OR a `build.rs` exists adjacent to
  the manifest
- `package.links` is `Some(...)` (links a native library)
- The lib is `#![no_std]` and the user's crate isn't (mismatch)

Warn (but proceed) if:

- Uses `#![feature(...)]` (requires nightly to compile the flat output)
- Has `include_str!` / `include_bytes!` with relative paths (we'd need
  to inline those too — v2)
- Has cfgs we don't understand (target-specific, etc.) — output will
  compile per host's resolution

### Name collisions

Because we use the dep's plain name as the mod name, two things can
collide at the flat file's crate root:

- **User's own mod has the same name as a transitive dep.** User has
  `mod log;` and depends on `tracing`, which transitively depends on
  the `log` crate.
- **Two versions of the same crate in the resolved graph.** Two `mod
  rand` declarations would be a hard rustc error.

For v1, both cases are detected up-front and the tool refuses with a
clear message:

```
error: cannot vendor `log` v0.4.22 — your crate already declares `mod log`
       at the crate root (src/log.rs:1)
help: rename the local module, or omit `log` from vendoring
```

```
error: cannot vendor two versions of `rand` (0.7.3 and 0.8.5) — the flat
       file would have two `mod rand` declarations at the crate root
```

A future improvement (see Open questions) is a `--vendor-rename
log=__local_log` escape hatch that lets the user opt into a
disambiguating prefix on a per-crate basis. v1 doesn't ship it; users
work around the rare collision by renaming the local mod or removing
the dep.

### Modes (CLI flags)

- `--vendor` — vendor what's vendorable; error on the rest with a
  clear list.
- `--vendor-strict` — same, but treat warnings as errors too.
- `--vendor-allow-external` — vendor what's vendorable; leave the rest
  as `extern crate` requirements and emit a Cargo.toml hint at the
  top of the output. Output is no longer single-file in the strict
  sense, but the dep set is greatly reduced.

## Phased delivery

Five phases over multiple PRs. Each is a meaningful unit on its own.

### Phase V0 — Reconnaissance (1 day)

Add a `--vendor-report` mode that runs `cargo metadata`, classifies
every dep as vendorable / unvendorable / warn, and prints a report.
No actual vendoring. This validates our classification logic against
the wild and gives the user a clear "could this work?" answer before
they invest in vendoring. Also gives us real data about what fraction
of common crates are vendorable.

### Phase V1 — Single-dep, no-feature, no-macro vendoring (✅ shipped)

Pipeline implemented for deps that: are pure Rust, declare no
feature-gated cfgs, have no `macro_rules!` using `$crate`, and don't
use `#[macro_export]`. `crate::` paths inside the vendored mod get
rewritten to `crate::<dep_name>::*` via a syn `VisitMut` pass that
correctly skips `pub(crate)` shorthand. `extern crate` declarations
are stripped. Collision detection refuses on user-mod-shadow and
multi-version. End-to-end: synthetic user crate + path-dep, vendor,
`rustc --crate-type=bin` accepts the output and the resulting
binary runs correctly.

In practice: itoa, lazy_static and most "small" crates *don't* fit
V1 because they have feature cfgs. Phase V2 unlocks them by
evaluating feature predicates against the resolved feature set.

### Phase V2 — Features and multi-dep (✅ shipped)

Three-valued cfg evaluator (`True` / `False` / `Unknown`) parses
`#[cfg(...)]` and `#[cfg_attr(..., …)]` predicates and evaluates
against the resolved feature set from cargo metadata:

- `feature = "X"` predicates → True/False from the feature set.
- `target_os`/`unix`/etc. → Unknown (left in source for rustc).
- `not`/`any`/`all` propagate Unknown correctly.

For False on **items** (functions, structs, mods, impl methods,
trait defaults — anywhere a syntactic Item lives), the entire item
is **deleted from the source** by computing its full byte range via
syn's `Spanned`. The walker recurses into ItemMod content / impl
items / trait items but never into items it has already marked for
deletion, so deletion edits never overlap with each other or with
per-attr edits. For False on **fields, variants, statements,
expressions** — places where syntactic deletion would require comma
handling we haven't built — we still rewrite to `#[cfg(any())]` to
force-off the gated element.

For True the cfg attr is stripped (item kept unconditionally). For
Unknown the attr is left alone for rustc to handle.

Multi-dep already worked from V1 (sibling mods at the crate root
resolve via absolute `use` paths). `DepEntry` now also carries the
resolved feature set + the dep's edition. `VendoredPackage` exposes
`max_edition` and the banner names the required `--edition`.

Smoke test against a real crates.io dep:
`vendor_real_itoa_through_cargo_cache` reads itoa's source from
`~/.cargo/registry/src/...`, vendors it (V1 had refused on feature
cfgs), assembles the flat output, and verifies rustc accepts it.

### Phase V3 — `$crate` rewriting + `#[macro_export]` (✅ shipped)

Walks every `macro_rules!` body (recursively, including macros nested
in inline mods). For each `$crate` token, inserts `::<crate_name>`
immediately after the `crate` ident — preserving Rust's macro
hygiene semantics while pointing into the right vendored mod. The
walker recurses through nested `Group` token trees, so `$crate`
inside macro args inside another macro invocation inside a
`macro_rules!` body is found and rewritten correctly.

Refuses if `$crate` appears in a position where prepending
`::<crate_name>` would produce invalid syntax (i.e., not followed by
`::`). The refusal is per-dep: that dep gets reported as
unvendorable, the rest still vendor.

For `#[macro_export]`: strips the attribute (which would otherwise
lift the macro to the *outer* crate root, leaking it beside the
vendored mod) and inserts `pub(crate) use <macro_name>;` after the
macro item. `pub(crate)` is required — `pub use` of a non-exported
macro is rejected by rustc — and is exactly what we want: the macro
stays callable as `<crate_name>::<macro_name>!()` from anywhere in
the flat file.

The four passes (cfg-attr rewrites, `$crate`, `#[macro_export]`,
`crate::` paths, `extern crate` removal) now share a single
deletion-range set computed once up front so none of them emit
edits inside spans that the cfg-deletion pass will remove —
preventing overlapping edits.

Eleven new tests, including `vendor_macro_with_dollar_crate_compiles_via_rustc`
which synthesizes a tiny logger with `#[macro_export] macro_rules!`
+ `$crate`, vendors it, compiles the flat output, and asserts the
macro actually expands and runs correctly at runtime.

### Phase V4 — Allow-external mode + polish (3-4 days)

`--vendor-allow-external` flag emits a Cargo.toml stub. License
banner (concatenate each vendored crate's `LICENSE-MIT` /
`LICENSE-APACHE`). Better diagnostics ("can't vendor `serde_derive`
because it's a proc-macro; consider replacing `#[derive(Serialize)]`
with manual impls or removing this dep"). Caching of cargo metadata
calls (they're reasonably fast but get called twice per invocation
today).

### Phase V5 (stretch) — Constrained build-script support

Many build scripts just emit `cargo:rustc-cfg=...` lines based on
probing rustc capabilities (`rustc_version` crate is the canonical
example). For these, we could detect the pattern, evaluate it
ourselves, and skip the build.rs. Not running arbitrary build.rs
code, just recognizing common patterns. Would unlock a meaningful
chunk of the ecosystem (notably `proc-macro2` itself uses this
pattern — though it's a proc-macro and we can't vendor it anyway).

## Key design calls (need decision before starting)

These shape the scope and aren't reversible cheaply once code is
written.

1. **Refuse vs. fall back when a dep is unvendorable.** I'd default
   to refusing (`--vendor` errors with a list of unvendorables). The
   fall-back mode (`--vendor-allow-external`) is opt-in. Alternative:
   default to fall-back, since that produces *some* output rather
   than none. Pick one, pin it as the default.

2. **Where do vendored crates live in the output?** `mod <name>` at
   the bottom of the file (proposed) keeps the user's code at top,
   which reads naturally. Alternative: top of the file, so vendored
   deps are "set up" before the user code references them. Rust
   doesn't care about order for `mod`, so this is purely aesthetic.

3. **Edition for the flat output.** Pick the user crate's edition?
   The highest among all vendored deps? A user-specified `--edition`
   flag? I'd do: highest edition seen, with `--edition` to override.
   Errors out if any vendored dep needs a newer edition than the
   chosen one.

4. **Multi-version refusal.** When two transitive deps pin different
   versions of the same crate (e.g. `rand` 0.7 and 0.8), v1 refuses
   outright (you can't have two `mod rand`). Supporting this would
   need a per-version naming scheme (`mod rand_0_7`, `mod rand_0_8`)
   *and* the path-rewriting pass we just avoided, applied selectively
   to disambiguated crates. Defer to a follow-up if anyone hits it.

5. **Dev/build dependencies.** Don't vendor — the flat output is
   meant to be compilable, not testable. `cargo metadata
   --filter-platform` already gives us the right scope; just exclude
   `dev-dependencies` and `build-dependencies` from the vendoring
   set.

6. **Workspace member deps.** If the user's crate has path-deps to
   sibling workspace members, vendor them as if they were external
   crates? I'd say yes — it's the same mechanism and people do this.

## Out of scope (explicit non-goals)

These came up while planning. Calling them out so they don't drift
back in:

- **Vendoring proc-macros.** Architecturally impossible without a
  separate dylib compilation step; that's outside cargo-flatten's
  scope.
- **Vendoring crates with build scripts.** Same — would require
  running arbitrary code at vendoring time. (Phase V5 might cover
  the narrow `rustc_version` pattern, but no general support.)
- **Cross-target vendoring.** Just vendor for the host. If the user
  wants to share a flat file that compiles on Linux from a Mac, they
  should run cargo-flatten on Linux.
- **Network fetching.** We trust cargo to have populated
  `~/.cargo/registry/src/...`. If a dep isn't there, fail with
  "run `cargo build` first".
- **Updating the manifest.** `--vendor-allow-external` emits a
  Cargo.toml *hint* (as a comment), not a real file. We don't write
  or modify Cargo.toml.
- **License compliance enforcement.** We *list* vendored crates'
  licenses in the banner; we don't enforce compatibility (MIT vs
  GPL etc.). That's the user's call.

## Open questions (worth deciding before V1)

- **`--vendor-rename` escape hatch.** A flag like
  `--vendor-rename log=__local_log` would let the user resolve a
  name collision (with a local mod or with another version) without
  touching their source. Not in v1; v1 just refuses with an error
  pointing the user at this future option. Worth designing the flag
  surface now so the v1 error message can mention the eventual
  syntax accurately.
- **`--fmt` interaction.** rustfmt may take 5-10x longer on a 200KB
  vendored output vs a 20KB hand-flattened one. Cap input size or
  warn?
- **Re-exports inside vendored crates.** `pub use serde::Deserialize`
  from a re-exporting helper crate works automatically (sibling mod
  resolution), but `pub use crate::foo::Bar` needs the same `crate::`
  rewriting as `use`. The visitor must handle both `UseTree` and
  `Path` nodes uniformly.
- **`extern crate self as foo`.** A real (if weird) construct. Vendored
  crates that use it become harder to rewrite — the rebound name
  participates in path resolution.
- **Snapshot tests.** Vendored output for serde is enormous and would
  make `cargo insta review` painful. Probably snapshot only structural
  elements (banner, mod skeleton) not the full inlined content.

## Suggested next step

Start with **Phase V0** (the report-only mode). It's a few days of
work, builds on `cargo metadata` parsing we'd need anyway, validates
the classifier against real crates, and gives us empirical data on
what % of common dep graphs are actually vendorable before committing
to the rest. If V0 surfaces that 80% of real crates have at least one
unvendorable transitive dep, that fundamentally changes the "single
file" pitch and we should reconsider the approach (maybe lean harder
on `--vendor-allow-external` as the default).
