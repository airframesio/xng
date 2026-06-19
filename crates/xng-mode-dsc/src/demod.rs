//! IQ → bits front end for DSC (MF/HF: 100 Bd binary FSK, ±85 Hz shift).
//!
//! Input: complex channel IQ at [`crate::CHANNEL_RATE`]. ITU-R M.493 MF/HF DSC
//! is narrow-band binary FSK at 100 Bd with a 170 Hz shift (±85 Hz about the
//! channel center): the "B" element (binary 0) is the lower tone, the "Y"
//! element (binary 1) the upper tone.
//!
//! Chain (mirrors the [`xng_mode_acars`] / AIS discriminator + timing pattern):
//! per-sample frequency discriminator → slow DC tracker (absorbs residual
//! carrier offset left by the DDC) → per-bit integrate-and-dump with
//! zero-crossing timing recovery → hard bit decision (positive mean frequency
//! = upper tone = Y = 1; negative = lower tone = B = 0).
//!
//! The discriminator-recovered bit stream is consumed by
//! [`crate::symbol::decode_bitstream`] once the symbol/phasing boundary has
//! been found by [`DscBitSync`] (the M.493 dot pattern / phasing characters).

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// DSC MF/HF symbol rate (bits per second).
pub const BAUD: f64 = 100.0;
/// Frequency shift each side of channel center (Hz). Full shift is 170 Hz.
pub const SHIFT_HZ: f64 = 85.0;
/// Timing loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.10;
/// Carrier-offset (discriminator DC) tracking factor. Slow: the shift is only
/// ±85 Hz, so the DC tracker must not chase the data itself.
const FREQ_ALPHA: f32 = 0.0005;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;

/// Samples per DSC bit at [`CHANNEL_RATE`].
pub fn samples_per_bit() -> f64 {
    CHANNEL_RATE / BAUD
}

/// Frequency-discriminator + timing-recovery FSK bit slicer.
///
/// Emits one hard bit per symbol period: `1` for the upper (Y) tone, `0` for
/// the lower (B) tone. Polarity follows the FSK convention directly (no
/// differential/NRZI step — M.493 is straight binary FSK).
pub struct FskDemod {
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Discriminator DC estimate (residual carrier frequency offset).
    freq_offset: f32,
    /// Bit-timing phase in samples; wraps at `samples_per_bit()`.
    timing: f64,
    spb: f64,
    /// Discriminator integrator over the current bit window.
    acc: f32,
    /// Smoothed channel power.
    level: f32,
}

impl FskDemod {
    pub fn new() -> Self {
        Self {
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            spb: samples_per_bit(),
            acc: 0.0,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append hard bit decisions to `bits`.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            // Frequency discriminator: phase advance per sample.
            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Tone transitions cross zero at bit boundaries; nudge the timing
            // phase so crossings land on the boundary.
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let err = self.timing - (self.timing / self.spb).round() * self.spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= self.spb {
                self.timing -= self.spb;
                // Positive mean frequency = upper tone (Y) = 1.
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

/// Phasing / symbol-boundary acquisition for the DSC bit stream.
///
/// M.493 prefixes every call with a phasing sequence: the DX stream carries the
/// 125 ("phasing") character and the RX stream carries a descending count
/// (111,110,…,104). [`crate::symbol::decode_bitstream`] needs the bit stream
/// aligned so that each 10-bit group starts on a symbol boundary, and
/// [`crate::symbol::deinterleave_dx_rx`] expects the first DX character to be a
/// phasing character.
///
/// This sync hunts every one of the 10 candidate bit phases for a run of valid
/// (zero-count-checked) symbols, and within those locks onto the phasing
/// character `125`, returning the bit offset of the first phasing symbol. From
/// that offset the standard DX/RX geometry (`deinterleave_dx_rx(.., 6, 2)`)
/// applies.
pub struct DscBitSync;

impl DscBitSync {
    /// Find the EARLIEST bit offset at which a DX phasing character (`125`)
    /// lands, confirmed by the next DX character (two symbols later) also being
    /// a valid phasing character (`125` again, or — at the phasing/data boundary
    /// — any valid symbol). Scanning by absolute bit position (not phase-major)
    /// makes this lock onto the first on-air phasing character it sees, the
    /// natural acquisition behaviour, rather than a chance `125` deep in a
    /// different bit phase. Returns `None` if no such alignment is found.
    pub fn find_phasing(bits: &[u8]) -> Option<usize> {
        const N: usize = crate::symbol::SYMBOL_BITS;
        let mut i = 0;
        while i + N <= bits.len() {
            let (val, ok) = crate::symbol::decode_symbol(&bits[i..i + N]);
            if ok && val == 125 {
                // Confirm the alignment with the following DX character (skip
                // the interleaved RX character at i+N): the next DX character
                // is another phasing 125 (mid-sequence) or, at the phasing/data
                // boundary, the format specifier — in all cases a valid symbol
                // that passes the zero-count check. A chance 125 at a wrong bit
                // phase almost never has a valid symbol exactly two slots on.
                let next = i + 2 * N;
                if next + N <= bits.len() {
                    let (_nval, nok) = crate::symbol::decode_symbol(&bits[next..next + N]);
                    if nok {
                        return Some(i);
                    }
                } else {
                    // Not enough trailing bits to confirm yet; report anyway so
                    // the caller can wait for more (it will re-confirm later).
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }
}
