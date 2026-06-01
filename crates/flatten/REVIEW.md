# cargo-flatten codebase review

Snapshot review covering `src/`, `expand/src/main.rs`, and the test suite as
of mid-2026. Three independent deep reads (one per major file group) plus
three devil's-advocate architecture reviews. This document is the
consolidated reference; individual action items live in `ROADMAP.md`.

Findings are sorted by impact, then mechanically by location. Verified
against actual code where the claim was non-obvious — a few subagent
findings turned out to be slightly off and are noted as such.

---

## A. Verified bugs

### A1. `find_item_end` mishandles char literals containing braces
**File:** `expand/src/main.rs:825-898`

Empirical reproduction: input `fn brace_in_char() { let c = '}'; let _ = 1; }` returns
position 31 (just past the char literal's `}`) instead of 46 (the function
body's closing brace). The char-literal arm at line 866 unconditionally
advances 1 byte; should peek ahead to detect the closing `'`. Same shape:
`'"'`, `'\u{007D}'`.

Real-world impact: any vendored crate with `'}'` or `'"'` char literals in a
function annotated by a third-party Attr macro produces duplicate-item
errors (the original function stays, the macro expansion is appended).

**Fix here, replace with syn long-term.** See B-Long-term.

### A2. `find_item_end` raw-string handling is fragile
**File:** `expand/src/main.rs:825-898`

`r#"contains "; }"#` — the inner `"` ends the byte-walker's string mode
early, then `}` decrements brace_depth and returns prematurely. Also
unhandled: `b"..."`, `c"..."`, `b'}'`, nested block comments
`/* /* */ */`. The function's doc claims to "skip string-literal content"
but doesn't recognize raw-string `r#`/`b`/`c` prefixes.

Same fix as A1.

### A3. `apply_edits`-style logic is duplicated 5x with divergent overlap policies
**Files:**
- `src/vendor.rs:735` (`rewrite_absolute_sibling_paths`) — no overlap check
- `src/vendor.rs:880` (`scrub_unresolvable_sibling_reexports`) — no overlap check
- `src/vendor.rs:2175` (`expand_cfg_if`) — no overlap check
- `src/vendor.rs:2407` (`rewrite_for_vendoring`) — exact-equal-ranges dedup; misses partial overlap
- `expand/src/main.rs:1041` (`apply_edits`) — handles partial overlap, merges strips, prefers content over strip

Two policies for what's conceptually one operation. Future bugs land at the
seam between policies; the abstraction was never named.

### A4. `format_with_rustfmt` write-then-wait is the textbook deadlock
**File:** `src/main.rs:407-416`

`stdin.write_all(&input)` then `child.wait_with_output()` with both stdin
and stdout piped. For multi-MB input (vendored output is 100k+ LOC),
rustfmt's stdout pipe buffer (~64KB default) fills, rustfmt blocks on
stdout, parent blocks on stdin → deadlock. Hasn't been observed because no
existing test combines `--fmt` with vendored output.

Fix: spawn a thread to drain stdout/stderr while writing stdin.

### A5. `expand_include_macros` recursion is unbounded
**File:** `src/source_file.rs:444-505`

The function recursively calls itself for every `include!()` it expands; no
depth counter. The outer `from_file_with_root_inner` has `MAX_DEPTH=128`
but the include expander is independent. Cyclic includes blow the stack.

Fix: thread a `depth` parameter, error past `MAX_DEPTH`.

### A6. `scrub_unresolvable_sibling_reexports` only runs under `--expand`
**File:** `src/main.rs:225`

The post-pass scrub block is gated on `if args.expand`. But auto-
externalized proc-macros happen regardless of `--expand` — vendor.rs
detects proc-macros via `Classification::Unvendorable` and any downstream
`pub use crate::FOO::macro_name;` will then dangle even in plain
`--vendor` mode.

Fix: gate on `!pkg.auto_inlined_proc_macros.is_empty()` instead, and move
the assembly logic from `main.rs` into `vendor::scrub_assembled`.

### A7. `rewrite_unstable_panic_calls` has zero context awareness
**File:** `expand/src/main.rs:1087-1147`

Pure byte search for `::core::panicking::panic("…")`. Will rewrite inside
doc comments containing the literal text, inside string literals containing
the needle (e.g. error messages), and inside raw strings. Also doesn't
handle multi-arg `panicking::panic_fmt(args)` or `concat!()` arguments.

The current implementation works for the rapier2d case because pretty-
printed expansions don't typically embed those forms in documentation, but
the failure mode is silent.

### A8. `String::from_utf8(body).expect("UTF-8")` panic in scrub path
**File:** `src/main.rs:251`

The `--minify` path returns a structured error in the same situation
(`InvalidData`); the scrub path panics. `--expand` shells out to a
subprocess so non-UTF-8 output is at least conceivable.

---

## B. Likely bugs (worth verifying or fixing opportunistically)

### B1. `parse_concat_out_dir` rejects 3-arg form
**File:** `src/source_file.rs:554`

Strict 5-token shape misses `concat!(env!("OUT_DIR"), "/", "file.rs")` —
common when devs split slash from filename. Walk all string literals after
`env!(OUT_DIR)` and concatenate.

### B2. `dep_uses_out_dir_include` only scans lib.rs/main.rs
**File:** `src/vendor.rs:260`

A crate using `env!("OUT_DIR")` only in a submodule (e.g. `src/codegen.rs`)
is wrongly classified Unvendorable. Walk the dep's `src/**/*.rs`.

### B3. `collect_macro_use_externals` drops cfg-gated unconditionally
**File:** `src/vendor.rs:4015`

`#[cfg(feature = "rt")] #[macro_use] extern crate FOO;` is dropped even
when `feature = "rt"` is known true. Other cfg-aware passes use the cfg
evaluator; this one just calls `.any(|a| a.path().is_ident("cfg"))`.

### B4. Multi-version dep ordering is non-deterministic
**File:** `src/vendor.rs:1624`

`to_vendor_sorted.sort_by(|a, b| a.name.cmp(&b.name))` discards the version
tiebreaker that `report` provides. Multi-version blocker messages report
different "existing" vs "duplicate" versions across runs.

### B5. `pub(crate) use` re-exports missed in collision detection
**File:** `src/vendor.rs:4477`

`bump_in_items` only matches `Visibility::Public(_)`. A `pub(crate) use foo::Bar;`
adjacent to a `mod Bar` could trigger E0255/E0428 in vendored output.

### B6. `inlined_macro_names` silently swallows IO errors
**File:** `src/main.rs:246`

`if let Ok(src) = std::fs::read_to_string(&lib)` discards errors. If a
proc-macro crate uses `[lib].path` other than `src/lib.rs`, the scrub pass
is incomplete and the user sees no warning.

### B7. `swallow_strip_separators` over-swallow risk
**File:** `expand/src/main.rs:1152-1171`

Largely fixed in commit `2a1fa71` (gated on `is_full_attr` to skip the
swallow for `#[…]` strips). The remaining edge: derive-list strips inside
`#[derive(NotStdlib1, NotStdlib2)]` where both adjacent strips want to
swallow the same comma. Existing collision merge at line 1043 handles it,
but only when both are exact-equal-range strips.

### B8. Wrapper `let _ = std::fs::write(...)` silently drops captures
**File:** `expand/src/main.rs:254-260, 312`

Failed dump-dir writes never log. Downstream `cargo-flatten` then either
misses the file or uses the un-rewritten cargo-cache source.

---

## C. Stale comments

| File:Line | Issue |
|---|---|
| `src/vendor.rs:5-10` | Module doc still describes "Phase V0/V1" with V1 caveats ("no $crate, no #[macro_export]") — all handled now |
| `src/vendor.rs:1455-1457` | Comment says `--expand`/`--expand-deep` defer expansion; only `--expand-deep` defers. Shallow `--expand` runs at line 1481. |
| `src/vendor.rs:543-547` | Promises future detection of "which proc-macros were consumed" that hasn't landed; field is just always-all |
| `src/vendor.rs:1942-1943` | "V1 simplification still in V2: only handle deps using the default src/lib.rs" — V1/V2 markers no longer in active use; codebase is past V5b |
| `src/vendor.rs:2322-2330` | `rewrite_for_vendoring` doc says it refuses on `$crate` and `#[macro_export]` — both are handled now |
| `src/source_file.rs:548-553` | Doc comment for `escape_as_str_literal` is attached to `parse_concat_out_dir` (function inserted ahead of where the doc belongs) |
| `expand/src/main.rs:819-824` | `find_item_end` doc claims to handle string literals — only handles plain `"…"`, not `r#"…"#`/`b"…"`/`c"…"` |
| `expand/src/main.rs:1138` | "resume scanning one byte ahead" stale post-UTF-8 fix; code advances by `len_utf8()` |
| `src/main.rs:546-554` | Lint allow comment lists `dangerous_implicit_autorefs` as caused by allocator-api2; it now also fires elsewhere |
| `src/minify.rs:226+237` | `let _ = first_char_pos;` is dead-binding leftover from earlier draft |

---

## D. Simplifications (high leverage)

### D1. Extract one `apply_edits(src, edits)` helper
Five sites duplicate `sort_by_key + replace_range back-to-front`. Pick the
most general policy (handles partial overlaps with merge-or-prefer-content)
and use it everywhere. Cuts ~50 lines, eliminates the policy-divergence
risk noted in A3.

### D2. Extract `proc_macro_dep_names(report)` helper
**File:** `src/vendor.rs:1465-1488`

Same iterator chain runs three times in `vendor_package`; the third
occurrence at lines 1521-1523 is dead code (auto_externals already
populated by then).

### D3. Extract `add_sysroot_lib_env(cmd)` helper
**File:** `src/vendor.rs:506-521` and `:600-613`

Two verbatim copies of the 16-line `DYLD_LIBRARY_PATH` block.

### D4. Move scrub-pass logic from main.rs into vendor.rs
**Files:** `src/main.rs:225-258` → `src/vendor.rs`

main.rs is currently a participant in the vendoring algorithm: collecting
siblings_names, scanning auto_inlined_proc_macros sources, building
inlined_macro_names. All vendor-domain. New API:
`vendor::scrub_assembled(&body, &pkg) -> String`.

### D5. Delete dead bindings
`src/minify.rs:226+237` `let _ = first_char_pos;` — dead.

### D6. Replace `String::from_utf8(body).expect(...)` with structured error
**File:** `src/main.rs:251`

Match the `--minify` path's `InvalidData` error.

### D7. `top_level_item_names` and `collect_crate_root_item_names` overlap
Both walk `file.items` collecting identifier names with very similar
arms. Either compose, or share a helper.

### D8. Three "walk UseTree leaves" duplicates
`process_use_tree` (line 3587), `collect_sibling_use_rewrites` (line 3709),
`collect_edition_2015_bare_path_rewrites` (line 3900) all pre-walk
`UseTree` to find the leading ident. Extract
`fn use_tree_leading_ident(t: &UseTree) -> Option<&Ident>`.

---

## E. Architectural recommendations (from devil's-advocate review)

Three subagent reviews converged on the same priority order.

### E1. Extract `cfg.rs` first (~500 lines, low risk)
Lift the cfg-expression evaluator + `cfg_if!` expander out of vendor.rs
into `src/vendor/cfg.rs`. Zero behavioural risk (no shared state, no
ordering deps). Immediately unlocks unit tests for the most error-prone
subsystem (three-valued logic, partial-eval rendering, build-script cfg
merging) — currently only tested transitively through `vendor_package`.

### E2. Move `--expand-deep` plumbing into `src/vendor/expand.rs` (~400 lines)
Different domain (subprocess orchestration, env-var contracts, dump-dir
contract) from the syn AST work. Cleanly separable.

### E3. Don't trait-ify the 14 rewrite passes yet
The passes look uniform but each takes a different combination of
`(crate_name, siblings, aliases, crate_root_items, deleted, edits)`. A
`RewritePass` trait would degenerate into a god-context struct. The
implicit pass-ordering protocol (`deleted` populated by phases 1-3,
consulted by phases 4-14, mutated mid-chain by
`collect_builtin_attr_macro_call_rewrites`) is genuinely two-phase-commit.
Splitting it across files would make ordering bugs harder, not easier.

**Right time to split:** after the next 10 vendoring tests pass without
anyone touching `rewrite_for_vendoring`'s body. Algorithm is still
churning; monolith makes one-line ordering fixes obvious.

### E4. Hybrid byte-edit / AST-rewrite line
Byte-edit is correct for **identifier-level token swaps** (`crate` →
`crate::clap`, `SIBLING::Foo` → `crate::SIBLING::Foo`) — small,
non-overlapping, comment-preserving wins.

It's brittle for **whole-item structural changes** (deleting items,
replacing `extern crate FOO` with `pub(crate) use crate::FOO`, injecting
`pub use` statements). Pilot conversion of `collect_macro_export_rewrites`
to AST-mutate + `prettyplease::unparse` per-item would prove the model.

