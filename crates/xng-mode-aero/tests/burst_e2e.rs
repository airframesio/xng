//! R/T burst loopback: SUs → burst waveform (carrier + alternating +
//! UW + interleaved coded data) → burst decoder.

use num_complex::Complex;
use xng_dsp::scramble::Lfsr15;
use xng_dsp::viterbi::Viterbi;
use xng_mode_aero::modulate::modulate;
use xng_mode_aero::{su, AeroBurstDecoder, CHANNEL_RATE};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

use xng_mode_aero::burst::interleave;

/// Build a T-burst bit stream: alternating preamble + UW + coded sections.
fn t_burst_bits(decoded_bytes: &[u8]) -> Vec<u8> {
    // decoded_bytes length must be 20 + 12g.
    assert!(decoded_bytes.len() >= 20 && (decoded_bytes.len() - 20) % 12 == 0);
    let mut bits: Vec<u8> =
        decoded_bytes.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)).collect();
    Lfsr15::new().apply(&mut bits);
    let coded = Viterbi::k7().encode(&bits);

    let mut out: Vec<u8> = (0..74).map(|i| (i % 2) as u8).collect(); // alternating
    for i in (0..32).rev() {
        out.push(((xng_mode_aero::frame::UW >> i) & 1) as u8);
    }
    interleave(&coded[..320], 5, &mut out);
    let mut off = 320;
    while off + 192 <= coded.len() {
        interleave(&coded[off..off + 192], 3, &mut out);
        off += 192;
    }
    out
}

#[test]
fn decodes_t_burst_with_acars() {
    // ACARS user data via P-style ISU chain inside a T burst.
    let mut user = vec![0xFF, 0xFF];
    user.extend(xng_acars::block::build(
        '2', "VT-ANB", None, "B6", '4', Some("M11A"), Some("AI0142"),
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
        false,
    ));
    let sus = su::build_isu_chain(0xA1B2C3, 0x44, 1, 7, &user);

    // T header: AES(3) + GES(1) + CRC(2).
    let mut bytes = vec![0xA1, 0xB2, 0xC3, 0x44];
    let crc = xng_dsp::checksum::HDLC_FCS.checksum(&bytes);
    bytes.extend(crc.to_le_bytes());
    for s in &sus {
        bytes.extend_from_slice(s);
    }
    // Pad to 20 + 12g.
    while bytes.len() < 20 || (bytes.len() - 20) % 12 != 0 {
        bytes.push(0);
    }

    let bits = t_burst_bits(&bytes);
    let rate = 1200.0;
    // Carrier tone section then modulated bits, with a CFO of 180 Hz.
    let cfo = 180.0;
    let spb = CHANNEL_RATE / rate;
    let tone_len = (126.0 * spb) as usize;
    let mut iq: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); 2000];
    let mut phase = 0.0f64;
    for _ in 0..tone_len {
        phase += std::f64::consts::TAU * cfo / CHANNEL_RATE;
        iq.push(Complex::from_polar(0.5, phase as f32));
    }
    iq.extend(modulate(&bits, rate, CHANNEL_RATE, cfo, 0.5));
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
    let e = events.iter().find(|e| e.acars.is_some()).expect("ACARS from T burst");
    assert_eq!(e.bit_rate, 1200);
    let b = e.acars.as_ref().unwrap();
    assert!(b.crc_ok);
    assert_eq!(b.core.tail.as_deref(), Some("VT-ANB"));
    assert_eq!(b.core.app.as_ref().unwrap()["app"], "adsc");
}

#[test]
fn decodes_r_burst() {
    // Short user payload split over R SUs.
    let payload: Vec<u8> = (0..25).map(|i| i as u8 ^ 0x33).collect();
    let r_sus = su::build_r_sus(0x123456, 0x07, 2, 3, &payload);
    assert_eq!(r_sus.len(), 3);

    // One burst per R SU (R bursts carry a single 19-byte SU).
    let mut dec = AeroBurstDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut events = Vec::new();
    for r_su in &r_sus {
        let mut bytes = r_su.clone();
        while bytes.len() < 20 {
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

        let spb = CHANNEL_RATE / 600.0;
        let mut iq: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); 1500];
        let mut phase = 0.0f64;
        for _ in 0..(150.0 * spb) as usize {
            phase += std::f64::consts::TAU * -90.0 / CHANNEL_RATE;
            iq.push(Complex::from_polar(0.5, phase as f32));
        }
        iq.extend(modulate(&burst_bits, 600.0, CHANNEL_RATE, -90.0, 0.5));
        iq.extend(vec![Complex::new(0.0, 0.0); 3000]);
        for chunk in iq.chunks(4096) {
            events.extend(dec.process(chunk));
        }
    }
    assert_eq!(events.len(), 1, "R reassembly should complete once");
    assert_eq!(events[0].user.data, payload);
    assert_eq!(events[0].user.aes_id, "123456");
    assert_eq!(events[0].bit_rate, 600);
}
