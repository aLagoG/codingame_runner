# cargo-flatten roadmap

This file tracks every actionable item identified in
[`REVIEW.md`](./REVIEW.md) plus the original feature backlog. Three
buckets: shipped, pending, declined.

Items reference REVIEW section codes (A1, B2, etc.) where applicable.

---

## Shipped

### Pre-review baseline
- syn-based scanner replacing the regex (`99a9484`)
- Phase 1 — output separators, `<crate>.rs` filename, header banner,
  `--stdout`, package arg defaults to `.`, `cargo flatten` subcommand
  handling, `parse_package` returns a struct (`4e239c8`)
- Phase 2 — manifest awareness, `TargetSelector`, `--lib` / `--bin` /
  `--example` / `--test`, custom `[lib].path` honored (`3166057`)
- Phase 3 — `insta` snapshot tests, `--check` mode, self-flatten
  meta-test, crate-level rustdoc, README (`e3f7739`)
- `--no-banner`, `[package].default-run`, `#[path]` inside inline mod
  blocks, `impl AsRef<Path>` on public APIs (`5b949e5`)
- `--tree` CLI flag with `ModuleTreeNode` summary (`4755d3b`)
- miette diagnostics — typed `FlattenError` with source spans
- `--fmt` flag — pipe output through `rustfmt`, honoring source rustfmt.toml
- Recursion depth limit (`MAX_DEPTH = 128`) on the parser
- V4 vendoring with feature-gated cfgs, `$crate` rewrites,
  `#[macro_export]` lifting, `cfg_if!` expansion, build-script cfg capture
- `--external` / `--external-deep` / `--external-file` for opting out of vendoring
- `--minify` — strip comments, collapse blank lines (note: this also covers
  the original "Collapse blank lines" item)
- V5 `--expand` — inline third-party proc-macros via rustc-dev (user crate only)
- V5b `--expand-deep` — extend proc-macro inlining to vendored deps via
  RUSTC_WRAPPER pattern
- Real-world coverage: clap, serde+derive, rand, regex, glam, nalgebra,
  rapier2d (all compile to single-file flat output)
- Tainted macro_rules detection for `paste::item!`-via-macro_rules patterns
- Build-script `OUT_DIR` capture for `include!(concat!(env!("OUT_DIR"), …))`
- Final-output sibling re-export scrubbing
  (`scrub_unresolvable_sibling_reexports`)

### Review-driven (this batch)
- **Fix #1: `find_item_end` char-literal + raw-string handling** —
  proper sub-tokenisation for `'…'`, `r#"…"#`, `b"…"`, `c"…"`, nested
  block comments. 19 unit tests added. (REVIEW A1, A2)
- **Fix #2: `format_with_rustfmt` deadlock** — drain stdout/stderr in
  threads while writing stdin. Regression smoke test on 1.4MB input.
  (REVIEW A4)
- **Fix #3: `expand_include_macros` recursion bound** — thread `depth`
  through; error past `MAX_DEPTH=128`. Cycle-detection regression
  test. (REVIEW A5)
- **Fix #4: scrub-pass orchestration → `vendor::scrub_assembled`** —
  30 lines of vendor-domain logic moved out of main.rs into a single
  `vendor::scrub_assembled(body, &pkg)` call. IO/UTF-8 errors now log
  warns instead of silent drop / panic. (REVIEW A6, A8, B6, D4)
- **Fix #5: `apply_simple_edits` + `proc_macro_dep_names` helpers** —
  4 sites of duplicated `sort + replace_range back-to-front` collapsed
  into one helper in `src/edits.rs` with 6 unit tests. Dead duplicate
  `proc_macro_dep_names` iteration in `vendor_package` removed.
  (REVIEW A3, D1, D2)
- **Fix #6: extract `cfg.rs`** — 520 lines of cfg-evaluator + cfg_if
  expander moved out of vendor.rs into `src/cfg.rs`. 24 new unit tests
  (parse, eval, simplify, format, cfg_if). Bug caught in the move:
  `cfg_if::cfg_if!` (multi-segment path) was not matched by `is_ident`.
  vendor.rs shrinks 4821 → 4406 lines. (REVIEW E1)
- **Fix #7: stale-comment sweep** — V0/V1 phase markers, misplaced
  doc comments, defer-comment misleading on shallow vs deep expand,
  obsolete promise of "detect which proc-macros consumed" detection,
  dead `let first_char_pos` binding in minify.rs. (REVIEW C)

### Coverage-probe-driven (this batch)
- **Coverage probe: tokio/axum/ratatui/bytemuck** — six new bug
  classes identified, all but the macro_rules-wrapped-mod issue
  fixed. Probe details in commit `305242d`.
- **Attr macro fragmentation on async-fn / impl** — pre-fix the
  user's `#[tokio::main] async fn main() { ... }` was getting
  REPLACED with fragmented expression text instead of the full
  transformed `fn main()`. Now detected via
  `detect_attr_macro_transformation` walking the post-expansion
  AST for non-root descendants whose outer expansion is a
  proc-macro Attr.
- **`::std::rt::begin_panic` rewrite** — bytemuck's
  `#[derive(Pod)]`-emitted `panic!()` expansion uses the std-side
  internal, not core's. Add to the rewrite needle list.
- **Warn-skipped `mod NAME;` no longer leaks into flat output** —
  cfg-gated mods whose target file is missing on the current
  platform get `;` swapped for `{ /* cfg-skipped */ }` so
  downstream cargo-build doesn't try to read the missing file.
- **Edition-2015 `try!()` rewrite** —
  `vendor::rewrite_try_macro` runs as part of the per-dep rewrite
  closure when the dep is edition 2015 AND its source contains
  `try!(`. Rewrites `try!(EXPR)` → `(EXPR)?`.