### E5. RUSTC_WRAPPER design — keep it; distribution remains unsolved
The `--expand-deep` design is the right call (cargo handles dep resolution,
extern flags, build scripts, target-conditional source). The
`Callbacks::after_expansion` + post-expansion AST walk + byte-edit-of-
original-source IS structurally better than `cargo expand`'s
pretty-print-everything approach.

**The unsolved problem:** distribution. Task #31 (Phase 5) is still
pending. `cargo install cargo-flatten-expand` will fail on most users'
machines because they lack `rustc-dev`. Need an installer that runs
`rustup component add rustc-dev llvm-tools-preview` and rebuilds the
expander against the current nightly toolchain.

Devil's-advocate strongest counter to keeping `--expand-deep`:
> "Half of `expand/src/main.rs` exists for transitive proc-macro handling
> (tainted-macro chain walk, `find_item_end`, `rewrite_unstable_panic_calls`).
> The crates that genuinely need `--expand-deep` are narrow (rapier2d /
> simba / paste). The maintenance bill compounds across every nightly
> forever."

We're keeping it for now (rapier2d is high-value coverage), but the
counter is on the table.

### E6. `ParseOptions` is at risk of becoming a grab-bag
**File:** `src/source_file.rs`

Two fields today (`rewrite_source`, `out_dir`); both are vendoring-specific
concerns leaking into the file-loading layer. Cleaner: make `SourceFile`
accept a `SourceLoader` trait where the vendoring loader is one impl that
internally does include expansion + rewrite + out_dir resolution. Less
invasive: document that any new `ParseOptions` field must be applicable to
the file-loading layer, not the vendoring pipeline.

