use std::{
    fmt::Display,
    io::{BufRead, Write},
    ops::{Add, AddAssign, Sub, SubAssign},
    str::FromStr,
};

use anyhow::{Context, bail};
use bot_common::{ReadFrom, SingleLine, WriteTo};

/// Tron has no per-match init payload. The alias exists so generic
/// code (the bot template, the runner) can name `<game>_defs::InitialInput`
/// uniformly across games — `()` already implements `ReadFrom` /
/// `WriteTo` as no-ops via `bot_common`'s blanket `()` impls.
pub type InitialInput = ();

#[derive(Debug, PartialEq, Eq, Copy, Clone, Hash, Default)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

impl Pos {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

macro_rules! impl_op {
    ($trait:ident, $method:ident, $op:tt) => {
        impl $trait<Pos> for Pos {
            type Output = Pos;
            fn $method(self, rhs: Pos) -> Pos {
                Pos {
                    x: self.x $op rhs.x,
                    y: self.y $op rhs.y,
                }
            }
        }

        impl $trait<Pos> for &Pos {
            type Output = Pos;
            fn $method(self, rhs: Pos) -> Pos {
                Pos {
                    x: self.x $op rhs.x,
                    y: self.y $op rhs.y,
                }
            }
        }

        impl $trait<&Pos> for Pos {
            type Output = Pos;
            fn $method(self, rhs: &Pos) -> Pos {
                Self {
                    x: self.x $op rhs.x,
                    y: self.y $op rhs.y,
                }
            }
        }

        impl $trait<&Pos> for &Pos {
            type Output = Pos;
            fn $method(self, rhs: &Pos) -> Pos {
                Pos {
                    x: self.x $op rhs.x,
                    y: self.y $op rhs.y,
                }
            }
        }
    };
}

impl_op!(Add, add, +);
impl_op!(Sub, sub, -);

macro_rules! impl_assign_op {
    ($trait:ident, $method:ident, $op:tt) => {
        impl $trait<Pos> for Pos {
            fn $method(&mut self, rhs: Pos) {
                self.x $op rhs.x;
                self.y $op rhs.y;
            }
        }

        impl $trait<&Pos> for Pos {
            fn $method(&mut self, rhs: &Pos) {
                self.x $op rhs.x;
                self.y $op rhs.y;
            }
        }
    };
}

impl_assign_op!(AddAssign, add_assign, +=);
impl_assign_op!(SubAssign, sub_assign, -=);

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct Line {
    pub start: Pos,
    pub end: Pos,
}

pub struct TurnInput {
    pub number_of_players: i32,
    pub player_number: i32,
    pub player_lines: Vec<Line>,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, PartialEq, Eq)]
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
}
