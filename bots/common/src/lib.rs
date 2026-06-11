//! Trait surface every CodinGame bot uses, deliberately kept tiny so
//! flattened bots stay vendor-clean for CG submission.
//!
//! The three traits — `ReadFrom`, `WriteTo`, and the `SingleLine`
//! marker that auto-derives them for any `FromStr + Display` type —
//! plus `()` impls for games whose `InitialInput` is empty, plus the
//! [`BotError`] type that every bot-side parsing helper produces.

use std::{
    fmt::{self, Display},
    io::{self, BufRead, Write},
    num::{ParseFloatError, ParseIntError},
    str::FromStr,
};

// ============================================================
//  Error type
// ============================================================

/// The error type bot-side parsing returns. Two variants:
///   * `Io` — the engine pipe died (EOF, broken pipe, malformed
///     UTF-8 from a non-stdio reader, …).
///   * `Parse` — we read a line but couldn't interpret it.
///
/// `From` impls cover the everyday sources — `io::Error`,
/// `ParseIntError`, `ParseFloatError`, and `&str` / `String` for
/// "missing field"-style messages — so per-game `FromStr` impls
/// stay one `?` per fallible call:
///
/// ```ignore
/// let (x, y) = s.split_once(' ').ok_or("Pos: missing space")?;
/// Ok(Pos { x: x.parse()?, y: y.parse()? })
/// ```
///
/// Vendored into every bundled bot — picking the lightest semantically-
/// honest enum keeps the cost ≈40 lines (vs the ~2,700 anyhow used to
/// add).
#[derive(Debug)]
pub enum BotError {
    Io(io::Error),
    Parse(String),
}

impl From<io::Error> for BotError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ParseIntError> for BotError {
    fn from(e: ParseIntError) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<ParseFloatError> for BotError {
    fn from(e: ParseFloatError) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<&str> for BotError {
    fn from(s: &str) -> Self {
        Self::Parse(s.into())
    }
}

impl From<String> for BotError {
    fn from(s: String) -> Self {
        Self::Parse(s)
    }
}

impl Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => Display::fmt(e, f),
            Self::Parse(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for BotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(_) => None,
        }
    }
}

pub type BotResult<T> = Result<T, BotError>;

// ============================================================
//  Parsing helpers
// ============================================================

/// Pull the next whitespace-delimited field off `it` or fail with a
/// "missing <field>" `BotError`. Used by per-game `FromStr` impls
/// over `SplitWhitespace` iterators.
pub fn next_field<'a>(it: &mut std::str::SplitWhitespace<'a>, field: &str) -> BotResult<&'a str> {
    it.next().ok_or_else(|| format!("missing {field}").into())
}

/// Like [`next_field`] but also `.parse::<i32>()`s the field.
pub fn next_i32(it: &mut std::str::SplitWhitespace, field: &str) -> BotResult<i32> {
    next_field(it, field)?.parse().map_err(Into::into)
}

// ============================================================
//  Wire-format primitives (stdio bots)
// ============================================================

/// Marker for types whose `Display`/`FromStr` impls produce/consume
/// exactly one line. Enables the blanket `ReadFrom` / `WriteTo` impls
/// below; opt out for multi-line types and hand-roll `ReadFrom` /
/// `WriteTo` instead.
pub trait SingleLine {}

pub trait ReadFrom: Sized {
    fn read_from(r: &mut impl BufRead) -> BotResult<Self>;
}

impl<T> ReadFrom for T
where
    T: FromStr + SingleLine,
    T::Err: Into<BotError>,
{
    fn read_from(r: &mut impl BufRead) -> BotResult<Self> {
        let mut s = String::new();
        r.read_line(&mut s)?;
        s.parse().map_err(Into::into)
    }
}

/// Output trait — takes any `Write` so the caller controls buffering.
///
/// `io::stdout()` acquires its global mutex on every write call, and
/// the returned handle is line-buffered on a TTY and block-buffered
/// on a pipe, so raw `writeln!(io::stdout(), ...)` per line means one
/// lock + (often) one syscall per line. The intended usage is to lock
/// stdout once and wrap it in a `BufWriter` so all writes go through
/// a single owned buffer:
///
/// ```ignore
/// let stdout = io::stdout().lock();
/// let mut out = io::BufWriter::new(stdout);
/// value.write_to(&mut out)?;
/// out.flush()?; // mandatory — buffered output is lost otherwise
/// ```
pub trait WriteTo {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()>;
}

impl<T> WriteTo for T
where
    T: Display + SingleLine,
{
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{self}")
    }
}

// `()` impls — useful for games whose `InitialInput` is empty.
impl ReadFrom for () {
    fn read_from(_: &mut impl BufRead) -> BotResult<Self> {
        Ok(())
    }
}

impl WriteTo for () {
    fn write_to(&self, _: &mut impl Write) -> io::Result<()> {
        Ok(())
    }
}
