pub mod engine;

pub use engine::{
    BotStatus, Defs, NoInitialInput, NoInitialInputFfi, NoInitialInputRef, TurnResult, WireInput,
    WireInputFfi, WireOutput,
};

use std::{
    fmt::Display,
    io::{self, BufRead, Write},
    str::FromStr,
};

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

// Output trait — takes any `Write` so the caller controls buffering.
//
// `io::stdout()` acquires its global mutex on every write call, and the
// returned handle is line-buffered on a TTY and block-buffered on a pipe, so
// raw `writeln!(io::stdout(), ...)` per line means one lock + (often) one
// syscall per line. The intended usage is to lock stdout once and wrap it in
// a `BufWriter` so all writes go through a single owned buffer:
//
//     let stdout = io::stdout().lock();
//     let mut out = io::BufWriter::new(stdout);
//     value.write_to(&mut out)?;
//     out.flush()?; // mandatory — buffered output is lost otherwise
//
// This gives line-by-line code without paying the per-line lock/syscall cost
// and without building an intermediate `String`.
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

/// Define the FFI surface for a bot dynamic library.
///
/// Two-arg form (most bots — games whose `Defs::InitialInput = ()` or that
/// don't care about init data):
///
/// ```ignore
/// // tron_rs/src/lib.rs
/// pub fn decide(turn: tron_defs::TurnRef<'_>) -> tron_defs::TurnOutput { … }
/// common::ffi_bot!(tron_defs::Ffi, decide);
/// ```
///
/// Three-arg form (games with non-trivial `InitialInput` whose bots want
/// to inspect or stash it):
///
/// ```ignore
/// fn on_init(init: <chess_defs::Initial as common::WireInput>::Ref<'_>) {
///     // store init in a `OnceCell`, etc.
/// }
/// common::ffi_bot!(chess_defs::Ffi, decide, on_init);
/// ```
///
/// Generates three `extern "C"` exports — `initialize`, `take_turn`,
/// `abi_version` — each wrapped in `catch_unwind` so a panic doesn't
/// unwind across the FFI boundary (UB).
#[macro_export]
macro_rules! ffi_bot {
    ($defs:ty, $decide:expr) => {
        // Two-arg form: default init handler ignores its argument.
        $crate::ffi_bot!($defs, $decide, |_| ());
    };
    ($defs:ty, $decide:expr, $init:expr) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn initialize(
            input: <<$defs as $crate::Defs>::InitialInput as $crate::WireInput>::Ffi<'_>,
        ) {
            // Bring the WireInputFfi trait into local scope so
            // `input.as_ref()` resolves without polluting the caller's
            // module-level namespace.
            use $crate::WireInputFfi as _;
            let _ = ::std::panic::catch_unwind(|| ($init)(input.as_ref()));
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn take_turn(
            input: <<$defs as $crate::Defs>::Input as $crate::WireInput>::Ffi<'_>,
        ) -> $crate::TurnResult<<$defs as $crate::Defs>::Output> {
            use $crate::WireInputFfi as _;
            match ::std::panic::catch_unwind(|| ($decide)(input.as_ref())) {
                Ok(output) => $crate::TurnResult {
                    status: $crate::BotStatus::Ok,
                    output,
                },
                Err(_) => $crate::TurnResult {
                    status: $crate::BotStatus::Panic,
                    output: ::std::default::Default::default(),
                },
            }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn abi_version() -> u32 {
            <$defs as $crate::Defs>::ABI_VERSION
        }
    };
}
