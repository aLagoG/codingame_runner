//! C++ bot for tron — see `bot.cpp` at the crate root.
//!
//! The Rust surface is intentionally empty: this crate exists so
//! `cargo build -p tron_v1_cpp` produces a `cdylib` (loaded by the runner)
//! with `bot.cpp` compiled and force-loaded in through `cc-rs` +
//! `build.rs`.
