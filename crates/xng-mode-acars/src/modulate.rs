//! ARINC 618 modulator: build frame octets and synthesize MSK-on-AM IQ.
//!
//! Exists for loopback testing of the decode chain and for generating
//! synthetic captures (`xng` test tooling). Follows the same spec text as
//! the decoder (see PROVENANCE.md) but shares no state with it, so a
//! convention error on either side shows up as a loopback failure.

use num_complex::Complex;
use std::f64::consts::TAU;
use xng_dsp::checksum::acars_crc;

const NAK: u8 = 0x15;
const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const ETX: u8 = 0x03;
const SYN: u8 = 0x16;
const ETB: u8 = 0x17;
const DEL: u8 = 0x7F;

/// Fields for one ACARS block.
pub struct FrameSpec<'a> {
    pub mode: char,
    /// Registration without dot padding (padded to 7 here).
    pub tail: &'a str,
    /// `None` → NAK (no acknowledgement).
    pub ack: Option<char>,
    /// Exactly 2 characters.
    pub label: &'a str,
    pub block_id: char,
    /// Downlink message sequence number (4 chars).
    pub msg_num: Option<&'a str>,
    /// Downlink flight id (6 chars).
    pub flight: Option<&'a str>,
    pub text: &'a str,
    /// Terminate with ETB (more blocks follow) instead of ETX.
    pub etb: bool,
}

/// Set bit 8 for odd parity.
fn with_parity(c: u8) -> u8 {
    if c.count_ones() % 2 == 0 {
        c | 0x80
    } else {
        c
    }
}

/// Octets from Mode through the suffix (odd parity applied) plus the two
/// BCS bytes (low byte first, no parity). SOH/DEL are not included.
pub fn frame_octets(spec: &FrameSpec) -> Vec<u8> {
    let mut chars: Vec<u8> = Vec::new();
    chars.push(spec.mode as u8);
    let addr = format!("{:.>7}", spec.tail);
    assert_eq!(addr.len(), 7, "tail too long: {}", spec.tail);
    chars.extend(addr.bytes());
    chars.push(spec.ack.map(|c| c as u8).unwrap_or(NAK));
    let label: Vec<u8> = spec.label.bytes().collect();
    assert_eq!(label.len(), 2, "label must be 2 chars");
    chars.extend(&label);
    chars.push(spec.block_id as u8);

    let has_text =
        !spec.text.is_empty() || spec.msg_num.is_some() || spec.flight.is_some();
    if has_text {
        chars.push(STX);
        if let Some(m) = spec.msg_num {
            assert_eq!(m.len(), 4, "msg_num must be 4 chars");
            chars.extend(m.bytes());
        }
        if let Some(f) = spec.flight {
            assert_eq!(f.len(), 6, "flight must be 6 chars");
            chars.extend(f.bytes());
        }
        chars.extend(spec.text.bytes());
    }
    chars.push(if spec.etb { ETB } else { ETX });

    let mut octets: Vec<u8> = chars.into_iter().map(with_parity).collect();
    let crc = acars_crc(&octets);
    octets.push((crc & 0xFF) as u8);
    octets.push((crc >> 8) as u8);
    octets
}

/// Complete burst bit stream: pre-key (all ones), bit sync `+ *`, char sync
/// SYN SYN, SOH, frame octets, DEL — each octet LSB-first.
pub fn burst_bits(spec: &FrameSpec) -> Vec<u8> {
    let mut bits = vec![1u8; 128]; // pre-key, parity waived
    let mut octets: Vec<u8> = vec![
        with_parity(b'+'),
        with_parity(b'*'),
        with_parity(SYN),
        with_parity(SYN),
        with_parity(SOH),
    ];
    octets.extend(frame_octets(spec));
    octets.push(DEL); // BCS suffix, no parity
    for o in octets {
        bits.extend((0..8).map(|i| (o >> i) & 1));
    }
    bits.extend([1u8; 8]); // let the last bit clock out
    bits
}

/// Differential MSK audio: 1200 Hz = bit change, 2400 Hz = no change;
/// phase-continuous, zero amplitude at bit transitions. Works at any
/// sample rate ≥ ~2× the 2400 Hz tone.
pub fn modulate_audio(bits: &[u8], sample_rate: f64) -> Vec<f32> {
    let spb = sample_rate / 2400.0;
    let mut audio = Vec::with_capacity((bits.len() as f64 * spb) as usize + 1);
    let mut phase: f64 = 0.0;
    let mut prev = 1u8; // pre-key state
    let mut emitted: usize = 0;
    for (i, &bit) in bits.iter().enumerate() {
        let freq = if bit != prev { 1200.0 } else { 2400.0 };
        prev = bit;
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            phase += TAU * freq / sample_rate;
            audio.push(phase.sin() as f32);
            emitted += 1;
        }
    }
    audio
}

/// AM-modulate audio onto a complex carrier at `freq_offset_hz` from the
/// capture center: `amplitude * (1 + mod_index * audio)`.
pub fn modulate_iq(
    audio: &[f32],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
    mod_index: f32,
) -> Vec<Complex<f32>> {
    audio
        .iter()
        .enumerate()
        .map(|(n, &a)| {
            let ph = TAU * freq_offset_hz * n as f64 / sample_rate;
            let env = amplitude * (1.0 + mod_index * a);
            Complex::new(ph.cos() as f32, ph.sin() as f32) * env
        })
        .collect()
}

/// Convenience: full burst as IQ at `sample_rate`.
pub fn burst_iq(
    spec: &FrameSpec,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let bits = burst_bits(spec);
    let audio = modulate_audio(&bits, sample_rate);
    modulate_iq(&audio, sample_rate, freq_offset_hz, amplitude, 0.85)
}
