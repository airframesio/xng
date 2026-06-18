//! FLEX 2-FSK (NRZ) demodulator, 4-FSK symbol slicer, and Sync 1 hunt.
//!
//! On air FLEX uses binary (2-level) or 4-level FSK with ~±4800 Hz deviation
//! (FLEX protocol PHY; deviation is implementation-set, commonly 4.8 kHz for
//! the 2-level mode, with the inner 4-level tones at ±1/3 of that). Data is NRZ
//! at a 1600 or 3200 **symbol** rate. Absolute polarity depends on the receiver
//! sideband, so the channel decoder tries both polarities and keeps whichever
//! locks the FLEX Sync 1 marker.
//!
//! Chain (mirrors the POCSAG/NAVTEX FSK demod structure):
//!   - per-sample frequency discriminator `arg(x · conj(x_prev))`,
//!   - slow DC tracker absorbing residual carrier/tuning offset,
//!   - integrate-and-dump per symbol with a zero-crossing timing nudge,
//!   - hard slice → one bit (2-level) or one 4-level symbol (0..=3) per symbol.
//!
//! Sync 1 (multimon-ng): the 64-bit `AAAA:A6C6AAAA:CCCC` word, where the fixed
//! middle 32 bits are [`crate::frame::SYNC_MARKER_B`] = `0xA6C6AAAA`. We hunt
//! that 32-bit marker (within a small bit-error tolerance) to lock the frame;
//! the 16-bit A-field selects the on-air rate/level mode (see [`FlexMode`]).
//!
//! # 4-level FSK (multimon-ng `demod_flex.c`)
//!
//! A 4-level symbol carries a dibit. The frequency-discriminator output is
//! sliced into one of four levels 0..=3 (lowest tone = 0, highest = 3) and
//! mapped to `(bit_a, bit_b)` by the FLEX Gray code:
//!
//! ```text
//!   sym  bit_a bit_b      (bit_a = sym>1 ; bit_b = sym==1 || sym==2)
//!    0     0     0
//!    1     0     1
//!    2     1     1
//!    3     1     0
//! ```
//!
//! Symbols are de-interleaved into up to four 88-word **phases** (A/B/C/D). At
//! the 1600 symbol rate, `bit_a→PhaseA`, `bit_b→PhaseB`. At the 3200 symbol
//! rate, consecutive symbols alternate (`phase_toggle`): even symbols
//! `→ PhaseA/PhaseB`, odd symbols `→ PhaseC/PhaseD`. Within a phase the 2816
//! bits (= 88 words × 32 bits) fill the word buffer column-interleaved:
//! `idx = ((counter>>5)&0xFFF8) | (counter&0x0007)` (multimon-ng `read_data`).

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
/// FLEX 4-level FSK at the 1600 symbol rate → 3200 information bps (Phases A,B).
pub const BAUD_3200: f64 = 3200.0;
/// FLEX 4-level FSK at the 3200 symbol rate → 6400 information bps
/// (Phases A,B,C,D).
pub const BAUD_6400: f64 = 6400.0;
/// Supported FLEX information bit rates: 1600 (2-FSK), 3200 & 6400 (4-FSK).
/// (The 3200-bps 2-FSK mode — 3200 sym/s, 2 levels, A-code `0x7B18` — is NOT
/// implemented here; see crate notes.)
pub const BAUDS: [f64; 3] = [BAUD_1600, BAUD_3200, BAUD_6400];

/// FLEX Sync-1 A-code for 1600 sym/s, 2-level FSK = 1600 bps.
/// (multimon-ng `flex_modes[] = { 0x870C, 1600, 2 }`.)
pub const A_CODE_1600_2: u16 = 0x870C;
/// FLEX Sync-1 A-code for 1600 sym/s, 4-level FSK = 3200 bps.
/// (multimon-ng `{ 0xB068, 1600, 4 }`.)
pub const A_CODE_1600_4: u16 = 0xB068;
/// FLEX Sync-1 A-code for 3200 sym/s, 2-level FSK = 3200 bps.
/// (multimon-ng `{ 0x7B18, 3200, 2 }`.)
pub const A_CODE_3200_2: u16 = 0x7B18;
/// FLEX Sync-1 A-code for 3200 sym/s, 4-level FSK = 6400 bps.
/// (multimon-ng `{ 0xDEA0, 3200, 4 }`.)
pub const A_CODE_3200_4: u16 = 0xDEA0;
/// Alternate FLEX Sync-1 A-code for 3200 sym/s, 4-level FSK = 6400 bps.
/// (multimon-ng `{ 0x4C7C, 3200, 4 }`.)
pub const A_CODE_3200_4_ALT: u16 = 0x4C7C;

