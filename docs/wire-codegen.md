# Wire-Format Codegen (Design Note)

**Status**: deferred. Today we hand-port the wire format to both Rust (`_defs/src/lib.rs`) and C++ (`_defs/include/<name>_defs.h` + `_defs/include/<name>_defs_io.h`) per game ("option B"). All three files are hand-written and kept in sync by hand. This doc records why a schema-driven codegen ("option E") is the right move once the count of games grows, and exactly what it would look like, so future-us doesn't re-derive it.

**Trigger to build**: when game count exceeds ~5–8 and any of the following starts hurting:
- Editing the wire format in three places (Rust `_defs/src/lib.rs` + C++ `_defs.h` + `_defs_io.h`) feels like the source of a real bug, not just busywork.
- A wire-format change ships in Rust but the C++ port is forgotten (drift).
- Adding a third language (Python, Go) for CodinGame deployment becomes interesting.

**Historical note**: at one point `_defs.h` was cbindgen-generated from the Rust `_defs/src/lib.rs` while `_defs_io.h` was hand-written. cbindgen was dropped along with FFI removal — both headers are now hand-written. The "three sources of truth" cost below counts each header separately as a result.

## Why not off-the-shelf (option D)

Every mainstream cross-language schema language (protobuf, Cap'n Proto, FlatBuffers, Thrift, MessagePack) emits either a binary format or its own opinionated text encoding (`field_name: value\n`-style). CodinGame's wire format is bespoke text — `1 2 3 4\n5 6 7 8` on three lines. No off-the-shelf schema can describe it.

The parser-generator angle (custom grammar → emit parsers in many languages) splits along language lines too: **ANTLR4** has no production Rust target, **bison/flex** is C/C++ only, **pest/peg/lalrpop** are Rust-only, **tree-sitter** is for syntax highlighting. Nothing writes one schema and emits matching Rust + C++ parsers for an arbitrary text format.

So a custom DSL is structurally the only viable path.

## Architecture

```
games/tron/defs/
├── Cargo.toml
├── wire.toml             ← single source of truth for fields + I/O format
├── build.rs              ← thin wrapper over the wire_codegen crate
├── include/
│   ├── tron_defs.h       ← codegen output: type declarations
│   └── tron_defs_io.h    ← codegen output: parse/format helpers
└── src/
    └── lib.rs            ← include!(generated.rs) + hand-written impls
```

One new workspace crate, `wire_codegen`, contains all the heavy lifting. Each `_defs/build.rs` becomes:

```rust
fn main() {
    wire_codegen::generate("wire.toml", "tron_defs");
}
```

To guarantee C++ bot crates can `#include` the generated headers before their own build script runs, the `_defs` Cargo.toml would re-add `links = "<name>_defs"` (dropped when cbindgen left). C++ bot crates already depend on `_defs` implicitly via cgio_build's include-dir lookup, so the order falls out naturally once `links =` is in place.

## DSL design

The grammar of CodinGame I/O is narrow. Four primitives cover ~95% of plausible games:

| `io` value | Shape | Example | Used by |
|---|---|---|---|
| `tokens_one_line` | Fields space-separated on one line. | `1 2 3 4` | `Pos`, `Line`, `TurnOutput` (tron) |
| `chars_packed` | One variant char per slot, no separator. | `.X.` | tic-tac-toe board row |
| `header_then_repeated` | Header line + N body items, count from a header field. | tron `TurnInput` | most "world state" inputs |
| `enum_as_string` | Enum variant ↔ stringified name. | `UP`/`DOWN`/... | `Direction` (tron) |

Plus `custom` for the one-off game whose format doesn't fit.

### Sample schema (`tron/tron_defs/wire.toml`)

```toml
[Direction]
kind     = "enum"
repr     = "u8"
io       = "enum_as_string"
variants = [
    { name = "Up",    io = "UP" },
    { name = "Down",  io = "DOWN" },
    { name = "Left",  io = "LEFT" },
    { name = "Right", io = "RIGHT" },
]

[Pos]
kind   = "struct"
repr_c = true
serde  = true
io     = "tokens_one_line"
fields = [
    { name = "x", type = "i32" },
    { name = "y", type = "i32" },
]

[Line]
kind   = "struct"
repr_c = true
io     = "tokens_one_line"
fields = [
    { name = "start", type = "Pos" },
    { name = "end",   type = "Pos" },
]

[TurnInput]
kind   = "struct"
io     = { kind = "header_then_repeated", header = ["number_of_players", "player_number"], repeated = "player_lines", count = "number_of_players" }
fields = [
    { name = "number_of_players", type = "i32" },
    { name = "player_number",     type = "i32" },
    { name = "player_lines",      type = "Vec<Line>" },
]

[TurnOutput]
kind   = "struct"
repr_c = true
serde  = true
io     = "tokens_one_line"
fields = [
    { name = "direction", type = "Direction" },
]
```

~40 lines. Captures fields, layout, FFI shape, serde participation, and wire format — for both languages.

## Generated outputs

**`OUT_DIR/generated.rs`** (included via `src/lib.rs`):

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pos { pub x: i32, pub y: i32 }

impl Display for Pos {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.x, self.y)
    }
}