### E7. main.rs orchestration belongs in a pipeline.rs / vendor.rs
`src/main.rs` lines 187-316 is the pipeline (vendor-or-flatten →
tree-or-body → fmt → minify → banner → write). CLI's job should be: parse
args, build a `PipelineRequest`, call `cargo_flatten::run(request)`, write
output. The scrub-pass-wired-into-main is direct evidence the boundary
is wrong.

---

## F. Missing tests (prioritized)

1. **`find_item_end` corner cases** as direct unit tests:
   - `'}'`, `'"'`, `'\u{7D}'` char literals
   - `r#"contains "; }"#` raw strings
   - `b"…"`, `c"…"`, `b'}'` byte/c-string literals
   - Nested block comments `/* /* */ */`
   - Item with no body: `pub struct Foo;`, `type T = U;`

2. **Cfg evaluator unit tests** — once `cfg.rs` is extracted: `cfg(all(True, X))`
   reduction, double-negation collapse, `Other` predicate verbatim,
   build-script `KEY="VAL"` form.

3. **`scrub_unresolvable_sibling_reexports`** — recently added; no direct tests:
   - Single-item `pub use crate::SIB::vector` → empty replacement
   - Multi-item with one removable: `pub use crate::SIB::{A, vector, C}`
     → `{A, C}`
   - Wildcard descent through `pub use base::*;` keeps `Allocator` etc.
   - `pub(crate)` and `pub(in path)` visibility preservation