- **Inner-attr cfg detection** — target file with
  `#![cfg(target_os = "windows")]` on a non-Windows build is now
  warn-skipped (treats it like a missing file).
- **Integration tests for tokio/ratatui/bytemuck added.** axum is
  intentionally NOT added (its `#[async_trait]` case has a
  different fragmentation shape that needs additional work).

### Cross-target portability batch
- **Compiler-set cfgs preserved verbatim** (Phase 1). `eval_cfg`
  short-circuits all cfgs in `is_compiler_set_predicate` (target_*,
  unix/windows shorthands, debug_assertions, test, proc_macro,
  panic) to Unknown so they pass through to the user's compile.
  Re-fixes the crossterm-feature-named-`windows` collision via
  the same allowlist (compiler-set names never consult features).
  Regression test: `vendor_preserves_compiler_set_cfgs_even_when_feature_collides`.
- **Inline ALL cfg_attr-path candidates** (Phase 2). socket2's
  `#[cfg_attr(unix, path = "sys/unix.rs")] #[cfg_attr(windows,
  path = "sys/windows.rs")] mod sys;` now emits BOTH cfg-gated
  mod blocks; user's compile picks one. Same for mio's
  selector/epoll/kqueue/poll. Required `SpliceKind::MultiCfg`
  + scanner returning all candidates with their PREDs. Regression
  test: `vendor_emits_all_cfg_attr_path_candidates_for_top_level_mod`.
- **Inline files with inactive inner `#![cfg]`** (Phase 3).
  windows-sys' `mod Wdk;` (file opens with `#![cfg(windows)]`)
  is inlined unconditionally; the inner cfg flows through and
  gates contents at user time. Regression test:
  `vendor_inlines_mod_whose_target_has_inactive_inner_cfg`.
- **Vendor inactive-lib.rs deps with cfg-gated injections**
  (Phase 4). crossterm_winapi (`#![cfg(windows)]` lib.rs) is
  vendored. Sibling-import injections referring to it get
  `#[cfg(windows)] use crate::crossterm_winapi;` so non-matching
  hosts compile cleanly while a Windows compile picks them up.
  Regression test: `vendor_inactive_lib_rs_dep_is_vendored_with_cfg_gated_injections`.

---

## Pending

### Coverage-probe followup batch (this session)
- **`#[async_trait]` Attr fragmentation** (`df2585a`) — Scanner now
  recurses into assoc items via `v::walk_assoc_item`; async_trait's
  per-method type-coercion code is detected and the impl block emits
  as one atomic unit. Axum vendors clean.
- **`mod NAME;` wrapped in custom cfg-macros** (`1be91a7`) —
  `inline_mods_inside_macros` walks proc_macro2 token streams,
  finds `mod IDENT ;` triplets nested in macro args, resolves the
  files (honoring `#[path = "..."]` / `#[cfg_attr(..., path = "...")]`
  via first-existing-file heuristic), and splices `mod IDENT {
  /* contents */ }`. Skips `cfg_if!` (handled separately by
  `crate::cfg::expand_cfg_if`). Recursive on inlined content.
  Tokio's "couldn't read file" errors gone (28 → 0 of that class).
- **Cfg-skipped mod cascade diagnostic** (`8d18996`) —
  thread-local `SKIPPED_MODS` collector + main.rs summary on stderr
  listing each skipped mod with its parent crate and reason, plus
  workaround hints (re-run on different target / `--external <CRATE>`).

### Real-world coverage state (snapshot)

Probed via end-to-end vendor + downstream `cargo build` at edition
2021 with `--external-preset infra --external-deep --expand
--expand-deep`. See `tests/integration.rs::real_vendor_*`.

| Crate | Vendor | Compile | Errors | Notes |
|-------|--------|---------|--------|-------|
| bytemuck | ✅ | ✅ | 0 | clean |
| clap | ✅ | ✅ | 0 | clean |
| glam | ✅ | ✅ | 0 | clean |
| nalgebra | ✅ | ✅ | 0 | clean (resolved via expand-deep + tainted-macro chain) |
| rand | ✅ | ✅ | 0 | clean (resolved by holistic build-script policy + `r#ident` mod fix + type-ascription `crate::` rewrite) |
| rayon | ✅ | ✅ | 0 | clean (no externals required after holistic build-script policy lifted `links="rayon-core"` + crossbeam-utils blocks) |
| regex | ✅ | ✅ | 0 | clean |
| serde | ✅ | ❌ | 2 | see below |
| ratatui | ✅ | ❌ | 3 | see below |
| tokio | ✅ | ❌ | 3 | see below |
| axum | ✅ | ❌ | 4 | see below |

**7/11 compile clean.** Total residual errors across the four broken
crates: **12** (excluding cargo summary lines).

### Per-crate residual errors

#### serde (2 errors)
1. `error[E0583]: file not found for module 'private'` — line 17286.
   Inside `crate_root!{...}` macro body. Skipped by the build-script-
   marker heuristic (`pub mod __private228` triggers skip — see
   commit `d87505f`). The build-script-generated `__private228`
   re-exports `crate::serde::private::*`, but the `mod private;`
   declaration is left unresolved. Inlining the file's contents
   regresses re-export resolution (extensively investigated in
   sessions documented in commits `d87505f` and earlier).
2. `error[E0425]: cannot find type Result in module _serde::__private228` —
   line 20. Downstream of #1: `__private228::Result` re-exports
   from `private::Result` which doesn't exist due to #1.

**Root cause**: serde's `crate_root!` macro mixes `mod private;`
(file inlining needed) with `include!(concat!(env!("OUT_DIR"),
"/private.rs"))` (build-script content; `--expand-deep` pre-inlines
this into the source verbatim). The two interact in ways our
inliner can't currently model.