/// One FLEX air mode resolved from the Sync-1 A-code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlexMode {
    /// On-air symbol rate (1600 or 3200 symbols/s).
    pub sym_rate: u32,
    /// Modulation levels (2 or 4).
    pub levels: u8,
}

impl FlexMode {
    /// Information bit rate = `sym_rate * levels/2`.
    pub fn baud(self) -> u32 {
        self.sym_rate * (self.levels as u32) / 2
    }

    /// Number of de-interleaved phases this mode fills (A only, A/B, A/C, or
    /// A/B/C/D).
    pub fn num_phases(self) -> usize {
        match (self.sym_rate, self.levels) {
            (1600, 2) => 1, // A
            (1600, 4) => 2, // A, B
            (3200, 2) => 2, // A, C
            (3200, 4) => 4, // A, B, C, D
            _ => 1,
        }
    }

    /// Resolve a mode from a 16-bit A-code, tolerating up to `max_err` bit
    /// errors. (multimon-ng `decode_mode` over `flex_modes[]`.)
    pub fn from_a_code(a: u16, max_err: u32) -> Option<FlexMode> {
        const MODES: [(u16, u32, u8); 5] = [
            (A_CODE_1600_2, 1600, 2),
            (A_CODE_1600_4, 1600, 4),
            (A_CODE_3200_2, 3200, 2),
            (A_CODE_3200_4, 3200, 4),
            (A_CODE_3200_4_ALT, 3200, 4),
        ];
        for &(code, sym_rate, levels) in &MODES {
            if (a ^ code).count_ones() <= max_err {
                return Some(FlexMode { sym_rate, levels });
            }
        }
        None
    }

    /// Resolve a mode from the desired information bit rate (the public
    /// `FlexChannelDecoder::new(.., baud)` argument).
    pub fn from_baud(baud: u32) -> Option<FlexMode> {
        match baud {
            1600 => Some(FlexMode {
                sym_rate: 1600,
                levels: 2,
            }),
            3200 => Some(FlexMode {
                sym_rate: 1600,
                levels: 4,
            }),
            6400 => Some(FlexMode {
                sym_rate: 3200,
                levels: 4,
            }),
            _ => None,
        }
    }
}

/// FLEX DATA-section length: 1760 ms of symbols.
/// (multimon-ng: "2816 bits @ 1600 bps and 5632 bits @ 3200 bps".)
pub fn data_symbols(sym_rate: u32) -> usize {
    (sym_rate as usize) * 1760 / 1000
}

/// De-interleave word/bit index within a phase from the running symbol/bit
/// counter (multimon-ng `read_data`:
/// `idx = ((counter>>5)&0xFFF8) | (counter&0x0007)`).
pub fn phase_idx(counter: u32) -> usize {
    (((counter >> 5) & 0xFFF8) | (counter & 0x0007)) as usize
}

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

/// Map a 4-level FLEX symbol (0..=3) to its `(bit_a, bit_b)` dibit.
///
/// (multimon-ng `read_data`: `bit_a = sym > 1`, `bit_b = sym == 1 || sym == 2`.)
/// This is a Gray code: adjacent symbols differ in exactly one bit.
pub fn symbol_to_dibit(sym: u8) -> (u8, u8) {
    let bit_a = (sym > 1) as u8;
    let bit_b = (sym == 1 || sym == 2) as u8;
    (bit_a, bit_b)
}

/// Inverse of [`symbol_to_dibit`]: the 4-level symbol carrying `(bit_a, bit_b)`.
/// Used by the modulator so the round trip is exact.
pub fn dibit_to_symbol(bit_a: u8, bit_b: u8) -> u8 {
    match (bit_a & 1, bit_b & 1) {
        (0, 0) => 0,
        (0, 1) => 1,
        (1, 1) => 2,
        (1, 0) => 3,
        _ => unreachable!(),
    }
}

