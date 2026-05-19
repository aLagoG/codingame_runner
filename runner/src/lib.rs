use std::path::Path;

use libloading::{Library, Symbol};
use tron_defs::{TurnInput, TurnInputFFI, TurnOutput};

// HRTB: the loaded function must accept a `TurnInputFFI<'a>` for any lifetime
// the caller picks. Without `for<'a>` the symbol would be locked to a single
// lifetime (typically `'static`) and unusable for borrowed inputs.
type TakeTurn = for<'a> unsafe extern "C" fn(TurnInputFFI<'a>) -> TurnOutput;

pub struct Bot {
    // Field order matters: `take_turn` must drop before `_lib`. A raw fn
    // pointer's drop is a no-op, so this is mostly about not freeing the
    // dylib while a pointer into it is still considered "live".
    take_turn: TakeTurn,
    _lib: Library,
}

impl Bot {
    /// SAFETY: `path` must point to a bot dynamic library exporting `take_turn`
    /// with the signature `extern "C" fn(TurnInputFFI<'_>) -> TurnOutput` and
    /// upholding its UB contracts (no unwinding past the boundary, no
    /// dereferencing the input pointer past `number_of_players`, etc.).
    pub unsafe fn load(path: &Path) -> anyhow::Result<Self> {
        let lib = unsafe { Library::new(path) }?;
        let sym: Symbol<TakeTurn> = unsafe { lib.get(b"take_turn") }?;
        let take_turn = *sym;
        Ok(Bot {
            take_turn,
            _lib: lib,
        })
    }

    pub fn run_turn(&self, input: &TurnInput) -> TurnOutput {
        // `input.as_ffi()` yields `TurnInputFFI<'_>` whose lifetime is tied to
        // `&input`, so the borrow checker proves `input` outlives the call.
        unsafe { (self.take_turn)(input.as_ffi()) }
    }
}
