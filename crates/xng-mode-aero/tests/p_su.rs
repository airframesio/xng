//! P-channel structured-SU integration tests (AERO-1.1/1.2/1.3): build a
//! P-channel frame carrying a control/broadcast SU, modulate it, run it
//! through the full `AeroChannelDecoder`, and assert the structured event
//! and the resulting `MessageBody::Aero`.
//!
//! Oracle: JAERO `aerol.h` AEROTypeP type names + `aerol.cpp` field
//! layouts. These are end-to-end through the real frame/Viterbi/scrambler
//! chain, so the SU must survive framing exactly as JAERO frames it.

use num_complex::Complex;
use xng_mode_aero::frame::FrameEncoder;
use xng_mode_aero::modulate::modulate;
use xng_mode_aero::{su, to_message, AeroChannelDecoder, CHANNEL_RATE};
use xng_types::{AppInfo, MessageBody, Mode, Provenance, StationIdentity};

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

/// Run one frame full of a single control SU (rest filled) through the
/// P-channel decoder and return the structured SU events seen.
fn decode_su10(su10: Vec<u8>) -> Vec<serde_json::Value> {
    let su = su::su_with_crc(su10);
    // Six SUs per 600 bps frame: the control SU plus five fill SUs.
    let mut frame_bytes = Vec::with_capacity(72);
    frame_bytes.extend_from_slice(&su);
    for _ in 0..5 {
        frame_bytes.extend_from_slice(&su::fill_su());
    }

    let mut enc = FrameEncoder::new(600);
    let mut bits: Vec<u8> = (0..160).map(|i| (i % 2) as u8).collect();
    // Two identical frames so the Viterbi overlap settles before the SU
    // we assert on.
    for f in 0..2u8 {
        bits.extend(enc.encode(&frame_bytes, f));
    }
    bits.extend((0..64).map(|i| (i % 2) as u8));

    let mut iq = modulate(&bits, 600.0, CHANNEL_RATE, 30.0, 0.5);
    let mut noise = Noise(0x1234_5678_9abc_def0);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }
    let mut dec = AeroChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(4096) {
        for e in dec.process(chunk) {
            assert_eq!(e.mode, Mode::AeroL);
            if let Some(v) = e.su_event.clone() {
                events.push(v);
            }
        }
    }
    events
}

#[test]
fn log_on_confirm_decodes_end_to_end() {
    // 0x11 = Log_on_confirm (GES → AES), AES 0xC0FFEE, GES 0x05.
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x11;
    su10[1..4].copy_from_slice(&[0xC0, 0xFF, 0xEE]);
    su10[4] = 0x05;
    let events = decode_su10(su10);
    let v = events
        .iter()
        .find(|v| v["su_type"] == "log-control")
        .expect("log-control event decoded through the full chain");
    assert_eq!(v["event"], "log-on-confirm");
    assert_eq!(v["direction"], "ges-to-aes");
    assert_eq!(v["aes_id"], "C0FFEE");
    assert_eq!(v["ges_id"], 0x05);

    // And it lands in MessageBody::Aero with the SU type as the kind.
    let event = xng_mode_aero::AeroEvent {
        user: su::AeroUserData {
            aes_id: "C0FFEE".to_owned(),
            ges_id: 0x05,
            qno: 0,
            refno: 0,
            data: Vec::new(),
        },
        acars: None,
        bit_rate: 600,
        su_event: Some(v.clone()),
        mode: Mode::AeroL,
    };
    let msg = to_message(&event, 1_545_000_000, -50.0, prov());
    match msg.body {
        MessageBody::Aero { kind, details } => {
            assert_eq!(kind, "log-control");
            assert_eq!(details["event"], "log-on-confirm");
        }
        other => panic!("expected MessageBody::Aero, got {other:?}"),
    }
}
