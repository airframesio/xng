//! Aero P-channel modulator for loopback testing: frame bits → MSK
//! waveform (±fb/4 deviation, phase-continuous), bit 1 = +fb/4 —
//! the direct mapping observed in off-air captures.

use num_complex::Complex;
use std::f64::consts::TAU;

/// Modulate a hard bit stream at `bit_rate` into IQ at `sample_rate`.
pub fn modulate(
    bits: &[u8],
    bit_rate: f64,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / bit_rate;
    let dev = bit_rate / 4.0;
    let mut out = Vec::with_capacity((bits.len() as f64 * spb) as usize + 1);
    let mut phase = 0.0f64;
    let mut emitted = 0usize;
    for (i, &b) in bits.iter().enumerate() {
        let level = if b == 1 { 1.0 } else { -1.0 };
        let f = freq_offset_hz + level * dev;
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * f / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}
