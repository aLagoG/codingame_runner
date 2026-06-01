//! Trait + macro surface every CodinGame bot uses, deliberately kept
//! tiny so flattened bots stay vendor-clean for CG submission.
//!
//! What lives here:
//!   * Wire-format traits (`ReadFrom`, `WriteTo`, `SingleLine`) for
//!     subprocess bots reading stdin / writing stdout.
//!   * FFI surface (`WireInput`, `WireInputFfi`, `WireOutput`,
//!     `NoInitialInput`*, `Defs`, `TurnResult`, `BotStatus`,
//!     `CounterFn`) for plugin bots.
//!   * `ffi_bot!` macro that emits the four `extern "C"` symbols
//!     every plugin must export.
//!   * Counter-callback machinery (`__set_counter_callback`,
//!     `emit_counter`) for the optional runner-side perf-counter
//!     pipe.
//!
//! What does NOT live here (intentionally):
//!   * `Game` / `FfiGame` / `Player` / `PluginPlayer` / `run_match`
//!     / `Replay` / `RunConfig` / `PlayerError` / `MatchError` /
//!     `GameRng` — engine-side machinery, kept in `crates/common`.
//!
//! `crates/common` re-exports everything in this crate, so engine-
//! side callers can keep writing `common::WireInput` etc.

use std::{
    ffi::c_char,
    fmt::Display,
    io::{self, BufRead, Write},
    marker::PhantomData,
    str::FromStr,
    sync::atomic::{AtomicPtr, Ordering},
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

// ============================================================
//  FFI return shape
// ============================================================

/// Status byte returned by every bot's `take_turn` FFI call. `Ok`
/// means `TurnResult::output` is valid; `Panic` means the bot's
/// `catch_unwind` shim intercepted a panic and `output` is
/// placeholder data the runner must ignore.
#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum BotStatus {
    Ok = 0,
    Panic = 1,
}

/// FFI return type of every bot's `take_turn`. Generic over the per-
/// game `O` (the game's `TurnOutput`), monomorphised by cbindgen
/// into a concrete C++ struct per game.
#[repr(C)]
#[derive(Debug)]
pub struct TurnResult<O> {
    pub status: BotStatus,
    pub output: O,
}

// ============================================================
//  Wire-input contract
// ============================================================

/// Contract on each game's owned per-tick input struct (the
/// `TurnInput` in each `_defs` crate).
///
/// Pairs the owned type with two views:
///   * `Ffi<'a>` — `#[repr(C)]` mirror sent across the plugin boundary.
///   * `Ref<'a>` — borrowed view bots read inside `decide(...)`.
///
/// Supertraits `ReadFrom + WriteTo` are what the engine uses to ferry
/// the input through subprocess pipes; the trait method pair is what
/// the FFI path uses. Implementing this trait is what makes a game's
/// `TurnInput` usable end-to-end. The cross-trait
/// `Ffi<'a>::Ref = Self::Ref<'a>` bound guarantees the same borrowed
/// type comes out of both `as_ref` paths.
pub trait WireInput: ReadFrom + WriteTo {
    type Ffi<'a>: WireInputFfi<'a, Ref = Self::Ref<'a>>
    where
        Self: 'a;

    type Ref<'a>
    where
        Self: 'a;

    fn as_ffi(&self) -> Self::Ffi<'_>;
    fn as_ref(&self) -> Self::Ref<'_>;
}

/// FFI-side companion to [`WireInput`].
pub trait WireInputFfi<'a> {
    type Ref;

    /// SAFETY: the impl relies on the invariants the FFI struct
    /// documents (pointers properly aligned, lengths in range,
    /// lifetime live). The trait wraps that in a safe method because
    /// every FFI struct's only constructor (`WireInput::as_ffi`)
    /// establishes them.
    fn as_ref(&self) -> Self::Ref;
}

// ============================================================
//  Sentinel `NoInitialInput`
// ============================================================

/// Sentinel `InitialInput` for games that don't ferry per-player data
/// at match start. Real games either use this (typical) or define
/// their own `WireInput`-implementing struct. One padding byte makes
/// the type non-zero-sized — without it, the `improper_ctypes` lint
/// would fire on the per-game extern block because zero-sized types
/// aren't FFI-safe.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoInitialInput {
    _padding: u8,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoInitialInputFfi<'a> {
    _padding: u8,
    _marker: PhantomData<&'a NoInitialInput>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoInitialInputRef<'a> {
    _marker: PhantomData<&'a NoInitialInput>,
}

impl WireInput for NoInitialInput {
    type Ffi<'a> = NoInitialInputFfi<'a>;
    type Ref<'a> = NoInitialInputRef<'a>;

    fn as_ffi(&self) -> NoInitialInputFfi<'_> {
        NoInitialInputFfi {
            _padding: 0,
            _marker: PhantomData,
        }
    }

    fn as_ref(&self) -> NoInitialInputRef<'_> {
        NoInitialInputRef {
            _marker: PhantomData,
        }
    }
}

impl<'a> WireInputFfi<'a> for NoInitialInputFfi<'a> {
    type Ref = NoInitialInputRef<'a>;

    fn as_ref(&self) -> NoInitialInputRef<'a> {
        NoInitialInputRef {
            _marker: PhantomData,
        }
    }
}

impl ReadFrom for NoInitialInput {
    fn read_from(_r: &mut impl BufRead) -> anyhow::Result<Self> {
        Ok(NoInitialInput::default())
    }
}

impl WriteTo for NoInitialInput {
    fn write_to(&self, _w: &mut impl Write) -> io::Result<()> {
        Ok(())
    }
}