**Fix sketch**: structural detector that recognises the build-
script-content arm shape and inlines `mod private;` AS an empty stub
that re-exports from the build-script content. Effort: 4-6 hr.

#### tokio (3 errors)
All three: `error: no rules expected '::'` on `crate::libc::EVFILT_*`
inside `debug_detail!()` invocations from `mio/src/sys/unix/selector/
kqueue.rs` (lines 149520, 149582, 149642 in flat output).

**Root cause**: `debug_detail!` is defined inside `cfg_os_poll!{...}`
in `mio/src/sys/unix/mod.rs` — a NESTED macro_rules. Our dep-wide
ident-pair-matcher scan (commits `9f80763`, `618d261`) walks
top-level `Item::Macro` only; nested macros are missed.

**Fix sketch**: extend scan to walk macro invocation tokens for
nested `macro_rules! NAME { ... }` definitions, BUT require ALL
arms to be pure ident-pair (no other fragment specifiers) to avoid
the over-match regression (tokio 4→13 errors when tried with ANY-arm
in `618d261` session). Effort: 3-4 hr.

#### axum (4 errors)
- 3× `error: no rules expected '::'` on `crate::libc::AF_*` inside
  `impl_debug!()` from `socket2/src/lib.rs` (lines 285196, 285258,
  285318). Same root cause as tokio above (`impl_debug!` is also
  inside a wrapping macro in socket2).
- 1× `error[E0583]: file not found for module 'private'` (line
  315131). Same as serde's case — axum transitively depends on serde.

#### ratatui (3 errors)
3× `error: no rules expected '::'` on `crate::libc::*` inside
`impl_debug!()` from `socket2`. Same root cause as axum's first
three errors (socket2's `impl_debug!` is the shared culprit; mio
and socket2 both pull socket2 transitively).

### Tail items

(a) Multi-edition output support — `--output-edition` flag with
per-edition transforms (unsafe extern, gen ident, match-ergonomics
2024 rewrites). Documented separately below.

(b) `rustc-env=VAR=VALUE` build-script directive support — the
holistic build-script policy currently treats this as a hard block
because we'd need to rewrite `env!("VAR")` calls in the dep's
source. Effort: 4-6 hr; would unblock crates that embed git hashes
or similar via build scripts.

### Crates currently in `--external-preset infra`

`presets/infra.txt` lists 18 crate names that always need external.
With the holistic build-script policy now in place (`a070a6b`),
several entries no longer require external classification:

| Crate | Still needs external? | Reason |
|-------|----------------------|--------|
| `parking_lot_core` | NO (could be removed) | Build script emits no link-related directives; new policy classifies as vendorable |
| `signal-hook` | NO (could be removed) | Same — build script emits cfgs only |
| `signal-hook-registry` | NO (could be removed) | No build script; fixed bare-Fn-trait alias issue (commit `0e4b353`) |
| `zmij` | NO (could be removed) | Pure library, no link directives |
| `serde_core` | YES | Multi-version conflict: serde re-exports types from serde_core; vendoring both produces type mismatches at API boundaries |
| `winapi` | YES | Build script emits `cargo::rustc-link-lib` for each Windows API the crate exposes — actual native linking required |
| `winapi-i686-pc-windows-gnu` | YES | Same as winapi (sub-crate) |
| `winapi-x86_64-pc-windows-gnu` | YES | Same as winapi |
| `windows-sys` | YES | Source uses link!{} macros that generate items from build-script bindings; `mod Win32` declaration looks for a file that doesn't exist (macro-generated). Vendoring would need to expand link!{} expansions and inline OUT_DIR-generated bindings — multi-day. |
| `windows-targets` | YES | Pure dispatch crate that conditionally re-exports the target-arch sub-crates (windows_x86_64_msvc, windows_aarch64_gnullvm, etc.); vendoring would also pull the target-arch crates, each of which has its own link directives |
| `windows_aarch64_gnullvm` | YES | Build script emits `cargo::rustc-link-lib` for Windows native libraries and `cargo::rustc-link-search` for the Windows-target-specific lib paths |
| `windows_aarch64_msvc` | YES | Same |
| `windows_i686_gnu` | YES | Same |
| `windows_i686_gnullvm` | YES | Same |
| `windows_i686_msvc` | YES | Same |
| `windows_x86_64_gnu` | YES | Same |
| `windows_x86_64_gnullvm` | YES | Same |
| `windows_x86_64_msvc` | YES | Same |
| `crossterm_winapi` | TARGET-SPECIFIC | Windows-only via `#![cfg(windows)]` inner attribute; vendorable on Windows host but the cfg-skip diagnostic punts on non-Windows. Listed in preset for cross-host portability. |

**Net**: 4 entries (`parking_lot_core`, `signal-hook`,
`signal-hook-registry`, `zmij`) could be safely removed from the
preset post-`a070a6b`. The remaining 14 are genuinely
unvendorable for the reasons stated. We're keeping the preset
list intact for now to preserve the existing "drop in, get a
working build" UX; users who want to vendor the now-vendorable
crates can omit `--external-preset infra` and rely on the
classifier.

#### Cluster A (legacy doc — RESOLVED): Cross-file ident-pair-matcher path rewriter
**Status**: partially resolved by `618d261` (per-file + dep-wide
top-level scan with ANY-arm criterion). 12 → 9 errors. Remaining
9 are the nested-macro_rules subset (tokio's `debug_detail!`
inside `cfg_os_poll!`); see "tokio (3 errors)" above.

#### Cluster B (legacy doc — RESOLVED): `mod NAME` inside macro_rules body
**Status**: resolved by `d87505f` for libc's `prelude!()` (allows
`mod types` inlining; reduced tokio/axum/ratatui by 1 error each).
serde's `crate_root!()` is intentionally skipped via build-script-
marker heuristic; see "serde (2 errors)" above for the residual
shape.

