# Tree-shaking — investigated and declined

This doc records the design investigation into source-level dead-code
elimination (DCE) for cargo-flatten's vendored output, why we chose
not to ship it, and what we shipped instead.

## TL;DR

JavaScript-bundler-style tree-shaking is **not feasible for Rust source
code at the cost level it would take to implement responsibly**. Rust's
runtime semantics — trait dispatch, blanket impls, derives, the `?`
operator's `From` traversal, `Drop`, format-args macros — defeat the
lexical reachability analysis that JS tools rely on. Both candidate
approaches (custom analyzer / rustc-as-oracle) yield single-digit-percent
size wins on top of what V2's cfg deletion already achieves, at the
cost of substantial correctness risk.

Shipped instead:
- `--minify` (strip comments + collapse blank lines): zero correctness
  risk, ~30–40 % size reduction on vendored output.
- Banner line documenting `rustc -C lto=fat -C opt-level=z` for
  binary-size DCE — done correctly by the linker.

## Why JS bundlers can do this and Rust can't

Tree-shaking in Rollup / esbuild / Webpack works because JS is honest
about what touches what:

- Imports/exports are explicit (`import { foo } from './bar'`).
- Methods are dynamic dispatch on objects, not language-level trait
  resolution.
- No type-directed metaprogramming.
- Macros aren't a thing.
- Most "is this used?" questions are answerable by reading the source.

Rust violates almost every one of those in ways that matter:

- **Trait method dispatch isn't visible from source.** `s.len()` on a
  `String` calls `str::len` via `Deref<Target=str>`. A lexical
  analyzer sees `.len()` and either guesses wrong or keeps every
  `len` in scope.
- **`?` walks the `From` impl graph.** None of those impls are
  lexically referenced. Delete one and `?` stops typechecking 80 KB
  into the file.
- **`format!("{}", x)` resolves to `<X as Display>::fmt`** through
  `format_args!` expansion. The expansion is invisible to a syn-based
  analyzer; it sees the un-expanded macro and no path to `Display`.
  So it deletes `impl Display for X`.
- **Blanket impls drive coherence.** `impl<T: Display> ToString for T`
  — delete it and every `.to_string()` breaks. Worse: deleting an
  "unreferenced" impl can change *which* impl `foo.bar()` resolves to
  elsewhere. Silent miscompilation.
