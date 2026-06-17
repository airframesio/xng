//! COSPAS-SARSAT First-Generation Beacon modulator — **self-generated** test
//! signal source for the demod loopback.
//!
//! This produces the C/S T.001 on-air waveform for a known beacon frame so the
//! [`crate::SarsatChannelDecoder`] demod front-end can be exercised end-to-end.
//! It is NOT an oracle: the decode *core* (`decode_hex`) stays anchored to the
//! `amsa-code/fgb-decoder` compliance vectors (see `tests/oracle.rs` /
//! PROVENANCE.md); this modulator only validates the modulate→demod path is
//! self-consistent.
//!
//! Modulation (C/S T.001 §2): biphase-L (Manchester) phase modulation of the
//! carrier at ±1.1 rad, data rate 400 bps. A logic `1` is the half-bit pair
//! (+1.1, −1.1) rad; a logic `0` is (−1.1, +1.1) rad — i.e. biphase-L with the
//! phase deviation as the "level". The transmitted frame is: 160 ms unmodulated
//! carrier, then 15 bit-sync `1`s, then the 9-bit frame sync, then the
//! 112-/144-bit message.

use num_complex::Complex;
use std::f64::consts::TAU;

/// Data rate (bits per second).
pub const BAUD: f64 = 400.0;
/// Phase deviation of the biphase-L modulation, radians (C/S T.001: 1.1 ± 0.1).
pub const PHASE_DEV: f64 = 1.1;
/// Normal-mode 9-bit frame sync (C/S T.001 distress message).
pub const FRAME_SYNC: [u8; 9] = [0, 0, 0, 1, 0, 1, 1, 1, 1];
/// Number of bit-sync `1`s preceding the frame sync.
pub const BIT_SYNC_ONES: usize = 15;

/// Convert a beacon hex string (15 or 30 hex) into the message data bits as
/// transmitted on air (MSB-first within each hex nibble).
///
/// * 30 hex (long): the 120 transmitted bits are message bits 25..=144 — the
///   format flag onward (frame sync already excluded).
/// * 15 hex (short): the 60 bits are message bits 26..=85 (the 15-hex beacon
///   ID); the format flag (bit 25) is prepended as `0` (short) so the on-air
///   stream after frame sync is well-formed.
pub fn message_bits_from_hex(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    let mut bits = Vec::with_capacity(hex.len() * 4 + 1);
    if hex.len() == 15 {
        // Format flag (bit 25): short frame.
        bits.push(0);
    }
    for c in hex.chars() {
        let v = c.to_digit(16).expect("hex digit") as u8;
        for shift in (0..4).rev() {
            bits.push((v >> shift) & 1);
        }
    }
    bits
}

/// Build the full on-air bit stream: 15 bit-sync `1`s, the 9-bit frame sync,
/// then the message data bits.
pub fn framed_bits(message_bits: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(BIT_SYNC_ONES + FRAME_SYNC.len() + message_bits.len());
    bits.extend(std::iter::repeat_n(1u8, BIT_SYNC_ONES));
    bits.extend_from_slice(&FRAME_SYNC);
    bits.extend_from_slice(message_bits);
    bits
}

/// Modulate framed bits into baseband IQ at `sample_rate`, with carrier offset
/// `freq_offset_hz` and starting carrier phase `phase0`.
///
/// `carrier_lead_bits` leading bit-periods of unmodulated carrier are prepended
/// (the C/S T.001 160 ms carrier; expressed in 400 bps bit-periods so the test
/// can keep it short). Biphase-L: each data bit is two half-bit phase symbols.
pub fn modulate_iq(
    framed: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    phase0: f64,
    amplitude: f32,
    carrier_lead_bits: usize,
) -> Vec<Complex<f32>> {
    let samples_per_bit = sample_rate / BAUD;
    let half = samples_per_bit / 2.0;

    // Phase-symbol stream: unmodulated carrier (phase deviation 0), then two
    // half-bit symbols per data bit.
    // biphase-L: 1 -> (+dev, -dev); 0 -> (-dev, +dev).
    let mut symbols: Vec<(f64, f64)> = Vec::new(); // (start_sample, deviation)
    let mut t = 0.0f64;
    for _ in 0..carrier_lead_bits {
        symbols.push((t, 0.0));
        t += samples_per_bit;
    }
    for &b in framed {
        let (a, c) = if b == 1 {
            (PHASE_DEV, -PHASE_DEV)
        } else {
            (-PHASE_DEV, PHASE_DEV)
        };
        symbols.push((t, a));
        symbols.push((t + half, c));
        t += samples_per_bit;
    }

    let total = t.round() as usize;
    let mut out = Vec::with_capacity(total);
    let mut carrier_phase = phase0;
    let mut sym_idx = 0usize;
    for n in 0..total {
        // Advance to the symbol covering sample n.
        while sym_idx + 1 < symbols.len() && (n as f64) >= symbols[sym_idx + 1].0 {
            sym_idx += 1;
        }
        let dev = symbols[sym_idx].1;
        carrier_phase += TAU * freq_offset_hz / sample_rate;
        let phase = carrier_phase + dev;
        out.push(Complex::new(phase.cos() as f32, phase.sin() as f32) * amplitude);
    }
    out
}

/// Convenience: full burst IQ for a beacon hex string.
pub fn burst_iq(
    hex: &str,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let framed = framed_bits(&message_bits_from_hex(hex));
    // ~50 bit-periods of unmodulated carrier (125 ms) — enough to let the
    // carrier-recovery one-pole settle before the bit-sync run.
    modulate_iq(&framed, sample_rate, freq_offset_hz, 0.0, amplitude, 50)
}