/// The sync bit a 4-level symbol contributes to the Sync-1 hunt: multimon-ng
/// feeds `(sym < 2) ? 1 : 0` into the 64-bit sync shift register, i.e. the
/// complement of `bit_a`. (For a 2-level demod, sym ∈ {0,3} so this is the
/// inverse of the sliced bit; polarity is resolved by trying both.)
pub fn symbol_sync_bit(sym: u8) -> u8 {
    (sym < 2) as u8
}

/// Streaming 4-level FSK → symbol demodulator for one FLEX channel.
///
/// Front end is the same frequency discriminator + slow DC tracker as
/// [`FskDemod`], but symbol timing uses a **Gardner** error detector (which,
/// unlike the 2-level zero-crossing nudge, is robust for multilevel FSK whose
/// inner tones cross zero mid-symbol). The discriminator is resampled by a
/// fractional symbol clock; at each symbol the center value is sliced into one
/// of four levels (0..=3) about ±[`SLICE_THRESHOLD`]·envelope — the inner/outer
/// split of multimon-ng's `buildSymbol` slicer.
pub struct SymbolDemod {
    samples_per_sym: f64,
    prev_sample: Complex<f32>,
    freq_offset: f32,
    /// Discriminator history for interpolation (center, half, prev-center).
    disc_hist: Vec<f32>,
    /// Fractional symbol-clock phase, in samples since the last symbol center.
    mu: f64,
    /// Last symbol-center discriminator value (for Gardner TED).
    last_center: f32,
    /// Mid-point discriminator value between last and current symbol.
    last_mid: f32,
    /// Outer-tone magnitude estimate: an EMA over the |center| values in the
    /// UPPER cluster (those ≥ half the current estimate). Tracking only the
    /// upper cluster keeps it unbiased by whatever inner/outer mix the data
    /// carries (a plain |center| mean would sag to 2/3·outer on random data).
    /// The inner/outer slice threshold is [`SLICE_THRESHOLD`]·`mag_hi` ≈ the
    /// midpoint between the inner (outer/3) and outer levels.
    mag_hi: f32,
    level: f32,
}

/// Outer/inner tone split as a fraction of the running envelope.
/// (multimon-ng `#define SLICE_THRESHOLD 0.667`.)
const SLICE_THRESHOLD: f32 = 0.667;
/// Magnitude-cluster (inner/outer) EMA tracking factor.
const MAG_ALPHA: f32 = 0.02;
/// Gardner timing-loop gain.
const GARDNER_GAIN: f64 = 0.02;
/// DC / carrier-offset tracking factor for the 4-level demod. MUCH slower than
/// the 2-level [`FREQ_ALPHA`]: 4-level FLEX has long idle runs of one tone, so a
/// fast tracker would wrongly absorb a sustained symbol and pull it toward the
/// slicer center. The DDC already centers the channel, so only a tiny static
/// residual must be removed.
const SYM_FREQ_ALPHA: f32 = 0.000_01;

impl SymbolDemod {
    /// Build a 4-level symbol demod for [`CHANNEL_RATE`] at `sym_rate` sym/s.
    pub fn new(sym_rate: f64) -> Self {
        let samples_per_sym = CHANNEL_RATE / sym_rate;
        assert!(
            samples_per_sym >= 4.0,
            "need ≥4 samples/symbol for FSK timing"
        );
        Self {
            samples_per_sym,
            prev_sample: Complex::new(0.0, 0.0),
            freq_offset: 0.0,
            disc_hist: Vec::new(),
            mu: 0.0,
            last_center: 0.0,
            last_mid: 0.0,
            mag_hi: 0.0,
            level: 0.0,
        }
    }

    /// Inner/outer slice threshold = [`SLICE_THRESHOLD`]·outer-magnitude.
    fn threshold(&self) -> f32 {
        self.mag_hi.max(1e-6) * SLICE_THRESHOLD
    }

    /// Slice a discriminator value into a 4-level symbol: sign picks the half,
    /// |value| vs the inner/outer threshold picks inner vs outer.
    fn slice(&self, v: f32) -> u8 {
        let thr = self.threshold();
        if v >= thr {
            3
        } else if v >= 0.0 {
            2
        } else if v > -thr {
            1
        } else {
            0
        }
    }

