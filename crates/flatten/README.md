# cargo-flatten

Flatten a Rust crate's source tree into a single `.rs` file.

Useful when you've written a small Rust program (single crate, few or no
external deps) and want to share it as one file — gist, paste, attach to
a bug report — that someone can compile directly.

## What it does

Reads the crate's entry source file (`src/lib.rs`, `src/main.rs`,
`src/bin/<name>.rs`, `examples/<name>.rs`, ...), recursively inlines every
external `mod foo;` declaration, and writes the result as one annotated
`.rs` file. Each inlined block carries a `// === src/foo.rs ===` separator
so a reader can navigate the output.

## What it does NOT do (by default)

- **Vendor external dependencies.** Anything from `Cargo.toml`
  `[dependencies]` stays as a `use foo::…` reference. The flat file still
  needs those crates resolvable when compiled. See `--vendor` below.
- **Expand proc-macros.** Generated items from `#[derive(...)]`,
  `#[tokio::main]`, `paste!{}`, etc. stay as macro calls. See `--expand`
  below.
- **Evaluate `#[cfg(...)]`.** Cfg-gated mods whose files don't exist are
  warn-skipped (the line is preserved verbatim). Cfg-gated mods that do
  resolve are inlined with the cfg attribute kept on the inline block, so
  the flat output respects the gate at compile time. (Inside `--vendor`,
  cargo `feature = "X"` predicates *are* evaluated against the user's
  resolved feature set; compiler-set cfgs like `target_os`, `unix`,
  `debug_assertions` are preserved.)

### Cross-target portability

The flat output is **target-portable**: vendor on macOS, run on Linux,
or vice versa. Compiler-set predicates (`target_os`, `target_family`,
`target_arch`, `target_env`, `target_endian`, `target_pointer_width`,
`target_vendor`, `target_has_atomic`, `target_feature`, the
`unix`/`windows` shorthands, `debug_assertions`, `test`, `proc_macro`,
`panic`) flow through verbatim and rustc evaluates them at the user's
build time.

Per-target file selection (mio's `mod selector;` with epoll/kqueue/poll
candidates, socket2's `mod sys;` with sys/unix.rs and sys/windows.rs)
emits ALL existing candidates as separate cfg-gated `mod NAME { ... }`
blocks; the user's compile picks one. Whole files / whole deps gated
by `#![cfg(target_os = "X")]` (windows-sys' `mod Wdk;`,
crossterm_winapi) are inlined unconditionally; the inner cfg flows
through and gates the contents at user time. Sibling-import
injections referring to deps with such inner cfgs get cfg-gated with
the same predicate.

The one limitation: build-script `--cfg=NAME` directives (rustix's
`apple` from build.rs detecting macOS) are baked at vendor time
because build.rs ran on the host. Deps that rely on build-script
cfgs behave as if those cfgs match the vendoring host.

See [`ROADMAP.md`](./ROADMAP.md) for what's planned and what's
deliberately out of scope.

## Quick start

```sh
# Build it
cargo install --path .

# Flatten the current crate; writes ./<crate-name>.rs
cargo flatten

# Or as a direct binary, with a different target
cargo-flatten path/to/crate --bin server -o server.rs

# Pipe to your clipboard
cargo flatten --stdout | pbcopy

# Just check that everything resolves, don't write
cargo flatten --check
```

## Vendoring deps with `--vendor`

`--vendor` inlines transitive dependencies as `pub mod <name>` blocks at
the crate root, so the flat output compiles without a Cargo project.
Refuses (in strict mode, the default) on deps that can't be vendored —
proc-macros, build scripts, native libraries, etc.

```sh
# Vendor every dep into a single file
cargo flatten --vendor

# Skip a specific dep (must be provided via Cargo.toml when compiling)
cargo flatten --vendor --external clap_derive --external thiserror-impl

# Skip a dep AND its entire transitive cone (avoids two copies of shared deps)
cargo flatten --vendor --external serde --external-deep

# Get a read-only audit of what would vendor cleanly
cargo flatten --vendor-report
```

See [`VENDORING.md`](./VENDORING.md) and [`EXTERNAL.md`](./EXTERNAL.md)
for the algorithm details.

## Inlining proc-macros with `--expand` / `--expand-deep`

`--expand` runs the user crate's source through a separate nightly binary
(`cargo-flatten-expand`, in `expand/`) that links rustc internals. It
inlines third-party proc-macro expansions (`#[derive(Serialize)]`,
`#[tokio::main]`, `paste!{}`, etc.) and strips the proc-macro crate from
the externalized set. Stdlib macros (`println!`, `vec!`,
`#[derive(Debug)]`) are left as macro calls so the flat output stays
readable.

`--expand-deep` extends the same treatment to *vendored deps*, not just
the user crate. Implemented as a `RUSTC_WRAPPER` shim around
`cargo build`, so cargo handles each dep's compilation context (extern
flags, edition, features, build-script cfgs) for free.

```sh
# Inline proc-macro derives in just the user crate
cargo flatten --vendor --expand

# Same, but also rewrite vendored deps' source
cargo flatten --vendor --expand --expand-deep
```

**Setup** (one-time per nightly toolchain): build the expander binary.
Its `expand/rust-toolchain.toml` pins nightly + the `rustc-dev` and
`llvm-tools-preview` components, so rustup auto-installs everything.

