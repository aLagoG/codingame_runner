//! Trait surface every CodinGame bot uses, deliberately kept tiny so
//! flattened bots stay vendor-clean for CG submission.
//!
//! Just three traits — `ReadFrom`, `WriteTo`, and the `SingleLine`
//! marker that auto-derives them for any `FromStr + Display` type.
//! Plus `()` impls for games whose `InitialInput` is empty.

use std::{
    fmt::Display,
    io::{self, BufRead, Write},
    str::FromStr,
};

// ============================================================
//  Wire-format primitives (stdio bots)
// ============================================================

/// Marker for types whose `Display`/`FromStr` impls produce/consume
/// exactly one line. Enables the blanket `ReadFrom` / `WriteTo` impls
/// below; opt out for multi-line types and hand-roll `ReadFrom` /
/// `WriteTo` instead.
pub trait SingleLine {}

pub trait ReadFrom: Sized {
    fn read_from(r: &mut impl BufRead) -> anyhow::Result<Self>;
}

impl<T> ReadFrom for T
where
    T: FromStr + SingleLine,
    T::Err: Into<anyhow::Error>,
{
    fn read_from(r: &mut impl BufRead) -> anyhow::Result<Self> {
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
    fn read_from(_: &mut impl BufRead) -> anyhow::Result<Self> {
        Ok(())
    }
}

impl WriteTo for () {
    fn write_to(&self, _: &mut impl Write) -> io::Result<()> {
        Ok(())
    }
}