impl FromStr for Pos {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut toks = s.split_whitespace();
        Ok(Pos {
            x: toks.next().context("missing x")?.parse()?,
            y: toks.next().context("missing y")?.parse()?,
        })
    }
}

impl SingleLine for Pos {}

// …same pattern for Line, Direction (with enum table), TurnOutput.
// TurnInput gets a hand-rolled ReadFrom/WriteTo that reads the header
// then loops `number_of_players` times reading Line.
```

**`include/tron_defs_io.h`**:

```cpp
#include "tron_defs.h"
#include <iostream>
#include <string>
#include <vector>

namespace cgio {

inline std::istream& operator>>(std::istream& in, Pos& v)        { return in >> v.x >> v.y; }
inline std::ostream& operator<<(std::ostream& out, const Pos& v) { return out << v.x << ' ' << v.y; }

// …Line, Direction (string↔enum table), TurnOutput

struct TurnInput {
    int32_t number_of_players;
    int32_t player_number;
    std::vector<Line> player_lines;
};

inline std::istream& operator>>(std::istream& in, TurnInput& v) {
    in >> v.number_of_players >> v.player_number;
    v.player_lines.resize(v.number_of_players);
    for (auto& line : v.player_lines) in >> line;
    return in;
}

}  // namespace cgio
```

## `wire_codegen` crate internals

Roughly 600–1000 lines of Rust split into three modules.

**Schema parser** (~150 lines, `serde` + `toml`). Loads `wire.toml` into a typed `Schema`. Validates cross-references (`count = "number_of_players"` must match a sibling field). Errors out clearly on missing types.

**Rust emitter** (~300–500 lines, `quote` + `prettyplease`). Walks the schema, emits struct definitions and `Display` / `FromStr` / `ReadFrom` / `WriteTo` impls per `io` primitive. `SingleLine` markers attach to anything that's `tokens_one_line` or `enum_as_string`.

**C++ emitter** (~200–300 lines, plain string templates — no AST library needed). Emits `operator<<`/`operator>>` overloads + owning C++ struct definitions where the FFI mirror isn't enough (Rust `Vec<Line>` → C++ `std::vector<Line>`).

The public API is a single function:

```rust
pub fn generate(schema_path: &str, crate_name: &str) {
    let schema = Schema::load(schema_path);
    write_rust(&schema, &out_dir().join("generated.rs"));
    write_cpp(&schema, &include_dir().join(format!("{crate_name}_io.h")));
    println!("cargo::rerun-if-changed={schema_path}");
}
```

## Migration plan from B (hand-port today)

1. Build `wire_codegen` and exercise it on a brand-new throwaway game (say, "rps" — rock-paper-scissors). Iterate on the DSL until the four primitives feel right.
2. Port **tron** first — small format, easy to validate. Adds the `header_then_repeated` primitive. Delete the manual `tron_defs_io.h` and the hand-written Rust impls. Verify both transports round-trip identically.
3. Port **fantastic_bits** — adds the per-tick entity list (kind-tagged rows + a state column). Delete the manual `fantastic_bits_defs_io.h` and corresponding Rust impls. Verify.
4. From game #3 onward, schema-first. Manual impls stay only behind the `io = "custom"` escape hatch.

## Known limitations / open questions

- **rust-analyzer doesn't see `include!(generated.rs)` until after a build.** Editing a fresh game has no completion for `Pos`/`Line` etc. until `cargo build` runs once. Workaround: keep a stale committed `generated.rs.snapshot` for IDE consumption, updated by CI. Adds noise but is opt-in.
- **Schema typos fail at codegen, not Rust compile time.** A bad `count = "..."` reference panics in `build.rs`, which is less ergonomic than `cannot find field`. Mitigate by validating early in `Schema::load` with anyhow + good context.
- **The DSL is a maintained surface.** New formats (CSV-like, JSON-shaped, char delimiters other than space) will force grammar growth. Plan: never extend without seeing two real games that need it; until then, use `io = "custom"`.
- **`#[repr(C)]` and `serde` derives are per-type opt-in flags in the schema.** Two flags now; if more cross-cutting concerns appear (Hash, Default, …) the schema's "derives" become a list, not booleans.
- **No support yet for non-stdin transport.** The codegen targets line-buffered stdio. If a future game uses something exotic (binary, framed, length-prefixed), the schema needs another `io` primitive.

## Estimated payoff at 20 games

| | Upfront | Per-game | 20-game total | Source of truth |
|---|---|---|---|---|
| B (current) | 0 | ~30 lines Rust + ~30 lines C++ | ~1200 lines, 40 places to edit | 2 (must stay in sync) |
| E (this doc) | ~800 lines codegen | ~30 lines `wire.toml` | ~800 + 600 = ~1400 lines, 20 schemas | 1 |

Break-even around 8 games. With 20 planned, E saves ~600 lines of duplication and (more importantly) makes Rust/C++ drift impossible.

## What this doc commits us to

Nothing yet — option B is in place and works. This doc exists so the next person (probably us, in three months) doesn't have to re-evaluate D and rediscover that nothing off the shelf fits. When the trigger hits, follow the migration plan above.
