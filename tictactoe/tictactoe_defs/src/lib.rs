use std::{
    fmt::{self, Display},
    io::{BufRead, Write},
    marker::PhantomData,
    str::FromStr,
};

use anyhow::{Context, bail};
use common::{Defs, ReadFrom, SingleLine, TurnResult, WireInput, WireInputFfi, WireOutput, WriteTo};

pub const BOARD_SIZE: usize = 3;
pub const BOARD_CELLS: usize = BOARD_SIZE * BOARD_SIZE;

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pos {
    pub row: i32,
    pub col: i32,
}

#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub enum Cell {
    Empty = 0,
    X = 1,
    O = 2,
}

impl Cell {
    /// Mark belonging to player 0 (`X`) or player 1 (`O`).
    pub fn for_player(p: u32) -> Cell {
        match p {
            0 => Cell::X,
            1 => Cell::O,
            _ => Cell::Empty,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInput {
    pub player_number: i32,
    pub board: [Cell; BOARD_CELLS],
}

pub struct TurnRef<'a> {
    pub player_number: i32,
    pub board: &'a [Cell; BOARD_CELLS],
}

// Fields are private — the only way to obtain a `TurnInputFFI<'a>` is
// `TurnInput::as_ffi`, which establishes the invariants relied on by `as_ref`:
//   1. `board` is a valid, properly-aligned pointer to a `[Cell; BOARD_CELLS]`.
//   2. The memory is live for `'a` (enforced by the lifetime + PhantomData).
#[repr(C)]
#[derive(Debug)]
pub struct TurnInputFFI<'a> {
    player_number: i32,
    board: *const Cell,
    _marker: PhantomData<&'a [Cell; BOARD_CELLS]>,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub struct TurnOutput {
    pub pos: Pos,
}

impl Default for TurnOutput {
    fn default() -> Self {
        TurnOutput {
            pos: Pos { row: 0, col: 0 },
        }
    }
}

/// Asserts `TurnOutput` satisfies the full bundled contract — see
/// [`common::WireOutput`].
impl WireOutput for TurnOutput {}

/// Bumped on any wire-type change. Plugins built against an older
/// `tictactoe_defs` export an older value; `PluginPlayer::load` reads it
/// through `abi_version()` and refuses mismatches before any UB-prone call
/// lands.
pub const ABI_VERSION: u32 = 1;

/// Marker type. Implementing [`common::Defs`] on it is the single line that
/// ratifies this crate's FFI surface — all of `WireInput`, `WireInputFfi`,
/// `WireOutput`, and `ABI_VERSION` are checked at this exact site.
pub struct Ffi;

impl Defs for Ffi {
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
    pub fn take_turn(input: TurnInputFFI<'_>) -> TurnResult<TurnOutput>;
    pub fn abi_version() -> u32;
}

// region: Wire-input impls
impl<'a> TurnInputFFI<'a> {
    pub fn player_number(&self) -> i32 {
        self.player_number
    }
}

impl WireInput for TurnInput {
    type Ffi<'a> = TurnInputFFI<'a>;
    type Ref<'a> = TurnRef<'a>;

    fn as_ffi(&self) -> TurnInputFFI<'_> {
        TurnInputFFI {
            player_number: self.player_number,
            board: self.board.as_ptr(),
            _marker: PhantomData,
        }
    }

    fn as_ref(&self) -> TurnRef<'_> {
        TurnRef {
            player_number: self.player_number,
            board: &self.board,
        }
    }
}

impl<'a> WireInputFfi<'a> for TurnInputFFI<'a> {
    type Ref = TurnRef<'a>;

    // Safe because every `TurnInputFFI<'a>` is constructed by `as_ffi`, which
    // establishes the invariants documented on the struct.
    fn as_ref(&self) -> TurnRef<'a> {
        TurnRef {
            player_number: self.player_number,
            board: unsafe { &*(self.board as *const [Cell; BOARD_CELLS]) },
        }
    }
}
// endregion: Wire-input impls

// region: Display impls
impl Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.row, self.col)
    }
}

impl Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Cell::Empty => ".",
            Cell::X => "X",
            Cell::O => "O",
        })
    }
}

fn write_board(board: &[Cell; BOARD_CELLS], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for row in 0..BOARD_SIZE {
        if row > 0 {
            writeln!(f)?;
        }
        for col in 0..BOARD_SIZE {
            write!(f, "{}", board[row * BOARD_SIZE + col])?;
        }
    }
    Ok(())
}

impl Display for TurnInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.player_number)?;
        write_board(&self.board, f)
    }
}

impl Display for TurnInputFFI<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.player_number)?;
        write_board(self.as_ref().board, f)
    }
}

impl Display for TurnOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.pos, f)
    }
}
// endregion: Display impls

// region: FromStr impls
impl FromStr for Pos {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (r, c) = s
            .trim()
            .split_once(' ')
            .with_context(|| format!("Failed parsing {s} as Pos"))?;
        Ok(Pos {
            row: r.parse()?,
            col: c.parse()?,
        })
    }
}

impl FromStr for Cell {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() != 1 {
            bail!("expected single char for Cell, got '{s}'");
        }
        Ok(match s.chars().next().unwrap() {
            '.' => Cell::Empty,
            'X' => Cell::X,
            'O' => Cell::O,
            other => bail!("invalid Cell char: {other}"),
        })
    }
}

