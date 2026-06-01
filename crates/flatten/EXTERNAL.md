# `--external` flag — design and implementation notes

Per-dep opt-out from vendoring. Named deps stay as external `use foo::...`
references in the flat output; the user is responsible for providing them
at compile time (Cargo.toml + `cargo build`, or `rustc --extern`).
Composes with everything else.

## Status (current)

**Shipped**:

- `--external NAME` (repeatable), `--external-file PATH` (one name
  per line, `#` comments + blank lines OK; repeatable; combines
  additively with `--external` flags), and `--external-preset NAME`
  (loads a baked-in curated list shipped with cargo-flatten — only
  preset today is `infra`, covering parking_lot_core, signal-hook(-registry),
  winapi*, windows-sys/-targets/_*, crossterm_winapi, serde_core,
  zmij). Curated-source names that don't match a transitive dep are
  silently dropped (the preset intentionally lists more than any one
  project will use); user-typed `--external NAME` typos still warn.
  All three combine additively with the BFS-with-cut-points algorithm.
- `--external-deep`: extends the cut to the entire transitive cone
  of every explicit external. Vendored crates that reference a
  newly-cut transitive get auto-promoted to a "Required" external
  in the banner — the user must list them in their Cargo.toml. See
  "When --external causes type duplication" below.
- Banner has up-to-three sections: unvendorable / user-excluded /
  required (auto-promoted via `--external-deep`).
- End-to-end via `cargo build` covered by tests for both flag
  combinations.

**Designed but removed before shipping**: `--vendor-extras` and the
orphan auto-promotion logic. They turn out to never fire in practice —
see "Design correction" below.

## Goal

Let users surgically exclude specific deps from vendoring without giving
up on the rest. Use cases:

- "Vendor my crate + small deps; keep `tokio` external because it's huge
  and I'll get it from cargo anyway."
- "Vendor what you can; I know `serde` is unvendorable (proc-macro),
  acknowledge that and keep it external rather than refusing the whole
  flatten."
- "Smaller output for sharing; I'll provide the few large ambient
  crates myself."

This sits alongside the planned V4 (`--vendor-allow-external`), which is
the global "auto-skip all unvendorables" mode. `--external <NAME>` is the
surgical, per-dep version. They compose: `--external` is for explicit
user choice; V4 is for "just deal with whatever you can't handle."

## CLI surface

Three flags involved:

```
--external <NAME>     Skip this dep when vendoring; keep external.
                      Repeatable. Matches by crate name (all versions).

--vendor-orphans      For transitive deps that the BFS would otherwise
                      auto-promote to "user provides" (because they're
                      cut by an --external but still referenced by a
                      vendored crate), vendor them instead. Trades a
                      larger output for a simpler Cargo.toml — the
                      user only needs to list the deps they explicitly
                      `--external`'d, not the auto-promoted set.

--vendor-report       Print the dep classification report and exit.
                      Already exists; this plan extends it to include
                      the externals breakdown when `--external` is
                      also passed (vendored / auto-promoted /
                      dropped-orphan / unvendorable).
```

clap:

```rust
#[arg(long = "external", value_name = "NAME", action = clap::ArgAction::Append)]
external: Vec<String>,

#[arg(long, requires = "external")]
vendor_orphans: bool,
```

`--external tokio --external serde --external log` is valid.
`--vendor-orphans` is meaningless without `--external` (clap enforces).

Mutually exclusive only with `--tree` and `--vendor-report` (those don't
write source output). Composes with `--vendor`, `--minify`, `--fmt`,
`--out`, `--stdout`, `--check`.

## The trade-off the flags expose

Three modes for handling the *referenced orphan* case (a vendored crate
needs a dep that was cut by `--external`):

| Mode | Output size | User's Cargo.toml | When |
|---|---|---|---|
| Default (auto-promote) | Smaller | One entry per cut dep + per orphan | Sharing as a small file is the priority |
| `--vendor-orphans` | Larger | One entry per cut dep only | Simple build is the priority |
| `--vendor-report` | (no output) | Read-only analysis | Decide what to do |