#### Cluster C (legacy doc — RESOLVED): Downstream of cluster B
**Status**: resolved insofar as cluster B is resolved. serde's
`_serde::__private228::Result` is still tracked as one of serde's
2 residual errors.

#### Cluster D (legacy doc — RESOLVED): rand zerocopy unvendorable
**Status**: resolved by `a070a6b` (holistic build-script policy)
and `7fb2d5d` (type-ascription rewrite). rand now compiles clean
without externals.

### Multi-edition output support

#### `--output-edition=2018|2021|2024` flag *(coverage probe — tokio, axum, ratatui, regex, nalgebra)*
Today the flat output is compiled at `max(user_edition, dep_editions)`
— the user crate's edition rules and resolver settings flow through
unchanged. That works when the user picks an edition the deps were
written for, but breaks both ways:

- **2024 outer + pre-2024 deps:** Rust 2024 forbids patterns deps
  written for 2021/2018 use freely (`extern { fn ... }` without
  `unsafe`, `let gen = …`, `&` to `static mut`, explicit `ref` inside
  implicitly-borrowed pattern, …). Surfaces as hard syntax / borrow
  errors in vendored output. See the breakdown in the conversation
  summary; tokio/axum/ratatui hit `unsafe extern`, regex/nalgebra
  hit `gen`, regex hits match-ergonomics 2024.

- **Pre-2024 outer + 2024 deps:** rare today (most ecosystem deps
  still target 2021) but will become common as the ecosystem moves.
  2024 deps may use `gen { … }` blocks, `impl Trait + use<>` capture
  syntax, `let else` patterns flagged by older lint rules, etc.

Goal: explicit `--output-edition=2018|2021|2024` flag (default to
`max_edition` for back-compat). For each edition, register a set of
"transform passes" that bridge the source edition to the output
edition. Passes are token-level pre-syn rewrites where possible
(reuse the `rewrite_bare_trait_object_aliases` /
`rewrite_pat_followed_by_pipe` shape).

**Per-edition transform inventory:**

*Pre-2024 → 2024:*
- `extern "ABI" { ... }` → `unsafe extern "ABI" { ... }`
- `#[no_mangle]` / `#[export_name]` / `#[link_section]` →
  `#[unsafe(no_mangle)]` etc.
- `gen` as identifier → `r#gen`
- `&[mut] STATIC_MUT` → unsafe block wrap
- explicit `ref`/`mut` inside `match &thing { ... }` → strip the
  explicit binding mode

*2024 → pre-2024:*
- `gen { ... }` blocks → expanded form (probably refuse-to-vendor;
  generators are non-trivially desugarable)
- `impl Trait + use<>` → strip the use-bound (may change semantics)
- Forward-compat for `expr` fragment specifier matches that 2024
  added — reject newer matches

*2018 → 2021/2024:*
- `try!(EXPR)` → `(EXPR)?` (already done for edition-2015, extend
  to 2018)
- bare `dyn`-less trait objects → inject `dyn`
- closure capture rules: deps that depended on the 2018 capture-the-
  whole-struct rule may need explicit field bindings (rare)

*2015 → anything later:*
- `extern crate FOO;` → drop (already done)
- `try!()` (already done)
- bare-trait objects (already done for `Fn`/`FnMut`/`FnOnce`,
  generalize to all trait-name-followed-by-`+` shapes)

**Implementation shape:**
- Add `--output-edition` to `main.rs` flags.
- Add `OutputEdition` enum to `VendorOptions`.
- Each transform pass tags its `(from_edition, to_edition)`
  applicability; the pipeline composes the passes between source and
  target.
- Per-dep transforms see the dep's own edition (already plumbed
  via `is_edition_2015`).

**Effort:** 1-2 days for the framework + 2024-target transforms (1a,
1b, 1c from the conversation). Additional 1 day per direction for the
remaining pairs. Risk: medium — match-ergonomics 2024 transform
needs AST-level analysis, not just token walking.

### Likely bugs

#### Type-ascription `:` confused with `::` in macro-body rewrites *(landed)*
`collect_macro_invocation_token_rewrites` skipped `crate::FOO` /
`SIBLING::FOO` rewrites whenever the previous token was a single `:`,
on the assumption it was the second `:` of a `something::crate::FOO`
non-leading path. But a single `:` is also type-ascription syntax —
`field: crate::Foo`, `arg: SIBLING::Foo`. Tokio's
`cfg_io_driver! { mod driver { struct Cfg { timer_flavor:
crate::runtime::TimerFlavor } } }` was the canonical real-world
casualty (~46 surviving errors on tokio dropped to 0 after the fix).
Fix: require the previous token's previous to also be a `:` with
Joint spacing before treating it as a non-leading-segment skip.
Regression test: `vendor_rewrites_crate_path_in_type_ascription_inside_macro`.

#### `#[cfg_attr(_, path)]` on top-level `mod NAME;` ignored *(landed)*
`scanner::extract_path_attr` only recognised plain `#[path = "..."]`,
silently dropping `#[cfg_attr(unix, path = "sys/unix.rs")]` style
attrs. socket2's `mod sys;` (and tokio's `mod windows`) consequently
fell through to standard resolution, found nothing, and warn-skipped
into an empty `mod sys {}` — leaving downstream cargo-build with
hundreds of `cannot find value SOL_SOCKET in module sys` errors. Fix:
collect ALL path candidates from `#[path]` AND `#[cfg_attr(_, path)]`
in attribute order, drop those whose cfg PRED is statically
known-false on the current target, then pick the first whose file
exists. Regression test:
`vendor_resolves_cfg_attr_path_for_top_level_mod`.

