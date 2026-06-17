//! ADS-L 2-FSK modulator for **self-generated** demod validation.
//!
//! Builds the on-air chip stream from a wire-byte frame exactly as the
//! SoftRF `adsl_proto_desc` describes — preamble `0x55`, the 8-byte sync
//! word, then the Manchester-encoded, FSK-inverted payload — and FM-modulates
//! it at ±50 kHz deviation. The companion [`crate::demod::FskDemod`] inverts
//! this transform.
//!
//! This is a **loopback aid for the IQ front-end only**: the demod test
//! modulates the *same* bytes the independent `decode_vectors` oracle pins,
//! so the decode core stays externally anchored while the modulate→demod
//! path is self-consistent (documented in PROVENANCE.md).

use crate::demod::{BAUD, DEVIATION_HZ, SYNC_CHIPS};
use num_complex::Complex;
use std::f64::consts::TAU;

/// Chip rate (Manchester doubles the data rate).
const CHIP_RATE: f64 = 2.0 * BAUD;

/// IEEE Manchester encode one data bit into two chips (MSB-first):
/// data `0` → `(1, 0)`, data `1` → `(0, 1)`.
fn manchester(bit: u8) -> (u8, u8) {
    if bit == 0 {
        (1, 0)
    } else {
        (0, 1)
    }
}

/// Build the full on-air **chip** stream for a wire-byte frame:
/// preamble + sync word + Manchester(payload) inverted.
///
/// `frame_bytes` is exactly what [`crate::Frame::parse`] accepts without the
/// optional length byte: Version + 20-byte payload + 3 CRC bytes.
pub fn chip_stream(frame_bytes: &[u8]) -> Vec<u8> {
    let mut chips: Vec<u8> = Vec::new();

    // Preamble: one byte of alternating chips (0x55 = 0,1,0,1,...).
    for i in 0..16 {
        chips.push((i % 2) as u8); // a couple of preamble bytes for AGC/timing
    }

    // Sync word: the 8 chip bytes verbatim, MSB-first.
    for &b in &SYNC_CHIPS {
        for i in (0..8).rev() {
            chips.push((b >> i) & 1);
        }
    }

    // Payload: Manchester-encode each data bit (MSB-first), then invert the
    // whole payload chip stream (RF_PAYLOAD_INVERTED).
    for &byte in frame_bytes {
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1;
            let (a, b) = manchester(bit);
            chips.push(a ^ 1); // inverted
            chips.push(b ^ 1);
        }
    }
    chips
}

/// FM-modulate a chip stream at ±[`DEVIATION_HZ`] deviation: chip `1` → high
/// tone, chip `0` → low tone (the discriminator recovers the sign).
pub fn modulate_iq(
    chips: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spc = sample_rate / CHIP_RATE;
    let mut out = Vec::with_capacity((chips.len() as f64 * spc) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut emitted: usize = 0;
    for (i, &chip) in chips.iter().enumerate() {
        let dev = if chip == 1 {
            DEVIATION_HZ
        } else {
            -DEVIATION_HZ
        };
        let freq = freq_offset_hz + dev;
        let end = (((i + 1) as f64) * spc).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Convenience: full burst IQ (preamble + sync + payload) for a wire-byte
/// frame, with leading/trailing silence so the demod settles.
pub fn burst_iq(
    frame_bytes: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let chips = chip_stream(frame_bytes);
    let mut iq = vec![Complex::new(0.0, 0.0); (sample_rate / CHIP_RATE) as usize * 8];
    iq.extend(modulate_iq(&chips, sample_rate, freq_offset_hz, amplitude));
    iq.extend(std::iter::repeat_n(
        Complex::new(0.0, 0.0),
        (sample_rate / CHIP_RATE) as usize * 8,
    ));
    iq
}