The flags don't fight each other — pick whichever matches what the user
cares about more.

### Concrete example

```
user's code:        uses A, uses C
A's deps:           bytes, mio
C's deps:           bytes, log
user's Cargo.toml:  A = "1", C = "1"

Run with: --external A
```

- **Default (auto-promote)**:
  Vendored: C, log
  Auto-promoted (must be in user Cargo.toml): A, bytes
  Dropped orphans: mio
  → User Cargo.toml: `A = "1"`, `bytes = "1"`. Two extra deps.

- **`--vendor-orphans`**:
  Vendored: C, log, bytes
  External (must be in user Cargo.toml): A
  Dropped orphans: mio
  → User Cargo.toml: `A = "1"`. Just the one external.
  → Output is ~bytes-source-size larger.

- **`--vendor-report --external A`**:
  Prints both columns of the above without touching the filesystem,
  so the user can pick.

## Behavior in two parts

### 1. Reachability with externals as cut points

When applying `--external A`, we **don't follow A's edges** during the
dep-graph walk. A's transitives are dropped from the "needed" set unless
some other vendored dep also reaches them.

```
needed = {}
queue = [user_crate]
while queue:
    cur = queue.pop()
    for d in normal_deps(cur):
        if d in user_externals:
            continue                  # cut — don't recurse
        if d in needed:
            continue
        needed.add(d)
        queue.push(d)
```

Concretely:

```
user → A (--external) → B → D
user → C → E
```

Walk: start at `user`. Visit `A` — it's in user_externals, cut. Don't
follow A's edges; B and D are NOT added. Visit `C`, add to needed.
Follow C's edges, visit E, add to needed. Done.

Result: `needed = {C, E}`. A, B, D are skipped — A explicitly, B and D
because they're orphans of A's exclusion.

### 2. Handling orphans-still-referenced (the trade-off lives here)

The above algorithm produces `needed`. But it's possible that:

```
user → A (--external) → B
user → C → B
```

Here C is vendored. C's source has `use B::...`. After we vendor C
(rewriting `use crate::B::...` per V1's path rewriter), the flat file
references `crate::B::...` — which doesn't exist because B isn't
vendored.

Two choices for handling this, picked by `--vendor-orphans`:

**Default — auto-promote B to "user provides"**:

```
external_must_provide = user_externals.clone()
for d in needed:
    for sub in normal_deps(d):
        if sub in needed: continue              # will be vendored
        if sub in external_must_provide: continue
        external_must_provide.add(sub)
```

Banner tells the user: put both A and B in Cargo.toml.

**With `--vendor-orphans` — vendor B alongside C**:

```
for d in needed:
    for sub in normal_deps(d):
        if sub in needed: continue              # already in
        if sub in user_externals: continue      # explicitly cut, leave it
        # Was cut as orphan but still referenced — pull it back in
        if is_vendorable(sub):
            needed.add(sub)
            queue.push(sub)                     # might pull more transitives
        else:
            external_must_provide.add(sub)      # can't vendor; user provides
```

Banner tells the user: put only A in Cargo.toml. (B is vendored, even
though A is external.)

In `--vendor-orphans` mode, the new BFS continuation can itself reach
new orphans of A's tree. They go through the same logic — vendorable
ones get vendored, unvendorable ones go to `external_must_provide`.
The result: A's vendorable transitive closure (that's still
referenced) is vendored; A and any unvendorable transitives stay
external.

### Why use the dep graph and not source-level analysis

Cargo metadata's edges are conservative: a dep is listed in another
dep's `[dependencies]` only when it's referenced from that dep's source.
So if C lists B as a dep, C uses B. We don't need to re-derive that by
parsing C's source. (False positives — C lists B but never uses it —
are rare and harmless: we'd just ask the user to provide B for nothing.
They'd notice it's not actually used.)

## Behavior with V1's path rewriter

