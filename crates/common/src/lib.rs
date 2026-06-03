//! Engine + runner-side machinery: `Game`, `Player`, `run_match`,
//! `Replay`. The companion `bot_common` crate has the (tiny) bot-
//! facing surface — kept separate so flattened bot submissions stay
//! vendor-clean for CodinGame. `common` is free to pull in heavier
//! deps (serde, tracing, …) since nothing here is ever vendored into
//! a bot.

pub mod engine;
