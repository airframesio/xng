//! RS41 GFSK demodulator (IQ → NRZ bits).
//!
//! The RS41 air interface is GFSK at 4800 baud (modulation index ≈ 1,
//! Gaussian-shaped, BT ≈ 0.5), one frame per second. Unlike AIS GMSK the
//! data is **NRZ** (the bit value maps straight to the FSK tone — a `1` is
//! the high tone, a `0` the low tone), with no NRZI / Manchester layer.
//!
//! Chain (mirrors the AIS [`crate`]-sibling `GmskDemod` structure, which the
//! task specifies reusing — GMSK and GFSK share the discriminator +
//! integrate-and-dump path): per-sample frequency discriminator → slow DC
//! tracker (absorbs the residual carrier offset the DDC did not remove +
//! receiver ppm) → per-symbol integrate-and-dump with zero-crossing timing
//! recovery → hard NRZ slice.
//!
//! Output is a stream of hard bits at one bit per symbol; the framer
//! (`framer.rs`) correlates the on-air sync word against it and packs the
//! recovered LSB-first octets.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// RS41 symbol rate.
pub const BAUD: f64 = 4_800.0;
/// Samples per symbol at [`CHANNEL_RATE`].
pub const SAMPLES_PER_SYM: usize = (CHANNEL_RATE / BAUD) as usize;

/// Compile-time invariant: the discriminator + integrate-and-dump needs at
/// least 2 samples/symbol.
const _: () = assert!(SAMPLES_PER_SYM >= 2);

/// Timing loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.10;
/// Carrier-offset (discriminator DC) tracking factor.
const FREQ_ALPHA: f32 = 0.001;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

/// GFSK frequency-discriminator demodulator producing hard NRZ bits.
pub struct GfskDemod {
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Discriminator DC estimate (residual carrier frequency offset).
    freq_offset: f32,
    /// Bit-timing phase in samples; wraps at SAMPLES_PER_SYM.
    timing: f64,
    /// Discriminator integrator over the current symbol window.
    acc: f32,
    /// Smoothed channel power.
    level: f32,
}

impl GfskDemod {
    pub fn new() -> Self {
        Self {
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            acc: 0.0,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append hard NRZ bits (high tone = 1) to `bits`.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        let spb = SAMPLES_PER_SYM as f64;
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            // Frequency discriminator: arg of the per-sample phase advance.
            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Tone transitions cross zero at symbol boundaries — nudge the
            // timing phase toward the crossing (Gardner-style early/late).
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let err = self.timing - (self.timing / spb).round() * spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= spb {
                self.timing -= spb;
                // NRZ hard slice: high tone (positive frequency) → 1.
                bits.push((self.acc >= 0.0) as u8);
                self.acc = 0.0;
            }
        }
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

impl Default for GfskDemod {
    fn default() -> Self {
        Self::new()
    }
}
