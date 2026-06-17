//! End-to-end decode of a complete ADS-L iConspicuity frame.
//!
//! VERIFICATION ANCHOR (not a loopback):
//!
//! The test vector `FRAME` below is produced by an **independent** ADS-L
//! frame generator written in Python (a separate language and codebase),
//! `gen_vector.py`, that mirrors the OGN/SoftRF reference C structure
//! (`ads-l.h ADSL_Packet`) and codec (`ognconv.cpp` XXTEA-key0 scramble,
//! the `PolyPass`/`0xFFFA0480` CRC-24). The asserted *physical* values are
//! the EASA ADS-L 4 SRD860 spec field encodings — including the published
//! worked examples (ground-speed field 0xC4 = 120 m/s §G.1.8, altitude
//! field 0x0528 = 1000 m §G.1.7, vertical-rate field 0x048 = +10 m/s
//! §G.1.9) and the spec lat/lon LSBs (1°/93206, 1°/46603 §G.1.5).
//!
//! This crate only *decodes* the pinned bytes; it never encodes them, so
//! the test exercises the byte layout, XXTEA descramble, CRC verification
//! and every field decode against an external reference rather than against
//! its own encoder.
//!
//! See PROVENANCE.md for full sourcing.

use xng_mode_adsl::{Frame, FrameError};

/// Independently generated ADS-L iConspicuity frame (with the leading OGN
/// Length byte 0x18). See gen_vector.py / PROVENANCE.md.
const FRAME: [u8; 25] = [
    0x18, 0x00, 0x57, 0xee, 0x00, 0x23, 0xd8, 0x06, 0x67, 0x95, 0x5b, 0x5b, 0x52, 0x47, 0xbe, 0xf2,
    0x49, 0x41, 0xc8, 0xd5, 0x6a, 0xaa, 0xcd, 0x0d, 0x5f,
];

#[test]
fn decodes_independent_spec_vector() {
    let frame = Frame::parse(&FRAME).expect("frame must parse with valid CRC");
    assert_eq!(frame.version, 0x00);
    assert_eq!(frame.payload_type(), 0x02, "iConspicuity");

    let m = frame.iconspicuity().expect("iConspicuity payload");

    // Header / address (§F.2.2).
    assert_eq!(m.address, 0x3C5EE2);
    assert_eq!(m.address_table, 5);
    assert_eq!(m.address_type, "icao");
    assert!(!m.relay);

    // Meta (§G.1.1–G.1.4).
    assert_eq!(m.timestamp_q, 40);
    assert_eq!(m.timestamp_s, Some(10.0));
    assert_eq!(m.flight_state, 2);
    assert_eq!(m.flight_state_name, "airborne");
    assert_eq!(m.aircraft_category, 4);
    assert_eq!(m.aircraft_category_name, "glider");
    assert_eq!(m.emergency, 1);
    assert_eq!(m.emergency_name, "no_emergency");

    // Position (§G.1.5): lat is an exact multiple of the LSB, lon within
    // one LSB of 8.5°.
    let lat = m.latitude_deg.expect("lat");
    let lon = m.longitude_deg.expect("lon");
    assert!((lat - 47.5).abs() < 1e-9, "lat {lat}");
    assert!((lon - 8.5).abs() < 1.0 / 46603.0 + 1e-9, "lon {lon}");

    // Velocity / altitude / climb — spec worked examples (§G.1.7–G.1.10).
    assert_eq!(m.ground_speed_mps, 120.0); // field 0xC4
    assert_eq!(m.altitude_hae_m, 1000); // field 0x0528
    assert_eq!(m.vertical_rate_mps, Some(10.0)); // field 0x048
    assert!((m.ground_track_deg - 90.0).abs() < 1e-9);

    // Integrity / source (§G.1.11–G.1.16).
    assert_eq!(m.source_integrity, 3);
    assert_eq!(m.design_assurance, 2);
    assert_eq!(m.navigation_integrity, 11);
    assert_eq!(m.horizontal_accuracy, 6);
    assert_eq!(m.vertical_accuracy, 3);
    assert_eq!(m.velocity_accuracy, 2);
}

#[test]
fn decodes_without_leading_length_byte() {
    // The same frame, minus the OGN Length byte: Version + 20 + CRC = 24.
    let frame = Frame::parse(&FRAME[1..]).expect("frame must parse");
    let m = frame.iconspicuity().expect("iConspicuity");
    assert_eq!(m.address, 0x3C5EE2);
    assert_eq!(m.altitude_hae_m, 1000);
}

#[test]
fn json_shape_is_structured() {
    let frame = Frame::parse(&FRAME).unwrap();
    let json = frame.iconspicuity().unwrap().to_json();
    assert_eq!(json["address"], 0x3C5EE2);
    assert_eq!(json["address_type"], "icao");
    assert_eq!(json["aircraft_category_name"], "glider");
    assert_eq!(json["altitude_hae_m"], 1000);
    assert_eq!(json["ground_speed_mps"], 120.0);
    assert_eq!(json["source_integrity"], 3);
    // optional fields are present when valid
    assert_eq!(json["vertical_rate_mps"], 10.0);
    assert!(json.get("latitude_deg").is_some());
}

#[test]
fn corrupted_crc_is_rejected() {
    let mut bad = FRAME;
    bad[10] ^= 0x01; // flip a bit in the scrambled payload
    assert!(matches!(Frame::parse(&bad), Err(FrameError::BadCrc)));
}

#[test]
fn no_fix_sentinel_yields_no_position() {
    // Build a frame whose latitude field is the 0xFFFFFF "no fix" sentinel.
    // We descramble the known-good frame, overwrite the lat field, re-scramble
    // and re-CRC using the crate's own primitives — but the *assertion* (that
    // 0xFFFFFF means "no fix", §G.1.5) is the external spec fact under test.
    use xng_mode_adsl::{crc, words_from_le, words_to_le, xxtea, LENGTH_FIELD, XXTEA_LOOPS};

    let good = Frame::parse(&FRAME).unwrap();
    let mut payload = good.payload;
    // Position lat occupies payload bytes 7..10.
    payload[7] = 0xFF;
    payload[8] = 0xFF;
    payload[9] = 0xFF;

    let mut words = words_from_le(&payload);
    xxtea::encrypt_key0(&mut words, XXTEA_LOOPS);
    let scrambled = words_to_le(&words);

    let mut body = Vec::new();
    body.push(good.version);
    body.extend_from_slice(&scrambled);
    let c = crc::calc(&body);
    body.push((c >> 16) as u8);
    body.push((c >> 8) as u8);
    body.push(c as u8);

    let mut framed = vec![LENGTH_FIELD];
    framed.extend_from_slice(&body);

    let f = Frame::parse(&framed).expect("re-CRC'd frame parses");
    let m = f.iconspicuity().unwrap();
    assert_eq!(m.latitude_deg, None);
    assert_eq!(m.longitude_deg, None);
}
