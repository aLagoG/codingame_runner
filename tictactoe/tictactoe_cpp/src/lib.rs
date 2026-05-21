//! C++ bot for tic-tac-toe — see `bot.cpp` at the crate root.
//!
//! The Rust surface is intentionally empty: this crate exists so
//! `cargo build -p tictactoe_cpp` produces a `cdylib` (loaded by the
//! runner) with `bot.cpp` compiled and force-loaded in through `cc-rs` +
//! `build.rs`.
