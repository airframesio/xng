//! RF loopback: SUs → P-channel frames → A-BPSK/MSK waveform → decoder.

use num_complex::Complex;
use xng_mode_aero::frame::FrameEncoder;
use xng_mode_aero::modulate::modulate;
use xng_mode_aero::{su, AeroChannelDecoder, CHANNEL_RATE};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

const ADSC_TEXT: &str =
    "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5";

/// Build a P-channel bit stream: idle + frames carrying one ACARS message.
fn p_channel_bits(rate: u32) -> Vec<u8> {
    let mut user = vec![0xFF, 0xFF];
    user.extend(xng_acars::block::build(
        '2', "VT-ANB", None, "B6", 'A', None, None, ADSC_TEXT, false,
    ));
    let mut sus = su::build_isu_chain(0xA1B2C3, 0x44, 1, 7, &user);
    while sus.len() % 6 != 0 {
        sus.push(su::fill_su());
    }

    let mut enc = FrameEncoder::new(rate);
    let mut bits: Vec<u8> = (0..160).map(|i| (i % 2) as u8).collect(); // idle
    for (f, chunk) in sus.chunks(6).enumerate() {
        let mut frame_bytes = Vec::with_capacity(72);
        for s in chunk {
            frame_bytes.extend_from_slice(s);
        }
        bits.extend(enc.encode(&frame_bytes, f as u8));
    }
    bits.extend((0..64).map(|i| (i % 2) as u8)); // tail idle
    bits
}

fn run(rate: u32, fs: f64, cfo: f64, offset: f64, noise_amp: f32) -> Vec<xng_mode_aero::AeroEvent> {
    let bits = p_channel_bits(rate);
    let mut iq = modulate(&bits, rate as f64, fs, offset + cfo, 0.5);
    let mut noise = Noise(0x0bad_cafe_dead_beef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * noise_amp, noise.next() * noise_amp);
    }
    let mut dec = AeroChannelDecoder::new(fs, offset).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(4096) {
        events.extend(dec.process(chunk));
    }
    events
}

#[test]
fn decodes_acars_at_600bps() {
    let events = run(600, CHANNEL_RATE, 30.0, 0.0, 0.02);
    let e = events.iter().find(|e| e.acars.is_some()).expect("ACARS event");
    assert_eq!(e.bit_rate, 600);
    let b = e.acars.as_ref().unwrap();
    assert!(b.crc_ok);
    assert_eq!(b.core.tail.as_deref(), Some("VT-ANB"));
    assert_eq!(b.core.label, "B6");
    assert_eq!(b.core.app.as_ref().unwrap()["app"], "adsc");
    assert_eq!(e.user.aes_id, "A1B2C3");
}

#[test]
fn decodes_acars_at_1200bps() {
    let events = run(1200, CHANNEL_RATE, -45.0, 0.0, 0.02);
    let e = events.iter().find(|e| e.acars.is_some()).expect("ACARS event");
    assert_eq!(e.bit_rate, 1200);
    assert!(e.acars.as_ref().unwrap().crc_ok);
}

#[test]
fn decodes_from_wideband_capture() {
    // 2.4 MS/s capture; Aero channel at +40 kHz with 25 Hz CFO error.
    let events = run(1200, 2_400_000.0, 25.0, 40_000.0, 0.01);
    let e = events.iter().find(|e| e.acars.is_some()).expect("ACARS event");
    assert!(e.acars.as_ref().unwrap().crc_ok);
    assert_eq!(e.acars.as_ref().unwrap().core.text, ADSC_TEXT);
}
