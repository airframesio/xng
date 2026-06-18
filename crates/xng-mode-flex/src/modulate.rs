//! FLEX 2-FSK modulator for self-generated demod validation.
//!
//! Turns a bit stream (dotting preamble + Sync 1 marker + FLEX data words) into
//! baud-rate ±deviation binary FSK IQ so [`crate::FlexChannelDecoder`] can be
//! exercised end-to-end without a recorded capture.
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate→demod path. The
//! waveform parameters (1600 Bd, ±4.8 kHz deviation) are the published FLEX
//! 2-level PHY, but the modulator is not an external reference. It validates
//! only that the demod inverts this modulation; the DECODE/framing core stays
//! spec-anchored by its own word/FIW/BIW tests. Tests using it are named
//! `*_synth_iq`.

use num_complex::Complex;
use std::f64::consts::TAU;

/// FLEX 2-level FSK deviation from center to each tone, Hz.
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