```sh
cargo build --manifest-path expand/Cargo.toml
```

The main binary discovers the expander at `expand/target/debug/cargo-flatten-expand`
relative to its compile-time `CARGO_MANIFEST_DIR`, on `$PATH`, or via
`$CARGO_FLATTEN_EXPAND`.

**Current limitations**:

- Statement-level `#[cfg(...)]` attrs (e.g. `#[cfg(feature = "dim3")] let
  x = nalgebra::vector![...]`) are not evaluated; both arms of a
  cfg-mutually-exclusive let-stmt pair survive into the flat output.
  Item-level cfg eval works correctly. Workaround: refactor to
  cfg-gated functions.
- Build-script-generated content (e.g. thiserror's `private.rs` reached
  via `include!(env!("OUT_DIR"))`) isn't captured. cargo runs the build
  script during the wrapper invocation, but the OUT_DIR contents aren't
  copied into the dump dir. Workaround: `--external thiserror` and
  add it to your `Cargo.toml`.
- The helper-attribute stripper uses a heuristic (any non-builtin
  single-segment attr on an item with a non-stdlib derive). Over-stripping
  is theoretically possible if a proc-macro DOES want to keep a
  non-builtin attr it doesn't define.
- `serde` ≥ 1.0.220 hits the `serde_core` split — it uses
  `#[path = "core/crate_root.rs"]` to reach a sibling crate's source,
  which our wrapper doesn't yet dump into the override directory.
  Workaround for now: pin `serde = "1.0.219"` or earlier.
- Crates with build scripts (`serde`, `libc`, `zerocopy`, etc.) can't
  be vendored — `--external` them and add to your `Cargo.toml`. With
  `--expand-deep`, *proc-macro* deps are auto-externalized, but
  build-script-having runtime deps still need explicit `--external`.

## What works in --expand-deep, today

Real-world crates we test (`tests/integration.rs`'s `real_vendor_*_
with_expand_deep_compiles`):

| Crate                              | Status   | Externals required             |
|------------------------------------|----------|--------------------------------|
| **clap** (`#[derive(Parser)]`)     | ✓ runs   | none                           |
| **serde** + serde_json + derive    | ✓ runs   | `--external serde` (build script) |
| **rand** (`thread_rng`, `gen`)     | ✓ runs   | `--external libc --external zerocopy` |
| **regex** (`captures`)             | ✓ runs   | none                           |
| **glam** (`Vec3::length`)          | ✓ runs   | none                           |
| **nalgebra** (`Vector3::norm`)     | ✓ runs   | none                           |
| **rapier2d**                       | ✓ runs   | none                           |
| **bytemuck** (`#[derive(Pod)]`)    | ✓ runs   | none                           |
| **tokio** (`#[tokio::main]`)       | partial  | vendors + Attr macro inlines + `cfg_io_driver!`-wrapped mods inline; remaining errors are libc/mio path resolution inside inlined content |
| **ratatui** (Block widget)         | partial  | vendors clean; `mod backend` cfg-skips cascade into missing-symbol errors (clearly diagnosed at flatten time) |
| **axum** (`Router` + `#[async_trait]`) | partial | vendors clean (async_trait Attr fragmentation fixed); same downstream path-resolution gaps as tokio |

## Target selection

| Flag             | Picks                                                 |
| ---------------- | ----------------------------------------------------- |
| (none)           | the unique bin if there's exactly one, else the lib   |
| `--lib`          | the library target                                    |
| `--bin NAME`     | the named bin (auto-discovered or in `[[bin]]`)       |
| `--example NAME` | the named example (`examples/` or `[[example]]`)      |
| `--test NAME`    | the named integration test (`tests/` or `[[test]]`)   |

If `Cargo.toml` is absent, only auto-selection works (chooses between
`src/main.rs` and `src/lib.rs`).

## How resolution works

The scanner uses `syn` to find external `mod NAME;` declarations and
records the byte range of the trailing `;` via
`proc_macro2::Span::byte_range()`. The splicer then replaces just that
`;` with ` { /* inlined */ }`, leaving visibility, attributes, and
`mod NAME` verbatim.

File lookup follows the
[Rust Reference's mod rules](https://doc.rust-lang.org/reference/items/modules.html):
`lib.rs` / `main.rs` / `mod.rs` are *mod-rs/root* files (submods resolve in
the file's own directory); other `.rs` files are *non-mod-rs* (submods
resolve in `<containing_dir>/<file_stem>/`). `#[path = "..."]` on a
top-level mod is honored (relative to the *containing file's directory*,
not the submod search dir — this is the surprising bit).

## Development

```sh
cargo test                    # 50+ tests, ~0.2s
cargo clippy --all-targets    # silent
cargo insta review            # accept snapshot changes
```

The smoke tests in `tests/integration.rs` check three real crates
(`itoa`, `anyhow`, `bitflags`) cloned into the gitignored
`test-crates/`. They silently skip when absent. To rehydrate:

```sh
mkdir -p test-crates && cd test-crates
git clone --depth 1 https://github.com/dtolnay/itoa.git
git clone --depth 1 https://github.com/dtolnay/anyhow.git
git clone --depth 1 https://github.com/bitflags/bitflags.git
```

## License

(none specified yet)
