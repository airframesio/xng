//! FLEX 2-FSK / 4-FSK modulator for self-generated demod validation.
//!
//! Turns a bit stream (dotting preamble + Sync 1 marker + FLEX data words) into
//! baud-rate ±deviation binary FSK IQ — and, for the 4-level modes, a full
//! symbol-domain frame (Sync 1 A-code + FIW + Sync 2 + column-interleaved
//! phases) into 4-level FSK IQ — so [`crate::FlexChannelDecoder`] can be
//! exercised end-to-end without a recorded capture.
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate→demod path. The
//! waveform parameters (1600/3200 sym/s, ±4.8 kHz outer deviation, inner tones
//! at ±1/3) are the published FLEX PHY, but the modulator is not an external
//! reference. It validates only that the demod inverts this modulation; the
//! DECODE/framing core stays spec-anchored by its own word/FIW/BIW tests. Tests
//! using it are named `*_synth_iq`.

use crate::demod;
use num_complex::Complex;
use std::f64::consts::TAU;

/// FLEX 2-level / 4-level outer FSK deviation from center to each outer tone, Hz.
pub const DEVIATION_HZ: f64 = 4_800.0;

/// Push a 32-bit word MSB-first (used for the Sync 1 marker, which the demod
/// hunts MSB-first).
fn push_word_msb(bits: &mut Vec<u8>, w: u32) {
    for i in (0..32).rev() {
        bits.push(((w >> i) & 1) as u8);
    }
}

/// Push a 32-bit word LSB-first (FLEX on-air data-word order).
pub fn push_word_lsb(bits: &mut Vec<u8>, w: u32) {
    for i in 0..32 {
        bits.push(((w >> i) & 1) as u8);
    }
}

/// Build the on-air bit stream for a FLEX transmission, matching the FLEX
/// Sync 1 structure `AAAA : A6C6AAAA : CCCC`:
/// `preamble_bits` of alternating 1/0 dotting, then the Sync 1 marker
/// (`0xA6C6AAAA`, MSB-first), then the 16-bit C field (inverted-A), then the
/// supplied 32-bit data words LSB-first (FIW, then the 88 phase words).
///
/// The decoder ([`crate::decode_bits`]) locks the 32-bit marker and then skips
/// the trailing 16-bit C field to reach the FIW, so the modulator emits that
/// 16-bit field to keep the on-air layout faithful.
pub fn frame_bits(preamble_bits: usize, data_words: &[u32]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(preamble_bits + 48 + data_words.len() * 32);
    for i in 0..preamble_bits {
        bits.push((i % 2 == 0) as u8);
    }
    push_word_msb(&mut bits, crate::frame::SYNC_MARKER_B);
    // 16-bit C field (inverted A). A is the per-rate sync code; for the test
    // waveform the exact value is immaterial — the decoder only skips 16 bits
    // here — so emit a representative inverted-A pattern (0x870C ^ 0xFFFF).
    let c = (0x870Cu32 ^ 0xFFFF) & 0xFFFF;
    for i in (0..16).rev() {
        bits.push(((c >> i) & 1) as u8);
    }
    for &w in data_words {
        push_word_lsb(&mut bits, w);
    }
    bits
}

