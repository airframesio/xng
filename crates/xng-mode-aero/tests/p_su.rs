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

/// Like `decode_su10` but returns the full decoded `Message`s (through
/// `to_message`), so the AERO-2 resolved-satellite tag and AERO-4 frame
/// header injected into `details` can be asserted end-to-end.
fn decode_su10_messages(su10: Vec<u8>) -> Vec<xng_types::Message> {
    let su = su::su_with_crc(su10);
    let mut frame_bytes = Vec::with_capacity(72);
    frame_bytes.extend_from_slice(&su);
    for _ in 0..5 {
        frame_bytes.extend_from_slice(&su::fill_su());
    }

    let mut enc = FrameEncoder::new(600);
    let mut bits: Vec<u8> = (0..160).map(|i| (i % 2) as u8).collect();
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
    let mut msgs = Vec::new();
    for chunk in iq.chunks(4096) {
        for e in dec.process(chunk) {
            msgs.push(to_message(&e, 1_545_000_000, -50.0, prov()));
        }
    }
    msgs
}

/// AERO-2 + AERO-4 end-to-end: a 0x0C satellite_identification SU, sent
/// through the full frame/Viterbi/scrambler chain, resolves the satellite
/// AND the events carry the parsed 16-bit frame header in `details`.
/// Oracle: JAERO 0x0C field layout + JAERO frame-header nibble split
/// (`aerol.cpp`). The encoder writes format id 1, superframe 0.
#[test]
fn satellite_resolution_and_frame_header_end_to_end() {
    // satid 20, longitude index 200 → 300° → 60.0°W (classic AOR-W slot),
    // Psmc1 channel 0x0123 global (beam = global).
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x0C;
    su10[2] = 0x29; // seqno 10, satid_hi 1
    su10[3] = 0x40; // satid_lo 4 → satid 20
    su10[5] = 200; // 60.0°W
    su10[6] = 0x01;
    su10[7] = 0x23; // Psmc1 channel 0x0123, no spot beam
    let msgs = decode_su10_messages(su10);

    // The satellite-id Aero message itself carries the resolved satellite,
    // the beam, and the parsed frame header in its details.
    let m = msgs
        .iter()
        .find_map(|m| match &m.body {
            xng_types::MessageBody::Aero { kind, details } if kind == "satellite-id" => {
                Some(details.clone())
            }
            _ => None,
        })
        .expect("satellite-id Aero message decoded end-to-end");

    // AERO-2: resolved satellite + beam.
    assert_eq!(m["resolved_satellite"]["satellite_id"], 20);
    assert_eq!(m["resolved_satellite"]["longitude_deg"], 60.0);
    assert_eq!(m["resolved_satellite"]["longitude_dir"], "W");
    assert_eq!(m["resolved_satellite"]["region"], "AOR-W");
    assert_eq!(m["beam"], "global");

    // AERO-4: parsed 16-bit frame header. The encoder writes format id 1,
    // superframe 0 (JAERO nibble split: formatid bits 15..12).
    assert_eq!(m["frame_header"]["format_id"], 1);
    assert_eq!(m["frame_header"]["superframe"], 0);
    // frame_counter1/2 are the running frame counter the encoder wrote
    // (0 or 1 for the two frames); both nibbles are equal per the encoder.
    let fc1 = m["frame_header"]["frame_counter1"].as_u64().unwrap();
    assert_eq!(fc1, m["frame_header"]["frame_counter2"].as_u64().unwrap());
    assert!(fc1 <= 1);
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
        channel: xng_mode_aero::AeroChannel::PChannel,
        frame_header: None,
        satellite: None,
    };
    let msg = to_message(&event, 1_545_000_000, -50.0, prov());
    match msg.body {
        MessageBody::Aero { kind, details } => {
            assert_eq!(kind, "log-control");
            assert_eq!(details["event"], "log-on-confirm");
            // AERO-8.2: channel + line rate injected into the details.
            assert_eq!(details["channel"], "p-channel");
            assert_eq!(details["line_bit_rate"], 600);
        }
        other => panic!("expected MessageBody::Aero, got {other:?}"),
    }
}