- **Derives expand to typed code we can't see.** `#[derive(Default)]`
  requires `Default` for every field type. `#[derive(Deserialize)]`
  names every field type's `Deserialize` impl in the macro expansion.
  The analyzer either special-cases every popular derive crate (it
  won't) or keeps every related impl whenever any derive is reachable
  (defeats the point).
- **`Drop`, `Deref`, default methods on traits, supertraits,
  associated types, UFCS on generics, trait objects, `#[fundamental]`,
  `#[lang]` items.** Each is its own way for "looks unused" to
  actually be load-bearing.

The pattern: **lexical reachability is a strict subset of semantic
reachability in Rust**, and the gap is wide enough to crash the build.

## Approach A — custom syn-based reachability analyzer

What it would take:

1. Build a symbol table mapping `(scope, ident) → Item` across the
   whole flat file.
2. Resolve `use` statements (including globs `use foo::*`) to populate
   import aliases.
3. Walk every Item's body collecting referenced paths/idents; resolve
   each via the symbol table.
4. Seed: `main` (bin) or all `pub` items at crate root (lib).
5. BFS through reference edges.
6. Conservative rules to handle the un-resolvable cases.
7. Delete unmarked items.

### Why it's not feasible

You're reimplementing chunks of rustc's `resolve` and `select` passes.
Specific landmines:

- **Glob imports.** `use foo::*;` means "every public item of `foo`,
  transitively, modulo shadowing." You'd need to fully enumerate
  `foo`'s pub surface, then unshadow against local names, then
  resolve every bare ident in scope against this multi-source table.
- **Macro hygiene.** Idents created inside a `macro_rules!` arm have
  a different `SyntaxContext` than idents from the call site. Two
  `let x` in the same scope can be distinct. Your symbol table needs
  to model spans/contexts or you'll resolve the wrong `x`.
- **`crate::` rewriting interaction.** V1's vendoring pass rewrote
  `crate::*` to `crate::<dep>::*` inside vendored mods. The shaker
  now sees both forms in the same file.
- **Associated-type projections** (`<T as Trait>::Output`) — without
  monomorphization, you'd conservatively keep every `Trait::Output`
  for every `impl Trait for _` in scope.
- **2015 vs 2018 path semantics.** Vendored crates can be edition-2015
  (paths are relative). V2 picks "highest edition seen" and emits the
  whole flat file at one edition. Resolving 2015-flavored paths in a
  2018+ file requires per-mod edition tracking that doesn't exist
  post-flatten.

The conservative rules needed to handle what you can't resolve collapse
the analysis into "keep everything":

- Keep all `impl` blocks where the type is reachable → keeps most of
  every dep.
- Keep all macros invoked anywhere → keeps macro definitions and
  everything their token bodies might reference.
- Keep all default trait methods → keeps `Iterator::collect` and
  friends.
- Keep all `From` impls because of `?` → keeps the entire error type
  graph.
- Keep all `Display`/`Debug` impls because of `format!` → keeps
  everything ever printed.

After applying every conservative rule, the actual unique deletions
amount to: a few private helper functions inside vendored deps,
`#[doc(hidden)]` items with zero lexical references. Maybe 5–10 % of
the flat file. **Months of engineering for single-digit savings, with
constant correctness risk.**

## Approach B — rustc-as-oracle iterative DCE

The idea: rewrite vendored `pub` to `pub(crate)`, run
`rustc -W dead_code -Dwarnings`, parse warnings, remove items,
iterate until stable.

### Why it's not feasible either

- **`pub → pub(crate)` is not semantically neutral.** Coherence
  (orphan rule), `#[fundamental]` selection, and `pub use`-of-private-item
  errors all change. Some vendored deps will stop compiling after the
  rewrite.
- **`dead_code` lint coverage is uneven.** It does NOT fire on: trait
  impls (almost ever), `macro_rules!` macros, type aliases. Those are
  exactly the things bloating real vendored output.
- **Convergence isn't guaranteed.** Removing item A can change
  construction-heuristic results for item B; you can oscillate or
  converge to different fixed points depending on iteration order.
- **Performance.** A 200 KB flat file with vendored deps takes
  `rustc --emit=metadata` several seconds. 5–20 iterations puts you in
  tens-of-seconds-per-flatten territory.
- **Lost user warnings.** Forcing `-Dwarnings -W dead_code` fights any
  other warning the user's code emits.

## Realistic benefit estimate

Honest accounting of where the bytes go in a typical V3-vendored output:

| Source | Roughly | Already-handled? |
|---|---|---|
| Feature-gated code | ~50–70 % of the original dep | ✅ V2 deletes it |
| Doc comments | ~15–25 % | Easy to strip |
| Comments + whitespace | ~10–15 % | Easy to strip |
| Trait impls (`Display`, `Debug`, `Error`, `From`) | ~5–15 % | Almost all needed via dispatch |
| Internal helpers actually unused by the entry | ~3–8 % | What tree-shaking would remove |

So the addressable surface for tree-shaking is single-digit percent of
an output that's already been cut down by V2 and is intended to be
human-readable. Versus a pile of correctness risk and weeks of work.

For the binary-size case — which is what users actually care about for
runtime — `rustc -C lto=fat -C opt-level=z` does textbook DCE at link
time. The unused items disappear from the binary regardless of whether
they appear in the source. That's already shipped, by people whose job
is to get this right.

## Failure mode that closes the case

The worst failure isn't "doesn't compile" (annoying but fixable). It's
silent miscompilation from a deleted blanket impl changing method
resolution: code compiles, runs, returns wrong answers. The flat output
is meant to be inspectable and shareable; once tree-shaking has been
wrong even once, every other use becomes suspect.

## What we shipped instead

### `--minify` flag

Strips line comments (`//`, `///`, `//!`), block comments (`/* */`,
including nested), and collapses runs of 3+ blank lines down to 2.
Preserves string/char/raw-string literals correctly.

Implementation: a small byte-level lexer in `src/minify.rs`. No syn
involved (deliberate — avoids losing original source structure).

Risk: ~zero. Comments and blank lines don't affect compilation. Macro
token spacing is preserved (we don't touch within-line whitespace).

Win: 30–40 % on vendored outputs. Well-documented deps spend a lot of
their bytes on doc comments.

### Banner line for binary-size DCE

The `--vendor` banner now includes:

```
// For minimum binary size: rustc -C lto=fat -C opt-level=z <this-file>.rs
```

This is the actually-correct DCE. Costs nothing to mention, points
users at the right tool.

## Possible follow-ups (low-risk only)

If we ever feel the itch to do more, the narrowest provably-safe
deletion would be: `#[doc(hidden)] pub fn` (or `pub struct`,
`pub const`) **inside vendored mods only** with **all** of:

- Zero lexical references in the entire flat file
- No `impl` block on it (if struct)
- No macro body that mentions it
- No generic parameter (no UFCS surprise)

This is the long tail of `__private_helper`-style items vendored crates
expose. Probably worth another 2–5 %. Don't call this "tree-shaking";
call it "internal helper stripping" so expectations stay honest.

Anything beyond that — actual reachability analysis — should not ship
unless the entire investigation above is overturned by new
information. If it ever does, run it gated behind an opt-in flag with
a `--shake-bisect` mode for when (not if) it removes something it
shouldn't.

## Out of scope

These came up while investigating, deliberately not pursued:

- **Source-level reachability with name resolution.** See Approach A.
- **rustc-as-oracle iteration.** See Approach B.
- **`-C lto` / `-C opt-level=z` invocation by cargo-flatten.** That's
  the user's compiler invocation, not ours; we just point at it.
- **Whitespace-level minification (collapsing all whitespace inside
  lines).** Marginal additional savings, hurts inspectability, easy to
  break across edge cases (macro_rules! token spacing especially).