/// Modulate a bit stream as `baud`-rate ±[`DEVIATION_HZ`] FSK IQ at
/// `sample_rate`. `bit = 1` → higher tone; `bit = 0` → lower tone. Continuous
/// phase. Matches the demod's "positive tone slices to 1" convention so the
/// round trip is non-inverted.
pub fn modulate_iq(
    bits: &[u8],
    sample_rate: f64,
    baud: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / baud;
    let mut out = Vec::with_capacity((bits.len() as f64 * spb) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut emitted: usize = 0;
    for (i, &bit) in bits.iter().enumerate() {
        let freq = freq_offset_hz
            + if bit != 0 {
                DEVIATION_HZ
            } else {
                -DEVIATION_HZ
            };
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Map a 4-level symbol (0..=3) to its FSK frequency offset, consistent with
/// the demod slicer (sym 0 = lowest tone, sym 3 = highest). The outer tones sit
/// at ±[`DEVIATION_HZ`], the inner tones at ±1/3 of that.
pub fn symbol_freq(sym: u8) -> f64 {
    let d = DEVIATION_HZ;
    match sym {
        0 => -d,
        1 => -d / 3.0,
        2 => d / 3.0,
        _ => d,
    }
}

/// Build the on-air **symbol** stream for a 4-level FLEX frame at `mode`:
///
/// `dotting` symbols of alternating outer tones, then Sync 1
/// (`A | 0xA6C6AAAA | ~A` as 64 sync bits → outer-tone symbols), then 16
/// dotting symbols, then the 32-symbol FIW (`fiw_word` MSB-into-LSB via
/// `sym>1`), then `sync2` dotting symbols, then the DATA section formed by
/// column-interleaving the supplied phase word buffers (A,B[,C,D]).
///
/// `phases` must hold [`crate::frame::WORDS_PER_PHASE`] words each, one entry
/// per active phase ([`demod::FlexMode::num_phases`]). The interleave inverts
/// [`demod::deinterleave_phases`] exactly. Sync/FIW use outer tones (0/3) so the
/// 2-level sync hunt and `sym>1` FIW read are unambiguous.
pub fn frame_symbols(
    dotting: usize,
    mode: demod::FlexMode,
    a_code: u16,
    fiw_word: u32,
    sync2: usize,
    phases: &[Vec<u32>],
) -> Vec<u8> {
    // The Sync-1 hunt reads `(sym<2)`, so a sync bit "1" must be a LOW tone
    // (sym 0) for the demod to lock at non-inverted polarity. The FIW is read
    // as `(sym>1)` (bit_a), so a FIW bit "1" must be a HIGH tone (sym 3). These
    // opposite senses are intrinsic to the FLEX PHY (the marker is defined in
    // the `sym<2` domain); the decoder's both-polarity resolution handles either.
    let sync_sym = |b: u8| -> u8 { if b != 0 { 0 } else { 3 } };
    let fiw_sym = |b: u8| -> u8 { if b != 0 { 3 } else { 0 } };

    let mut syms = Vec::new();
    // Dotting: alternate outer tones.
    for i in 0..dotting {
        syms.push(if i % 2 == 0 { 3 } else { 0 });
    }

    // Sync 1: 64 bits = A(16) | marker(32) | ~A(16), MSB-first.
    let sync64: u64 = ((a_code as u64) << 48)
        | ((crate::frame::SYNC_MARKER_B as u64) << 16)
        | ((!a_code) as u64 & 0xFFFF);
    for i in (0..64).rev() {
        syms.push(sync_sym(((sync64 >> i) & 1) as u8));
    }

    // 16 dotting symbols before the FIW.
    for i in 0..16 {
        syms.push(if i % 2 == 0 { 3 } else { 0 });
    }

    // FIW: 32 symbols, first-emitted symbol becomes bit 0 after 32 right-shifts,
    // i.e. emit bit 0 first (LSB-first emission).
    for i in 0..32 {
        syms.push(fiw_sym(((fiw_word >> i) & 1) as u8));
    }

    // Sync 2 dotting.
    for i in 0..sync2 {
        syms.push(if i % 2 == 0 { 3 } else { 0 });
    }

    // DATA: column-interleave the phases (inverse of deinterleave_phases).
    syms.extend(interleave_phases(mode, phases));

    // Trailing guard dotting: the symbol demod samples each symbol's center
    // half a period before the newest sample, so a few guard symbols ensure the
    // final DATA symbol completes (on air this is the next frame's preamble).
    for i in 0..16 {
        syms.push(if i % 2 == 0 { 3 } else { 0 });
    }
    syms
}

/// Column-interleave phase word buffers into the DATA symbol stream — the exact
/// inverse of [`demod::deinterleave_phases`] (no polarity inversion). For each
/// symbol-counter position the de-interleaver fills `idx = phase_idx(counter)`,
/// bit `(counter mod 256)/8` of that word, by shifting in MSB-first (so bit 0 of
/// the word was shifted in first). We replay that order, recovering bit_a/bit_b
/// per phase and packing back to a 4-level symbol.
pub fn interleave_phases(mode: demod::FlexMode, phases: &[Vec<u32>]) -> Vec<u8> {
    let n_data = demod::data_symbols(mode.sym_rate);
    let four = mode.levels == 4;
    let two_phase_clock = mode.sym_rate == 3200;
    let mut out = Vec::with_capacity(n_data);

    let mut counter: u32 = 0;
    let mut toggle = 0u8;
    let words_per_phase = crate::frame::WORDS_PER_PHASE;

    let bit_of = |phase: &Vec<u32>, idx: usize, bitpos: usize| -> u8 {
        if idx < phase.len() {
            ((phase[idx] >> bitpos) & 1) as u8
        } else {
            0
        }
    };

    while out.len() < n_data {
        let idx = demod::phase_idx(counter) % words_per_phase.max(1);
        // bit position within the word: (counter mod 256) / 8 (LSB-first fill).
        let bitpos = ((counter % 256) / 8) as usize;
        let (pa, pb) = if two_phase_clock && toggle == 1 {
            (2usize, 3usize)
        } else {
            (0usize, 1usize)
        };
        let bit_a = phases.get(pa).map_or(0, |p| bit_of(p, idx, bitpos));
        let bit_b = if four {
            phases.get(pb).map_or(0, |p| bit_of(p, idx, bitpos))
        } else {
            0
        };
        out.push(demod::dibit_to_symbol(bit_a, bit_b));

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
    out
}

/// Modulate a 4-level **symbol** stream as `sym_rate`-rate FSK IQ at
/// `sample_rate`, with the four tones at [`symbol_freq`]. Continuous phase.
pub fn modulate_symbols_iq(
    syms: &[u8],
    sample_rate: f64,
    sym_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let sps = sample_rate / sym_rate;
    let mut out = Vec::with_capacity((syms.len() as f64 * sps) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut emitted: usize = 0;
    for (i, &sym) in syms.iter().enumerate() {
        let freq = freq_offset_hz + symbol_freq(sym);
        let end = (((i + 1) as f64) * sps).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Add complex AWGN at a controlled `snr_db` to a copy of `iq`. Deterministic
/// PRNG so the BER test is reproducible. SNR referenced to mean signal power.
pub fn add_awgn(iq: &[Complex<f32>], snr_db: f64, seed: u64) -> Vec<Complex<f32>> {
    let sig_pow: f64 = iq.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / iq.len().max(1) as f64;
    let snr_lin = 10f64.powf(snr_db / 10.0);
    let noise_pow = sig_pow / snr_lin;
    let sigma = (noise_pow / 2.0).sqrt();
    let mut rng = Lcg::new(seed);
    iq.iter()
        .map(|&s| {
            let (n_i, n_q) = rng.gaussian_pair();
            Complex::new(s.re + (sigma * n_i) as f32, s.im + (sigma * n_q) as f32)
        })
        .collect()
}

/// Small LCG + Box-Muller for reproducible AWGN in tests.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_mul(6364136223846793005).wrapping_add(1),
        }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
    fn next_unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    fn gaussian_pair(&mut self) -> (f64, f64) {
        let u1 = self.next_unit();
        let u2 = self.next_unit();
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = TAU * u2;
        (r * theta.cos(), r * theta.sin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bits_has_preamble_then_marker() {
        let bits = frame_bits(8, &[]);
        // 8 preamble + 32 marker + 16 C field.
        assert_eq!(bits.len(), 56);
        assert_eq!(&bits[..8], &[1, 0, 1, 0, 1, 0, 1, 0]);
        let mut w = 0u32;
        for &b in &bits[8..40] {
            w = (w << 1) | b as u32;
        }
        assert_eq!(w, crate::frame::SYNC_MARKER_B);
    }

    #[test]
    fn modulate_emits_expected_sample_count() {
        let iq = modulate_iq(&[1, 0, 1, 0], 64_000.0, 1600.0, 0.0, 1.0);
        assert_eq!(iq.len(), 4 * 40); // 40 samples/bit at 64k/1600
    }

    #[test]
    fn awgn_preserves_length() {
        let iq = modulate_iq(&[1, 0, 1, 0, 1, 1, 0, 0], 64_000.0, 1600.0, 0.0, 1.0);
        let noisy = add_awgn(&iq, 10.0, 42);
        assert_eq!(noisy.len(), iq.len());
    }
}
