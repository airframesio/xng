//! Native Airspy HF+ / Discovery capture via libairspyhf (BSD 3-clause).
//!
//! Direct FFI against the libairspyhf C API (declarations transcribed from
//! the airspyhf repo's airspyhf.h). The HF+ is the workhorse HFDL receiver:
//! 9 kHz .. 31 MHz plus 60 .. 260 MHz, streaming float32 IQ at a discrete
//! set of rates (768/384/256/192 kS/s depending on firmware) — 768 kS/s
//! divides cleanly into every xng HF/VHF channel rate. The library performs
//! IQ correction and low-IF conversion internally, so samples arriving at the
//! callback are already calibrated baseband IQ.

use crate::pump::{SamplePump, sample_channel};
use crate::{IqSource, SdrError};
use num_complex::Complex;
use std::os::raw::{c_int, c_void};
use std::sync::mpsc::SyncSender;

mod ffi {
    use num_complex::Complex;
    use std::os::raw::{c_int, c_void};

    pub enum Device {}

    // airspyhf_complex_float_t is {float re; float im;} — layout-identical to
    // (repr(C)) Complex<f32>.
    #[repr(C)]
    pub struct Transfer {
        pub device: *mut Device,
        pub ctx: *mut c_void,
        pub samples: *mut Complex<f32>,
        pub sample_count: c_int,
        pub dropped_samples: u64,
    }

    #[repr(C)]
    pub struct LibVersion {
        pub major: u32,
        pub minor: u32,
        pub revision: u32,
    }

    pub const AIRSPYHF_SUCCESS: c_int = 0;

    pub type SampleBlockCb = unsafe extern "C" fn(*mut Transfer) -> c_int;

    extern "C" {
        pub fn airspyhf_lib_version(version: *mut LibVersion);
        pub fn airspyhf_list_devices(serials: *mut u64, count: c_int) -> c_int;
        pub fn airspyhf_open(device: *mut *mut Device) -> c_int;
        pub fn airspyhf_open_sn(device: *mut *mut Device, serial_number: u64) -> c_int;
        pub fn airspyhf_close(device: *mut Device) -> c_int;
        pub fn airspyhf_get_samplerates(device: *mut Device, buffer: *mut u32, len: u32) -> c_int;
        pub fn airspyhf_set_samplerate(device: *mut Device, samplerate: u32) -> c_int;
        pub fn airspyhf_set_freq(device: *mut Device, freq_hz: u32) -> c_int;
        pub fn airspyhf_set_hf_agc(device: *mut Device, flag: u8) -> c_int;
        pub fn airspyhf_set_hf_agc_threshold(device: *mut Device, flag: u8) -> c_int;
        pub fn airspyhf_set_hf_att(device: *mut Device, att_index: u8) -> c_int;
        pub fn airspyhf_set_hf_lna(device: *mut Device, flag: u8) -> c_int;
        pub fn airspyhf_start(device: *mut Device, cb: SampleBlockCb, ctx: *mut c_void) -> c_int;
        pub fn airspyhf_stop(device: *mut Device) -> c_int;
    }
}

/// libairspyhf version triple (linkage smoke test; works without hardware).
pub fn lib_version() -> (u32, u32, u32) {
    let mut v = ffi::LibVersion { major: 0, minor: 0, revision: 0 };
    unsafe { ffi::airspyhf_lib_version(&mut v) };
    (v.major, v.minor, v.revision)
}

