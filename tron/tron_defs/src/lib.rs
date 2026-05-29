// Treat `improper_ctypes` as an error. The `unsafe extern "C" { ... }` block
// below references `TurnInputFFI<'_>` and `TurnResult<TurnOutput>`; if either
// drops `#[repr(C)]` (or gains a non-FFI-safe field) the lint fires at the
// extern block — the closest thing Rust has to a "must be repr(C)" check.
#![deny(improper_ctypes)]

use std::{
    fmt::Display,
    io::{BufRead, Write},
    marker::PhantomData,
    str::FromStr,
};

use anyhow::{Context, bail};
use common::{
    Defs, NoInitialInput, NoInitialInputFfi, ReadFrom, SingleLine, TurnResult, WireInput,
    WireInputFfi, WireOutput, WriteTo,
};
use serde::{Deserialize, Serialize};

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize, Hash)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize)]
pub struct Line {
    pub start: Pos,
    pub end: Pos,
}

pub struct TurnInput {
    pub number_of_players: i32,
    pub player_number: i32,
    pub player_lines: Vec<Line>,
}

pub struct TurnRef<'a> {
    pub number_of_players: i32,
    pub player_number: i32,
    pub player_lines: &'a [Line],
}

// Fields are private — the only way to obtain a `TurnInputFFI<'a>` is
// `TurnInput::as_ffi`, which establishes the invariants relied on by `as_ref`:
//   1. `player_lines` is a valid, properly-aligned pointer to a contiguous
//      array of `Line`s.
//   2. The array has at least `number_of_players` elements.
//   3. The memory is live for `'a` (enforced by the lifetime + PhantomData).
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub struct TurnInputFFI<'a> {
    number_of_players: i32,
    player_number: i32,
    player_lines: *const Line,
    _marker: PhantomData<&'a [Line]>,
}

#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutput {
    pub direction: Direction,
}

impl Default for TurnOutput {
    fn default() -> Self {
        TurnOutput {
            direction: Direction::Down,
        }
    }
}

/// Asserts `TurnOutput` satisfies the full bundled contract — see
/// [`common::WireOutput`].
impl WireOutput for TurnOutput {}

/// Bumped on any wire-type change. Plugins built against an older `tron_defs`
/// export an older value; `PluginPlayer::load` reads it through `abi_version()`
/// and refuses mismatches before any UB-prone call lands.
pub const ABI_VERSION: u32 = 1;

/// Marker type. Implementing [`common::Defs`] on it is the single line that
/// ratifies this crate's FFI surface — all of `WireInput`, `WireInputFfi`,
/// `WireOutput`, and `ABI_VERSION` are checked at this exact site.
pub struct Ffi;

impl Defs for Ffi {
    type InitialInput = NoInitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    const ABI_VERSION: u32 = ABI_VERSION;
}

// `extern "C" { ... }` block: declares the FFI signatures bots must export.
// Used only by cbindgen as a reachability root for the header — no symbols
// are introduced into `_defs.rlib` (so no collision with the real ones the
// bot's `common::ffi_bot!` macro defines downstream). Keep in sync with the
// macro. `TurnResult` is generic over the per-game output; cbindgen
// monomorphises it into a concrete C++ struct.
unsafe extern "C" {
    pub fn initialize(input: NoInitialInputFfi<'_>);
    pub fn take_turn(input: TurnInputFFI<'_>) -> TurnResult<TurnOutput>;
    pub fn abi_version() -> u32;
}

// region: Wire-input impls
impl<'a> TurnInputFFI<'a> {
    pub fn number_of_players(&self) -> i32 {
        self.number_of_players
    }

    pub fn player_number(&self) -> i32 {
        self.player_number
    }
}

impl WireInput for TurnInput {
    type Ffi<'a> = TurnInputFFI<'a>;
    type Ref<'a> = TurnRef<'a>;

    fn as_ffi(&self) -> TurnInputFFI<'_> {
        assert!(self.player_lines.len() == self.number_of_players as usize);

        TurnInputFFI {
            number_of_players: self.number_of_players,
            player_number: self.player_number,
            player_lines: self.player_lines.as_ptr(),
            _marker: PhantomData,
        }
    }

    fn as_ref(&self) -> TurnRef<'_> {
        TurnRef {
            number_of_players: self.number_of_players,
            player_number: self.player_number,
            player_lines: &self.player_lines,
        }
    }
}

impl<'a> WireInputFfi<'a> for TurnInputFFI<'a> {
    type Ref = TurnRef<'a>;

    // Safe because every `TurnInputFFI<'a>` is constructed by `as_ffi`, which
    // establishes the three invariants documented on the struct.
    fn as_ref(&self) -> TurnRef<'a> {
        TurnRef {
            number_of_players: self.number_of_players,
            player_number: self.player_number,
            player_lines: unsafe {
                std::slice::from_raw_parts(self.player_lines, self.number_of_players as usize)
            },
        }
    }
}
// endregion: Wire-input impls

