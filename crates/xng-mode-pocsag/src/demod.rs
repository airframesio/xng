//! POCSAG 2-FSK (NRZ) demodulator and frame synchroniser.
//!
//! On air POCSAG is binary FSK at 512 / 1200 / 2400 baud with roughly
//! ±4.5 kHz deviation (ITU-R M.584-2 §1; the deviation is implementation-set,
//! commonly 4.5 kHz). The data is NRZ: a **space** (lower tone) is logical 1
//! and a **mark** (higher tone) is logical 0 in the conventional POCSAG
//! convention, but since the absolute polarity depends on the receiver's
//! sideband, the channel decoder tries both polarities and keeps whichever
//! locks the sync codeword.
//!
//! Chain (mirrors the NAVTEX FSK demod structure for a wider-shift, faster
//! signal):
//!   - per-sample frequency discriminator `arg(x · conj(x_prev))`,
//!   - slow DC tracker absorbing residual carrier/tuning offset,
//!   - integrate-and-dump per bit at the selected baud, with a zero-crossing
//!     timing nudge,
//!   - hard slice → one bit per symbol.
//!
//! Framing (preamble hunt → sync lock → batch read) is done on the recovered
//! bit history by [`crate::PocsagChannelDecoder`].

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// Carrier-offset (discriminator DC) tracking factor. Slow: soaks up fixed
/// tuning error but not the per-bit FSK swing.
const FREQ_ALPHA: f32 = 0.0003;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;
/// Timing-loop gain (fraction of phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.10;

/// Supported POCSAG baud rates (ITU-R M.584-2 §1).
pub const BAUDS: [f64; 3] = [512.0, 1200.0, 2400.0];

/// Streaming FSK→bits demodulator for one POCSAG channel at a fixed baud.
pub struct FskDemod {
    samples_per_bit: f64,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    freq_offset: f32,
    timing: f64,
    acc: f32,
    level: f32,
}

impl FskDemod {
    /// Build a demod for [`CHANNEL_RATE`] at `baud`.
    pub fn new(baud: f64) -> Self {
        let samples_per_bit = CHANNEL_RATE / baud;
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

    /// Feed channel IQ; append one bit decision per recovered symbol to `bits`.
    /// A positive (higher-frequency) tone slices to 1; negative to 0. The
    /// absolute POCSAG polarity is resolved later by the sync hunt.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

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

/// Assemble 32 bits (MSB-first) starting at `bits[start]` into a u32 codeword.
/// Returns `None` if fewer than 32 bits remain.
pub fn word_at(bits: &[u8], start: usize) -> Option<u32> {
    if start + 32 > bits.len() {
        return None;
    }
    let mut w = 0u32;
    for &b in &bits[start..start + 32] {
        w = (w << 1) | (b as u32 & 1);
    }
    Some(w)
}

/// Hamming distance between two 32-bit words.
fn hd(a: u32, b: u32) -> u32 {
    (a ^ b).count_ones()
}

/// Locate the POCSAG frame-sync codeword in a bit history.
///
/// Scans every bit offset; at each, reads a 32-bit word (MSB-first) and tests
/// it (and its inversion, for unknown FSK polarity) against the sync codeword
/// `0x7CD215D8` within `max_err` bit errors. Returns
/// `Some((bit_offset, inverted))` for the first match, where `inverted` means
/// the whole bit stream's polarity must be flipped to read codewords.
pub fn find_sync(bits: &[u8], max_err: u32) -> Option<(usize, bool)> {
    let sync = crate::bch::SYNC_CODEWORD;
    if bits.len() < 32 {
        return None;
    }
    for off in 0..=(bits.len() - 32) {
        let w = word_at(bits, off).unwrap();
        if hd(w, sync) <= max_err {
            return Some((off, false));
        }
        if hd(!w, sync) <= max_err {
            return Some((off, true));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bch::SYNC_CODEWORD;

    #[test]
    fn word_at_is_msb_first() {
        let bits = [1u8, 0, 0, 0, 0, 0, 0, 0]; // partial: only 8 bits
        assert_eq!(word_at(&bits, 0), None); // need 32
        let mut bits32 = vec![0u8; 32];
        bits32[0] = 1; // MSB set
        assert_eq!(word_at(&bits32, 0), Some(0x8000_0000));
    }

    #[test]
    fn find_sync_locates_codeword_with_offset() {
        // 5 junk bits, then the 32-bit sync codeword MSB-first.
        let mut bits = vec![1u8, 0, 1, 1, 0];
        for i in (0..32).rev() {
            bits.push(((SYNC_CODEWORD >> i) & 1) as u8);
        }
        let (off, inv) = find_sync(&bits, 2).expect("sync must be found");
        assert_eq!(off, 5);
        assert!(!inv);
    }

    #[test]
    fn find_sync_handles_inverted_polarity() {
        let mut bits = Vec::new();
        for i in (0..32).rev() {
            bits.push(((!SYNC_CODEWORD >> i) & 1) as u8);
        }
        let (off, inv) = find_sync(&bits, 2).expect("inverted sync must be found");
        assert_eq!(off, 0);
        assert!(inv);
    }

    #[test]
    fn find_sync_tolerates_bit_errors() {
        let mut bits = Vec::new();
        let corrupted = SYNC_CODEWORD ^ 0b11; // 2 bit errors
        for i in (0..32).rev() {
            bits.push(((corrupted >> i) & 1) as u8);
        }
        assert!(find_sync(&bits, 2).is_some());
        assert!(find_sync(&bits, 1).is_none());
    }
}
