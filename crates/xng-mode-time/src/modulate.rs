//! Time-signal modulators for self-generated demod validation.
//!
//! Synthesizes the two flagship stations' audio and AM-modulates it onto a
//! complex baseband carrier, so the [`crate::TimeChannelDecoder`] front end can
//! be exercised end to end without a recorded capture:
//!
//! - [`chu_audio`] / `chu_iq` — CHU AFSK: per-second 1000 Hz tick + MARK
//!   preamble + the 110 data bits as Bell-103 (2225/2025 Hz) 8N2 at 300 baud.
//! - [`wwv_audio`] / `wwv_iq` — WWV/WWVH: a 100 Hz subcarrier whose per-second
//!   pulse length codes each bit (170/470/770 ms, 30 ms suppressed lead-in,
//!   sec-0 hole), plus the seconds tick (1000 Hz WWV / 1200 Hz WWVH).
//!
//! VERIFICATION NOTE: this is a *self-generated* modulate→demod path. The
//! waveform parameters (Bell-103 tones, 300 baud, 8N2; 100 Hz subcarrier
//! 170/470/770 ms PWM) are the published broadcast facts (PROVENANCE.md), but
//! the modulator is not an external reference — it validates only that the
//! demod inverts this modulation. The DECODE cores (BCD/redundancy/framing)
//! stay anchored by their own table tests. Tests using this are `*_synth`.

use crate::chu;
use crate::wwv::{self, Symbol};
use num_complex::Complex;
use std::f64::consts::TAU;

/// AM-modulate a real audio buffer onto a complex baseband carrier: the audio
/// rides as the envelope `(1 + depth·audio)`, so [`crate::audio::am_envelope`]
/// recovers it. `freq_offset_hz` places the carrier off channel center (the
/// DDC / envelope DC tracker absorbs it). Audio is assumed normalized to
/// roughly [-1, 1].
pub fn am_modulate(
    audio: &[f32],
    sample_rate: f64,
    freq_offset_hz: f64,
    depth: f32,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let mut out = Vec::with_capacity(audio.len());
    let mut phase = 0.0f64;
    let dp = TAU * freq_offset_hz / sample_rate;
    for &a in audio {
        phase += dp;
        let env = amplitude * (1.0 + depth * a);
        out.push(Complex::new(env * phase.cos() as f32, env * phase.sin() as f32));
    }
    out
}

/// Continuous-phase tone generator: append `n` samples of `freq_hz` at
/// `amplitude` to `out`, advancing `phase` (radians) in place.
fn push_tone(out: &mut Vec<f32>, freq_hz: f64, sample_rate: f64, n: usize, amplitude: f32, phase: &mut f64) {
    let dp = TAU * freq_hz / sample_rate;
    for _ in 0..n {
        *phase += dp;
        out.push(amplitude * phase.sin() as f32);
    }
}

/// Append `n` samples of silence to `out`.
fn push_silence(out: &mut Vec<f32>, n: usize) {
    out.extend(std::iter::repeat_n(0.0f32, n));
}

// ---------------------------------------------------------------------------
// CHU AFSK audio.
// ---------------------------------------------------------------------------

/// 11-bit 8N2 on-air sequence for one byte (start=space, 8 data LSB-first, two
/// stop=mark). Matches [`chu::decode_8n2`].
fn chu_char_bits(byte: u8) -> Vec<u8> {
    let mut v = vec![0u8]; // start = space
    for i in 0..8 {
        v.push((byte >> i) & 1);
    }
    v.push(1);
    v.push(1);
    v
}

