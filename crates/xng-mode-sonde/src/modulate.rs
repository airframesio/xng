//! RS41 GFSK modulator — **self-generated, for the synthetic demod test only**.
//!
//! There is no captured RS41 IQ vector vendored in this crate (only the
//! published byte-level oracle frames from rs1729/RS). To validate the
//! IQ → bits → bytes demodulator end to end, the `*_synth_iq` test modulates
//! a *known on-air oracle frame* into GFSK IQ with this encoder, runs it
//! through [`crate::SondeChannelDecoder`], and asserts the recovered frame's
//! decoded fields equal the published values.
//!
//! This modulate→demod path is therefore self-consistent **by construction**;
//! the DECODE core (whitening / RS / sub-block parse) stays oracle-anchored
//! by its existing byte-level tests. See PROVENANCE.md.
//!
//! Modulation: GFSK, 4800 baud, NRZ (bit value → FSK tone), Gaussian-shaped
//! (BT ≈ 0.5), modulation index ≈ 1 (deviation ≈ ±2.4 kHz). Bytes are
//! transmitted LSB-first.

use crate::demod::BAUD;
use num_complex::Complex;
use std::f64::consts::TAU;

/// RS41 frequency deviation (modulation index ≈ 1 at 4800 bd).
const DEVIATION_HZ: f64 = 2_400.0;

/// Expand a byte stream to its transmitted bits (each byte LSB-first).
pub fn bytes_to_lsb_bits(bytes: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for &b in bytes {
        for i in 0..8 {
            bits.push((b >> i) & 1);
        }
    }
    bits
}

/// Gaussian-shaped GFSK modulation of NRZ bits into IQ.
///
/// `bits` are NRZ (1 → high tone, 0 → low tone). A short preamble of
/// alternating bits is prepended by the caller to let the demod's timing /
/// DC loops settle before the sync word.
pub fn modulate_iq(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / BAUD;
    const BT: f64 = 0.5;
    const SPAN: f64 = 2.0; // Gaussian pulse support in symbols, each side

    // NRZ levels: 1 → +1, 0 → -1.
    let levels: Vec<f64> = bits.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();

    let a = (2.0 * std::f64::consts::PI / (2.0f64.ln()).sqrt()) * BT;
    let erf = |x: f64| {
        // Abramowitz-Stegun 7.1.26
        let t = 1.0 / (1.0 + 0.3275911 * x.abs());
        let y = 1.0
            - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
                + 0.254829592)
                * t
                * (-x * x).exp();
        if x < 0.0 { -y } else { y }
    };
    // Integrated Gaussian phase pulse q(t), 0 → 1/2 over one symbol.
    let q = |t: f64| -> f64 {
        0.25 * (erf(a * (t + 0.5) / std::f64::consts::SQRT_2)
            - erf(a * (t - 0.5) / std::f64::consts::SQRT_2))
    };

    let nsamples = ((levels.len() as f64 + 2.0 * SPAN) * spb).ceil() as usize;
    let mut out = Vec::with_capacity(nsamples);
    let mut phase = 0.0f64;
    for n in 0..nsamples {
        let t_sym = n as f64 / spb - SPAN;
        let lo = ((t_sym - SPAN).floor().max(0.0)) as usize;
        let hi = ((t_sym + SPAN).ceil()).min(levels.len() as f64 - 1.0) as usize;
        let mut f_inst = 0.0;
        for (k, &lv) in levels.iter().enumerate().take(hi + 1).skip(lo) {
            f_inst += lv * q(t_sym - k as f64);
        }
        // q sums to 1/2 per symbol → ×2·DEVIATION for ±2400 Hz steady state.
        let freq = freq_offset_hz + 2.0 * DEVIATION_HZ * f_inst;
        phase += TAU * freq / sample_rate;
        out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
    }
    out
}

/// Build a full RS41 burst: an alternating preamble, then the on-air frame
/// bytes modulated as GFSK NRZ IQ at `sample_rate`.
pub fn burst_iq(
    on_air_frame: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let mut bits: Vec<u8> = (0..64).map(|i| (i % 2) as u8).collect(); // preamble
    bits.extend(bytes_to_lsb_bits(on_air_frame));
    bits.extend((0..32).map(|i| (i % 2) as u8)); // tail
    modulate_iq(&bits, sample_rate, freq_offset_hz, amplitude)
}
