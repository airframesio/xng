//! AERO-8.1 — `to_message` must tag each event with the physical channel it
//! came from: L-band P-channel events as `Mode::AeroL`, C-band feeder R/T
//! burst events as `Mode::AeroC`. Before the fix `to_message` hard-coded
//! `Mode::AeroL`, so every C-band burst mislabelled as `aero-l`.
//!
//! Oracle: the two modes are distinct physical channels in JAERO
//! (`AeroL::ChannelType {PChannel, RChannel, TChannel}` on L-band vs the
//! C-band feeder bursts handled by the burst demodulators); xng exposes them
//! as separate `Mode` variants (`mode.rs` `aero-l` / `aero-c`). This test
//! drives a real loopback through each decoder and asserts the message mode
//! string, so it cannot pass if `to_message` reverts to a constant mode.

use num_complex::Complex;
use xng_dsp::scramble::Lfsr15;
use xng_dsp::viterbi::Viterbi;
use xng_mode_aero::burst::interleave;
use xng_mode_aero::frame::FrameEncoder;
use xng_mode_aero::modulate::modulate;
use xng_mode_aero::{su, to_message, AeroBurstDecoder, AeroChannelDecoder, CHANNEL_RATE};
use xng_types::{AppInfo, Mode, Provenance, StationIdentity};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn prov() -> Provenance {
    Provenance {
        station: StationIdentity::new("TEST-AERO"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    }
}

/// One ACARS user-data payload reused by both channels.
fn acars_user() -> Vec<u8> {
    let mut user = vec![0xFF, 0xFF];
    user.extend(xng_acars::block::build(
        '2', "VT-ANB", None, "B6", '4', Some("M11A"), Some("AI0142"),
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
        false,
    ));
    user
}

#[test]
fn p_channel_event_is_aero_l() {
    // L-band P-channel loopback (mirrors end_to_end.rs).
    let mut sus = su::build_isu_chain(0xA1B2C3, 0x44, 1, 7, &acars_user());
    while sus.len() % 6 != 0 {
        sus.push(su::fill_su());
    }
    let mut enc = FrameEncoder::new(600);
    let mut bits: Vec<u8> = (0..160).map(|i| (i % 2) as u8).collect();
    for (f, chunk) in sus.chunks(6).enumerate() {
        let mut frame_bytes = Vec::with_capacity(72);
        for s in chunk {
            frame_bytes.extend_from_slice(s);
        }
        bits.extend(enc.encode(&frame_bytes, f as u8));
    }
    bits.extend((0..64).map(|i| (i % 2) as u8));
    let mut iq = modulate(&bits, 600.0, CHANNEL_RATE, 30.0, 0.5);
    let mut noise = Noise(0x0bad_cafe_dead_beef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }
    let mut dec = AeroChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(4096) {
        events.extend(dec.process(chunk));
    }
    let e = events.iter().find(|e| e.acars.is_some()).expect("P-channel ACARS event");
    assert_eq!(e.mode, Mode::AeroL);
    let msg = to_message(e, 1_545_000_000, -50.0, prov());
    assert_eq!(msg.mode, Mode::AeroL);
    assert_eq!(msg.mode.to_string(), "aero-l");
}

#[test]
fn c_band_burst_event_is_aero_c() {
    // C-band feeder T-burst loopback (mirrors burst_e2e.rs).
    let sus = su::build_isu_chain(0xA1B2C3, 0x44, 1, 7, &acars_user());
    let mut bytes = vec![0xA1, 0xB2, 0xC3, 0x44];
    let crc = xng_dsp::checksum::HDLC_FCS.checksum(&bytes);
    bytes.extend(crc.to_le_bytes());
    for s in &sus {
        bytes.extend_from_slice(s);
    }
    while bytes.len() < 20 || (bytes.len() - 20) % 12 != 0 {
        bytes.push(0);
    }

    let mut bits: Vec<u8> =
        bytes.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)).collect();
    Lfsr15::new().apply(&mut bits);
    let coded = Viterbi::k7().encode(&bits);
    let mut burst_bits: Vec<u8> = (0..74).map(|i| (i % 2) as u8).collect();
    for i in (0..32).rev() {
        burst_bits.push(((xng_mode_aero::frame::UW >> i) & 1) as u8);
    }
    interleave(&coded[..320], 5, &mut burst_bits);
    let mut off = 320;
    while off + 192 <= coded.len() {
        interleave(&coded[off..off + 192], 3, &mut burst_bits);
        off += 192;
    }

    let rate = 1200.0;
    let cfo = 180.0;
    let spb = CHANNEL_RATE / rate;
    let tone_len = (126.0 * spb) as usize;
    let mut iq: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); 2000];
    let mut phase = 0.0f64;
    for _ in 0..tone_len {
        phase += std::f64::consts::TAU * cfo / CHANNEL_RATE;
        iq.push(Complex::from_polar(0.5, phase as f32));
    }
    iq.extend(modulate(&burst_bits, rate, CHANNEL_RATE, cfo, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 3000]);
    let mut noise = Noise(0xfeed_f00d_dead_c0de);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = AeroBurstDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(4096) {
        events.extend(dec.process(chunk));
    }
    let e = events.iter().find(|e| e.acars.is_some()).expect("C-band burst ACARS event");
    assert_eq!(e.mode, Mode::AeroC);
    let msg = to_message(e, 3_686_000_000, -50.0, prov());
    assert_eq!(msg.mode, Mode::AeroC, "C-band feeder burst must label as aero-c, not aero-l");
    assert_eq!(msg.mode.to_string(), "aero-c");
}
