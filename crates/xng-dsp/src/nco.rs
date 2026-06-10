//! Numerically controlled oscillator for frequency translation.

use crate::IqSample;
use std::f64::consts::TAU;

/// NCO that mixes a complex stream by `-freq_hz` (i.e. shifts a signal at
/// `+freq_hz` down to 0 Hz). Phase is tracked in f64 to avoid drift over
/// long captures.
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
        for x in input {
            let (sin, cos) = self.phase.sin_cos();
            *x *= IqSample::new(cos as f32, sin as f32);
            self.phase += self.step;
            if self.phase > TAU {
                self.phase -= TAU;
            } else if self.phase < -TAU {
                self.phase += TAU;
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
}
