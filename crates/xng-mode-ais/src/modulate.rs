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

/// Convenience: full burst IQ for a message bit string.
pub fn burst_iq(
    message_bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    modulate_iq(&hdlc_bits(&wire_bytes_from_message_bits(message_bits)), sample_rate, freq_offset_hz, amplitude)
}