// ============================================================
//  Wire-output contract
// ============================================================

/// Bundled contract on each game's `TurnOutput`. Implementing this is
/// the single line that asserts — at the `_defs` crate site — that
/// the type satisfies every requirement the rest of the system will
/// eventually need:
///
///   * `ReadFrom + WriteTo` — stdio between runner and subprocess bots.
///   * `Default` — placeholder the `ffi_bot!` macro stores on the panic path.
///   * `'static` — no borrowed fields.
///
/// Replay-side serialization deliberately *doesn't* require `Serialize`
/// here — the runner serializes outputs as their wire-format text in
/// `crates/common`'s `Replay`, not as serde objects, which keeps this
/// trait (and the per-game `TurnOutput`) free of `serde_derive` and
/// therefore vendorable for CodinGame submission.
///
/// No blanket impl: write `impl WireOutput for TurnOutput {}`
/// explicitly so missing pieces fail at *this* line instead of three
/// crates downstream.
pub trait WireOutput: ReadFrom + WriteTo + Default + 'static {}

// ============================================================
//  Per-game `Defs` marker
// ============================================================

/// Crate-level contract for a `_defs` crate: implement on a unit
/// marker (conventionally `Ffi`) to ratify a complete FFI surface.
///
/// ```ignore
/// pub struct Ffi;
/// impl bot_common::Defs for Ffi {
///     type InitialInput = NoInitialInput;
///     type Input = TurnInput;
///     type Output = TurnOutput;
///     const ABI_VERSION: u32 = ABI_VERSION;
/// }
/// ```
///
/// The single `impl Defs` line forces the compiler to check every
/// required trait at this exact site — not three crates downstream.
pub trait Defs {
    type InitialInput: WireInput;
    type Input: WireInput;
    type Output: WireOutput;

    /// Bumped on any wire-type change. The runner reads this at load
    /// time via the plugin's `abi_version()` symbol and refuses
    /// mismatches before any UB-prone call.
    const ABI_VERSION: u32;
}

// ============================================================
//  Counter callback (Rust-bot perf pipe)
// ============================================================

/// FFI signature for the counter callback the runner registers via a
/// bot's `set_counter_callback` export.
pub type CounterFn = unsafe extern "C" fn(*const c_char, f64);

// Each loaded plugin links its own copy of `bot_common` (as an rlib),
// so `COUNTER_CALLBACK` is a per-plugin static — no cross-plugin
// interference even though we use a global. The bot's
// `set_counter_callback` (generated by `ffi_bot!`) stores the
// runner-supplied function pointer here; `emit_counter` reads it
// lock-free on every call and short-circuits when null.
static COUNTER_CALLBACK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Internal. Called by the `set_counter_callback` symbol that
/// `ffi_bot!` generates. Don't call from bot code — use the macro.
#[doc(hidden)]
pub fn __set_counter_callback(cb: Option<CounterFn>) {
    let ptr = match cb {
        Some(f) => f as *mut (),
        None => std::ptr::null_mut(),
    };
    COUNTER_CALLBACK.store(ptr, Ordering::Release);
}

/// Emit a counter to the runner if one's registered, otherwise a
/// no-op. Cheap when off (one atomic load + null check).
///
/// `key` must not contain interior NULs; if it does the counter is
/// silently dropped (bad keys aren't worth crashing the bot).
pub fn emit_counter(key: &str, value: f64) {
    let ptr = COUNTER_CALLBACK.load(Ordering::Acquire);
    if ptr.is_null() {
        return;
    }
    // SAFETY: only ever set from a `CounterFn` value in
    // `__set_counter_callback`, so the transmute round-trips.
    let f: CounterFn = unsafe { std::mem::transmute(ptr) };
    let Ok(cstr) = std::ffi::CString::new(key) else {
        return;
    };
    // SAFETY: `cstr.as_ptr()` is valid for the call; the callback
    // copies the contents out before returning.
    unsafe { f(cstr.as_ptr(), value) };
}

// ============================================================
//  `ffi_bot!` macro — generates the four `extern "C"` exports
// ============================================================

/// Define the FFI surface for a bot dynamic library.
///
/// Two-arg form (most bots — games whose `Defs::InitialInput =
/// NoInitialInput` or that don't care about init data):
///
/// ```ignore
/// pub fn decide(turn: tron_defs::TurnRef<'_>) -> tron_defs::TurnOutput { … }
/// bot_common::ffi_bot!(tron_defs::Ffi, decide);
/// ```
///
/// Three-arg form (games with non-trivial `InitialInput` whose bots
/// want to inspect or stash it):
///
/// ```ignore
/// fn on_init(init: <chess_defs::Initial as bot_common::WireInput>::Ref<'_>) {
///     // store init in a `OnceCell`, etc.
/// }
/// bot_common::ffi_bot!(chess_defs::Ffi, decide, on_init);
/// ```
///
/// Generates four `extern "C"` exports — `initialize`, `take_turn`,
/// `abi_version`, `set_counter_callback` — each wrapped in
/// `catch_unwind` so a panic doesn't unwind across the FFI boundary
/// (UB).
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

        /// Counter-callback hook. The runner passes a `CounterFn`
        /// pointer when `tournament --counters` is set; we stash it
        /// in `bot_common`'s per-plugin static so subsequent
        /// `bot_common::emit_counter("key", value)` calls forward to
        /// the runner. Bots that don't care about counters simply
        /// don't call `emit_counter`.
        #[unsafe(no_mangle)]
        pub extern "C" fn set_counter_callback(cb: ::std::option::Option<$crate::CounterFn>) {
            $crate::__set_counter_callback(cb);
        }
    };
}
