//! AIS GMSK demodulator.
//!
//! Input: complex channel IQ at 48 kHz (5 samples/bit at 9600 bd). GMSK is
//! FM with h=0.5 (±2400 Hz deviation), Gaussian-shaped (BT=0.4).
//!
//! Chain: per-sample frequency discriminator → slow DC tracker (absorbs
//! carrier frequency offset from ship + receiver ppm error) → per-bit
//! integrate-and-dump with zero-crossing timing recovery → NRZI decode
//! (ITU-R M.1371: a zero is encoded as a change of level; a one as no
//! change).

use crate::CHANNEL_RATE;
use num_complex::Complex;

const BAUD: f64 = 9_600.0;
const SAMPLES_PER_BIT: usize = 5;
/// Timing loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.15;
/// Carrier-offset (discriminator DC) tracking factor.
const FREQ_ALPHA: f32 = 0.002;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

pub struct GmskDemod {
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Discriminator DC estimate (carrier frequency offset).
    freq_offset: f32,
    /// Bit-timing phase in samples; wraps at SAMPLES_PER_BIT.
    timing: f64,
    /// Discriminator integrator over the current bit window.
    acc: f32,
    /// Last detected level (sign of the previous bit's frequency).
    prev_level: i8,
    /// Smoothed channel power.
    level: f32,
}

impl GmskDemod {
    pub fn new() -> Self {
        assert_eq!(CHANNEL_RATE as usize, (BAUD as usize) * SAMPLES_PER_BIT);
        Self {
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            acc: 0.0,
            prev_level: 1,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append NRZI-decoded bits to `bits`.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Level transitions cross zero at bit boundaries.
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let spb = SAMPLES_PER_BIT as f64;
                let err = self.timing - (self.timing / spb).round() * spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= SAMPLES_PER_BIT as f64 {
                self.timing -= SAMPLES_PER_BIT as f64;
                let level: i8 = if self.acc < 0.0 { -1 } else { 1 };
                self.acc = 0.0;
                // NRZI: level change = 0, no change = 1.
                bits.push((level == self.prev_level) as u8);
                self.prev_level = level;
            }
        }
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

impl Default for GmskDemod {
    fn default() -> Self {
        Self::new()
    }
}
