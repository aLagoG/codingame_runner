//! Engine + runner-side helpers, plus a re-export of `bot_common` for
//! callers that historically wrote `common::WireInput` and friends.
//!
//! Splits by audience:
//!   * `bot_common` (re-exported here) — the bot-facing surface
//!     (`ReadFrom`, `WriteTo`, `WireInput`, `Defs`, `ffi_bot!`, …).
//!     Tiny dep footprint so flattened bot submissions stay
//!     vendor-clean for CodinGame.
//!   * `engine` (this crate) — the runner-side machinery (`Game`,
//!     `Player`, `PluginPlayer`, `run_match`, `Replay`, …). Free to
//!     pull in heavy deps (libloading, serde, tracing, …) since
//!     nothing here is ever vendored into a bot submission.

pub mod engine;

// Re-export the bot-facing surface so engine-side code can continue
// referring to `common::WireInput`, `common::ReadFrom`, etc., without
// caring that the definitions moved to `bot_common`. Also covers the
// `ffi_bot!` macro indirectly: macros aren't part of `pub use`'s
// glob, but the few callers of `common::ffi_bot!` are bot crates
// that now depend on `bot_common` directly anyway.
pub use bot_common::{
    __set_counter_callback, BotStatus, CounterFn, Defs, NoInitialInput, NoInitialInputFfi,
    NoInitialInputRef, ReadFrom, SingleLine, TurnResult, WireInput, WireInputFfi, WireOutput,
    WriteTo, emit_counter, ffi_bot,
};
