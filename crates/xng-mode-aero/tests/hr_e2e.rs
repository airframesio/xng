//! 10.5 kbps OQPSK P-channel loopback.

use num_complex::Complex;
use xng_mode_aero::frame::FrameEncoder;
use xng_mode_aero::oqpsk::{hr_frame_bits, modulate_oqpsk, BIT_RATE, CHANNEL_RATE_HR};
use xng_mode_aero::{su, AeroChannelDecoder};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn hr_bits_with_acars() -> Vec<u8> {
    let mut user = vec![0xFF, 0xFF];
    user.extend(xng_acars::block::build(
        '2', "VT-ANB", None, "B6", '4', Some("M11A"), Some("AI0142"),
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
        false,
    ));
    let mut sus = su::build_isu_chain(0xA1B2C3, 0x44, 1, 7, &user);
    while sus.len() % 26 != 0 {
        sus.push(su::fill_su());
    }
    let mut enc = FrameEncoder::new(BIT_RATE);
    // Run-in + two frames (the payload fits in one; the second keeps the
    // stream alive past the decoder's pipeline). The run-in models the
    // always-on P channel preceding our frame: pseudorandom (scrambled
    // idle) bits long enough for the demod's coarse CFO acquisition and
    // carrier lock (~1.5 s).
    let mut idle = 0x1234_5678_9abc_def0u64;
    let mut bits: Vec<u8> = (0..18_000)
        .map(|_| {
            idle = idle.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((idle >> 33) & 1) as u8
        })
        .collect();
    for (f, chunk) in sus.chunks(26).enumerate() {
        let mut frame_bytes = Vec::with_capacity(312);
        for s in chunk {
            frame_bytes.extend_from_slice(s);
        }
        bits.extend(hr_frame_bits(&mut enc, &frame_bytes, f as u8));
    }
    bits.extend((0..400).map(|i| (i % 2) as u8));
    bits
}

/// Bit-level: hr frame stream → HrFramer → SUs → ACARS. Validates the
/// dual-rail UW search, header/dummy skip, 64×78 interleaver, Viterbi,
/// descrambler, and SU layer — everything except the OQPSK demod.
#[test]
fn hr_framing_bit_level() {
    use xng_mode_aero::oqpsk::HrFramer;
    let bits = hr_bits_with_acars();
    let mut framer = HrFramer::new();
    let mut users = Vec::new();
    for &b in &bits {
        framer.push(if b == 1 { 1.0 } else { -1.0 }, b, &mut users);
    }
    let user = users.first().expect("user data reassembles");
    let acars = su::parse_acars(&user.data).expect("ACARS parses");
    assert!(acars.crc_ok);
    assert_eq!(acars.core.tail.as_deref(), Some("VT-ANB"));
    assert_eq!(acars.core.app.as_ref().unwrap()["app"], "adsc");
}

/// Bit-level with rail inversions: the dual-rail UW hypotheses must
/// correct per-rail polarity for the whole frame.
#[test]
fn hr_framing_rail_inversion() {
    use xng_mode_aero::oqpsk::HrFramer;
    let bits = hr_bits_with_acars();
    let mut framer = HrFramer::new();
    let mut users = Vec::new();
    for (k, &b) in bits.iter().enumerate() {
        let b = b ^ (k % 2 == 0) as u8; // invert one rail
        framer.push(if b == 1 { 1.0 } else { -1.0 }, b, &mut users);
    }
    assert!(!users.is_empty(), "inverted-rail frame must still decode");
}

#[test]
fn decodes_acars_at_10500() {
    let bits = hr_bits_with_acars();
    let mut iq = modulate_oqpsk(&bits, CHANNEL_RATE_HR, 120.0, 0.5);
    let mut noise = Noise(0xaa55_1234_9999_0001);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }
    let mut dec = AeroChannelDecoder::new(CHANNEL_RATE_HR, 0.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(8192) {
        events.extend(dec.process(chunk));
    }
    let e = events.iter().find(|e| e.acars.is_some()).expect("ACARS at 10.5k");
    assert_eq!(e.bit_rate, 10_500);
    let b = e.acars.as_ref().unwrap();
    assert!(b.crc_ok);
    assert_eq!(b.core.tail.as_deref(), Some("VT-ANB"));
    assert_eq!(b.core.app.as_ref().unwrap()["app"], "adsc");
}

#[test]
fn decodes_from_wideband_capture() {
    let bits = hr_bits_with_acars();
    let fs = 2_400_000.0;
    // Generate at 48k then naive-upsample is costly; modulate directly at
    // 240 kHz (5x) and decode via DDC from there (240k/48k integer).
    let mut iq = modulate_oqpsk(&bits, 240_000.0, 15_000.0 + 80.0, 0.4);
    let _ = fs;
    let mut noise = Noise(0x0102_0304_0506_0708);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }
    let mut dec = AeroChannelDecoder::new(240_000.0, 15_000.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(65_536) {
        events.extend(dec.process(chunk));
    }
    let e = events.iter().find(|e| e.acars.is_some()).expect("ACARS at 10.5k via DDC");
    assert!(e.acars.as_ref().unwrap().crc_ok);
}
