//! ATCS binary-FSK demodulator (AAR Spec-200 data radio).
//!
//! Input: complex channel IQ at [`crate::CHANNEL_RATE`] (5 samples/bit at
//! 4800 bd). The ATCS RF link is direct 2-FSK (mark/space tones either side
//! of the channel center), carrying a synchronous NRZI-encoded HDLC bit
//! stream: a transmitter sends bit synchronization (40 alternating 1/0 →
//! a steady tone alternation), a frame-synchronization sequence, then the
//! flag-bounded HDLC frames.
//!
//! Chain (the AIS [`xng_mode_ais`]-style GFSK pattern; the only difference
//! is the baud/deviation): per-sample frequency discriminator → slow DC
//! tracker (absorbs carrier frequency offset from the radio + receiver ppm
//! error, and also recenters off the alternating-bit preamble) → per-bit
//! integrate-and-dump with zero-crossing timing recovery → NRZI decode
//! (HDLC/NRZI: a `0` is encoded as a level transition, a `1` as no change).
//!
//! There is no shared FSK/discriminator primitive in `xng-dsp`; this is the
//! discriminator + timing-recovery pattern copied from
//! `xng-mode-ais::demod::GmskDemod` (binary FSK is the BT→∞ limit of GMSK),
//! retuned for ATCS's 4800 bd. The downstream HDLC deframer + Spec-200
//! decode are the crate's existing, externally-anchored decode core.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// ATCS link rate.
const BAUD: f64 = 4_800.0;
/// Samples per bit at [`CHANNEL_RATE`].
const SAMPLES_PER_BIT: usize = 5;
/// Timing loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.15;
/// Carrier-offset (discriminator DC) tracking factor. Slow enough not to
/// chase the data, fast enough to settle during the alternating-bit /
/// flag preamble that precedes every frame.
const FREQ_ALPHA: f32 = 0.002;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

/// 2-FSK discriminator demod producing NRZI-decoded HDLC link bits.
pub struct FskDemod {
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Discriminator DC estimate (carrier frequency offset / FSK midpoint).
    freq_offset: f32,
    /// Bit-timing phase in samples; wraps at SAMPLES_PER_BIT.
    timing: f64,
    /// Discriminator integrator over the current bit window.
    acc: f32,
    /// Last detected tone level (sign of the previous bit's frequency).
    prev_level: i8,
    /// Smoothed channel power.
    level: f32,
}

impl FskDemod {
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

    /// Feed channel IQ; append NRZI-decoded link bits to `bits`.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            // Frequency discriminator: phase advance between consecutive
            // samples. Mark tone → one sign, space tone → the other.
            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Tone transitions cross zero at bit boundaries; nudge the
            // timing phase so crossings land on the boundary.
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
                // NRZI: no level change = 1, level change = 0.
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

impl Default for FskDemod {
    fn default() -> Self {
        Self::new()
    }
}