// region: Display impls
impl Display for Pos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.x, self.y)
    }
}

impl Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.start, self.end)
    }
}

impl Display for TurnInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.number_of_players, self.player_number)?;
        for line in &self.player_lines {
            write!(f, "\n{line}")?;
        }
        Ok(())
    }
}

impl Display for TurnInputFFI<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let view = self.as_ref();
        writeln!(f, "{} {}", view.number_of_players, view.player_number)?;
        for line in view.player_lines {
            writeln!(f, "{line}")?;
        }
        Ok(())
    }
}

impl Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Direction::Up => "UP",
            Direction::Down => "DOWN",
            Direction::Left => "LEFT",
            Direction::Right => "RIGHT",
        })
    }
}

impl Display for TurnOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.direction, f)
    }
}
// endregion: Display impls

// region: FromStr impls
impl FromStr for Pos {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (x, y) = s
            .trim()
            .split_once(" ")
            .with_context(|| format!("Failed parsing {s} as Pos"))?;
        Ok(Pos {
            x: x.parse()?,
            y: y.parse()?,
        })
    }
}

impl FromStr for Line {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (start, end) = s.split_at(
            s.match_indices(' ')
                .nth(1)
                .map(|(i, _)| i)
                .with_context(|| format!("Failed parsing {s} as Line"))?,
        );
        Ok(Line {
            start: start.parse()?,
            end: end.parse()?,
        })
    }
}

impl FromStr for TurnInput {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::read_from(&mut s.as_bytes())
    }
}

impl FromStr for Direction {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "UP" => Direction::Up,
            "DOWN" => Direction::Down,
            "LEFT" => Direction::Left,
            "RIGHT" => Direction::Right,
            _ => bail!("Unreconized direction {s}"),
        })
    }
}

impl FromStr for TurnOutput {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(TurnOutput {
            direction: s.parse()?,
        })
    }
}
// endregion: FromStr impls

// region: SingleLine markers
impl SingleLine for Pos {}
impl SingleLine for Line {}
impl SingleLine for Direction {}
impl SingleLine for TurnOutput {}
// endregion: SingleLine markers

// region: ReadFrom / WriteTo impls
impl ReadFrom for TurnInput {
    fn read_from(r: &mut impl BufRead) -> anyhow::Result<Self> {
        let mut header = String::new();
        r.read_line(&mut header)?;
        let (n, p) = header
            .trim()
            .split_once(' ')
            .context("Failed reading header")?;
        let number_of_players: i32 = n.parse()?;
        let player_number: i32 = p.parse()?;

        let mut player_lines = Vec::with_capacity(number_of_players as usize);
        for _ in 0..number_of_players {
            let mut buf = String::new();
            r.read_line(&mut buf)?;
            player_lines.push(buf.parse()?);
        }

        Ok(TurnInput {
            number_of_players,
            player_number,
            player_lines,
        })
    }
}

impl WriteTo for TurnInput {
    fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "{} {}", self.number_of_players, self.player_number)?;
        for line in &self.player_lines {
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}
// endregion: ReadFrom / WriteTo impls

#[cfg(test)]
mod test {
    use crate::*;
    use anyhow::Result;

    #[test]
    fn parse_pos() -> Result<()> {
        let mut _pos: Pos = "1 2".parse()?;
        _pos = " -1 -1 ".parse()?;
        Ok(())
    }

    #[test]
    fn display_pos() -> Result<()> {
        let pos = Pos { x: 1, y: 2 };
        assert!(pos.to_string() == "1 2");
        Ok(())
    }

    #[test]
    fn pos_round_trip() -> Result<()> {
        let pos = Pos { x: 1, y: 2 };
        assert!(pos == pos.to_string().parse()?);
        Ok(())
    }

    #[test]
    fn parse_line() -> Result<()> {
        let mut _line: Line = "1 2 3 4".parse()?;
        _line = "-1 -2 -3 -4".parse()?;
        Ok(())
    }

    #[test]
    fn display_line() -> Result<()> {
        let line = Line {
            start: Pos { x: 1, y: 2 },
            end: Pos { x: 3, y: 4 },
        };
        assert!(line.to_string() == "1 2 3 4");
        Ok(())
    }

    #[test]
    fn line_round_trip() -> Result<()> {
        let line = Line {
            start: Pos { x: 1, y: 2 },
            end: Pos { x: 3, y: 4 },
        };
        assert!(line == line.to_string().parse()?);
        Ok(())
    }

    #[test]
    fn parse_turn_input() -> Result<()> {
        let _input: TurnInput = "2 0\n1 2 3 4\n5 6 7 8".parse()?;
        Ok(())
    }

