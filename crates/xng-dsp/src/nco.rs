//! Numerically controlled oscillator for frequency translation.

use crate::IqSample;
use std::f64::consts::TAU;

/// How many samples may be produced by incremental rotation before the
/// rotator is re-derived from the exact f64 phase. Each step costs two
/// multiply-adds of relative error ~1e-7 (f32), so a few hundred samples keeps
/// the accumulated magnitude/phase error far below the noise floor while
/// amortising the `sin_cos` over the whole run.
const RESYNC_INTERVAL: usize = 256;

/// NCO that mixes a complex stream by `-freq_hz` (i.e. shifts a signal at
/// `+freq_hz` down to 0 Hz).
///
/// Phase is tracked in f64 so it does not drift over long captures, but the
/// per-sample rotation is applied incrementally (one complex multiply) rather
/// than calling `sin_cos` for every sample — the transcendental dominated this
/// loop, and the mix runs at the full capture rate on every channel. The
/// incremental rotator is re-derived from the exact phase every
/// `RESYNC_INTERVAL` samples so f32 rounding cannot accumulate.
pub struct Nco {
    phase: f64,
    step: f64,
}

impl Nco {
    pub fn new(freq_hz: f64, sample_rate: f64) -> Self {
        Self { phase: 0.0, step: -TAU * freq_hz / sample_rate }
    }

    pub fn set_freq(&mut self, freq_hz: f64, sample_rate: f64) {
        self.step = -TAU * freq_hz / sample_rate;
    }

    /// Mix `input` in place.
    pub fn mix(&mut self, input: &mut [IqSample]) {
        // Per-sample rotation factor (constant for a fixed step).
        let (ss, sc) = self.step.sin_cos();
        let rot = IqSample::new(sc as f32, ss as f32);

        for chunk in input.chunks_mut(RESYNC_INTERVAL) {
            // Re-derive the rotator from the exact f64 phase at each chunk
            // boundary: bounds f32 error and keeps `phase` authoritative.
            let (sin, cos) = self.phase.sin_cos();
            let mut cur = IqSample::new(cos as f32, sin as f32);
            for x in chunk.iter_mut() {
                *x *= cur;
                cur *= rot;
            }
            self.phase += self.step * chunk.len() as f64;
            if self.phase > TAU {
                self.phase %= TAU;
            } else if self.phase < -TAU {
                self.phase = -((-self.phase) % TAU);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_tone_to_dc() {
        let fs = 48_000.0;
        let f = 6_000.0;
        let mut buf: Vec<IqSample> = (0..4800)
            .map(|i| {
                let ph = TAU * f * i as f64 / fs;
                IqSample::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        let mut nco = Nco::new(f, fs);
        nco.mix(&mut buf);
        // After mixing, the signal should be ~constant (DC)
        let mean: IqSample = buf.iter().sum::<IqSample>() / buf.len() as f32;
        assert!(mean.norm() > 0.99, "tone should land at DC, |mean|={}", mean.norm());
    }

    /// The incremental rotator must not drift in magnitude or phase over a
    /// long run, and must match an exact per-sample `sin_cos` mix. This is the
    /// guarantee that lets the hot loop avoid the transcendental.
    #[test]
    fn incremental_rotation_matches_exact_and_does_not_drift() {
        let fs = 2_400_000.0;
        let f = 137_000.0;
        // Several seconds' worth of samples, fed in uneven blocks so the
        // resync boundaries do not line up with the chunking.
        let n = 3_000_000usize;
        let input: IqSample = IqSample::new(1.0, 0.0);

        let mut nco = Nco::new(f, fs);
        let mut worst_mag_err = 0.0f64;
        let mut worst_phase_err = 0.0f64;
        let mut idx = 0usize;
        for blk in [1usize, 7, 101, 1024, 4096, 65_536].iter().cycle() {
            if idx >= n {
                break;
            }
            let len = (*blk).min(n - idx);
            let mut buf = vec![input; len];
            nco.mix(&mut buf);
            for (k, x) in buf.iter().enumerate() {
                // Exact reference: the mix is multiplication by e^{j·step·i}.
                let exact_ph = -TAU * f * (idx + k) as f64 / fs;
                let (es, ec) = exact_ph.sin_cos();
                worst_mag_err = worst_mag_err.max(((x.norm() as f64) - 1.0).abs());
                // Phase difference against the exact rotator.
                let dot = (x.re as f64) * ec + (x.im as f64) * es;
                let cross = (x.im as f64) * ec - (x.re as f64) * es;
                worst_phase_err = worst_phase_err.max(cross.atan2(dot).abs());
            }
            idx += len;
        }
        assert!(worst_mag_err < 1e-4, "magnitude drifted by {worst_mag_err}");
        assert!(worst_phase_err < 1e-3, "phase drifted by {worst_phase_err} rad");
    }
}
