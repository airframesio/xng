//! EOT/HOT Manchester-FSK modulator for self-generated demod validation.
//!
//! Turns a logical bit stream (frame sync + 45 data + 18 BCH = the 74-bit
//! packet, with a bit-sync preamble) into 1200-baud Manchester-encoded binary
//! FSK IQ, so [`crate::EotChannelDecoder`] can be exercised end to end without
//! a recorded capture.
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate->demod path. The
//! waveform parameters (1200 Bd, Manchester line coding, narrowband 2-FSK) are
//! the published on-air EOT facts (1200-baud FSK, EOT->HOT 457.9375 MHz,
//! HOT->EOT 452.9375 MHz; see SIGIDWIKI "End of Train Device (EOTD)" and the
//! cited PyEOT/EOTDecode decoders, both of which Manchester-decode a 1200-baud
//! stream). The modulator itself is NOT an external reference: it validates
//! only that the demod inverts this modulation. The DECODE/framing core stays
//! anchored to the documented field map by `frame.rs` / `bch.rs` tests.
//! Tests using this path are named `*_synth_iq` and reported as synthetic.

use num_complex::Complex;
use std::f64::consts::TAU;

/// Symbol/baud rate of the line code (one Manchester half-bit pair = one data
/// bit). The Manchester chip rate is therefore 2 * BAUD.
pub const BAUD: f64 = 1200.0;
/// FSK shift from channel center to each tone, Hz. EOT is narrowband (~8 kHz
/// channel); a ±1800 Hz shift (a conventional ~1.5 modulation index near the
/// 1200-baud line rate) sits comfortably inside it. This is a modulator
/// choice for the synthetic test, not a claimed spec value.
pub const SHIFT_HZ: f64 = 1_800.0;

/// Manchester-encode a logical bit stream (IEEE 802.3 convention used by the
/// cited decoders' demod: logical 1 -> high-then-low chip pair `[1,0]`,
/// logical 0 -> low-then-high `[0,1]`).
pub fn manchester_encode(bits: &[u8]) -> Vec<u8> {
    let mut chips = Vec::with_capacity(bits.len() * 2);
    for &b in bits {
        if b != 0 {
            chips.push(1);
            chips.push(0);
        } else {
            chips.push(0);
            chips.push(1);
        }
    }
    chips
}

/// Modulate a *chip* stream (already Manchester-encoded) as 2-FSK IQ at the
/// chip rate `2 * BAUD`. `chip = 1` (mark) -> `freq_offset_hz + SHIFT_HZ`,
/// `chip = 0` (space) -> `freq_offset_hz - SHIFT_HZ`. Continuous phase.
pub fn modulate_chips(
    chips: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let chip_rate = 2.0 * BAUD;
    let spc = sample_rate / chip_rate; // samples per chip
    let mut out = Vec::with_capacity((chips.len() as f64 * spc) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut emitted: usize = 0;
    for (i, &chip) in chips.iter().enumerate() {
        let freq = freq_offset_hz + if chip != 0 { SHIFT_HZ } else { -SHIFT_HZ };
        let end = (((i + 1) as f64) * spc).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
            emitted += 1;
        }
    }
    out
}

/// Full burst IQ for one logical packet: a bit-sync (alternating) preamble,
/// then the packet bits, Manchester-encoded and FSK-modulated, with a short
/// trailing flush so the timing loop can settle.
///
/// `packet_bits` should already contain the 11-bit frame sync + 45 data + 18
/// check (i.e. the 74-bit packet); the alternating preamble carries the clock.
pub fn burst_iq(
    packet_bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    // ~24 bits of alternating 1/0 bit-sync preamble (the on-air clock run),
    // arranged to end in the `...101010` tail the sync hunt keys on (last bit
    // 0, immediately before the frame sync's leading 1).
    let mut bits: Vec<u8> = (0..24).map(|i| ((i + 1) % 2) as u8).collect();
    bits.extend_from_slice(packet_bits);

    let mut chips = manchester_encode(&bits);
    // Trailing idle chips so the last data bit's window closes cleanly.
    chips.extend(std::iter::repeat_n(0u8, 8));

    modulate_chips(&chips, sample_rate, freq_offset_hz, amplitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manchester_encode_is_ieee_convention() {
        assert_eq!(manchester_encode(&[1]), vec![1, 0]);
        assert_eq!(manchester_encode(&[0]), vec![0, 1]);
        assert_eq!(manchester_encode(&[1, 0]), vec![1, 0, 0, 1]);
    }

    #[test]
    fn modulate_emits_expected_sample_count() {
        // 4 chips at chip rate 2400 with sample_rate 24000 => 10 samples/chip.
        let iq = modulate_chips(&[1, 0, 1, 0], 24_000.0, 0.0, 1.0);
        assert_eq!(iq.len(), 4 * 10);
    }
}