#### Bare-ident `cfg(windows)` / `cfg(unix)` confused with Cargo features of the same name *(landed)*
The cfg evaluator's `CfgExpr::Bare(name)` arm checked
`features.contains(name)` for ALL bare idents — so when crossterm
declared a Cargo feature literally named `windows` (which enables
`crossterm_winapi`), `cfg(windows)` evaluated True on macOS / Linux,
and the cfg-attr rewriter stripped the attr from items like
`pub use self::windows::position;`, leaving the item active
year-round. Fix: short-circuit `unix` / `windows` / `wasm32` to bind
to `std::env::consts::FAMILY` / `ARCH` directly. Other bare idents
(build-script `--cfg=NAME`, `loom`, `assert_no_panic`, …) keep the
features-set lookup. Regression test:
`vendor_treats_bare_cfg_target_predicates_as_target_not_feature`.

#### Skip vendoring deps whose lib.rs has inactive inner cfg *(landed)*
crossterm_winapi (Windows-only) opens its lib.rs with
`#![cfg(windows)]`. We were vendoring it as `pub mod
crossterm_winapi { #![cfg(windows)] ... }` (entire body evaporates on
non-Windows) AND injecting `use crate::crossterm_winapi;` at the top
of every other vendored mod — yielding ~341 unresolved-import errors
per non-Windows build of ratatui. Fix: pre-filter such deps out of
`to_vendor_sorted` and the sibling set so no other dep references
them. Regression test:
`vendor_skips_dep_whose_lib_rs_has_inactive_inner_cfg`.

#### `pub use std::os::unix::io::*` mistakenly downgraded to `pub(crate) use` *(landed)*
`collect_extern_crate_reexport_downgrades` was meant to catch the
`extern crate FOO; pub use FOO;` pattern (where FOO was an alias for
`core` / `std` / `alloc`). The check fired on ANY `pub use stdlib::…`
re-export — including legitimate item re-exports like rustix's
`pub use std::os::unix::io::{AsFd, AsRawFd, …}`. Downgrading those to
`pub(crate)` produced E0365 "crate public, cannot re-export outside"
in callers (~17 errors on ratatui). Fix: narrow the predicate to the
actual extern-crate shape (`pub use stdlib;`, `pub use stdlib::*;`,
`pub use stdlib as ALIAS;`); leave specific-item re-exports alone.

#### Cfg-aware path picking inside macro-args mod resolver *(landed)*
`inline_mods_inside_macros` already collected `#[cfg_attr(_, path =
...)]` candidates for `mod NAME;` declarations nested inside macro
invocations, but picked the first-existing-file regardless of cfg.
mio's `mod selector;` lists `selector/epoll.rs` (Linux),
`selector/kqueue.rs` (BSD/macOS), and `selector/poll.rs` (other) —
all three exist in the source tree, so naive picking inlined epoll.rs
on macOS, leaving ~30 `cannot find value EPOLL_*` errors. Fix:
extract each cfg PRED alongside its path; first-pass picks the first
candidate whose PRED is NOT statically known-false; second-pass falls
back to first-existing for unevaluatable PREDs. Required adding an
`any(...)` arm to `cfg_predicate_known_false` (previously only
`all(...)` was handled).

#### Cfg-feature-gated items inside custom cfg-macro invocations *(landed)*
Tokio's `cfg_NAME!` family (cfg_io_driver, cfg_not_wasip1, …) defines
macros whose matcher is `($($item:item)*)` and body is
`$( #[cfg(...)] $item )*`. Call sites pass `#[cfg(feature = "X")]`
attrs in the args that get pasted onto items — for example
`cfg_not_wasip1! { #[cfg(feature = "net")] pub(crate) use addr::to_socket_addrs; }`.
Pre-fix, the cfg-attr rewriter only processed `macro_rules!`
DEFINITIONS, so the args stayed verbatim and evaporated at user
compile time (the user crate doesn't enable the dep's "net" feature).
Items referenced via `pub(crate) use addr::to_socket_addrs` then
resolved to nothing — ~7 unresolved-import errors per such cluster.
Fix shape:
1. Pre-scan every `.rs` file under each dep's `src/` for `macro_rules!`
   definitions whose matcher matches `$($i:item)*` (cross-file scan
   because definitions and invocations live in different files).
2. Make `collect_macro_body_cfg_rewrites` process invocation tokens
   for any macro in that set, in addition to the existing
   macro_rules-definition pass.
3. Heuristic narrow enough to leave cfg_if! / pick! / similar DSL
   macros alone — they have a different matcher shape (`if
   #[cfg($e:meta)] { … } else if …`) that doesn't match `$($i:item)*`.
Regression test:
`vendor_bakes_cfg_attrs_in_item_list_macro_invocations`. Reduced
axum errors 34 → 22, exposed the "mutually-exclusive macro pair"
underlying issue on tokio (next item).

