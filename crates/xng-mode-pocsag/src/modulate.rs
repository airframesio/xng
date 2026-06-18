//! POCSAG 2-FSK modulator for self-generated demod validation.
//!
//! Turns a bit stream (preamble + sync + codewords, MSB-first) into baud-rate
//! ±deviation binary FSK IQ so [`crate::PocsagChannelDecoder`] can be exercised
//! end-to-end without a recorded capture.
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate→demod path. The
//! waveform parameters (512/1200/2400 Bd, ±4.5 kHz deviation) are the published
//! POCSAG / ITU-R M.584-2 spec, but the modulator itself is not an external
//! reference. It validates only that the demod inverts this modulation; the
//! DECODE/framing core stays spec-anchored by its own codeword tests. Tests
//! using it are named `*_synth_iq`.

use num_complex::Complex;
use std::f64::consts::TAU;

/// Typical POCSAG FSK deviation from center to each tone, Hz.
pub const DEVIATION_HZ: f64 = 4_500.0;

/// Build the full on-air bit stream for a batch transmission:
/// `preamble_bits` of alternating 1/0, then the sync codeword (MSB-first),
/// then the supplied 32-bit codewords (MSB-first each).
pub fn frame_bits(preamble_bits: usize, codewords: &[u32]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(preamble_bits + 32 + codewords.len() * 32);
    // Preamble: alternating, starting with 1 (POCSAG reversal sequence).
    for i in 0..preamble_bits {
        bits.push((i % 2 == 0) as u8);
    }
    push_word(&mut bits, crate::bch::SYNC_CODEWORD);
    for &cw in codewords {
        push_word(&mut bits, cw);
    }
    bits
}

fn push_word(bits: &mut Vec<u8>, w: u32) {
    for i in (0..32).rev() {
        bits.push(((w >> i) & 1) as u8);
    }
}

/// Modulate a bit stream as `baud`-rate ±[`DEVIATION_HZ`] FSK IQ at
/// `sample_rate`.
///
/// `bit = 1` → `freq_offset_hz + DEVIATION_HZ`; `bit = 0` →
/// `freq_offset_hz - DEVIATION_HZ`. Continuous phase across bits. This matches
/// the demod's "positive tone slices to 1" convention, so the round trip is
/// non-inverted.
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
        let freq = freq_offset_hz + if bit != 0 { DEVIATION_HZ } else { -DEVIATION_HZ };
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Add complex AWGN at a controlled `snr_db` (signal power vs noise power) to a
/// copy of `iq`. Deterministic PRNG so the BER test is reproducible.
///
/// SNR is referenced to the mean signal power of `iq`. Noise is added
/// independently to I and Q (each gets half the noise power).
pub fn add_awgn(iq: &[Complex<f32>], snr_db: f64, seed: u64) -> Vec<Complex<f32>> {
    let sig_pow: f64 =
        iq.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / iq.len().max(1) as f64;
    let snr_lin = 10f64.powf(snr_db / 10.0);
    let noise_pow = sig_pow / snr_lin;
    // Per-component standard deviation (I and Q each carry half the power).
    let sigma = (noise_pow / 2.0).sqrt();
    let mut rng = Lcg::new(seed);
    iq.iter()
        .map(|&s| {
            let (n_i, n_q) = rng.gaussian_pair();
            Complex::new(s.re + (sigma * n_i) as f32, s.im + (sigma * n_q) as f32)
        })
        .collect()
}

/// Small linear-congruential PRNG + Box-Muller, for reproducible AWGN in tests.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed.wrapping_mul(6364136223846793005).wrapping_add(1) }
    }
    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes LCG constants.
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
    fn next_unit(&mut self) -> f64 {
        // Uniform in (0,1].
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    /// Two independent standard-normal samples (Box-Muller).
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
    fn frame_bits_has_preamble_then_sync() {
        let bits = frame_bits(8, &[]);
        // 8 preamble bits then 32 sync bits.
        assert_eq!(bits.len(), 40);
        // Preamble alternates starting with 1.
        assert_eq!(&bits[..8], &[1, 0, 1, 0, 1, 0, 1, 0]);
        // Reconstruct the sync word from bits[8..40].
        let mut w = 0u32;
        for &b in &bits[8..40] {
            w = (w << 1) | b as u32;
        }
        assert_eq!(w, crate::bch::SYNC_CODEWORD);
    }

    #[test]
    fn modulate_emits_expected_sample_count() {
        let iq = modulate_iq(&[1, 0, 1, 0], 38_400.0, 1200.0, 0.0, 1.0);
        // 4 bits at 32 samples/bit.
        assert_eq!(iq.len(), 4 * 32);
    }

    #[test]
    fn awgn_preserves_length_and_adds_power() {
        let iq = modulate_iq(&[1, 0, 1, 0, 1, 1, 0, 0], 38_400.0, 1200.0, 0.0, 1.0);
        let noisy = add_awgn(&iq, 10.0, 42);
        assert_eq!(noisy.len(), iq.len());
        // Noisy power should exceed clean (noise added).
        let clean_pow: f64 = iq.iter().map(|s| s.norm_sqr() as f64).sum();
        let noisy_pow: f64 = noisy.iter().map(|s| s.norm_sqr() as f64).sum();
        assert!(noisy_pow > clean_pow * 0.5);
    }
}
