//! SDR capture and IQ replay sources.
//!
//! Everything downstream of this crate consumes the [`IqSource`] trait, so
//! decode cores are agnostic to whether samples come from hardware — SoapySDR
//! behind the `soapy` feature, or native Airspy backends behind `airspy`
//! (R2/Mini, libairspy) and `airspyhf` (HF+/Discovery, libairspyhf) — or from
//! recorded IQ files (always available — the basis for regression testing
//! decode cores against golden captures).

#[cfg(feature = "airspy")]
pub mod airspy;
#[cfg(feature = "airspyhf")]
pub mod airspyhf;
pub mod file;
#[cfg(any(feature = "airspy", feature = "airspyhf"))]
mod pump;
#[cfg(feature = "soapy")]
pub mod soapy;

pub use file::{FileIqSource, IqFormat};

use num_complex::Complex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SdrError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("end of stream")]
    EndOfStream,
    #[error("device error: {0}")]
    Device(String),
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// A source of complex baseband samples at a known rate and center frequency.
pub trait IqSource: Send {
    /// Sample rate in Hz.
    fn sample_rate(&self) -> f64;
    /// RF center frequency in Hz (0 for files without metadata).
    fn center_freq_hz(&self) -> u64;
    /// Read up to `buf.len()` samples. Returns the number of samples read;
    /// `Err(SdrError::EndOfStream)` when the source is exhausted.
    fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError>;
}
