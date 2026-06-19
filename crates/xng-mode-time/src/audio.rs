//! Shared audio front-end DSP for the time-signal decoders.
//!
//! Both flagship decoders (CHU AFSK, WWV/WWVH 100 Hz BCD) work on an audio
//! envelope recovered from an AM carrier, so the common pieces live here:
//!
//! - [`am_envelope`] — coherent-magnitude AM demod (`|x|`) producing real
//!   audio samples, with a slow DC-removal high-pass so the audio is centred.
//! - [`Goertzel`] — a single-bin DFT tone-power estimator (the classic
//!   Goertzel recurrence), used by the WWV 100 Hz subcarrier detector and the
//!   CHU AFSK mark/space discriminator.
//! - [`Biquad`] — a direct-form-II transposed biquad, with [`Biquad::bandpass`]
//!   and [`Biquad::lowpass`] designers (RBJ cookbook), used for the CHU
//!   1900–2350 Hz AFSK passband and the WWV envelope smoothing.
//!
//! All textbook DSP (RBJ audio-EQ cookbook, Goertzel). No external code.

use num_complex::Complex;
use std::f64::consts::PI;

/// AM-demodulate complex IQ to real audio: take the carrier magnitude `|x|`
/// (envelope) and remove its DC component with a one-pole high-pass so the
/// audio swings around zero. `dc_alpha` sets the high-pass corner (smaller =
/// lower corner; ~0.001 keeps tones above a few Hz).
pub fn am_envelope(iq: &[Complex<f32>], dc_alpha: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(iq.len());
    let mut dc = 0.0f32;
    for &x in iq {
        let env = x.norm();
        dc += dc_alpha * (env - dc);
        out.push(env - dc);
    }
    out
}

/// Single-bin Goertzel tone-power estimator. Feed `add` per audio sample;
/// after `block` samples call [`Goertzel::power`] for the in-band energy and
/// [`Goertzel::reset`] to start the next block. (A sliding per-sample form is
/// not needed here — both decoders integrate over fixed windows.)
#[derive(Debug, Clone)]
pub struct Goertzel {
    coeff: f32,
    s_prev: f32,
    s_prev2: f32,
    n: usize,
}

impl Goertzel {
    /// Tune to `freq_hz` at `sample_rate`.
    pub fn new(freq_hz: f64, sample_rate: f64) -> Self {
        let w = 2.0 * PI * freq_hz / sample_rate;
        Self {
            coeff: (2.0 * w.cos()) as f32,
            s_prev: 0.0,
            s_prev2: 0.0,
            n: 0,
        }
    }

    /// Accumulate one audio sample.
    #[inline]
    pub fn add(&mut self, x: f32) {
        let s = x + self.coeff * self.s_prev - self.s_prev2;
        self.s_prev2 = self.s_prev;
        self.s_prev = s;
        self.n += 1;
    }

    /// In-band power of the block accumulated so far (Goertzel magnitude²).
    pub fn power(&self) -> f32 {
        self.s_prev * self.s_prev + self.s_prev2 * self.s_prev2
            - self.coeff * self.s_prev * self.s_prev2
    }

    /// Number of samples accumulated since the last reset.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Clear the integrator for the next block.
    pub fn reset(&mut self) {
        self.s_prev = 0.0;
        self.s_prev2 = 0.0;
        self.n = 0;
    }
}

/// Direct-form-II transposed biquad (RBJ audio-EQ cookbook coefficients).
#[derive(Debug, Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Constant-skirt-gain bandpass centred at `f0` with quality `q`.
    pub fn bandpass(f0: f64, q: f64, sample_rate: f64) -> Self {
        let w0 = 2.0 * PI * f0 / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        // RBJ "BPF (constant 0 dB peak gain)".
        let b0 = alpha;
        let b1 = 0.0;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cs;
        let a2 = 1.0 - alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// Low-pass at `f0` with quality `q`.
    pub fn lowpass(f0: f64, q: f64, sample_rate: f64) -> Self {
        let w0 = 2.0 * PI * f0 / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        let b1 = 1.0 - cs;
        let b0 = b1 / 2.0;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cs;
        let a2 = 1.0 - alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Filter one sample (transposed direct form II).
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Filter a whole buffer in place into a new vector.
    pub fn filter(&mut self, xs: &[f32]) -> Vec<f32> {
        xs.iter().map(|&x| self.process(x)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn tone(freq: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (TAU * (freq / sr) as f32 * i as f32).sin()).collect()
    }

    #[test]
    fn goertzel_peaks_on_tuned_tone() {
        let sr = 8_000.0;
        let n = 800; // 0.1 s
        let on = tone(1_000.0, sr, n);
        let off = tone(2_000.0, sr, n);

        let mut g_on = Goertzel::new(1_000.0, sr);
        let mut g_off = Goertzel::new(1_000.0, sr);
        for &s in &on {
            g_on.add(s);
        }
        for &s in &off {
            g_off.add(s);
        }
        // Far more energy when the tone matches the bin.
        assert!(g_on.power() > 50.0 * g_off.power(), "{} vs {}", g_on.power(), g_off.power());
    }

    #[test]
    fn bandpass_passes_center_rejects_far() {
        let sr = 12_000.0;
        let mut bp = Biquad::bandpass(2_125.0, 4.0, sr);
        let inb = bp.filter(&tone(2_125.0, sr, 4_000));
        let mut bp2 = Biquad::bandpass(2_125.0, 4.0, sr);
        let oob = bp2.filter(&tone(500.0, sr, 4_000));
        // Steady-state RMS over the back half (skip the transient).
        let rms = |v: &[f32]| {
            let tail = &v[v.len() / 2..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };
        assert!(rms(&inb) > 5.0 * rms(&oob), "{} vs {}", rms(&inb), rms(&oob));
    }

    #[test]
    fn am_envelope_recovers_tone_from_carrier() {
        // AM: carrier at 0 (baseband), 1 kHz audio at 50% depth, complex IQ.
        let sr = 8_000.0;
        let n = 8_000;
        let iq: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let a = 1.0 + 0.5 * (TAU * (1_000.0 / sr) as f32 * i as f32).sin();
                Complex::new(a, 0.0)
            })
            .collect();
        let audio = am_envelope(&iq, 0.001);
        // The recovered audio should hold strong 1 kHz energy and, once the
        // one-pole DC tracker has settled (steady-state tail), near-zero DC.
        let mut g = Goertzel::new(1_000.0, sr);
        for &s in &audio {
            g.add(s);
        }
        assert!(g.power() > 1.0);
        let tail = &audio[audio.len() / 2..];
        let mean: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(mean.abs() < 0.05, "audio DC not removed (steady state): {mean}");
    }
}
