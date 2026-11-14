//! xng — next-generation multi-mode SDR decoder, as a library.
//!
//! The `xng` binary is a thin CLI over this crate. Other front ends (the
//! desktop app) embed the same decode runtime through [`engine`], so every
//! front end shares one decode path and one set of outputs.

pub mod beam;
pub mod bus;
pub mod commands;
pub mod engine;
pub mod freq;
pub mod outputs;
pub mod runtime;
pub mod satmap;
pub mod sdr_args;
pub mod sdr_open;
pub mod tui;
pub mod tuning;