fn parse_board_row(s: &str) -> anyhow::Result<[Cell; BOARD_SIZE]> {
    let chars: Vec<char> = s.trim().chars().collect();
    if chars.len() != BOARD_SIZE {
        bail!(
            "expected {BOARD_SIZE} chars in board row, got '{}'",
            s.trim()
        );
    }
    let mut row = [Cell::Empty; BOARD_SIZE];
    for (i, c) in chars.iter().enumerate() {
        row[i] = match c {
            '.' => Cell::Empty,
            'X' => Cell::X,
            'O' => Cell::O,
            other => bail!("invalid Cell char: {other}"),
        };
    }
    Ok(row)
}

impl FromStr for TurnInput {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::read_from(&mut s.as_bytes())
    }
}

impl FromStr for TurnOutput {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(TurnOutput { pos: s.parse()? })
    }
}
// endregion: FromStr impls

// region: SingleLine markers
impl SingleLine for Pos {}
impl SingleLine for Cell {}
impl SingleLine for TurnOutput {}
// endregion: SingleLine markers

// region: ReadFrom / WriteTo impls
impl ReadFrom for TurnInput {
    fn read_from(r: &mut impl BufRead) -> anyhow::Result<Self> {
        let mut header = String::new();
        r.read_line(&mut header)?;
        let player_number: i32 = header.trim().parse().context("Failed reading player_number")?;

        let mut board = [Cell::Empty; BOARD_CELLS];
        for row in 0..BOARD_SIZE {
            let mut buf = String::new();
            r.read_line(&mut buf)?;
            let parsed = parse_board_row(&buf)?;
            board[row * BOARD_SIZE..row * BOARD_SIZE + BOARD_SIZE].copy_from_slice(&parsed);
        }

        Ok(TurnInput {
            player_number,
            board,
        })
    }
}

impl WriteTo for TurnInput {
    fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.player_number)?;
        for row in 0..BOARD_SIZE {
            for col in 0..BOARD_SIZE {
                write!(w, "{}", self.board[row * BOARD_SIZE + col])?;
            }
            writeln!(w)?;
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
        let p: Pos = "1 2".parse()?;
        assert!(p == Pos { row: 1, col: 2 });
        Ok(())
    }

    #[test]
    fn display_pos() -> Result<()> {
        let p = Pos { row: 1, col: 2 };
        assert!(p.to_string() == "1 2");
        Ok(())
    }

    #[test]
    fn pos_round_trip() -> Result<()> {
        let p = Pos { row: 1, col: 2 };
        assert!(p == p.to_string().parse()?);
        Ok(())
    }

    #[test]
    fn parse_cell() -> Result<()> {
        assert!(Cell::Empty == ".".parse()?);
        assert!(Cell::X == "X".parse()?);
        assert!(Cell::O == "O".parse()?);
        assert!("Z".parse::<Cell>().is_err());
        assert!("XX".parse::<Cell>().is_err());
        Ok(())
    }

    #[test]
    fn display_cell() {
        assert!(Cell::Empty.to_string() == ".");
        assert!(Cell::X.to_string() == "X");
        assert!(Cell::O.to_string() == "O");
    }

    #[test]
    fn parse_turn_input() -> Result<()> {
        let input: TurnInput = "0\n...\n.X.\n..O".parse()?;
        assert!(input.player_number == 0);
        assert!(input.board[4] == Cell::X);
        assert!(input.board[8] == Cell::O);
        Ok(())
    }

    #[test]
    fn display_turn_input() {
        let mut board = [Cell::Empty; BOARD_CELLS];
        board[4] = Cell::X;
        board[8] = Cell::O;
        let input = TurnInput {
            player_number: 0,
            board,
        };
        assert!(input.to_string() == "0\n...\n.X.\n..O");
    }

    #[test]
    fn turn_input_round_trip() -> Result<()> {
        let mut board = [Cell::Empty; BOARD_CELLS];
        board[0] = Cell::X;
        board[4] = Cell::O;
        board[8] = Cell::X;
        let input = TurnInput {
            player_number: 1,
            board,
        };
        let parsed: TurnInput = input.to_string().parse()?;
        assert!(parsed == input);
        Ok(())
    }

    #[test]
    fn parse_turn_output() -> Result<()> {
        let out: TurnOutput = "1 2".parse()?;
        assert!(out.pos == Pos { row: 1, col: 2 });
        Ok(())
    }

    #[test]
    fn display_turn_output() {
        let out = TurnOutput {
            pos: Pos { row: 0, col: 1 },
        };
        assert!(out.to_string() == "0 1");
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
    fn turn_input_stdio_round_trip() -> Result<()> {
        let mut board = [Cell::Empty; BOARD_CELLS];
        board[2] = Cell::X;
        board[6] = Cell::O;
        let input = TurnInput {
            player_number: 0,
            board,
        };
        let parsed = stdio_round_trip(input.clone())?;
        assert!(parsed == input);
        Ok(())
    }

    #[test]
    fn ffi_round_trip() {
        let mut board = [Cell::Empty; BOARD_CELLS];
        board[3] = Cell::X;
        let input = TurnInput {
            player_number: 1,
            board,
        };
        let ffi = input.as_ffi();
        let view = ffi.as_ref();
        assert!(view.player_number == 1);
        assert!(view.board[3] == Cell::X);
        assert!(view.board[0] == Cell::Empty);
    }
}