4. **Round-trip parse of expand output** — feed `apply_edits` output through
   `syn::parse_file`, assert it parses. Fast smoke test, would catch many
   regressions.

5. **`expand_include_macros` cycle detection** — `a` → `b` → `a` should error
   with `MAX_DEPTH`, not blow the stack.

6. **`format_with_rustfmt` with 2 MB synthetic source** — surfaces the deadlock.

7. **`minify.rs`**: c-strings (`c"hello"`), string continuation (`"foo\<NL>bar"`),
   `\u{xxxx}` inside strings, `r###"…"###`.

8. **Plain `--vendor` (no `--expand`) on a project with auto-externalized
   proc-macros** — currently the scrub pass doesn't run; downstream may fail
   with `unresolved import`. Test would expose A6.

9. **`safe_inject_point` corner cases** — source starting with `//!`, source
   with `#![feature(...)]`, source with no inner attrs, source with no
   trailing `\n`.

10. **Multi-version dep ordering determinism** — two invocations on the same
    dep graph should produce the same blocker message order.

---

## G. Decisions not to take (yet)

| Decision | Reason |
|---|---|
| Trait-ify the 14 rewrite passes | Algorithm still churning; uniform shape is cosmetic; god-context risk |
| Drop `--expand-deep` to simplify expand binary | Rapier2d coverage is worth the maintenance cost (for now) |
| Migrate to pure AST-rewrite + `prettyplease` | Loses comment / blank-line / indentation fidelity that the project values |
| Replace `find_item_end` with syn parse | Adds dep to expand crate; fix the targeted bugs first, revisit later |
| Split `rewrite_for_vendoring`'s 14 passes across files | Premature; ordering is the contract and reads better in one place |