#### Mutually-exclusive cfg-macro pairs producing duplicate definitions *(landed via loom=False)*
Tokio's `cfg_signal_internal_and_unix!` and `cfg_not_signal_internal!`
both contain a `fn create_signal_driver` definition — first uses
`cfg(any(feature = "signal", all(unix, feature = "process")))`,
second uses `cfg(any(loom, not(unix), not(any(feature = "signal",
all(unix, feature = "process")))))`. Pre-fix the positive branch
baked to `cfg(all())` (vendor-time features True) but the negation
stayed Unknown (because `loom` was Unknown) → both branches active
at user compile time → duplicate `create_signal_driver`.
Tried-and-reverted approach: cfg_X / cfg_not_X pair detection
(over-gated tokio's feature-only macros, regressed 24 → 178 errors).
Landed approach: special-case bare `loom` cfg as False at vendor
time. This is testing-only (only set when running `loom` itself, never
in production), so the user will never enable it downstream.
With `loom = False`, the negation cleanly evaluates False at vendor
time, bakes to `cfg(any())`, gates out at user compile time. Other
opt-in cfgs (`tokio_unstable`, `debug_assertions`) stay Unknown.
Regression test: `eval_bare_loom_is_false_not_unknown`. Reduced
tokio errors 24 → 16, axum 22 → 17.

#### `include!()` inside macro_rules body / macro invocation *(landed)*
serde's `crate_root! { include!(concat!(env!("OUT_DIR"),
"/private.rs")); }` was unreachable to `expand_include_macros`
because syn's visitor doesn't traverse macro tokens. The flat
output preserved the bare `include!(...)` which then failed at
user compile time with "environment variable OUT_DIR not defined".
Fix: `walk_tokens_for_includes` recurses through proc_macro2
tokens of every macro invocation found at the AST level, looking
for `include!()` / `include_str!()` patterns. Also consumes the
trailing `;` so the inlined content sits cleanly at item position
(a stray `;` after item-level inlined content errors with
"macro expansion ignores `;` and any tokens following").
Regression test:
`vendor_expands_include_macros_inside_macro_invocations`.
Reduced axum errors 17 → 14.

#### Sibling-import injection collides with `mod NAME` inside cfg-macro *(landed)*
tokio's `pub mod util` has `cfg_rt! { mod sync_wrapper; ... }`.
The inline-macro pass splices that to `mod sync_wrapper { ... }`
inside the macro tokens. syn::parse_file doesn't see inside macro
tokens, so `top_level_item_names` missed `sync_wrapper`. Sibling-
import injection then emitted `use crate::sync_wrapper;`
(sync_wrapper is also vendored as a sibling crate) → E0255 "the
name sync_wrapper is defined multiple times". Fix:
`mods_inside_item_list_macros` walks proc_macro2 tokens looking
for `mod NAME { ... }` / `mod NAME ;` triplets nested in
item-list macro invocations. Names go into `already_declared`,
so injection skips. Regression test:
`vendor_inject_imports_skips_mods_inside_item_list_macros`.

#### Cfg-attr baking inside arbitrary macro invocations *(landed)*
Earlier the cfg-attr rewriter only ran on `macro_rules!` bodies
and item-list-macro invocations. bitflags!-style DSL macros
(used by rustix to declare `OutputModes` bitflags with
`#[cfg(...)] const X = ...`) weren't covered, leaving cfgs
unbaked → at user time the user crate doesn't have the dep's
features → wrong items active → `cannot find c::OLCUC` style
errors. Fix: process the body of EVERY non-macro_rules macro
invocation. Safe across DSL macros (cfg_if!, pick!) because their
`#[cfg(...)]` predicates evaluate the same at vendor time as at
user compile time. Reduced ratatui errors 5 → 2.

#### `cfg(feature = "X")` passed as macro ARGUMENT (not body) *(landed)*
Either's `impl_specific_ref_and_mut!(::std::path::Path, cfg(feature = "std"), …)`
passes a cfg expression as a macro argument that becomes
`#[cfg(feature = "std")]` on the emitted impl. Different shape from
the item-list-macro case (cfg is in arg position, not in body item
position). Pre-fix: arg stayed verbatim → impl gated out at user
compile time → only generic `AsRef<Target>` impl remained → couldn't
handle unsized targets → ratatui's 6-error `[u8] cannot be known`
cluster. Fix: walk every macro invocation's tokens and bake bare
`cfg(EXPR)` (only when EXPR references a Cargo feature, to minimise
blast radius). Skip when preceded by `#`/`:` (already inside an attr
or path), `if`/`else` (cfg_if-style DSL), or inside a `[...]` Group
(attribute body — handled by the attr-form branch). Regression test:
`vendor_bakes_bare_cfg_passed_as_macro_argument`. Reduced ratatui
errors 11 → 5.

#### `parse_concat_out_dir` accept 3-arg form  *(REVIEW B1)*
`concat!(env!("OUT_DIR"), "/", "file.rs")` — common when devs split
slash from filename. Walk all string literals after `env!(OUT_DIR)` and
concatenate.
Effort: 1 hour.

#### `dep_uses_out_dir_include` scan submodules  *(REVIEW B2)*
Walk `src/**/*.rs` not just `src/lib.rs` / `src/main.rs`. A crate with
`env!("OUT_DIR")` only in a submodule is wrongly classified Unvendorable
today.
Effort: 1 hour.

#### `collect_macro_use_externals` honor cfg evaluation  *(REVIEW B3)*
Currently drops `#[cfg(...)] #[macro_use] extern crate FOO;`
unconditionally. Should consult the cfg evaluator.
Effort: 1 hour.

#### Deterministic multi-version dep ordering  *(REVIEW B4)*
`to_vendor_sorted.sort_by(|a, b| a.name.cmp(&b.name))` should chain
`.then(a.version.cmp(&b.version))`. Multi-version blocker messages are
non-deterministic across runs today.
Effort: 15 minutes.

#### `pub(crate) use` collision detection  *(REVIEW B5)*
`bump_in_items` only matches `Visibility::Public(_)`. Extend to
`Visibility::Restricted(_)` with `path == "crate"`.
Effort: 1 hour.

#### Surface IO errors in `inlined_macro_names`  *(REVIEW B6)*
`if let Ok(src) = ...` silently drops missing-lib errors. Log a warning;
ideally read `[lib].path` from the dep manifest.
Effort: 1 hour.

#### Wrapper IO error logging  *(REVIEW B8)*
`expand/src/main.rs:254-260, 312` use `let _ = std::fs::write(...)`. Log
failures; downstream silently uses unrewritten cargo cache otherwise.
Effort: 1 hour.

#### `rewrite_unstable_panic_calls`: also handle `::std::rt::begin_panic` *(coverage probe — bytemuck)*
The `panic!()` macro_rules in std expands to `::std::rt::begin_panic("…")`,
not `::core::panicking::panic`. Same shape as the existing rewrite — extend
the needle list to cover both. Surfaced by bytemuck's `#[derive(Pod)]`
runtime check that uses `panic!("derive(Pod) was applied to a type with
padding")`. Effort: 30 minutes.