/// Build the AFSK audio for the 110-bit data field of one CHU second: each bit
/// is `chu::BAUD`-long, MARK (2225 Hz) for 1, SPACE (2025 Hz) for 0, continuous
/// phase. `packet_bytes` is the 10-byte packet (5 data + 5 redundancy).
fn chu_afsk_field(packet_bytes: &[u8], sample_rate: f64, amplitude: f32, phase: &mut f64) -> Vec<f32> {
    let spb = (sample_rate / chu::BAUD).round() as usize;
    let mut bits = Vec::with_capacity(chu::PACKET_BITS);
    for &b in packet_bytes {
        bits.extend(chu_char_bits(b));
    }
    let mut out = Vec::with_capacity(bits.len() * spb);
    for &bit in &bits {
        let f = if bit != 0 { chu::MARK_HZ } else { chu::SPACE_HZ };
        push_tone(&mut out, f, sample_rate, spb, amplitude, phase);
    }
    out
}

/// Synthesize one CHU second of audio for a 10-byte packet, following the
/// broadcast per-second timing: 0–10 ms = 1000 Hz tick, 10–133.3 ms = MARK
/// (2225 Hz idle) preamble, 133.3–500 ms = the 110 AFSK data bits, then silence
/// to the end of the second. Returns audio at `sample_rate` for exactly 1 s.
pub fn chu_second_audio(packet_bytes: &[u8], sample_rate: f64, amplitude: f32) -> Vec<f32> {
    let total = sample_rate.round() as usize; // 1 second
    let tick_n = (sample_rate * 0.010).round() as usize;
    // The AFSK field is 110 bits at 300 baud = 366.67 ms; it must start so it
    // ends at 500 ms. We compute the field first to know its exact length.
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(total);

    // 0–10 ms: 1000 Hz tick.
    push_tone(&mut out, 1000.0, sample_rate, tick_n, amplitude, &mut phase);

    // Data field (110 bits). Its length in samples:
    let field = chu_afsk_field(packet_bytes, sample_rate, amplitude, &mut phase.clone());
    let field_n = field.len();
    let end_500 = (sample_rate * 0.500).round() as usize;
    let preamble_n = end_500.saturating_sub(out.len() + field_n);

    // 10 ms .. (start of data): MARK preamble (idle mark = 2225 Hz).
    push_tone(&mut out, chu::MARK_HZ, sample_rate, preamble_n, amplitude, &mut phase);
    // Data bits (regenerate with the continued phase for continuity).
    let field = chu_afsk_field(packet_bytes, sample_rate, amplitude, &mut phase);
    out.extend(field);

    // Pad / trim to exactly one second.
    let n = out.len();
    if n < total {
        push_silence(&mut out, total - n);
    } else {
        out.truncate(total);
    }
    out
}

/// Full CHU audio for a sequence of seconds' packets (one 10-byte packet per
/// second), concatenated. `amplitude` is the audio tone amplitude before AM.
pub fn chu_audio(packets: &[[u8; chu::PACKET_BYTES]], sample_rate: f64, amplitude: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for p in packets {
        out.extend(chu_second_audio(p, sample_rate, amplitude));
    }
    out
}

/// Convenience: CHU AM IQ for a single second's packet.
pub fn chu_iq(
    packet_bytes: &[u8; chu::PACKET_BYTES],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let audio = chu_second_audio(packet_bytes, sample_rate, 1.0);
    am_modulate(&audio, sample_rate, freq_offset_hz, 0.8, amplitude)
}

// ---------------------------------------------------------------------------
// WWV / WWVH 100 Hz subcarrier audio.
// ---------------------------------------------------------------------------

/// Nominal pulse length (seconds) carried by a symbol, BEFORE the 30 ms
/// suppressed lead-in (the modulator emits `pulse - 30 ms` of tone after a
/// 30 ms gap, matching the broadcast / [`wwv::tone_length`] measurement).
fn symbol_pulse_s(sym: Symbol) -> f64 {
    match sym {
        Symbol::Zero => 0.200,
        Symbol::One => 0.500,
        Symbol::Marker => 0.800,
        Symbol::Hole => 0.0,
    }
}