/// Serial numbers of connected Airspy HF devices.
pub fn enumerate() -> Result<Vec<u64>, SdrError> {
    let count = unsafe { ffi::airspyhf_list_devices(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(SdrError::Device(format!("airspyhf_list_devices: {count}")));
    }
    let mut serials = vec![0u64; count as usize];
    if count > 0 {
        let got = unsafe { ffi::airspyhf_list_devices(serials.as_mut_ptr(), count) };
        serials.truncate(got.max(0) as usize);
    }
    Ok(serials)
}

/// The sample rates a connected device advertises (briefly opens it).
pub fn device_rates(serial: Option<u64>) -> Result<Vec<u32>, SdrError> {
    let mut dev: *mut ffi::Device = std::ptr::null_mut();
    let ret = match serial {
        Some(sn) => unsafe { ffi::airspyhf_open_sn(&mut dev, sn) },
        None => unsafe { ffi::airspyhf_open(&mut dev) },
    };
    if ret != ffi::AIRSPYHF_SUCCESS {
        return Err(SdrError::Device(format!("airspyhf_open: error {ret}")));
    }
    let rates = supported_rates(dev);
    unsafe { ffi::airspyhf_close(dev) };
    Ok(rates)
}

/// Manual gain settings derived from a requested dB value.
///
/// The HF+ front end has no conventional gain knob — only a 0..48 dB
/// attenuator in 6 dB steps plus a +6 dB preamp. We map `gain_db` onto that
/// scale so bigger numbers still mean more gain: 0 => full 48 dB attenuation,
/// 48 => no attenuation, above 48 => preamp on.
pub fn hf_gain_settings(gain_db: f64) -> (u8, u8) {
    let lna = gain_db > 48.0;
    let eff = (gain_db - if lna { 6.0 } else { 0.0 }).clamp(0.0, 48.0);
    let att_index = ((48.0 - eff) / 6.0).round() as u8;
    (lna as u8, att_index)
}

struct StreamCtx {
    tx: SyncSender<Vec<Complex<f32>>>,
}

unsafe extern "C" fn rx_callback(transfer: *mut ffi::Transfer) -> c_int {
    let t = &*transfer;
    if t.sample_count > 0 {
        let samples = std::slice::from_raw_parts(t.samples, t.sample_count as usize);
        let ctx = &*(t.ctx as *const StreamCtx);
        let _ = ctx.tx.try_send(samples.to_vec());
    }
    0
}

/// A single-channel RX capture from an Airspy HF+ / Discovery.
pub struct AirspyHfIqSource {
    dev: *mut ffi::Device,
    // Referenced by the USB callback until airspyhf_stop returns.
    _ctx: Box<StreamCtx>,
    pump: SamplePump,
    sample_rate: f64,
    center_freq_hz: u64,
}

// The device handle is only touched from Drop; samples arrive via the channel.
unsafe impl Send for AirspyHfIqSource {}

fn check(what: &str, ret: c_int) -> Result<(), SdrError> {
    if ret == ffi::AIRSPYHF_SUCCESS { Ok(()) } else { Err(SdrError::Device(format!("{what}: error {ret}"))) }
}

impl AirspyHfIqSource {
    /// Open (optionally by serial), tune, and start streaming.
    ///
    /// Hardware AGC when `gain_db` is omitted (the recommended mode); a value
    /// selects attenuator/preamp via [`hf_gain_settings`].
    pub fn open(
        serial: Option<u64>,
        sample_rate: f64,
        center_freq_hz: u64,
        gain_db: Option<f64>,
        bias_t: bool,
    ) -> Result<Self, SdrError> {
        let mut dev: *mut ffi::Device = std::ptr::null_mut();
        let ret = match serial {
            Some(sn) => unsafe { ffi::airspyhf_open_sn(&mut dev, sn) },
            None => unsafe { ffi::airspyhf_open(&mut dev) },
        };
        if ret != ffi::AIRSPYHF_SUCCESS {
            return Err(SdrError::Device(format!(
                "airspyhf_open: error {ret} (no Airspy HF+ connected{}?)",
                if serial.is_some() { " with that serial" } else { "" }
            )));
        }
        let guard = scopeguard(dev);

        let rate = sample_rate.round() as u32;
        if unsafe { ffi::airspyhf_set_samplerate(dev, rate) } != ffi::AIRSPYHF_SUCCESS {
            let supported = supported_rates(dev);
            return Err(SdrError::Config(format!(
                "device refused {rate} S/s; supported rates: {}",
                supported.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
            )));
        }

        match gain_db {
            None => {
                check("set_hf_agc", unsafe { ffi::airspyhf_set_hf_agc(dev, 1) })?;
                check("set_hf_agc_threshold", unsafe { ffi::airspyhf_set_hf_agc_threshold(dev, 0) })?;
            }
            Some(g) => {
                let (lna, att) = hf_gain_settings(g);
                check("set_hf_agc", unsafe { ffi::airspyhf_set_hf_agc(dev, 0) })?;
                check("set_hf_lna", unsafe { ffi::airspyhf_set_hf_lna(dev, lna) })?;
                check("set_hf_att", unsafe { ffi::airspyhf_set_hf_att(dev, att) })?;
            }
        }
        if bias_t {
            // airspyhf_set_bias_tee needs libairspyhf >= 1.8 (Ranger); the
            // widely-packaged 1.6.x lacks the symbol, and HF+ models have no
            // bias tee at all.
            return Err(SdrError::Config(
                "the airspyhf backend does not support a bias tee (HF+ hardware has none)".into(),
            ));
        }
        check("set_freq", unsafe { ffi::airspyhf_set_freq(dev, center_freq_hz as u32) })?;

        let (tx, pump) = sample_channel();
        let ctx = Box::new(StreamCtx { tx });
        check("start", unsafe {
            ffi::airspyhf_start(dev, rx_callback, &*ctx as *const StreamCtx as *mut c_void)
        })?;

        std::mem::forget(guard);
        Ok(Self { dev, _ctx: ctx, pump, sample_rate, center_freq_hz })
    }
}

fn supported_rates(dev: *mut ffi::Device) -> Vec<u32> {
    let mut count: u32 = 0;
    if unsafe { ffi::airspyhf_get_samplerates(dev, &mut count, 0) } != ffi::AIRSPYHF_SUCCESS || count == 0 {
        return Vec::new();
    }
    let mut rates = vec![0u32; count as usize];
    if unsafe { ffi::airspyhf_get_samplerates(dev, rates.as_mut_ptr(), count) } != ffi::AIRSPYHF_SUCCESS {
        return Vec::new();
    }
    rates
}

/// Close `dev` on early-exit paths in `open` (defused with `mem::forget`).
fn scopeguard(dev: *mut ffi::Device) -> impl Drop {
    struct G(*mut ffi::Device);
    impl Drop for G {
        fn drop(&mut self) {
            unsafe { ffi::airspyhf_close(self.0) };
        }
    }
    G(dev)
}

impl Drop for AirspyHfIqSource {
    fn drop(&mut self) {
        unsafe {
            ffi::airspyhf_stop(self.dev);
            ffi::airspyhf_close(self.dev);
        }
    }
}

impl IqSource for AirspyHfIqSource {
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
        enumerate().expect("enumerate");
    }

    #[test]
    fn hf_gain_mapping() {
        assert_eq!(hf_gain_settings(54.0), (1, 0)); // preamp, no attenuation
        assert_eq!(hf_gain_settings(48.0), (0, 0)); // no attenuation
        assert_eq!(hf_gain_settings(24.0), (0, 4)); // 24 dB attenuation
        assert_eq!(hf_gain_settings(0.0), (0, 8)); // full 48 dB attenuation
        assert_eq!(hf_gain_settings(-10.0), (0, 8));
        assert_eq!(hf_gain_settings(100.0), (1, 0));
    }
}