---

## H. Spot-check log

A few high-confidence agent claims I verified before publishing:

- `find_item_end` char-literal bug: **CONFIRMED** via standalone test
  (returns 31 vs expected 46 for `'}';` body)
- `find_item_end` raw-string bug: PARTIALLY confirmed (the simple
  `r#"contains } and ;"#` case actually works because of the order in which
  brace-counter and string-walker interact; the harder case
  `r#"contains "; }"#` was not run but follows from the algorithm)
- `format_with_rustfmt` deadlock shape: **CONFIRMED** by reading the source
  (write_all then wait_with_output with both pipes piped — textbook)
- `apply_edits` duplication: **CONFIRMED** via grep — 5 sites with
  divergent overlap policies
- 14 rewrite passes / 35 collect|walk functions: **CONFIRMED** via grep
  (35 functions, 26 take `deleted` arg)

Findings I did NOT verify (treated as plausible-claim, included in roadmap
as work-to-do):

- `parse_concat_out_dir` 3-arg-form rejection (likely)
- `collect_macro_use_externals` cfg-gating bug (read code; matches claim)
- `pub(crate) use` collision in `bump_in_items` (read code; matches claim)
- Multi-version determinism (read code; matches claim)

---

*Generated 2026-05-06. Action items split into ROADMAP.md.*