Today, V1 rewrites `crate::*` → `crate::<dep>::*` inside vendored mods.
That rewrite assumes `<dep>` is a sibling mod inside our flat file's
crate root. It DOESN'T touch `use otherdep::...` paths in vendored
sources — those resolve naturally:

- If `otherdep` is also vendored as a sibling mod → 2018+ absolute use
  paths find the sibling mod. ✅
- If `otherdep` is in the extern prelude (because user has it in their
  Cargo.toml) → 2018+ absolute use paths find it via extern prelude. ✅

So vendored C's `use B::Foo` works whether B is a sibling vendored mod
or in the extern prelude — no path rewriting needed for the external
case. The flat file's textual content is identical; what changes is
whether `mod B { ... }` exists at the crate root.

This is convenient: implementing `--external` doesn't need any new
source-rewriting logic. We just don't emit the `mod B { ... }` block.

## Banner format

Up to four sections, in order. Only present if non-empty.

**Default mode (no `--vendor-orphans`)**:

```
// External requirements (could not vendor):
//   serde_derive 1.0.225 — proc-macro
//   ring 0.17.0 — has build script + links openssl

// Excluded by --external (you provide via Cargo.toml):
//   tokio 1.40

// Required by vendored deps (also add to your Cargo.toml):
//   bytes 1.10  (used by vendored `hyper`)
//   futures-core 0.3  (used by vendored `tower`)

// To compile:
//   1. Create a Cargo.toml with all of the above as direct dependencies.
//   2. Place this file at src/main.rs (or src/lib.rs).
//   3. cargo build.
```

**With `--vendor-orphans`**:

```
// External requirements (could not vendor):
//   serde_derive 1.0.225 — proc-macro

// Excluded by --external (you provide via Cargo.toml):
//   tokio 1.40

// Vendored as transitives (kept in this file because --vendor-orphans):
//   bytes 1.10  (would otherwise be required from your Cargo.toml)
//   futures-core 0.3
//   hashbrown 0.14

// To compile:
//   1. Create a Cargo.toml with the External + Excluded items only.
//   2. Place this file at src/main.rs (or src/lib.rs).
//   3. cargo build.
```

