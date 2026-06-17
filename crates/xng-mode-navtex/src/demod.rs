//! NAVTEX / SITOR-B narrow-shift FSK demodulator.
//!
//! Input: complex channel IQ at [`crate::CHANNEL_RATE`], already mixed to
//! baseband and decimated by the crate's [`xng_dsp::Ddc`]. On air NAVTEX is
//! 100-baud binary FSK with a ±85 Hz shift (mark/space tones 170 Hz apart);
//! it is *not* Gaussian-shaped, so a plain frequency discriminator recovers
//! the tone polarity directly.
//!
//! Chain (mirrors the AIS [`xng_mode_ais::demod::GmskDemod`] structure for a
//! narrow-shift, low-rate FSK signal):
//!
//! - per-sample frequency discriminator (`arg(x · conj(x_prev))`),
//! - slow DC tracker that absorbs residual carrier offset (tuning error,
//!   receiver ppm), so only the FSK swing remains,
//! - per-bit integrate-and-dump with zero-crossing timing recovery at the
//!   100 Bd symbol clock,
//! - mark/space slicing → one bit decision per symbol period.
//!
//! The emitted bit stream is then packed into 7-bit CCIR 476 codes
//! ([`crate::ccir476::pack_bits`], LSB-first) and handed to the verified
//! [`crate::decode_symbols`] decode core. This module only produces the
//! symbol stream; FEC-B diversity and framing live above it.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// NAVTEX symbol/baud rate (CCIR 476 B-mode).
pub const BAUD: f64 = 100.0;
/// Carrier-offset (discriminator DC) tracking factor. Slow: only soaks up
/// fixed tuning error, never the per-bit FSK swing.
const FREQ_ALPHA: f32 = 0.0005;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;
/// Timing-loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.10;

/// Streaming FSK→bits demodulator for one NAVTEX channel.
pub struct FskDemod {
    samples_per_bit: f64,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Discriminator DC estimate (carrier frequency offset).
    freq_offset: f32,
    /// Bit-timing phase in samples; wraps at `samples_per_bit`.
    timing: f64,
    /// Discriminator integrator over the current bit window.
    acc: f32,
    /// Smoothed channel power.
    level: f32,
}

impl FskDemod {
    /// Build a demod for the fixed [`CHANNEL_RATE`] / 100 Bd NAVTEX signal.
    pub fn new() -> Self {
        let samples_per_bit = CHANNEL_RATE / BAUD;
        assert!(samples_per_bit >= 4.0, "need ≥4 samples/bit for FSK timing");
        Self {
            samples_per_bit,
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            acc: 0.0,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append one bit decision per recovered symbol to
    /// `bits` (1 = mark / positive frequency, 0 = space / negative).
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Tone transitions cross zero at bit boundaries; nudge the clock.
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let spb = self.samples_per_bit;
                let err = self.timing - (self.timing / spb).round() * spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= self.samples_per_bit {
                self.timing -= self.samples_per_bit;
                // Mark (higher freq, positive discriminator) = 1.
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

impl Default for FskDemod {
    fn default() -> Self {
        Self::new()
    }
}

/// Pack a bit stream (1 bit per symbol, in arrival order) into CCIR 476
/// 7-bit codes, LSB-first per [`crate::ccir476::pack_bits`].
///
/// `bit_phase` selects which of the 7 possible 7-bit alignments to start on
/// (the symbol-boundary search lives in the channel decoder, which tries all
/// seven and keeps the one whose stream decodes). Bits before `bit_phase`
/// and a trailing partial group are dropped.
pub fn pack_codes(bits: &[u8], bit_phase: usize) -> Vec<u8> {
    if bit_phase >= bits.len() {
        return Vec::new();
    }
    bits[bit_phase..]
        .chunks_exact(7)
        .map(|c| {
            let mut soft = [0i32; 7];
            for (i, &b) in c.iter().enumerate() {
                soft[i] = if b != 0 { 1 } else { -1 };
            }
            crate::ccir476::pack_bits(&soft)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccir476;

    #[test]
    fn pack_codes_lsb_first_round_trip() {
        // Bits for 'A' = 0x47 = 0b1000111 → LSB-first [1,1,1,0,0,0,1].
        let bits = [1u8, 1, 1, 0, 0, 0, 1];
        let codes = pack_codes(&bits, 0);
        assert_eq!(codes, vec![0x47]);
        assert_eq!(ccir476::decode(codes[0], false), ccir476::Decoded::Char('A'));
    }

    #[test]
    fn pack_codes_respects_phase_and_drops_partial() {
        // Two junk bits, then 'A', then a partial trailing group.
        let mut bits = vec![0u8, 1];
        bits.extend([1, 1, 1, 0, 0, 0, 1]); // 'A'
        bits.extend([1, 0, 1]); // partial, must be dropped
        let codes = pack_codes(&bits, 2);
        assert_eq!(codes, vec![0x47]);
    }
}
