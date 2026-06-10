//! DSP primitives for xng's decode cores.
//!
//! Everything here is implemented from textbook DSP (harris, Lyons,
//! Proakis) and public standards documents — no code derived from
//! GPL-licensed decoders. See `docs/ARCHITECTURE.md` §6 (provenance rules).

pub mod channelizer;
pub mod checksum;
pub mod ddc;
pub mod fir;
pub mod nco;
pub mod window;

pub use channelizer::PfbChannelizer;
pub use ddc::Ddc;
pub use fir::{lowpass_taps, Fir};
pub use nco::Nco;

pub type IqSample = num_complex::Complex<f32>;
