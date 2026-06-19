//! PPM loopback using published Mode S frames as payloads.

use num_complex::Complex;
use xng_mode_adsb::modulate::frame_iq;
use xng_mode_adsb::AdsbDecoder;

const ID_FRAME: [u8; 14] = [
    0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98,
];
const POS_FRAME: [u8; 14] = [
    0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC, 0x28, 0x63, 0xA7,
];

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

#[test]
fn decodes_two_published_frames_at_2msps() {
    let spu = 2;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); 1000];
    iq.extend(frame_iq(&ID_FRAME, spu, 0.6));
    iq.extend(vec![Complex::new(0.0, 0.0); 2000]);
    iq.extend(frame_iq(&POS_FRAME, spu, 0.4));
    iq.extend(vec![Complex::new(0.0, 0.0); 1000]);
    let mut noise = Noise(0xdead_beef_0bad_cafe);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }

    let mut dec = AdsbDecoder::new(2_000_000.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(777) {
        frames.extend(dec.process(chunk));
    }

    assert_eq!(frames.len(), 2, "expected both frames: {frames:?}");
    assert_eq!(frames[0].icao, 0x4840D6);
    assert_eq!(frames[0].callsign.as_deref(), Some("KLM1023"));
    assert_eq!(frames[1].icao, 0x40621D);
    assert_eq!(frames[1].altitude_ft, Some(38_000));
}

// XM-1: every decoded frame carries a finite noise floor + SNR, and the SNR
// falls when more noise is mixed into the same signal. We deliberately do NOT
// assert an absolute dBFS value against dump1090 (no calibrated numeric oracle
// for the floor) — only the measurement's internal consistency + ordering.
#[test]
fn frame_snr_is_finite_and_drops_with_more_noise() {
    let build = |noise_amp: f32| -> Vec<Complex<f32>> {
        let mut iq = vec![Complex::new(0.0f32, 0.0f32); 1000];
        iq.extend(frame_iq(&POS_FRAME, 2, 0.6));
        iq.extend(vec![Complex::new(0.0, 0.0); 2000]);
        let mut noise = Noise(0x1234_5678_9abc_def0);
        for s in &mut iq {
            *s += Complex::new(noise.next() * noise_amp, noise.next() * noise_amp);
        }
        iq
    };
    let decode = |iq: &[Complex<f32>]| {
        let mut dec = AdsbDecoder::new(2_000_000.0).unwrap();
        let mut frames = Vec::new();
        for chunk in iq.chunks(777) {
            frames.extend(dec.process(chunk));
        }
        frames
    };

    let quiet = decode(&build(0.01));
    assert!(!quiet.is_empty(), "frame decodes at low noise");
    let f = &quiet[0];
    assert!(f.level_dbfs.is_finite() && f.noise_dbfs.is_finite(), "{f:?}");
    let snr_quiet = f.level_dbfs - f.noise_dbfs;
    assert!(snr_quiet > 0.0, "signal sits above the floor: {snr_quiet}");

    let noisy = decode(&build(0.06));
    let g = noisy.first().expect("frame still decodes at higher noise");
    let snr_noisy = g.level_dbfs - g.noise_dbfs;
    assert!(snr_noisy < snr_quiet, "more noise → lower SNR: {snr_noisy} vs {snr_quiet}");
}

#[test]
fn works_at_higher_sample_rates() {
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); 500];
    iq.extend(frame_iq(&ID_FRAME, 8, 0.5)); // 8 MS/s
    iq.extend(vec![Complex::new(0.0, 0.0); 500]);

    let mut dec = AdsbDecoder::new(8_000_000.0).unwrap();
    let frames = dec.process(&iq);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].callsign.as_deref(), Some("KLM1023"));
}

#[test]
fn rejects_unsupported_rates() {
    // 2.4 MS/s (non-integer samples/µs) runs the fractional path now.
    assert!(AdsbDecoder::new(2_400_000.0).is_ok());
    assert!(AdsbDecoder::new(1_000_000.0).is_err());
}

/// Native 2.4 MS/s (12 samples per 5 µs): synthesize at 12 MS/s
/// (integer modulator grid) and decimate by 5 — exact, no resampler.
#[test]
fn decodes_at_2400ksps_fractional_path() {
    let mut hi = vec![Complex::new(0.0f32, 0.0f32); 6000];
    hi.extend(frame_iq(&ID_FRAME, 12, 0.6));
    hi.extend(vec![Complex::new(0.0, 0.0); 12000]);
    hi.extend(frame_iq(&POS_FRAME, 12, 0.4));
    hi.extend(vec![Complex::new(0.0, 0.0); 6000]);
    let mut iq: Vec<Complex<f32>> = hi.into_iter().step_by(5).collect();
    let mut noise = Noise(0x0123_4567_89ab_cdef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }

    let mut dec = AdsbDecoder::new(2_400_000.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(777) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 2, "expected both frames: {frames:?}");
    assert_eq!(frames[0].callsign.as_deref(), Some("KLM1023"));
    assert_eq!(frames[1].altitude_ft, Some(38_000));
}