#[test]
fn call_announcement_decodes_end_to_end() {
    // AERO-1.2: 0x21 Call_announcement, AES 0xA1B2C3, GES 0x44,
    // rx channel 4000, tx channel 2000.
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x21;
    su10[1..4].copy_from_slice(&[0xA1, 0xB2, 0xC3]);
    su10[4] = 0x44;
    su10[6] = (4000u16 >> 8) as u8;
    su10[7] = (4000u16 & 0xFF) as u8;
    su10[8] = (2000u16 >> 8) as u8;
    su10[9] = (2000u16 & 0xFF) as u8;
    let events = decode_su10(su10);
    let v = events
        .iter()
        .find(|v| v["su_type"] == "call-announcement")
        .expect("call-announcement decoded through the full chain");
    assert_eq!(v["aes_id"], "A1B2C3");
    assert_eq!(v["ges_id"], 0x44);
    assert_eq!(v["receive_mhz"], 4000.0 * 0.0025 + 1510.0);
    assert_eq!(v["transmit_mhz"], 2000.0 * 0.0025 + 1611.5);
}

#[test]
fn t_channel_assignment_decodes_end_to_end() {
    // AERO-1.2: 0x51 T_channel_assignment, AES 0x123456, GES 0x07.
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x51;
    su10[1..4].copy_from_slice(&[0x12, 0x34, 0x56]);
    su10[4] = 0x07;
    let events = decode_su10(su10);
    let v = events
        .iter()
        .find(|v| v["su_type"] == "t-channel-assignment")
        .expect("t-channel-assignment decoded through the full chain");
    assert_eq!(v["aes_id"], "123456");
    assert_eq!(v["ges_id"], 0x07);
}

#[test]
fn pr_control_isu_decodes_end_to_end() {
    // AERO-1.4: 0x40 P/R-channel control ISU through the full chain.
    // GES 0x2A, bit-rate code 1 → 1200 bps, Pd channel 0x0123.
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x40;
    su10[4] = 0x2A; // GES (octet 5)
    su10[7] = 0x10; // byte8 high nibble = bit-rate code 1
    su10[8] = 0x01; // byte9 (channel high, no spot beam)
    su10[9] = 0x23; // byte10 (channel low)
    let events = decode_su10(su10);
    let v = events
        .iter()
        .find(|v| v["su_type"] == "pr-channel-control-isu")
        .expect("pr-channel-control-isu decoded through the full chain");
    assert_eq!(v["ges_id"], 0x2A);
    assert_eq!(v["bit_rate"], 1200);
    assert_eq!(v["pd_mhz"], 0x0123 as f64 * 0.0025 + 1510.0);
    assert_eq!(v["spotbeam"], false);

    // And it lands in MessageBody::Aero with the SU type as the kind.
    let event = xng_mode_aero::AeroEvent {
        user: su::AeroUserData {
            aes_id: String::new(),
            ges_id: 0x2A,
            qno: 0,
            refno: 0,
            data: Vec::new(),
        },
        acars: None,
        bit_rate: 600,
        su_event: Some(v.clone()),
        mode: Mode::AeroL,
        channel: xng_mode_aero::AeroChannel::PChannel,
        frame_header: None,
        satellite: None,
    };
    let msg = to_message(&event, 1_545_000_000, -50.0, prov());
    match msg.body {
        MessageBody::Aero { kind, details } => {
            assert_eq!(kind, "pr-channel-control-isu");
            // Decoded protocol bit_rate (Pd carrier) is preserved;
            // the physical line rate is surfaced separately (AERO-8.2).
            assert_eq!(details["bit_rate"], 1200);
            assert_eq!(details["line_bit_rate"], 600);
            assert_eq!(details["channel"], "p-channel");
        }
        other => panic!("expected MessageBody::Aero, got {other:?}"),
    }
}

#[test]
fn eirp_table_decodes_end_to_end() {
    // AERO-1.4: 0x28 EIRP-table broadcast through the full chain.
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x28;
    let events = decode_su10(su10);
    assert!(
        events.iter().any(|v| v["su_type"] == "eirp-table-broadcast"),
        "eirp-table-broadcast decoded through the full chain"
    );
}

#[test]
fn satellite_id_decodes_end_to_end() {
    // AERO-1.3: 0x0C satellite_identification through the full chain.
    // satid 5, seqno 10, 150.0°E, Psmc1 channel 0x0200.
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x0C;
    su10[2] = 40; // seqno 10, satid_hi 0
    su10[3] = 0x50; // satid_lo 5
    su10[5] = 100; // 150.0°E
    su10[6] = 0x02;
    su10[7] = 0x00; // Psmc1 channel 0x0200
    let events = decode_su10(su10);
    let v = events
        .iter()
        .find(|v| v["su_type"] == "satellite-id")
        .expect("satellite-id decoded through the full chain");
    assert_eq!(v["satellite_id"], 5);
    assert_eq!(v["seq"], 10);
    assert_eq!(v["longitude_deg"], 150.0);
    assert_eq!(v["longitude_dir"], "E");
    assert_eq!(v["psmc1_mhz"], 0x0200 as f64 * 0.0025 + 1510.0);
}
