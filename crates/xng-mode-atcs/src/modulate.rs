//! ATCS RF modulator for the synthetic demod loopback test.
//!
//! NOTE — self-generated: this modulator exists only to exercise the
//! IQ → bits front end ([`crate::demod::FskDemod`]) end-to-end. The
//! modulate → demod path is self-consistency, not an external oracle: no
//! public ATCS IQ vector exists. The DECODE core (HDLC deframe + Spec-200
//! header) stays oracle-anchored by its own tests (the CRC-16/X-25
//! catalogue value and the AAR header layout / sigidwiki sample); see
//! PROVENANCE.md. The synthetic test asserts only that a KNOWN spec-derived
//! frame survives the modulate → FSK-demod → deframe → decode round trip.
//!
//! Chain: [`crate::frame::hdlc_bits`] (opening flag, bit-stuffed payload +
//! true FCS, closing flag) → NRZI encode (a `0` flips the tone level) →
//! 2-FSK at ±[`DEVIATION_HZ`].

use num_complex::Complex;
use std::f64::consts::TAU;

const BAUD: f64 = 4_800.0;
/// FSK deviation each side of channel center (mark/space split = ±this).
pub const DEVIATION_HZ: f64 = 1_800.0;

/// A bit-sync preamble of alternating bits (Spec-200 sends bit
/// synchronization before frame sync); helps the discriminator DC tracker
/// and timing loop settle before the flag arrives.
pub fn preamble_bits(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 2) as u8).collect()
}

/// NRZI-encode (a `0` toggles the tone level, a `1` holds it) and
/// FM-modulate as 2-FSK at ±[`DEVIATION_HZ`] around `freq_offset_hz`.
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
        // NRZI: a 0 transitions the level, a 1 keeps it.
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

/// Full burst IQ for an HDLC payload: bit-sync preamble + framed bit stream,
/// NRZI-encoded and 2-FSK modulated.
pub fn burst_iq(
    payload: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let mut bits = preamble_bits(40); // Spec-200 40-bit bit-sync
    bits.extend(crate::frame::hdlc_bits(payload));
    bits.extend(preamble_bits(8)); // trailing idle
    modulate_iq(&bits, sample_rate, freq_offset_hz, amplitude)
}
