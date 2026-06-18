//! EOT/HOT 1200-baud Manchester-FSK demodulator.
//!
//! Input: complex channel IQ at [`crate::CHANNEL_RATE`], already mixed to
//! baseband and decimated by the crate's [`xng_dsp::Ddc`]. On air EOT is
//! narrowband 1200-baud binary FSK with Manchester line coding (each data bit
//! is two opposite chips), per the cited PyEOT / EOTDecode decoders.
//!
//! Chain (mirrors the narrow-shift FSK structure used by the NAVTEX core):
//!
//! - per-sample frequency discriminator (`arg(x . conj(x_prev))`),
//! - slow DC tracker that absorbs residual carrier/tuning offset so only the
//!   FSK swing remains,
//! - per-CHIP integrate-and-dump at the Manchester chip rate (`2 * BAUD`),
//!   with zero-crossing timing recovery (chip transitions are dense in
//!   Manchester coding, giving the loop plenty of edges),
//! - mark/space slicing -> one chip decision per chip period.
//!
//! The emitted **chip** stream is Manchester-paired into logical bits by the
//! channel decoder (which tries both pairing phases) and scanned for the frame
//! sync. This module only produces the chip stream and the level estimate.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// Manchester chip rate = two chips per 1200-baud data bit.
pub const CHIP_RATE: f64 = 2.0 * crate::modulate::BAUD;
/// Carrier-offset (discriminator DC) tracking factor. Slow: soaks up fixed
/// tuning error, never the per-chip FSK swing.
const FREQ_ALPHA: f32 = 0.0005;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;
/// Timing-loop gain (fraction of phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.10;

/// Streaming FSK->chips demodulator for one EOT channel.
pub struct FskDemod {
    samples_per_chip: f64,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    freq_offset: f32,
    timing: f64,
    acc: f32,
    level: f32,
}

impl FskDemod {
    /// Build a demod for the fixed [`CHANNEL_RATE`] / 2400-chip EOT signal.
    pub fn new() -> Self {
        let samples_per_chip = CHANNEL_RATE / CHIP_RATE;
        assert!(
            samples_per_chip >= 4.0,
            "need >=4 samples/chip for FSK timing"
        );
        Self {
            samples_per_chip,
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            acc: 0.0,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append one chip decision per recovered chip to `chips`
    /// (1 = mark / positive frequency, 0 = space / negative).
    pub fn process(&mut self, input: &[Complex<f32>], chips: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Chip transitions cross zero at chip boundaries; nudge the clock.
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let spc = self.samples_per_chip;
                let err = self.timing - (self.timing / spc).round() * spc;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= self.samples_per_chip {
                self.timing -= self.samples_per_chip;
                chips.push((self.acc >= 0.0) as u8);
                self.acc = 0.0;
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

/// Manchester-decode a chip stream into logical bits, starting at chip offset
/// `phase` (0 or 1 selects which chip begins a bit pair). A bit pair `[1,0]`
/// decodes to logical 1, `[0,1]` to logical 0; an invalid pair (`[0,0]` or
/// `[1,1]`, e.g. from noise) decodes to the first chip's value as a soft
/// fallback so the sync hunt can still slide over it.
pub fn manchester_decode(chips: &[u8], phase: usize) -> Vec<u8> {
    if phase >= chips.len() {
        return Vec::new();
    }
    chips[phase..]
        .chunks_exact(2)
        .map(|c| match (c[0], c[1]) {
            (1, 0) => 1,
            (0, 1) => 0,
            _ => c[0], // ambiguous; keep something so framing can re-sync
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manchester_decode_inverts_encode() {
        let bits = [1u8, 0, 1, 1, 0, 0, 1];
        let chips = crate::modulate::manchester_encode(&bits);
        assert_eq!(manchester_decode(&chips, 0), bits.to_vec());
    }

    #[test]
    fn manchester_decode_phase_one_skips_leading_chip() {
        // [0,1,0] -> phase 1 pairs (1,0) -> logical 1.
        assert_eq!(manchester_decode(&[0, 1, 0], 1), vec![1]);
    }
}
