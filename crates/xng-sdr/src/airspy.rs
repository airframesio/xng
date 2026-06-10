//! Native Airspy R2 / Mini capture via libairspy (BSD 3-clause).
//!
//! Direct FFI against the libairspy C API (declarations transcribed from
//! airspyone_host's airspy.h), so Airspy hardware works without a SoapyAirspy
//! shim. The library delivers float32 IQ on its own USB thread; samples cross
//! into [`crate::IqSource::read`] through [`crate::pump::SamplePump`].
//!
//! Rates: the device advertises a discrete list (R2: 2.5/10 Msps, Mini:
//! 3/6 Msps), but firmware >= 1.0.7 accepts arbitrary rates, which libairspy
//! forwards in kHz when the request is not in the list. We therefore attempt
//! whatever was asked and only report the advertised list if the device
//! refuses.

use crate::pump::{SamplePump, sample_channel};
use crate::{IqSource, SdrError};
use num_complex::Complex;
use std::os::raw::{c_int, c_void};
use std::sync::mpsc::SyncSender;

mod ffi {
    use std::os::raw::{c_int, c_void};

    pub enum Device {}

    #[repr(C)]
    pub struct Transfer {
        pub device: *mut Device,
        pub ctx: *mut c_void,
        pub samples: *mut c_void,
        pub sample_count: c_int,
        pub dropped_samples: u64,
        pub sample_type: c_int,
    }

    #[repr(C)]
    pub struct LibVersion {
        pub major: u32,
        pub minor: u32,
        pub revision: u32,
    }

    pub const AIRSPY_SAMPLE_FLOAT32_IQ: c_int = 0;
    pub const AIRSPY_SUCCESS: c_int = 0;

    pub type SampleBlockCb = unsafe extern "C" fn(*mut Transfer) -> c_int;

    extern "C" {
        pub fn airspy_lib_version(version: *mut LibVersion);
        pub fn airspy_list_devices(serials: *mut u64, count: c_int) -> c_int;
        pub fn airspy_open(device: *mut *mut Device) -> c_int;
        pub fn airspy_open_sn(device: *mut *mut Device, serial_number: u64) -> c_int;
        pub fn airspy_close(device: *mut Device) -> c_int;
        pub fn airspy_get_samplerates(device: *mut Device, buffer: *mut u32, len: u32) -> c_int;
        pub fn airspy_set_samplerate(device: *mut Device, samplerate: u32) -> c_int;
        pub fn airspy_set_sample_type(device: *mut Device, sample_type: c_int) -> c_int;
        pub fn airspy_set_freq(device: *mut Device, freq_hz: u32) -> c_int;
        pub fn airspy_set_vga_gain(device: *mut Device, value: u8) -> c_int;
        pub fn airspy_set_lna_agc(device: *mut Device, value: u8) -> c_int;
        pub fn airspy_set_mixer_agc(device: *mut Device, value: u8) -> c_int;
        pub fn airspy_set_linearity_gain(device: *mut Device, value: u8) -> c_int;
        pub fn airspy_set_rf_bias(device: *mut Device, value: u8) -> c_int;
        pub fn airspy_start_rx(device: *mut Device, cb: SampleBlockCb, ctx: *mut c_void) -> c_int;
        pub fn airspy_stop_rx(device: *mut Device) -> c_int;
    }
}

/// libairspy version triple (linkage smoke test; works without hardware).
pub fn lib_version() -> (u32, u32, u32) {
    let mut v = ffi::LibVersion { major: 0, minor: 0, revision: 0 };
    unsafe { ffi::airspy_lib_version(&mut v) };
    (v.major, v.minor, v.revision)
}