    #[test]
    fn display_turn_input() -> Result<()> {
        let input = TurnInput {
            number_of_players: 2,
            player_number: 0,
            player_lines: vec![
                Line {
                    start: Pos { x: 1, y: 2 },
                    end: Pos { x: 3, y: 4 },
                },
                Line {
                    start: Pos { x: 5, y: 6 },
                    end: Pos { x: 7, y: 8 },
                },
            ],
        };
        assert!(input.to_string() == "2 0\n1 2 3 4\n5 6 7 8");
        Ok(())
    }

    #[test]
    fn turn_input_round_trip() -> Result<()> {
        let input = TurnInput {
            number_of_players: 2,
            player_number: 0,
            player_lines: vec![
                Line {
                    start: Pos { x: 1, y: 2 },
                    end: Pos { x: 3, y: 4 },
                },
                Line {
                    start: Pos { x: 5, y: 6 },
                    end: Pos { x: 7, y: 8 },
                },
            ],
        };
        let parsed: TurnInput = input.to_string().parse()?;
        assert!(parsed.number_of_players == input.number_of_players);
        assert!(parsed.player_number == input.player_number);
        assert!(parsed.player_lines == input.player_lines);
        Ok(())
    }

    #[test]
    fn parse_direction() -> Result<()> {
        assert!(Direction::Up == "UP".parse()?);
        assert!(Direction::Down == "DOWN".parse()?);
        assert!(Direction::Left == "LEFT".parse()?);
        assert!(Direction::Right == " RIGHT ".parse()?);
        assert!("SIDEWAYS".parse::<Direction>().is_err());
        Ok(())
    }

    #[test]
    fn display_direction() -> Result<()> {
        assert!(Direction::Up.to_string() == "UP");
        assert!(Direction::Down.to_string() == "DOWN");
        assert!(Direction::Left.to_string() == "LEFT");
        assert!(Direction::Right.to_string() == "RIGHT");
        Ok(())
    }

    #[test]
    fn direction_round_trip() -> Result<()> {
        for d in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ] {
            let parsed: Direction = d.to_string().parse()?;
            assert!(d == parsed);
        }
        Ok(())
    }

    #[test]
    fn parse_turn_output() -> Result<()> {
        let output: TurnOutput = "UP".parse()?;
        assert!(output.direction == Direction::Up);
        Ok(())
    }

    #[test]
    fn display_turn_output() -> Result<()> {
        let output = TurnOutput {
            direction: Direction::Up,
        };
        assert!(output.to_string() == "UP");
        Ok(())
    }

    #[test]
    fn turn_output_round_trip() -> Result<()> {
        let output = TurnOutput {
            direction: Direction::Up,
        };
        assert!(output == output.to_string().parse()?);
        Ok(())
    }

    fn stdio_round_trip<T>(value: T) -> Result<T>
    where
        T: ReadFrom + WriteTo,
    {
        let mut buf = Vec::new();
        value.write_to(&mut buf)?;
        Ok(T::read_from(&mut buf.as_slice())?)
    }

    #[test]
    fn pos_stdio_round_trip() -> Result<()> {
        let pos = Pos { x: 1, y: 2 };
        assert!(pos == stdio_round_trip(Pos { x: 1, y: 2 })?);
        let _ = pos;
        Ok(())
    }

    #[test]
    fn line_stdio_round_trip() -> Result<()> {
        let line = Line {
            start: Pos { x: 1, y: 2 },
            end: Pos { x: 3, y: 4 },
        };
        let parsed = stdio_round_trip(Line {
            start: Pos { x: 1, y: 2 },
            end: Pos { x: 3, y: 4 },
        })?;
        assert!(line == parsed);
        Ok(())
    }

    #[test]
    fn direction_stdio_round_trip() -> Result<()> {
        for d in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ] {
            let mut buf = Vec::new();
            d.write_to(&mut buf)?;
            let parsed = Direction::read_from(&mut buf.as_slice())?;
            assert!(d == parsed);
        }
        Ok(())
    }

    #[test]
    fn turn_output_stdio_round_trip() -> Result<()> {
        let output = TurnOutput {
            direction: Direction::Right,
        };
        let parsed = stdio_round_trip(TurnOutput {
            direction: Direction::Right,
        })?;
        assert!(output == parsed);
        Ok(())
    }

    #[test]
    fn turn_input_stdio_round_trip() -> Result<()> {
        let input = TurnInput {
            number_of_players: 2,
            player_number: 0,
            player_lines: vec![
                Line {
                    start: Pos { x: 1, y: 2 },
                    end: Pos { x: 3, y: 4 },
                },
                Line {
                    start: Pos { x: 5, y: 6 },
                    end: Pos { x: 7, y: 8 },
                },
            ],
        };
        let mut buf = Vec::new();
        input.write_to(&mut buf)?;
        let parsed = TurnInput::read_from(&mut buf.as_slice())?;
        assert!(parsed.number_of_players == input.number_of_players);
        assert!(parsed.player_number == input.player_number);
        assert!(parsed.player_lines == input.player_lines);
        Ok(())
    }
}
