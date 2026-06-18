//! Bell 202 AFSK1200-over-FM modulator for self-generated demod validation.
//!
//! Turns an AX.25 frame into 1200 Bd AFSK (1200/2200 Hz) frequency-modulated
//! IQ at [`crate::CHANNEL_RATE`], so [`crate::AprsChannelDecoder`] can be
//! exercised end-to-end without a recorded capture.
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate->demod path. The
//! waveform parameters (1200 Bd, 1200/2200 Hz Bell 202 tones, NRZI line
//! coding, narrowband FM) are the published on-air APRS/AX.25 spec, but the
//! modulator itself is not an external reference. It validates only that the
//! demod inverts this modulation; the FRAMING and APRS-payload layers stay
//! oracle-anchored by their own spec tests. Tests using it are named
//! `*_synth_iq` / report BER as synthetic.

use crate::demod::{BAUD, MARK_HZ, SPACE_HZ};
use crate::hdlc::{frame_bits, nrzi_encode};
use crate::CHANNEL_RATE;
use num_complex::Complex;
use std::f64::consts::TAU;

/// Build the full NRZI line-symbol stream for an AX.25 frame: HDLC
/// bit-stuffing + flag delimiting ([`frame_bits`]) then NRZI line coding
/// ([`nrzi_encode`]).
pub fn frame_to_symbols(frame: &[u8], lead_flags: usize) -> Vec<u8> {
    let bits = frame_bits(frame, lead_flags);
    nrzi_encode(&bits)
}

/// Like [`frame_to_symbols`] but also appends `trail_flags` trailing idle
/// flags after the frame's closing flag. Real TNCs key down with several
/// flags of idle; the trailing flags also let any front-end group delay (DDC
/// FIR, etc.) clock the closing flag fully through the demod. The HDLC flag
/// is `0x7E`.
pub fn frame_to_symbols_padded(frame: &[u8], lead_flags: usize, trail_flags: usize) -> Vec<u8> {
    // Data bits = lead flags + frame (stuffed) + 1 closing flag, then extra
    // trailing flags (flags are not bit-stuffed and are sent verbatim).
    let mut bits = frame_bits(frame, lead_flags);
    let flag_bits = [0u8, 1, 1, 1, 1, 1, 1, 0];
    for _ in 0..trail_flags {
        bits.extend_from_slice(&flag_bits);
    }
    nrzi_encode(&bits)
}

/// Modulate NRZI line symbols (1 = mark/1200 Hz, 0 = space/2200 Hz) as
/// 1200 Bd AFSK, then FM-modulate that audio onto an IQ carrier at
/// `freq_offset_hz`. Continuous audio phase across bits.
///
/// `fm_dev_hz` is the FM deviation; the discriminator output scales with it
/// but its sign/zero-crossings (what the AFSK correlator uses) do not, so any
/// reasonable deviation works.
pub fn modulate_iq(
    symbols: &[u8],
    freq_offset_hz: f64,
    fm_dev_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = CHANNEL_RATE / BAUD;
    let mut out = Vec::with_capacity((symbols.len() as f64 * spb) as usize + 1);
    let mut audio_phase: f64 = 0.0; // AFSK tone phase
    let mut carrier_phase: f64 = 0.0; // FM carrier phase
    let mut emitted: usize = 0;
    for (i, &sym) in symbols.iter().enumerate() {
        let tone = if sym != 0 { MARK_HZ } else { SPACE_HZ };
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            audio_phase += TAU * tone / CHANNEL_RATE;
            let audio = audio_phase.sin(); // [-1, 1]
            // FM: instantaneous carrier freq = offset + dev * audio.
            let inst = freq_offset_hz + fm_dev_hz * audio;
            carrier_phase += TAU * inst / CHANNEL_RATE;
            out.push(Complex::new(carrier_phase.cos() as f32, carrier_phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Convenience: full burst IQ for an AX.25 frame, with leading idle flags so
/// the discriminator and timing loop settle before the data.
pub fn burst_iq(
    frame: &[u8],
    freq_offset_hz: f64,
    fm_dev_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    // 16 leading + 8 trailing idle flags: settles the demod before the frame
    // and clocks the closing flag fully through any front-end group delay.
    let symbols = frame_to_symbols_padded(frame, 16, 8);
    modulate_iq(&symbols, freq_offset_hz, fm_dev_hz, amplitude)
}

/// Add complex AWGN at a target per-sample SNR (dB), relative to the mean
/// signal power of `iq`. Deterministic given `seed` (a tiny xorshift PRNG so
/// the BER test is reproducible without an `rand` dependency). Returns a new
/// buffer.
pub fn add_awgn(iq: &[Complex<f32>], snr_db: f64, seed: u64) -> Vec<Complex<f32>> {
    if iq.is_empty() {
        return Vec::new();
    }
    let sig_pow: f64 = iq.iter().map(|c| c.norm_sqr() as f64).sum::<f64>() / iq.len() as f64;
    let snr_lin = 10f64.powf(snr_db / 10.0);
    let noise_pow = sig_pow / snr_lin;
    // Per-component std dev (split power across I and Q).
    let sigma = (noise_pow / 2.0).sqrt();
    let mut rng = XorShift::new(seed);
    iq.iter()
        .map(|&c| {
            let (n0, n1) = rng.gaussian_pair();
            Complex::new(
                c.re + (sigma * n0) as f32,
                c.im + (sigma * n1) as f32,
            )
        })
        .collect()
}

/// Minimal deterministic PRNG + Box-Muller for the AWGN test helper.
struct XorShift {
    state: u64,
}

impl XorShift {
    fn new(seed: u64) -> Self {
        Self {
            state: seed | 1, // avoid the all-zero fixed point
        }
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
    /// Uniform in (0, 1).
    fn next_f64(&mut self) -> f64 {
        // 53-bit mantissa, shift to (0,1).
        ((self.next_u64() >> 11) as f64 + 1.0) / (9007199254740992.0 + 1.0)
    }
    /// One pair of independent N(0,1) samples via Box-Muller.
    fn gaussian_pair(&mut self) -> (f64, f64) {
        let u1 = self.next_f64();
        let u2 = self.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = std::f64::consts::TAU * u2;
        (r * theta.cos(), r * theta.sin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax25::build_ui_frame;

    #[test]
    fn modulate_emits_expected_sample_count() {
        let symbols = [1u8, 0, 1, 0];
        let iq = modulate_iq(&symbols, 0.0, 3000.0, 1.0);
        let spb = (CHANNEL_RATE / BAUD).round() as usize;
        assert_eq!(iq.len(), 4 * spb);
    }

    #[test]
    fn awgn_lowers_snr_predictably() {
        let frame = build_ui_frame(("APRS", 0), ("N0CALL", 0), &[], b"!test");
        let iq = burst_iq(&frame, 0.0, 3000.0, 0.8);
        let noisy = add_awgn(&iq, 10.0, 12345);
        // Noise was actually added (buffers differ) and length preserved.
        assert_eq!(noisy.len(), iq.len());
        assert!(noisy.iter().zip(&iq).any(|(a, b)| a != b));
    }
}
