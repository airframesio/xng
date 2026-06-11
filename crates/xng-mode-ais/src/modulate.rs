//! AIS modulator for loopback testing: message bits → wire bytes + FCS →
//! stuffed HDLC bit stream with training/flags → NRZI → FM (MSK-shaped;
//! real GMSK is Gaussian-smoothed but the discriminator decoder handles
//! both).

use num_complex::Complex;
use std::f64::consts::TAU;
use xng_dsp::checksum::hdlc_fcs;

const BAUD: f64 = 9_600.0;
const DEVIATION_HZ: f64 = 2_400.0;

/// Pack a message bit string (MSB-first field order) into wire octets
/// (arrival-LSB-first) and append the FCS.
pub fn wire_bytes_from_message_bits(message_bits: &[u8]) -> Vec<u8> {
    assert_eq!(message_bits.len() % 8, 0, "AIS messages are octet-aligned");
    let mut bytes: Vec<u8> = message_bits
        .chunks_exact(8)
        .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << (7 - i))))
        .collect();
    let fcs = hdlc_fcs(&bytes);
    bytes.extend_from_slice(&fcs.to_le_bytes());
    bytes
}

/// Build the transmitted bit stream: training sequence, opening flag,
/// bit-stuffed payload, closing flag, tail.
pub fn hdlc_bits(wire_bytes: &[u8]) -> Vec<u8> {
    let mut bits: Vec<u8> = (0..24).map(|i| (i % 2) as u8).collect(); // 0101… training
    let flag = [0, 1, 1, 1, 1, 1, 1, 0];
    bits.extend(flag);
    let mut ones = 0;
    for &b in wire_bytes {
        for i in 0..8 {
            let bit = (b >> i) & 1;
            bits.push(bit);
            if bit == 1 {
                ones += 1;
                if ones == 5 {
                    bits.push(0); // stuff
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    bits.extend(flag);
    bits.extend([0, 1, 0, 1, 0, 1, 0, 1]); // tail/turnaround
    bits
}

/// NRZI-encode (0 = level change) and FM-modulate at ±2400 Hz deviation.
pub fn modulate_iq(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / BAUD;
    let mut out = Vec::with_capacity((bits.len() as f64 * spb) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut level: f64 = 1.0;
    let mut emitted: usize = 0;
    for (i, &bit) in bits.iter().enumerate() {
        if bit == 0 {
            level = -level;
        }
        let freq = freq_offset_hz + level * DEVIATION_HZ;
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// GMSK modulator (ITU-R M.1371: BT = 0.4, h = 0.5): the NRZI level
/// stream drives a Gaussian frequency pulse (±2T support) integrated
/// into a continuous phase. This is the realistic waveform — the
/// rectangular-frequency `modulate_iq` is loopback-blind to any
/// receive processing that depends on the true pulse shape.
pub fn modulate_iq_gmsk(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / BAUD;
    const BT: f64 = 0.4;
    const SPAN: f64 = 2.0; // pulse support in bits, each side

    // Gaussian frequency pulse g(t), normalized so each bit advances
    // the phase by ±π/2: g = (Q(a(t-T/2)) - Q(a(t+T/2)))-style smooth
    // pulse; implemented as the difference of error functions.
    let a = (2.0 * std::f64::consts::PI / (2.0f64.ln()).sqrt()) * BT;
    let erf = |x: f64| {
        // Abramowitz-Stegun 7.1.26
        let t = 1.0 / (1.0 + 0.3275911 * x.abs());
        let y = 1.0
            - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736)
                * t
                + 0.254829592)
                * t
                * (-x * x).exp();
        if x < 0.0 { -y } else { y }
    };
    // Integrated phase pulse q(t): 0 → 1/2 over the pulse, in bit units.
    let q = |t: f64| -> f64 {
        0.25 * (erf(a * (t + 0.5) / std::f64::consts::SQRT_2)
            - erf(a * (t - 0.5) / std::f64::consts::SQRT_2))
    };

    // NRZI levels per bit.
    let mut levels = Vec::with_capacity(bits.len());
    let mut level = 1.0f64;
    for &b in bits {
        if b == 0 {
            level = -level;
        }
        levels.push(level);
    }

    let nsamples = ((bits.len() as f64 + 2.0 * SPAN) * spb).ceil() as usize;
    let mut out = Vec::with_capacity(nsamples);
    let mut phase = 0.0f64;
    for n in 0..nsamples {
        let t_bit = n as f64 / spb - SPAN;
        // Instantaneous frequency = h/2 · Σ level_k · g(t-k); integrate
        // numerically with the per-sample sum of pulse contributions.
        let lo = ((t_bit - SPAN).floor().max(0.0)) as usize;
        let hi = ((t_bit + SPAN).ceil()).min(levels.len() as f64 - 1.0) as usize;
        let mut f_inst = 0.0;
        for (k, &lv) in levels.iter().enumerate().take(hi + 1).skip(lo) {
            f_inst += lv * q(t_bit - k as f64);
        }
        // q sums to 1/2 per bit → multiply by 2·DEVIATION for ±2400 Hz
        // steady-state on long runs.
        let freq = freq_offset_hz + 2.0 * DEVIATION_HZ * f_inst;
        phase += TAU * freq / sample_rate;
        out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
    }
    out
}

/// Full burst IQ with GMSK shaping.
pub fn burst_iq_gmsk(
    message_bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    modulate_iq_gmsk(
        &hdlc_bits(&wire_bytes_from_message_bits(message_bits)),
        sample_rate,
        freq_offset_hz,
        amplitude,
    )
}

/// Convenience: full burst IQ for a message bit string.
pub fn burst_iq(
    message_bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    modulate_iq(&hdlc_bits(&wire_bytes_from_message_bits(message_bits)), sample_rate, freq_offset_hz, amplitude)
}
