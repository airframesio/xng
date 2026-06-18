//! FLEX 2-FSK (NRZ) demodulator and Sync 1 hunt.
//!
//! On air FLEX 1600 bps 2-level FSK uses binary FSK with ~±4800 Hz deviation
//! (FLEX protocol PHY; deviation is implementation-set, commonly 4.8 kHz for
//! the 2-level mode). Data is NRZ at 1600 sym/s. Absolute polarity depends on
//! the receiver sideband, so the channel decoder tries both polarities and
//! keeps whichever locks the FLEX Sync 1 marker.
//!
//! Chain (mirrors the POCSAG/NAVTEX FSK demod structure):
//!   - per-sample frequency discriminator `arg(x · conj(x_prev))`,
//!   - slow DC tracker absorbing residual carrier/tuning offset,
//!   - integrate-and-dump per symbol at 1600 Bd with a zero-crossing timing
//!     nudge,
//!   - hard slice → one bit per symbol.
//!
//! Sync 1 (multimon-ng): the 64-bit `AAAA:A6C6AAAA:CCCC` word, where the fixed
//! middle 32 bits are [`crate::frame::SYNC_MARKER_B`] = `0xA6C6AAAA`. We hunt
//! that 32-bit marker (within a small bit-error tolerance) to lock the frame.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// Carrier-offset (discriminator DC) tracking factor. Slow: soaks up fixed
/// tuning error but not the per-symbol FSK swing.
const FREQ_ALPHA: f32 = 0.0003;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;
/// Timing-loop gain (fraction of phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.10;

/// FLEX 2-level FSK symbol rate (bits/s) supported by this core.
pub const BAUD_1600: f64 = 1600.0;
/// Supported FLEX bauds (this core implements 1600 bps 2-FSK only; see crate
/// docs / notes for what is skipped).
pub const BAUDS: [f64; 1] = [BAUD_1600];

/// Streaming FSK→bits demodulator for one FLEX channel at a fixed baud.
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
    /// A positive (higher-frequency) tone slices to 1; negative to 0. Absolute
    /// FLEX polarity is resolved later by the Sync 1 hunt.
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

/// Assemble 32 bits starting at `bits[start]` into a u32 word, MSB-first.
/// Returns `None` if fewer than 32 bits remain.
pub fn word_at_msb(bits: &[u8], start: usize) -> Option<u32> {
    if start + 32 > bits.len() {
        return None;
    }
    let mut w = 0u32;
    for &b in &bits[start..start + 32] {
        w = (w << 1) | (b as u32 & 1);
    }
    Some(w)
}

/// Assemble 32 bits starting at `bits[start]` into a u32 word, **LSB-first**
/// (FLEX on-air bit order: first bit received is bit 0). Returns `None` if
/// fewer than 32 bits remain.
pub fn word_at_lsb(bits: &[u8], start: usize) -> Option<u32> {
    if start + 32 > bits.len() {
        return None;
    }
    let mut w = 0u32;
    for (i, &b) in bits[start..start + 32].iter().enumerate() {
        w |= (b as u32 & 1) << i;
    }
    Some(w)
}

/// Hamming distance between two 32-bit words.
fn hd(a: u32, b: u32) -> u32 {
    (a ^ b).count_ones()
}

/// Locate the FLEX Sync 1 marker (`0xA6C6AAAA`, MSB-first on the wire) in a bit
/// history.
///
/// Scans every bit offset; at each, reads a 32-bit word MSB-first and tests it
/// (and its inversion, for unknown FSK polarity) against the marker within
/// `max_err` bit errors. Returns `Some((bit_offset, inverted))` where
/// `bit_offset` is the index of the FIRST bit of the marker and `inverted`
/// means the whole stream's polarity must be flipped to read words.
pub fn find_sync(bits: &[u8], max_err: u32) -> Option<(usize, bool)> {
    let marker = crate::frame::SYNC_MARKER_B;
    if bits.len() < 32 {
        return None;
    }
    for off in 0..=(bits.len() - 32) {
        let w = word_at_msb(bits, off).unwrap();
        if hd(w, marker) <= max_err {
            return Some((off, false));
        }
        if hd(!w, marker) <= max_err {
            return Some((off, true));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::SYNC_MARKER_B;

    #[test]
    fn word_at_orderings() {
        let mut bits = vec![0u8; 32];
        bits[0] = 1; // first bit
        assert_eq!(word_at_msb(&bits, 0), Some(0x8000_0000)); // MSB
        assert_eq!(word_at_lsb(&bits, 0), Some(0x0000_0001)); // LSB
        assert_eq!(word_at_msb(&bits[..8], 0), None);
    }

    #[test]
    fn find_sync_locates_marker_with_offset() {
        let mut bits = vec![1u8, 0, 1, 1, 0, 0, 1]; // 7 junk bits
        for i in (0..32).rev() {
            bits.push(((SYNC_MARKER_B >> i) & 1) as u8);
        }
        let (off, inv) = find_sync(&bits, 2).expect("marker must be found");
        assert_eq!(off, 7);
        assert!(!inv);
    }

    #[test]
    fn find_sync_handles_inverted_polarity() {
        let mut bits = Vec::new();
        for i in (0..32).rev() {
            bits.push(((!SYNC_MARKER_B >> i) & 1) as u8);
        }
        let (off, inv) = find_sync(&bits, 2).expect("inverted marker must be found");
        assert_eq!(off, 0);
        assert!(inv);
    }

    #[test]
    fn find_sync_tolerates_bit_errors() {
        let mut bits = Vec::new();
        let corrupted = SYNC_MARKER_B ^ 0b11; // 2 errors
        for i in (0..32).rev() {
            bits.push(((corrupted >> i) & 1) as u8);
        }
        assert!(find_sync(&bits, 2).is_some());
        assert!(find_sync(&bits, 1).is_none());
    }
}