/// Serial numbers of connected Airspy devices.
pub fn enumerate() -> Result<Vec<u64>, SdrError> {
    let count = unsafe { ffi::airspy_list_devices(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(SdrError::Device(format!("airspy_list_devices: {count}")));
    }
    let mut serials = vec![0u64; count as usize];
    if count > 0 {
        let got = unsafe { ffi::airspy_list_devices(serials.as_mut_ptr(), count) };
        serials.truncate(got.max(0) as usize);
    }
    Ok(serials)
}

/// Tuner range from airspy.h: 24 MHz .. 1.75 GHz.
pub const FREQ_MIN_HZ: u64 = 24_000_000;
pub const FREQ_MAX_HZ: u64 = 1_750_000_000;

/// The sample rates a connected device advertises (briefly opens it).
pub fn device_rates(serial: Option<u64>) -> Result<Vec<u32>, SdrError> {
    let mut dev: *mut ffi::Device = std::ptr::null_mut();
    let ret = match serial {
        Some(sn) => unsafe { ffi::airspy_open_sn(&mut dev, sn) },
        None => unsafe { ffi::airspy_open(&mut dev) },
    };
    if ret != ffi::AIRSPY_SUCCESS {
        return Err(SdrError::Device(format!("airspy_open: error {ret}")));
    }
    let rates = supported_rates(dev);
    unsafe { ffi::airspy_close(dev) };
    Ok(rates)
}

/// Map a requested dB gain onto the R820T linearity-optimized composite gain
/// (22 steps spanning roughly 0..45 dB).
pub fn linearity_index(gain_db: f64) -> u8 {
    (gain_db / 45.0 * 21.0).round().clamp(0.0, 21.0) as u8
}

struct StreamCtx {
    tx: SyncSender<Vec<Complex<f32>>>,
}

unsafe extern "C" fn rx_callback(transfer: *mut ffi::Transfer) -> c_int {
    let t = &*transfer;
    if t.sample_type == ffi::AIRSPY_SAMPLE_FLOAT32_IQ && t.sample_count > 0 {
        let n = t.sample_count as usize;
        let samples = std::slice::from_raw_parts(t.samples as *const Complex<f32>, n);
        let ctx = &*(t.ctx as *const StreamCtx);
        // Drop the transfer if the consumer is behind; stalling the USB
        // thread only moves the overflow into the device.
        let _ = ctx.tx.try_send(samples.to_vec());
    }
    0
}

/// A single-channel RX capture from an Airspy R2/Mini.
pub struct AirspyIqSource {
    dev: *mut ffi::Device,
    // Referenced by the USB callback until airspy_stop_rx returns.
    _ctx: Box<StreamCtx>,
    pump: SamplePump,
    sample_rate: f64,
    center_freq_hz: u64,
}

// The device handle is only touched from Drop; samples arrive via the channel.
unsafe impl Send for AirspyIqSource {}

fn check(what: &str, ret: c_int) -> Result<(), SdrError> {
    if ret == ffi::AIRSPY_SUCCESS { Ok(()) } else { Err(SdrError::Device(format!("{what}: error {ret}"))) }
}

impl AirspyIqSource {
    /// Open (optionally by serial), tune, and start streaming.
    ///
    /// `gain_db` of 0..45 maps onto the linearity composite gain; hardware
    /// AGC (LNA+mixer, VGA mid-scale) when omitted.
    pub fn open(
        serial: Option<u64>,
        sample_rate: f64,
        center_freq_hz: u64,
        gain_db: Option<f64>,
        bias_t: bool,
    ) -> Result<Self, SdrError> {
        if !(FREQ_MIN_HZ..=FREQ_MAX_HZ).contains(&center_freq_hz) {
            return Err(SdrError::Config(format!(
                "airspy tunes 24 MHz .. 1.75 GHz; {center_freq_hz} Hz is outside"
            )));
        }
        let mut dev: *mut ffi::Device = std::ptr::null_mut();
        let ret = match serial {
            Some(sn) => unsafe { ffi::airspy_open_sn(&mut dev, sn) },
            None => unsafe { ffi::airspy_open(&mut dev) },
        };
        if ret != ffi::AIRSPY_SUCCESS {
            return Err(SdrError::Device(format!(
                "airspy_open: error {ret} (no Airspy R2/Mini connected{}?)",
                if serial.is_some() { " with that serial" } else { "" }
            )));
        }
        let guard = scopeguard(dev);

        check("set_sample_type", unsafe {
            ffi::airspy_set_sample_type(dev, ffi::AIRSPY_SAMPLE_FLOAT32_IQ)
        })?;

        let rate = sample_rate.round() as u32;
        if unsafe { ffi::airspy_set_samplerate(dev, rate) } != ffi::AIRSPY_SUCCESS {
            // Verified on a Mini running the final firmware (v1.0.0-rc10-6):
            // off-list rates are refused even though libairspy forwards
            // them, so the advertised list is the only safe guidance.
            let supported = supported_rates(dev);
            return Err(SdrError::Config(format!(
                "device refused {rate} S/s; use one of its supported rates: {}",
                supported.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
            )));
        }

        match gain_db {
            Some(g) => check("set_linearity_gain", unsafe {
                ffi::airspy_set_linearity_gain(dev, linearity_index(g))
            })?,
            None => {
                check("set_lna_agc", unsafe { ffi::airspy_set_lna_agc(dev, 1) })?;
                check("set_mixer_agc", unsafe { ffi::airspy_set_mixer_agc(dev, 1) })?;
                check("set_vga_gain", unsafe { ffi::airspy_set_vga_gain(dev, 13) })?;
            }
        }
        if bias_t {
            check("set_rf_bias", unsafe { ffi::airspy_set_rf_bias(dev, 1) })?;
        }
        check("set_freq", unsafe { ffi::airspy_set_freq(dev, center_freq_hz as u32) })?;

        let (tx, pump) = sample_channel();
        let ctx = Box::new(StreamCtx { tx });
        check("start_rx", unsafe {
            ffi::airspy_start_rx(dev, rx_callback, &*ctx as *const StreamCtx as *mut c_void)
        })?;

        std::mem::forget(guard);
        Ok(Self { dev, _ctx: ctx, pump, sample_rate, center_freq_hz })
    }
}

fn supported_rates(dev: *mut ffi::Device) -> Vec<u32> {
    let mut count: u32 = 0;
    if unsafe { ffi::airspy_get_samplerates(dev, &mut count, 0) } != ffi::AIRSPY_SUCCESS || count == 0 {
        return Vec::new();
    }
    let mut rates = vec![0u32; count as usize];
    if unsafe { ffi::airspy_get_samplerates(dev, rates.as_mut_ptr(), count) } != ffi::AIRSPY_SUCCESS {
        return Vec::new();
    }
    rates
}

/// Close `dev` on early-exit paths in `open` (defused with `mem::forget`).
fn scopeguard(dev: *mut ffi::Device) -> impl Drop {
    struct G(*mut ffi::Device);
    impl Drop for G {
        fn drop(&mut self) {
            unsafe { ffi::airspy_close(self.0) };
        }
    }
    G(dev)
}

impl Drop for AirspyIqSource {
    fn drop(&mut self) {
        unsafe {
            ffi::airspy_stop_rx(self.dev);
            ffi::airspy_close(self.dev);
        }
    }
}

impl IqSource for AirspyIqSource {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq_hz(&self) -> u64 {
        self.center_freq_hz
    }

    fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError> {
        self.pump.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linkage_and_enumeration_work_without_hardware() {
        let (major, _, _) = lib_version();
        assert!(major >= 1);
        // No device attached in CI: must still return cleanly.
        enumerate().expect("enumerate");
    }

    #[test]
    fn linearity_gain_mapping() {
        assert_eq!(linearity_index(0.0), 0);
        assert_eq!(linearity_index(45.0), 21);
        assert_eq!(linearity_index(100.0), 21);
        assert_eq!(linearity_index(-3.0), 0);
        assert_eq!(linearity_index(21.0), 10);
    }
}
