//! DSC MF/HF modulator: build the CCIR-493 bit stream and synthesize 100 Bd
//! binary FSK IQ.
//!
//! Exists purely for SYNTHETIC validation of the [`crate::demod`] front end and
//! for generating test captures. It follows the same M.493 conventions as the
//! decoder but shares no state with it, so a convention error on either side
//! shows up as a loopback failure. The decode core itself stays anchored to its
//! external oracle vectors (see PROVENANCE.md) — this modulator does NOT verify
//! the decode, only the IQ→bits demod path.

use crate::symbol::{zero_count, SYMBOL_BITS};
use num_complex::Complex;
use std::f64::consts::TAU;

/// DX phasing character (M.493): sent in the leading DX slots.
pub const DX_PHASING: i32 = 125;

/// Encodes one CCIR-493 symbol value into its 10 transmitted bits: 7 info bits
/// (B1..B7, LSB first) followed by the 3-bit zero-count check (MSB first). This
/// is the exact inverse of [`crate::symbol::decode_symbol`].
pub fn symbol_to_bits(value: i32, out: &mut Vec<u8>) {
    let v = value as u8;
    for j in 0..7 {
        out.push((v >> j) & 1);
    }
    let check = zero_count(v);
    out.push((check >> 2) & 1);
    out.push((check >> 1) & 1);
    out.push(check & 1);
}

/// Builds the aligned DX/RX interleaved bit stream that
/// [`crate::decode_from_bits`] expects to recover `data_symbols`.
///
/// Geometry mirrors `deinterleave_dx_rx(chars, 6, 2)`: characters alternate
/// DX, RX, DX, RX, …; the first 6 DX characters are phasing (`125`); the data
/// symbols follow as DX characters. The RX stream is filled with valid phasing
/// characters — with the DX data intact the de-interleaver never falls back to
/// it, so any valid symbol there is fine.
pub fn frame_bits(data_symbols: &[i32]) -> Vec<u8> {
    // DX stream: 6 phasing + the data symbols.
    let mut dx: Vec<i32> = vec![DX_PHASING; 6];
    dx.extend_from_slice(data_symbols);
    // RX stream: same length, all valid phasing characters.
    let rx: Vec<i32> = vec![DX_PHASING; dx.len()];

    let mut bits = Vec::with_capacity(dx.len() * 2 * SYMBOL_BITS);
    for k in 0..dx.len() {
        symbol_to_bits(dx[k], &mut bits);
        symbol_to_bits(rx[k], &mut bits);
    }
    bits
}

/// 100 Bd binary FSK IQ at `sample_rate`, centered at `freq_offset_hz` from the
/// capture center, with ±`shift_hz` deviation: bit 1 (Y, upper tone) →
/// `freq_offset + shift`, bit 0 (B, lower tone) → `freq_offset − shift`.
/// Phase-continuous (the integral of the instantaneous frequency).
pub fn modulate_iq(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    shift_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / 100.0;
    let mut iq = Vec::with_capacity((bits.len() as f64 * spb) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut emitted: usize = 0;
    for (i, &bit) in bits.iter().enumerate() {
        let f = if bit != 0 {
            freq_offset_hz + shift_hz
        } else {
            freq_offset_hz - shift_hz
        };
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * f / sample_rate;
            iq.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    iq
}

/// Convenience: full call as IQ at `sample_rate`, ±85 Hz shift, with a leading
/// idle so the discriminator's DC tracker and timing loop settle before data.
pub fn call_iq(
    data_symbols: &[i32],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    // Lead-in: extra phasing reversals (alternating elements) give the timing
    // loop transitions to lock onto, exactly as the on-air dot pattern does.
    let mut bits: Vec<u8> = Vec::new();
    for _ in 0..40 {
        bits.push(1);
        bits.push(0);
    }
    bits.extend(frame_bits(data_symbols));
    modulate_iq(&bits, sample_rate, freq_offset_hz, 85.0, amplitude)
}
