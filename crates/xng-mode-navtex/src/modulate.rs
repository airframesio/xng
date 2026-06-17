//! NAVTEX / SITOR-B modulator for self-generated demod validation.
//!
//! Turns a CCIR 476 symbol stream (the same interleaved DX/RX layout the
//! decode core consumes) into 100-baud ±85 Hz binary FSK IQ, so the
//! [`crate::NavtexChannelDecoder`] front end can be exercised end-to-end
//! without a recorded capture.
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate→demod path. The
//! waveform parameters (100 Bd, ±85 Hz shift) are the published on-air NAVTEX
//! spec, and the symbol codes come from the oracle CCIR 476 tables, but the
//! modulator itself is not an external reference. It validates only that the
//! demod inverts this modulation; the DECODE core remains oracle-anchored by
//! its own table/FEC/framing tests. Tests using it are named `*_synth_iq`.

use num_complex::Complex;
use std::f64::consts::TAU;

/// FSK shift from center to each tone, Hz (NAVTEX spec: ±85 Hz, 170 Hz total).
pub const SHIFT_HZ: f64 = 85.0;
/// Symbol/baud rate.
pub const BAUD: f64 = 100.0;

/// Expand an interleaved DX/RX CCIR 476 symbol stream into the LSB-first bit
/// sequence carried on air: each 7-bit code is sent LSB-first (matching
/// [`crate::ccir476::pack_bits`]).
pub fn symbols_to_bits(symbols: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(symbols.len() * 7);
    for &code in symbols {
        for i in 0..7 {
            bits.push((code >> i) & 1);
        }
    }
    bits
}

/// Modulate a bit stream as 100-baud ±85 Hz FSK IQ.
///
/// `bit = 1` (mark) → `freq_offset_hz + SHIFT_HZ`; `bit = 0` (space) →
/// `freq_offset_hz - SHIFT_HZ`. Continuous phase across bits. `freq_offset_hz`
/// places the carrier off the channel center (the DDC / discriminator DC
/// tracker must absorb it).
pub fn modulate_iq(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / BAUD;
    let mut out = Vec::with_capacity((bits.len() as f64 * spb) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut emitted: usize = 0;
    for (i, &bit) in bits.iter().enumerate() {
        let freq = freq_offset_hz + if bit != 0 { SHIFT_HZ } else { -SHIFT_HZ };
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Convenience: full burst IQ from an interleaved symbol stream, with a few
/// leading/trailing idle bits so the discriminator and timing loop can
/// settle before and after the data.
pub fn burst_iq(
    symbols: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let mut bits = vec![1u8; 14]; // ~2 symbols of mark-idle preamble
    bits.extend(symbols_to_bits(symbols));
    bits.extend(std::iter::repeat_n(0u8, 14)); // trailing flush
    modulate_iq(&bits, sample_rate, freq_offset_hz, amplitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_to_bits_is_lsb_first() {
        // 0x47 = 0b1000111 → LSB-first [1,1,1,0,0,0,1].
        assert_eq!(symbols_to_bits(&[0x47]), vec![1, 1, 1, 0, 0, 0, 1]);
    }

    #[test]
    fn modulate_emits_expected_sample_count() {
        let iq = modulate_iq(&[1, 0, 1, 0], 4_800.0, 0.0, 1.0);
        // 4 bits at 48 samples/bit.
        assert_eq!(iq.len(), 4 * 48);
    }
}
