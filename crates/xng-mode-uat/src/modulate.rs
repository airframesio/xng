//! UAT 2-ary CPFSK modulator for self-generated demod validation.
//!
//! This builds the on-air UAT bit stream from a known with-parity frame —
//! preamble dead-time, the 36-bit sync word, then the RS codeword octets
//! (MSB-first) — and FM-modulates it as continuous-phase binary FSK at the
//! UAT deviation. It exists ONLY to drive the demodulator end-to-end in
//! tests; the DECODE core stays oracle-anchored by its dump978 vectors. See
//! PROVENANCE.md ("self-generated modulate→demod path").

use crate::demod::{BIT_RATE, SYNC_DOWNLINK, SYNC_LEN, SYNC_UPLINK};
use num_complex::Complex;
use std::f64::consts::TAU;

/// UAT modulation index h ≈ 0.6 ⇒ tone deviation = h·R/2.
const DEVIATION_HZ: f64 = 0.6 * BIT_RATE / 2.0;

/// Emit the 36-bit sync word as MSB-first bits.
fn sync_bits(downlink: bool) -> Vec<u8> {
    let word = if downlink { SYNC_DOWNLINK } else { SYNC_UPLINK };
    (0..SYNC_LEN).rev().map(|i| ((word >> i) & 1) as u8).collect()
}

/// Build the transmitted bit stream for a with-parity frame: a short quiet
/// lead-in (so the demod's DC tracker and timing settle), the sync word,
/// then the frame octets MSB-first.
pub fn frame_bits(frame: &[u8], downlink: bool) -> Vec<u8> {
    let mut bits: Vec<u8> = Vec::new();
    // Lead-in: alternating tones give the timing loop zero crossings.
    bits.extend((0..48).map(|i| (i % 2) as u8));
    bits.extend(sync_bits(downlink));
    for &byte in frame {
        for i in (0..8).rev() {
            bits.push((byte >> i) & 1);
        }
    }
    bits
}

/// Continuous-phase FSK-modulate a bit stream: `1` is the upper tone, `0`
/// the lower, at `freq_offset_hz` ± [`DEVIATION_HZ`].
pub fn modulate_iq(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / BIT_RATE;
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

/// Full burst IQ for a with-parity frame at the given capture rate.
pub fn burst_iq(
    frame: &[u8],
    downlink: bool,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    modulate_iq(&frame_bits(frame, downlink), sample_rate, freq_offset_hz, amplitude)
}
