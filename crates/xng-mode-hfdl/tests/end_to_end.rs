//! HFDL RF loopback at all four data rates.

use num_complex::Complex;
use xng_mode_hfdl::modulate::{burst_symbols, modulate};
use xng_mode_hfdl::{fec::SETTINGS, pdu, HfdlChannelDecoder, CHANNEL_RATE};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn acars_mpdu() -> Vec<u8> {
    let block = xng_acars::block::build(
        '2', "N471XG", None, "B6", '4', Some("M11A"), Some("UA0042"),
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
        false,
    );
    pdu::build_mpdu_downlink(3, 0xC7, &[pdu::build_lpdu_acars(&block)])
}

fn run(setting_idx: usize, payload: &[u8], cfo: f64, offset: f64) -> Vec<pdu::HfdlEvent> {
    let s = &SETTINGS[setting_idx];
    assert!(payload.len() <= s.payload_bytes(), "payload too big for setting");
    let syms = burst_symbols(payload, s);
    // The decoder applies the +1440 Hz subcarrier shift internally, so
    // the modulated burst sits at offset + 1440.
    let mut iq = vec![Complex::new(0.0, 0.0); 3000];
    iq.extend(modulate(&syms, CHANNEL_RATE, offset + 1440.0 + cfo, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 3000]);
    let mut noise = Noise(0xd00d_f00d_0042_4242 + setting_idx as u64);
    for x in &mut iq {
        *x += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }
    let mut dec = HfdlChannelDecoder::new(CHANNEL_RATE, offset).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(8192) {
        events.extend(dec.process(chunk));
    }
    events
}

#[test]
fn decodes_spdu_at_300bps() {
    let spdu = pdu::build_spdu(7, 1234, 52);
    let events = run(0, &spdu, 20.0, 0.0);
    let sq = events.iter().find(|e| e.kind == "squitter").expect("squitter decodes");
    assert_eq!(sq.details["gs_id"], 7);
    assert_eq!(sq.details["frame_index"], 1234);
    assert_eq!(sq.details["systable_version"], 52);
}

#[test]
fn decodes_acars_at_600bps() {
    let events = run(1, &acars_mpdu(), -35.0, 0.0);
    let e = events.iter().find(|e| e.kind == "acars").expect("ACARS decodes");
    let b = e.acars.as_ref().unwrap();
    assert!(b.crc_ok);
    assert_eq!(b.core.tail.as_deref(), Some("N471XG"));
    assert_eq!(b.core.app.as_ref().unwrap()["app"], "adsc");
}

#[test]
fn decodes_acars_at_1200bps() {
    let events = run(2, &acars_mpdu(), 50.0, 0.0);
    assert!(events.iter().any(|e| e.kind == "acars" && e.acars.as_ref().unwrap().crc_ok));
}

#[test]
fn decodes_acars_at_1800bps() {
    let events = run(3, &acars_mpdu(), 15.0, 0.0);
    assert!(events.iter().any(|e| e.kind == "acars" && e.acars.as_ref().unwrap().crc_ok));
}

#[test]
fn decodes_from_wideband_capture() {
    // 240 kHz capture slice; HFDL channel at +30 kHz with 25 Hz CFO.
    let s = &SETTINGS[3];
    let syms = burst_symbols(&acars_mpdu(), s);
    let mut iq = vec![Complex::new(0.0, 0.0); 30_000];
    iq.extend(modulate(&syms, 240_000.0, 30_000.0 + 1440.0 + 25.0, 0.4));
    iq.extend(vec![Complex::new(0.0, 0.0); 30_000]);
    let mut noise = Noise(0xfeed_cafe_1234_5678);
    for x in &mut iq {
        *x += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }
    let mut dec = HfdlChannelDecoder::new(240_000.0, 30_000.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(65_536) {
        events.extend(dec.process(chunk));
    }
    assert!(events.iter().any(|e| e.kind == "acars" && e.acars.as_ref().unwrap().crc_ok));
}