#### `rewrite_unstable_panic_calls` context awareness  *(REVIEW A7)*
Add a small lexer to skip needles inside `//`, `/* */`, `"…"`, `r#"…"#`.
Also handle `panicking::panic_fmt(args)` and `concat!()` arguments.
Effort: half a day. Risk: medium — lexer drift is the same risk that
afflicts find_item_end.

#### `String::from_utf8(...).expect(...)` → structured error  *(REVIEW A8)*
`src/main.rs:251`. Match the `--minify` path's `InvalidData` error.
Effort: 15 minutes.

#### Attr macro on async-fn / impl emits fragmented expression text *(coverage probe — axum, tokio)*
`#[tokio::main]` and `#[async_trait]` Attr-macro expansions transform an
async fn / async impl into a `Builder::block_on(async { ... })` shape that
contains both item-position and expression-position code. The wrapper's
`process_expanded_node` visits an inner Expr at non-root context and
pretty-prints THAT NODE, then stuffs the result into bang_groups keyed by
the outer attr's host-item span. Result: only the inner snippet survives
in the host span, the rest of the expansion is lost, and parsing the
output as item-position fails ("expected item, found keyword `async`").

Same root cause hits both crates. Fix shape: when an Attr macro's host
item is an `impl` or `fn`, the WHOLE pretty-printed item should land at
the host span, not individual nested Exprs. Likely needs the
`process_one_item` / `process_assoc_item` paths to claim the host span
and prevent the inner-expr visitor from also emitting a replacement.

Effort: 1-2 days. Risk: medium — touches the visitor logic that drove
rapier2d coverage and previously had subtle ordering bugs.

#### Edition-2015 dep using `try!()` in vendored output *(coverage probe — ratatui via cassowary)*
Vendored `pub mod foo { ... }` blocks are compiled at the OUTER user
crate's edition. Edition-2015 deps that use `try!()` (which became
unusable in 2021 — `try` is reserved) fail to compile. Same applies to
other 2015-isms (bare `extern crate`, `dyn` not required, etc.) that we
mostly already rewrite — but `try!()` is uniquely awkward.

Fix shape: detect deps with `edition = "2015"` and that contain `try!`,
either rewrite `try!(EXPR)` → `(EXPR)?` (semantics-preserving) or refuse
to vendor with a clear "--external this dep" error.

Effort: half a day for the rewrite path. Risk: low — `try!(EXPR)` →
`(EXPR)?` is mechanically safe.

#### Warn-skipped `mod NAME;` leaks into flat output *(coverage probe — ratatui via rustix)*
When a cfg-gated mod's file is missing on the current platform, we keep
the `mod NAME;` declaration verbatim in the source. This used to be
harmless (downstream cargo-build resolves the file via the original
mod tree), but the flat single-file output has no mod tree — downstream
fails with `couldn't read src/foo/mod.rs: No such file or directory`.

Fix shape: convert warn-skipped mods to empty `mod NAME {}` blocks (or
delete them entirely with the cfg) so the flat output is self-
contained.

Effort: 2 hours. Risk: low.

#### `windows-sys` `mod Wdk` not warn-skipped *(landed)*
windows-sys' lib.rs has `pub mod Wdk;` un-cfg-gated; the file
`src/Wdk/mod.rs` exists only on Windows builds. Our scanner warn-skips
mods marked with `#[cfg(...)]` on the declaration but here the cfg
guard is INSIDE the file (as `#![cfg(...)]` inner attr) which we don't
look at. Result: vendoring fails with `mod Wdk not found` on macOS/
Linux even though cargo handles it fine.

Fix: `file_has_inactive_inner_cfg` reads the target file and walks
inner attrs for `#![cfg(target_os/family/arch/pointer_width/endian =
"X")]` predicates; warn-skips if any evaluates to known-false.
Regression test: `vendor_warn_skips_mod_whose_target_has_inactive_inner_cfg`.

#### Multiple repeat offenders for `--external`-required infra crates *(coverage probe — many)*
`signal-hook`, `signal-hook-registry`, `parking_lot_core`, `winapi*`,
`windows-sys`, `serde_core`, `zmij`, `tokio` (sometimes), `mio` (often)
have build scripts that we can't vendor. Today the user discovers them
one by one via repeated runs.

Fix shape: ship a curated `--external-file` containing the well-known
infrastructure crates that virtually always need to be external. Or,
better, auto-suggest them in the strict-mode error: "try
`--external signal-hook,parking_lot_core,...` (these crates always need
external because <reason>)".

Effort: 1 hour for the curated file; 3 hours for auto-suggestions.

#### `signal-hook-registry` syn parse failure *(coverage probe — multiple)*
"expected `;`" surfaces every time signal-hook-registry is in the dep
graph. Same shape as the libm bug we fixed during rapier2d push (over-
swallow strip removed a comma → field had no terminator). Worth
investigating whether the existing `is_full_attr` guard misses a case.

Effort: 2-3 hours (investigation + targeted fix).

### Missing tests *(REVIEW F)*

#### Direct unit tests for `find_item_end`
Cover char literals (`'}'`, `'"'`, `'\u{7D}'`), raw strings (`r#"..."#`),
byte/c-strings, nested block comments, no-body items.
Effort: 1-2 hours.

#### Round-trip parse of `apply_edits` output
Feed wrapper output through `syn::parse_file`, assert it parses. Cheap
regression net.
Effort: 1 hour.

#### `scrub_unresolvable_sibling_reexports` direct tests
Covers wildcard descent, single-item removal, multi-item filtering,
visibility preservation.
Effort: 2 hours.

