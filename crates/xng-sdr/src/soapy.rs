//! SoapySDR-backed device capture.
//!
//! SoapySDR is the multi-vendor abstraction (and the only route to SDRplay,
//! whose API is proprietary). A pure-Rust driver path (seify/rtl-sdr-rs) can
//! slot in behind [`crate::IqSource`] later without touching decode cores.

use crate::{IqSource, SdrError};
use num_complex::Complex;

/// Summary of an enumerated SoapySDR device.
#[derive(Debug, Clone)]
pub struct DeviceSummary {
    pub driver: String,
    pub label: String,
    pub args: String,
}

/// Enumerate available SoapySDR devices, optionally filtered by Soapy args
/// (e.g. `driver=rtlsdr`).
pub fn enumerate(filter: &str) -> Result<Vec<DeviceSummary>, SdrError> {
    let devices = soapysdr::enumerate(filter).map_err(|e| SdrError::Device(e.to_string()))?;
    Ok(devices
        .into_iter()
        .map(|args| DeviceSummary {
            driver: args.get("driver").unwrap_or("unknown").to_owned(),
            label: args.get("label").unwrap_or("").to_owned(),
            args: args.to_string(),
        })
        .collect())
}

/// Advertised RX sample-rate ranges for the device `args` selects, as
/// `(min, max, step)` tuples in Hz (`step == 0.0` means the range is
/// continuous — the common case for RTL-SDR/HackRF/SDRplay). Returns an empty
/// vec when the device can't be opened or reports nothing, so callers fall
/// back to the mode's plan rate. Opens the device briefly (no RX stream) and
/// closes it on return.
pub fn sample_rate_ranges(args: &str) -> Vec<(f64, f64, f64)> {
    let Ok(dev) = soapysdr::Device::new(args) else {
        return Vec::new();
    };
    dev.get_sample_rate_range(soapysdr::Direction::Rx, 0)
        .map(|ranges| ranges.iter().map(|r| (r.minimum, r.maximum, r.step)).collect())
        .unwrap_or_default()
}

/// A single-channel RX capture from a SoapySDR device.
pub struct SoapyIqSource {
    stream: soapysdr::RxStream<Complex<f32>>,
    sample_rate: f64,
    center_freq_hz: u64,
}

impl SoapyIqSource {
    /// Open a device by Soapy args (e.g. `driver=airspyhf`), tune it, and
    /// activate an RX stream on channel 0.
    pub fn open(args: &str, sample_rate: f64, center_freq_hz: u64, gain_db: Option<f64>) -> Result<Self, SdrError> {
        let dev = soapysdr::Device::new(args).map_err(|e| SdrError::Device(e.to_string()))?;
        let dir = soapysdr::Direction::Rx;
        dev.set_sample_rate(dir, 0, sample_rate)
            .map_err(|e| SdrError::Device(format!("set_sample_rate: {e}")))?;
        dev.set_frequency(dir, 0, center_freq_hz as f64, ())
            .map_err(|e| SdrError::Device(format!("set_frequency: {e}")))?;
        match gain_db {
            Some(g) => dev
                .set_gain(dir, 0, g)
                .map_err(|e| SdrError::Device(format!("set_gain: {e}")))?,
            None => {
                // Prefer hardware AGC where available.
                if dev.has_gain_mode(dir, 0).unwrap_or(false) {
                    let _ = dev.set_gain_mode(dir, 0, true);
                }
            }
        }
        let mut stream = dev
            .rx_stream::<Complex<f32>>(&[0])
            .map_err(|e| SdrError::Device(format!("rx_stream: {e}")))?;
        stream
            .activate(None)
            .map_err(|e| SdrError::Device(format!("activate: {e}")))?;
        Ok(Self { stream, sample_rate, center_freq_hz })
    }
}

impl Drop for SoapyIqSource {
    fn drop(&mut self) {
        // Deactivate before the stream closes: skipping this leaves some
        // drivers (rtlsdr) wedged for the next open — observed as
        // stream-read timeouts when sequential dwells reuse one dongle.
        let _ = self.stream.deactivate(None);
    }
}

impl IqSource for SoapyIqSource {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq_hz(&self) -> u64 {
        self.center_freq_hz
    }

    fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError> {
        self.stream
            .read(&mut [buf], 1_000_000)
            .map_err(|e| SdrError::Device(format!("stream read: {e}")))
    }
}