    /// Feed channel IQ; append one 4-level symbol (0..=3) per recovered symbol.
    ///
    /// Each input sample yields a discriminator value pushed onto a small
    /// history; whenever the fractional symbol clock `mu` advances past a full
    /// symbol period, the symbol-center and half-symbol discriminator values are
    /// linearly interpolated, the center is sliced, and a Gardner TED nudges the
    /// clock (`e = (center − prev_center)·mid`).
    pub fn process(&mut self, input: &[Complex<f32>], syms: &mut Vec<u8>) {
        let sps = self.samples_per_sym;
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);
            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += SYM_FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            self.disc_hist.push(disc);
            self.mu += 1.0;

            // When a whole symbol has elapsed, sample center + midpoint.
            if self.mu >= sps {
                // Indices (from the end of disc_hist) for the symbol center and
                // the half-symbol point, interpolated.
                let n = self.disc_hist.len();
                // The newest sample is the symbol boundary; center is half a
                // symbol back, midpoint is 3/4 symbol back (quarter before
                // boundary == midpoint between centers).
                let center = self.interp_back(n, sps * 0.5);
                let mid = self.interp_back(n, sps * 0.25);

                // Track the outer-tone magnitude from the UPPER cluster only
                // (|center| ≥ half the current estimate). Seed on the first
                // symbol from the outer-tone dotting preamble.
                let mag = center.abs();
                if self.mag_hi <= 1e-6 {
                    self.mag_hi = mag;
                } else if mag >= 0.5 * self.mag_hi {
                    self.mag_hi += MAG_ALPHA * (mag - self.mag_hi);
                }
                syms.push(self.slice(center));

                // Gardner TED on the discriminator: e = mid·(center − prev).
                let e = (self.last_mid as f64) * (center - self.last_center) as f64;
                self.last_center = center;
                self.last_mid = mid;
                self.mu -= sps - GARDNER_GAIN * e.clamp(-1.0, 1.0);

                // Drop consumed history, keep a small tail for interpolation.
                let keep = (sps as usize) + 4;
                if self.disc_hist.len() > keep {
                    let drop = self.disc_hist.len() - keep;
                    self.disc_hist.drain(0..drop);
                }
            }
        }
    }

    /// Linearly interpolate the discriminator `back` samples before the newest
    /// (index `n-1`) entry of the history buffer.
    fn interp_back(&self, n: usize, back: f64) -> f32 {
        if n == 0 {
            return 0.0;
        }
        let pos = (n as f64 - 1.0) - back;
        if pos <= 0.0 {
            return self.disc_hist[0];
        }
        let i = pos.floor() as usize;
        let frac = (pos - i as f64) as f32;
        let a = self.disc_hist[i.min(n - 1)];
        let b = self.disc_hist[(i + 1).min(n - 1)];
        a + (b - a) * frac
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

/// Locate the 64-bit FLEX Sync 1 (`A | 0xA6C6AAAA | ~A`) in a stream of
/// **sync bits** and resolve the on-air mode from the recovered A-code.
///
/// `sync_bits[i] = symbol_sync_bit(sym_i)`. Scans each 64-bit window: the middle
/// 32 bits must match [`crate::frame::SYNC_MARKER_B`] (≤`max_err`), and the top
/// 16 bits (A) must be the bit-complement of the bottom 16 (~A) (≤`max_err`).
/// Both polarities are tried. Returns `(bit_offset, inverted, mode)` where
/// `bit_offset` is the index of the FIRST sync bit (the MSB of A) and `mode` is
/// resolved from the A-code; an unrecognized A-code yields no match.
/// (multimon-ng `flex_sync` / `flex_sync_check` / `decode_mode`.)
pub fn find_sync_mode(
    sync_bits: &[u8],
    max_err: u32,
) -> Option<(usize, bool, FlexMode)> {
    if sync_bits.len() < 64 {
        return None;
    }
    for off in 0..=(sync_bits.len() - 64) {
        let mut buf: u64 = 0;
        for &b in &sync_bits[off..off + 64] {
            buf = (buf << 1) | (b as u64 & 1);
        }
        for inverted in [false, true] {
            let w = if inverted { !buf } else { buf };
            let m = ((w & 0x0000_FFFF_FFFF_0000) >> 16) as u32;
            let code_high = ((w & 0xFFFF_0000_0000_0000) >> 48) as u16;
            let code_low = (!(w & 0x0000_0000_0000_FFFF)) as u16;
            if (m ^ crate::frame::SYNC_MARKER_B).count_ones() <= max_err
                && (code_low ^ code_high).count_ones() <= max_err
            {
                if let Some(mode) = FlexMode::from_a_code(code_high, max_err) {
                    return Some((off, inverted, mode));
                }
            }
        }
    }
    None
}

/// De-interleave a DATA-section symbol stream into the mode's phase word
/// buffers. Returns one 88-entry `Vec<u32>` per active phase (A first, then
/// B/C/D per [`FlexMode::num_phases`]), each word in FLEX-native orientation
/// (first-received data bit = bit 0). `inverted` flips every symbol's polarity
/// (the polarity resolved at sync). (multimon-ng `read_data` phase routing.)
pub fn deinterleave_phases(syms: &[u8], mode: FlexMode, inverted: bool) -> Vec<Vec<u32>> {
    let mut phases = [
        vec![0u32; crate::frame::WORDS_PER_PHASE],
        vec![0u32; crate::frame::WORDS_PER_PHASE],
        vec![0u32; crate::frame::WORDS_PER_PHASE],
        vec![0u32; crate::frame::WORDS_PER_PHASE],
    ];
    let four_level = mode.levels == 4;
    let two_phase_clock = mode.sym_rate == 3200;
    let mut counter: u32 = 0;
    let mut toggle = 0u8;

    for &raw_sym in syms {
        // Polarity: inverting the symbol order reverses level numbering
        // (sym -> 3 - sym), matching the demod's both-polarity sync resolution.
        let sym = if inverted { 3 - raw_sym.min(3) } else { raw_sym };
        let (bit_a, bit_b) = symbol_to_dibit(sym);
        let idx = phase_idx(counter);

        // At 1600 sym/s there is a single (A,B) clock; at 3200 sym/s symbols
        // alternate (A,B) / (C,D).
        let (slot_a, slot_b) = if two_phase_clock && toggle == 1 {
            (2usize, 3usize) // C, D
        } else {
            (0usize, 1usize) // A, B
        };
        // bit_a -> PhaseA/C ; bit_b -> PhaseB/D (only for 4-level).
        phases[slot_a][idx] = (phases[slot_a][idx] >> 1) | ((bit_a as u32) << 31);
        if four_level {
            phases[slot_b][idx] = (phases[slot_b][idx] >> 1) | ((bit_b as u32) << 31);
        }

        if two_phase_clock {
            if toggle == 1 {
                counter += 1;
                toggle = 0;
            } else {
                toggle = 1;
            }
        } else {
            counter += 1;
        }
    }

    let n = mode.num_phases();
    // Phase order: A,B,C,D. For (3200,2) the active phases are A,C — but this
    // crate only exposes the 4-level modes, so A,B (n=2) and A,B,C,D (n=4).
    (0..n).map(|p| phases[p].clone()).collect()
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

    /// SPEC table (multimon-ng `read_data`): the 4-level symbol → (bit_a, bit_b)
    /// Gray map must match verbatim, and dibit→symbol must invert it.
    #[test]
    fn symbol_dibit_gray_map_matches_spec() {
        // sym : bit_a bit_b   (bit_a = sym>1 ; bit_b = sym==1 || sym==2)
        let table = [
            (0u8, (0u8, 0u8)),
            (1, (0, 1)),
            (2, (1, 1)),
            (3, (1, 0)),
        ];
        for (sym, (a, b)) in table {
            assert_eq!(symbol_to_dibit(sym), (a, b), "sym {sym} dibit");
            assert_eq!(dibit_to_symbol(a, b), sym, "dibit ({a},{b}) -> sym");
        }
        // Gray property: adjacent symbols differ in exactly one dibit bit.
        for s in 0u8..3 {
            let (a0, b0) = symbol_to_dibit(s);
            let (a1, b1) = symbol_to_dibit(s + 1);
            assert_eq!((a0 ^ a1) + (b0 ^ b1), 1, "sym {s}->{} not Gray", s + 1);
        }
    }

    /// SPEC A-codes (multimon-ng `flex_modes[]`): each documented code resolves
    /// to the right symbol rate + level count and information baud.
    #[test]
    fn a_code_modes_match_spec() {
        let cases = [
            (A_CODE_1600_2, 1600u32, 2u8, 1600u32),
            (A_CODE_1600_4, 1600, 4, 3200),
            (A_CODE_3200_2, 3200, 2, 3200),
            (A_CODE_3200_4, 3200, 4, 6400),
            (A_CODE_3200_4_ALT, 3200, 4, 6400),
        ];
        for (code, sr, lv, baud) in cases {
            let m = FlexMode::from_a_code(code, 0).unwrap_or_else(|| panic!("{code:#06x}"));
            assert_eq!(m.sym_rate, sr, "{code:#06x} sym_rate");
            assert_eq!(m.levels, lv, "{code:#06x} levels");
            assert_eq!(m.baud(), baud, "{code:#06x} baud");
        }
        assert_eq!(A_CODE_1600_4, 0xB068);
        assert_eq!(A_CODE_3200_4, 0xDEA0);
        assert_eq!(A_CODE_3200_2, 0x7B18);
        assert_eq!(A_CODE_3200_4_ALT, 0x4C7C);
        // An unrelated 16-bit value resolves to nothing.
        assert!(FlexMode::from_a_code(0x0000, 0).is_none());
    }

    #[test]
    fn from_baud_maps_information_rates() {
        assert_eq!(
            FlexMode::from_baud(3200).unwrap(),
            FlexMode { sym_rate: 1600, levels: 4 }
        );
        assert_eq!(
            FlexMode::from_baud(6400).unwrap(),
            FlexMode { sym_rate: 3200, levels: 4 }
        );
        assert_eq!(FlexMode::from_baud(1600).unwrap().num_phases(), 1);
        assert_eq!(FlexMode::from_baud(3200).unwrap().num_phases(), 2);
        assert_eq!(FlexMode::from_baud(6400).unwrap().num_phases(), 4);
        assert!(FlexMode::from_baud(2400).is_none());
    }

    #[test]
    fn data_section_lengths_match_spec() {
        // multimon-ng: 2816 symbols @1600 sym/s, 5632 @3200 sym/s (1760 ms).
        assert_eq!(data_symbols(1600), 2816);
        assert_eq!(data_symbols(3200), 5632);
    }

    /// `phase_idx` block-interleave: each of the 88 words is filled exactly 32
    /// times over a full phase, and within an 8-word block the bits round-robin.
    #[test]
    fn phase_idx_fills_88_words_evenly() {
        let mut counts = [0u32; 88];
        for c in 0..(88u32 * 32) {
            counts[phase_idx(c)] += 1;
        }
        assert!(counts.iter().all(|&v| v == 32), "uneven phase fill");
        // First 8 counters address words 0..=7 in order (column interleave).
        for c in 0..8u32 {
            assert_eq!(phase_idx(c), c as usize);
        }
        // Counter 256 starts the next 8-word block (words 8..15).
        assert_eq!(phase_idx(256), 8);
    }

    /// find_sync_mode locks the full 64-bit Sync-1 and recovers the mode from
    /// the A-code, in both polarities.
    #[test]
    fn find_sync_mode_locks_and_resolves_mode() {
        let a = A_CODE_3200_4; // 6400 bps mode
        let sync64: u64 = ((a as u64) << 48)
            | ((SYNC_MARKER_B as u64) << 16)
            | ((!a) as u64 & 0xFFFF);
        let mut bits = vec![1u8, 0, 1]; // junk
        for i in (0..64).rev() {
            bits.push(((sync64 >> i) & 1) as u8);
        }
        let (off, inv, mode) = find_sync_mode(&bits, 3).expect("sync must lock");
        assert_eq!(off, 3);
        assert!(!inv);
        assert_eq!(mode.baud(), 6400);
        assert_eq!(mode.levels, 4);

        // Inverted polarity.
        let mut binv = Vec::new();
        for i in (0..64).rev() {
            binv.push(((!sync64 >> i) & 1) as u8);
        }
        let (_, inv2, mode2) = find_sync_mode(&binv, 3).expect("inverted sync");
        assert!(inv2);
        assert_eq!(mode2.baud(), 6400);
    }
}