#### `format_with_rustfmt` deadlock test
Synth 2 MB source, run with `--fmt`, assert it returns within reasonable
time. Currently no test exists.
Effort: 1 hour (after fixing the deadlock).

#### `--vendor` scrub for auto-externalized proc-macros (no `--expand`)
Test that the scrub pass runs even without `--expand`, surfacing REVIEW A6.
Effort: 1 hour.

#### `minify.rs` corner cases
c-strings, string continuations, `\u{xxxx}`, `r###"..."###`.
Effort: 1 hour.

#### `safe_inject_point` corner cases
Inner doc attrs, `#![feature(...)]`, no inner attrs, no trailing newline.
Effort: 1 hour.

#### `--expand-deep` distribution test (Phase 5)
Once distribution is solved, integration test that runs the install path
on a clean `rustup` toolchain.

### Larger refactors

#### Replace `find_item_end` with syn parse  *(REVIEW B / decisions log)*
Re-parse each captured file with syn in the wrapper. Eliminates an entire
class of bugs (raw strings, char literals, nested comments). Adds a `syn`
dep to the `expand` crate (~6 sec compile cost in CI). Worth doing once
the targeted fix is in place and we have data on whether the bug class
actually surfaces in real crates.
Effort: 1 day.

#### Move `--expand-deep` plumbing into `src/vendor/expand.rs`  *(REVIEW E2)*
~400 lines of subprocess orchestration / env-var contracts / dump-dir
contract. Different domain from the syn AST work; cleanly separable. Do
after `cfg.rs` extraction proves out the directory layout.
Effort: half a day.

#### Pipeline.rs / move main.rs orchestration into vendor  *(REVIEW E7)*
main.rs lines 187-316 are the pipeline (vendor → tree-or-body → fmt →
minify → banner → write). Should be one `cargo_flatten::run(request)` call.
Effort: 1 day.

#### `SourceLoader` trait to clean up `ParseOptions` grab-bag  *(REVIEW E6)*
Make `SourceFile` accept a trait where the vendoring loader is one impl
that internally does include expansion + rewrite + out_dir resolution.
Currently both `rewrite_source` and `out_dir` are vendoring-specific
fields leaking into the file-loading layer.
Effort: 1-2 days.

#### Hybrid byte-edit / AST-rewrite pilot  *(REVIEW E4)*
Pilot conversion of `collect_macro_export_rewrites` to AST-mutate +
`prettyplease::unparse` per-item, with byte-splice back into the
original source. If it works, migrate other structural passes. Keep
ident-level passes as byte-edit.
Effort: 2-3 days for the pilot.

### Phase 5: distribution + launcher model
**Issue:** `cargo install cargo-flatten-expand` fails on most users'
machines because they lack `rustc-dev`. The expander binary needs to be
rebuilt against each nightly toolchain. This is the largest blocker to
shipping `--expand-deep` widely.

Possible designs:
- Launcher binary that shells out to `rustup component add` then
  `cargo install --git ... --locked`
- Bundled prebuilt for known nightlies (storage cost, version matrix)
- Document the manual install and point users at it

Effort: 1+ week. Risk: high.

### Robustness (pre-review backlog)

#### Cycle detection for mod resolution  *(separate from REVIEW A5 which is for include!)*
**Why.** A symlink loop in `src/`, or two files mutually referencing each
other through `#[path]`, would recurse forever. `MAX_DEPTH=128` limits
stack but doesn't produce a useful error.

**Plan.**
- Thread `&mut Vec<PathBuf>` (visit stack) through `from_file_with_root`.
- Push canonicalized resolved path on entry, pop on exit.
- Before recursing, check `stack.contains(&canonical)`. Error with the
  cycle as a chain.

**Tests.** Symlink loop, mutual `#[path]` reference.
Effort: 2-3 hours.

---

## Out of scope / declined

These come up but are deliberately left out:

### Pre-review backlog
- **Workspace flattening.** Each member is a separate invocation; we
  could detect a workspace root and list members in the error, but
  multi-target output is not on the table.
- **Watch mode / dev loop.** Out of the tool's "snapshot a crate as one
  file" purpose. A separate `cargo watch -- cargo flatten ...` covers it
  fine.
- **Unflatten / reverse operation.** Splitting a flat file back into a
  mod tree is plausible but a different tool.
- **Inner attribute hoisting.** A child file's `#![allow(...)]` becomes
  `mod foo { #![allow(...)] }`. Semantically still applies to that mod
  only, which is correct; hoisting to the crate root would change
  meaning.
- **Typo suggestions for missing mods.** Adding a `strsim` dep just to
  whisper "did you mean `bar`?" is more noise than signal.

### Review-driven decisions
- **Trait-ify the 14 rewrite passes.** The passes look uniform but each
  takes a different combination of `(crate_name, siblings, aliases,
  crate_root_items, deleted, edits)`. A `RewritePass` trait would
  degenerate into a god-context struct or pile of `Option<>` parameters.
  The implicit pass-ordering protocol is genuinely two-phase-commit;
  splitting it across files would make ordering bugs harder, not easier.
  Revisit after the algorithm has stabilised (10 vendoring tests pass
  without anyone touching `rewrite_for_vendoring`).
- **Drop `--expand-deep` to simplify the expand binary.** Devil's
  advocate's strongest case ("half of `expand/src/main.rs` exists for
  transitive proc-macros, the maintenance bill compounds across every
  nightly forever"). We're keeping it because rapier2d coverage is high
  value, but the counter is on the table.
- **Migrate to pure AST-rewrite + `prettyplease`.** Loses comment /
  blank-line / indentation fidelity that the project values. The hybrid
  pilot above is the compromise.
- **Split `rewrite_for_vendoring`'s 14 passes across files.** Premature;
  ordering is the contract and reads better in one place.