/// Synthesize one second of WWV/WWVH audio for a symbol: a `tick_hz` seconds
/// tick (5 ms), then — after the 30 ms tone-suppressed lead-in — a 100 Hz
/// subcarrier burst of `pulse - 30 ms`, then silence to the end of the second.
/// A [`Symbol::Hole`] emits the tick only (the reference second).
pub fn wwv_second_audio(sym: Symbol, tick_hz: f64, sample_rate: f64, amplitude: f32) -> Vec<f32> {
    let total = sample_rate.round() as usize;
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(total);

    // Seconds tick (short burst at the top of the second).
    let tick_n = (sample_rate * 0.005).round() as usize;
    push_tone(&mut out, tick_hz, sample_rate, tick_n, amplitude, &mut phase);
    // Fill from end-of-tick to the 30 ms lead-in with silence.
    let leadin_n = (sample_rate * wwv::LEADIN_S).round() as usize;
    let after_tick = out.len();
    if after_tick < leadin_n {
        push_silence(&mut out, leadin_n - after_tick);
    }

    let pulse = symbol_pulse_s(sym);
    if pulse > 0.0 {
        // Tone length actually emitted = nominal − 30 ms lead-in.
        let tone_n = ((pulse - wwv::LEADIN_S) * sample_rate).round().max(0.0) as usize;
        let mut sp = 0.0f64;
        push_tone(&mut out, wwv::SUBCARRIER_HZ, sample_rate, tone_n, amplitude, &mut sp);
    }
    let n = out.len();
    if n < total {
        push_silence(&mut out, total - n);
    } else {
        out.truncate(total);
    }
    out
}

/// Full WWV/WWVH audio for a 60-symbol minute. `tick_hz` = 1000 (WWV) or 1200
/// (WWVH).
pub fn wwv_audio(symbols: &[Symbol], tick_hz: f64, sample_rate: f64, amplitude: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for &sym in symbols {
        out.extend(wwv_second_audio(sym, tick_hz, sample_rate, amplitude));
    }
    out
}

/// Convenience: WWV/WWVH AM IQ for a full minute of symbols.
pub fn wwv_iq(
    symbols: &[Symbol],
    tick_hz: f64,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let audio = wwv_audio(symbols, tick_hz, sample_rate, 1.0);
    am_modulate(&audio, sample_rate, freq_offset_hz, 0.8, amplitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chu_char_bits_is_8n2_lsb_first() {
        // 0x6D = 0b0110_1101 -> LSB-first data [1,0,1,1,0,1,1,0].
        let b = chu_char_bits(0x6D);
        assert_eq!(b[0], 0); // start
        assert_eq!(&b[1..9], &[1, 0, 1, 1, 0, 1, 1, 0]);
        assert_eq!(&b[9..11], &[1, 1]); // two stops
    }

    #[test]
    fn chu_second_is_one_second_long() {
        let sr = 12_000.0;
        let pkt = [0x61, 0x59, 0x12, 0x34, 0x56, 0x61, 0x59, 0x12, 0x34, 0x56];
        let a = chu_second_audio(&pkt, sr, 1.0);
        assert_eq!(a.len(), sr as usize);
    }

    #[test]
    fn wwv_pulse_lengths_match_symbols() {
        let sr = 8_000.0;
        // A binary-1 second should have ~470 ms of 100 Hz tone present.
        let a = wwv_second_audio(Symbol::One, 1000.0, sr, 1.0);
        let len = wwv::tone_length(&a, sr);
        assert!((len - 0.470).abs() < 0.05, "got {len}");
        let a0 = wwv_second_audio(Symbol::Zero, 1000.0, sr, 1.0);
        let len0 = wwv::tone_length(&a0, sr);
        assert!((len0 - 0.170).abs() < 0.05, "got {len0}");
        let am = wwv_second_audio(Symbol::Marker, 1000.0, sr, 1.0);
        let lenm = wwv::tone_length(&am, sr);
        assert!((lenm - 0.770).abs() < 0.06, "got {lenm}");
        let ah = wwv_second_audio(Symbol::Hole, 1000.0, sr, 1.0);
        assert!(wwv::tone_length(&ah, sr) < 0.05);
    }
}