The third section is the one that flips between modes — auto-promoted
("add these too") vs vendored-as-transitives ("we kept these in the
file for you").

## Implementation steps

**1. Extend `VendorOptions`** (`src/vendor.rs`):

```rust
pub struct VendorOptions {
    pub strict: bool,
    pub external: HashSet<String>,
    /// If true, vendor orphan transitives that are still referenced by
    /// other vendored deps instead of auto-promoting them to the
    /// "user provides" list.
    pub vendor_orphans: bool,
}
```

**2. Replace the simple-loop in `vendor_package` with the two-pass algorithm:**

```rust
fn determine_targets(
    metadata: &Metadata,
    user_externals: &HashSet<String>,
) -> (Vec<DepEntry>, Vec<DepEntry>, Vec<DepEntry>) {
    // Returns (vendored_targets, user_externals, auto_promoted_externals)
    let needed = bfs_with_external_cuts(metadata, user_externals);
    let auto_promoted = orphan_pass(metadata, &needed, user_externals);
    // ... build DepEntry vecs
}
```

The existing per-dep classification + vendoring loop runs only over
`vendored_targets`. The other two vecs go into `pkg.external` with
distinguishing reasons.

**3. Wire into CLI** (`src/main.rs`):

```rust
#[arg(long = "external", value_name = "NAME", action = clap::ArgAction::Append)]
external: Vec<String>,

#[arg(long, requires = "external")]
vendor_orphans: bool,
```

```rust
let opts = VendorOptions {
    strict: true,
    external: args.external.iter().cloned().collect(),
    vendor_orphans: args.vendor_orphans,
};
```

**3a. Extend `--vendor-report`** (`src/vendor.rs::report` + its
`Display` impl): when called with `external` non-empty, print the
externals breakdown alongside the existing classification report.
Show what would happen in BOTH modes (default and `--vendor-orphans`)
so the user can compare and pick — that's the "explain" path.

The new sections in the report output:

```
With --external tokio:

Mode: default (auto-promote orphans)
  Vendored:                C, log
  Excluded by --external:  tokio
  Auto-promoted:           bytes  (used by vendored `C`)
  Dropped orphans:         mio
  → Cargo.toml needs:      tokio, bytes
  → Estimated output size: ~12 KB

Mode: --vendor-orphans
  Vendored:                C, log, bytes
  Excluded by --external:  tokio
  Dropped orphans:         mio
  → Cargo.toml needs:      tokio
  → Estimated output size: ~28 KB
```

The size estimates use cargo metadata's per-package `manifest_path`
+ `wc -c` on the lib's source as a rough proxy. Approximate, not
authoritative.

**4. Update banner writer** to render the three sections distinctly.
DepEntry's `classification` reason string carries the distinction:

- `Classification::Unvendorable(reasons)` — existing case
- `"explicitly excluded via --external"` — user-specified
- `"transitive of <dep> — required by vendored crates"` — auto-promoted

Or — cleaner — extend `DepEntry` with an explicit `ExternalReason` enum:

```rust
pub enum ExternalReason {
    Unvendorable(Vec<String>),
    UserExcluded,
    AutoPromoted { because: Vec<String> },  // names of vendored deps that need it
}
```

That's better. Migration: `Classification::Unvendorable` becomes one
variant of `ExternalReason`. Or keep `Classification` and add a parallel
`exclusion_reason: Option<ExternalReason>` field. Pick whichever feels
cleanest at code-time.

**5. Validation: warn on unknown names.** If `--external bogus_name`
matches no dep in the resolved graph, emit a `tracing::warn!`. Don't
error — they might've spelled the package name differently than they
think (`serde_json` vs `serde-json`).

**6. Validation: refuse to externalize the user's own crate name.**
Doesn't make sense; warn and ignore.

## Edge cases

- **Two versions of the same crate, one excluded.** Name-only matching
  excludes both versions. Probably what the user wants. Version-precise
  control (`--external foo@0.7`) is a future flag.

- **Skipping a dev dep or build dep.** We exclude both from the graph
  walk anyway (they're never normal-kind), so `--external` against them
  is a no-op (and triggers the unknown-name warning).

- **Skipping every dep.** Result is an unvendored flat file — same as
  not passing `--vendor` at all (well, with a slightly different banner).
  Fine; it's a corner of the design space, not a bug.

- **An auto-promoted dep is also a proc-macro/build-script crate.** That
  was always going to be unvendorable; user externalization avoids the
  refusal. Still goes in the "auto-promoted" section.

- **Cycles in the dep graph.** cargo doesn't allow them at the dep
  level; not a concern.

- **A user's own crate uses something that isn't in their direct deps
  but is in a transitive.** Compilation would fail today (extern prelude
  doesn't have transitives). Not a `--external` issue.

## Test plan

Eleven tests, all in `tests/integration.rs`:

1. `external_keeps_dep_out_of_vendored_set` — synthesize a path-dep,
   pass `--external pure_dep`, verify `pkg.vendored` is empty and
   `pkg.external` lists `pure_dep` with reason `UserExcluded`.

2. `external_overrides_unvendorable_refusal` — synthesize a dep with a
   build script, pass `--external buildy`, verify success (no refusal).

3. `external_unknown_name_warns_but_succeeds` — pass `--external
   nonexistent`, verify success and a warning.

4. `external_does_not_emit_mod_block_for_skipped_dep` — render the
   assembled output, verify no `mod pure_dep { ... }` appears.

5. `external_orphan_dep_is_dropped_from_output` — synthesize
   `user → A → B` where user code only references A. Pass `--external A`.
   Verify `pkg.vendored` is empty (B was an orphan of A's exclusion);
   `pkg.external` lists A only.

6. `external_orphan_still_used_by_other_vendored_dep_gets_promoted` —
   default mode: synthesize `user → A → B` and `user → C → B`.
   `--external A`. Verify `pkg.vendored = {C}`, `pkg.external` lists
   both A (UserExcluded) and B (AutoPromoted because C uses it).

7. `vendor_orphans_keeps_orphans_in_output` — same fixture as #6, but
   pass `--external A --vendor-orphans`. Verify `pkg.vendored = {C, B}`
   and `pkg.external` lists A only.

8. `vendor_orphans_drops_unreferenced_orphans` — synthesize
   `user → A → B` (B unreferenced from anything else). Pass
   `--external A --vendor-orphans`. Verify B is still dropped (the
   flag only pulls back referenced orphans).

9. `vendor_orphans_with_unvendorable_orphan` — synthesize
   `user → A (--external) → B (proc-macro)`, with C also referencing B.
   Pass `--external A --vendor-orphans`. Verify B is in
   `pkg.external` (auto-promoted, since unvendorable can't be vendored
   even with the flag).

10. `external_banner_distinguishes_modes` — render banners for both
    modes on the same fixture, verify section headings differ as
    documented.

11. `vendor_report_explains_externals` — pass `--external A
    --vendor-report` against a fixture, verify both mode columns
    appear in the printed report.

12. `external_end_to_end_via_cargo` — synthesize user crate + path-dep,
    vendor with `--external pure_dep`, write flat output as
    `src/main.rs` in a new throwaway crate whose Cargo.toml lists
    `pure_dep` as a path dep, run `cargo build`, verify it compiles.
    (Tests the actual "user provides via cargo" workflow.)

Plus 2 unit tests in `vendor.rs`: one for the default BFS-with-cut-points
algorithm, one for the `--vendor-orphans` continuation.

## Phasing

Single PR. ~150 lines of code (BFS + the orphan continuation under
`--vendor-orphans` + `ExternalReason` enum + extended report renderer)
+ ~350 lines of tests. ~1 day's work, including the cargo-based end-
to-end test (which needs a running cargo, not just rustc) and the
extended `--vendor-report` rendering.

## Where this fits

Orthogonal to the rest of the roadmap. Doesn't need to wait for or
block V4. If V4 lands later, the two compose cleanly:
`--vendor-allow-external` becomes "treat all unvendorables as if the
user passed `--external` for each."

Worth backporting a paragraph into `VENDORING.md`'s open-questions
section so the design choice is documented as resolved, with a pointer
to this file.

## When `--external` causes type duplication (and how to fix it)

Consider this dep graph:

```
user → A → B → C → C_core
user → A → C_core           (A also depends on C_core directly)
```

User passes `--external C`. The BFS:

```
seed = {A}                  (user's direct deps)
A is not external → needed. Walk A.deps = {B, C_core}.
B is not external → needed. Walk B.deps = {C}.
C is in external → cut.
C_core is not external → needed. (Reached via A.)
result: needed = {A, B, C_core}, external = {C}
```

C_core gets vendored — because vendored A directly depends on it. The
BFS reaches it through A before ever encountering the cut at C. This is
the BFS doing the right thing structurally, but it might not be what
you want at runtime: now there are *two* copies of C_core in the
final binary — the one in the flat file (used by vendored A) and the
one cargo pulls in transitively via external C.

If A's API surface returns C_core types and your code also touches
external C, you'll hit type-mismatch errors because rustc treats
vendored-`C_core::Bar` and cargo-provided-`C_core::Bar` as different
nominal types.

### Workaround 1: name the transitive too

Pass C_core as external as well:

```sh
cargo flatten --vendor --external C --external C_core
```

The BFS now cuts at both. A's `use C_core::Foo` becomes an
extern-prelude reference, which resolves to the cargo-provided
C_core. **You'd need to add `C_core` to your Cargo.toml** as a direct
dep too, because Rust's extern prelude only contains *direct* deps,
not transitives.

### Workaround 2: `--external-deep`

Same effect, but automatic — externalises the whole transitive cone
of every explicit external:

```sh
cargo flatten --vendor --external C --external-deep
```

Now the BFS cuts at C *and* everything reachable from C (including
C_core). Vendored A's `use C_core::Foo` becomes a reference to a
crate the BFS removed. The auto-promote pass detects this: any cut
transitive that's still referenced by a vendored crate goes into the
banner's "Required by vendored deps" section, so the user knows to
add it to their Cargo.toml alongside the explicit externals.

The trade-off:
- Without `--external-deep`: simpler Cargo.toml (just `C`); risk of
  duplicated transitives and type-mismatch errors at API boundaries.
- With `--external-deep`: more Cargo.toml entries (`C` + auto-promoted
  transitives); single shared instance at runtime; smaller flat
  output.

Pick based on whether your code traverses the boundary between the
external and the vendored side.

## Design correction

The original plan (above) included two-mode handling for "orphan
transitives still referenced by other vendored crates": auto-promote
to a "user must provide" list (default) or vendor them anyway
(`--vendor-extras`). This was based on the worry that vendored crate
`C` might `use B::...` when `B` was cut from the BFS by `--external A`.

While implementing the algorithm and writing tests, the first
auto-promote test failed with a real surprise: the case it was meant
to test never actually arises.

**Why it doesn't fire**: cargo metadata's resolver graph faithfully
captures every `[dependencies]` entry. If vendored crate C uses B in
its source, then B is in C's `[dependencies]`, so cargo metadata has
the edge C → B. The BFS therefore reaches B through C, and B ends up
in `to_vendor` regardless of how many *other* paths to B were cut by
`--external`.

For the auto-promote case to fire, you'd need a vendored crate to
reference a dep that isn't in cargo metadata's edges — which only
happens if cargo metadata has bugs (unlikely) or if we vendor crates
not represented in the graph (we don't). In every realistic scenario,
the BFS handles it correctly: cut what's cut, vendor what's reachable.

**What we kept**:

- The BFS-with-cut-points algorithm itself (works correctly).
- `ExternalReason::Required { because }` variant in the public API.

**What we removed**:

- `--vendor-extras` CLI flag (the "vendor orphans instead of
  promoting" mode that was meant to pair with auto-promote).
- The orphan-handling loop that ran *unconditionally* after BFS
  scanning vendored deps for off-graph references.
- Tests that asserted that auto-promote always happened.

### Update: auto-promote came back via `--external-deep`

Right after writing this correction we found a real, common case
where auto-promote IS the right behaviour: when the user opts into
externalising more than they explicitly named (via `--external-deep`).
There, the cut moves *upstream* of references that vendored crates
make, so we genuinely have vendored-crate-references-cut-deps. The
auto-promote loop is back, but conditional on `expanded_externals`
having members the user didn't explicitly name. The banner's
"Required by vendored deps" section is back too.

So the only thing actually retired is the *unconditional* orphan
loop and `--vendor-extras`. The `Required` variant + the loop are
both alive and used by the `--external-deep` path.

## Open questions

- **Spell the flag `--external` vs `--no-vendor` vs `--keep-extern`?**
  `--external` matches the existing banner section title and is shortest.
  Going with it.

- **Spell `--vendor-orphans` vs `--include-transitives` vs
  `--no-auto-promote`?** Going with `--vendor-orphans` because it's
  action-oriented (verb-first) and pairs naturally with `--external`.
  Open to bikeshed.

- **Glob support: `--external "tokio*"`?** Not now. Real users will hit
  this if they have many small related crates they want to skip; add as
  follow-up if asked.

- **Emit a Cargo.toml stub?** Would help the workflow but writing files
  is currently out-of-scope per VENDORING.md's non-goals. Could be a
  separate `--emit-manifest` flag later. For now the banner says what
  the user needs.

- **Should auto-promoted externals be opt-out (vs the new opt-in via
  `--vendor-orphans`)?** Maybe a `--external-strict` that errors on
  any auto-promotion, forcing the user to be explicit about every
  external. Probably not worth it for v1 — the banner already lists
  the auto-promoted set so the user can see what they need.

- **Estimated output sizes in the explain mode** — using cargo's
  on-disk sources as a proxy is rough (we don't account for cfg
  deletion or minification). Worth flagging in the report output as
  approximate, not authoritative.
