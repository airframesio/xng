//! Oracle-anchored decode tests.
//!
//! Every vector here is a real entry from the externally published reference
//! decoder `amsa-code/fgb-decoder` compliance kit
//! (`src/test/resources/compliance-kit/<HEX>.json`, Apache-2.0). The filename
//! is the input hex; the asserted fields are copied from that file's decoded
//! JSON. This validates the field layout, BCH(21,15)/BCH(12,7) generator
//! polynomials, the 15-/22-hex beacon-ID derivation, and the position
//! arithmetic against an independent implementation — not a self-consistency
//! loopback. See PROVENANCE.md.

use xng_mode_sarsat::{decode_hex, Format};

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
}

// compliance-kit/8DA41A02C17FDFF83B4235FFFFFFFF.json
// Standard Location (Long), ELT - Serial, France (218).
#[test]
fn std_location_elt_serial() {
    let b = decode_hex("8DA41A02C17FDFF83B4235FFFFFFFF").unwrap();
    assert_eq!(b.message_type, "Standard Location (Long)");
    assert_eq!(b.format, Format::Long);
    assert_eq!(b.hex_id, "1B48340582FFBFF");
    assert_eq!(b.country_code, 218);
    assert_eq!(b.protocol_type, "ELT - Serial");
    assert_eq!(b.cs_type_approval, Some(104));
    assert_eq!(b.beacon_serial_number, Some(705));
    assert!(b.bch1.ok, "PDF-1 BCH should verify");
    // tail is FFFFFFFF -> no position, no PDF-2 field.
    assert!(b.position.is_none());
    assert!(b.bch2.is_none());
}

// compliance-kit/8E8628D187874181D738F700000000.json
// Standard Location (Long), EPIRB - Serial, with coarse position (southern
// hemisphere). Italy (232).
#[test]
fn std_location_epirb_serial_with_coarse_position() {
    let b = decode_hex("8E8628D187874181D738F700000000").unwrap();
    assert_eq!(b.protocol_type, "EPIRB - Serial");
    assert_eq!(b.hex_id, "1D0C51A30EFFBFF");
    assert_eq!(b.country_code, 232);
    assert_eq!(b.cs_type_approval, Some(163));
    assert_eq!(b.beacon_serial_number, Some(4487));
    let c = b.coarse_position.expect("coarse position");
    approx(c.latitude, -7.25);
    approx(c.longitude, 12.0);
    // offset is the no-offset default -> refined == coarse.
    let p = b.position.expect("position");
    approx(p.latitude, -7.25);
    approx(p.longitude, 12.0);
    assert!(b.bch1.ok);
}

// compliance-kit/A3E7B10016150D364D8B3689C09437.json
// Standard Location (Long), PLB - Serial, full coarse + offset position +
// PDF-2. Vietnam (574).
#[test]
fn std_location_plb_serial_offset_position_and_bch2() {
    let b = decode_hex("A3E7B10016150D364D8B3689C09437").unwrap();
    assert_eq!(b.protocol_type, "PLB - Serial");
    assert_eq!(b.hex_id, "47CF62002CFFBFF");
    assert_eq!(b.country_code, 574);
    assert_eq!(b.cs_type_approval, Some(708));
    assert_eq!(b.beacon_serial_number, Some(22));
    let c = b.coarse_position.unwrap();
    approx(c.latitude, 21.0);
    approx(c.longitude, 105.5);
    let p = b.position.unwrap();
    approx(p.latitude, 21.041_111_111_111_11);
    approx(p.longitude, 105.49);
    assert!(b.bch1.ok);
    let bch2 = b.bch2.expect("PDF-2 present on positioned long message");
    assert!(bch2.ok);
    assert_eq!(bch2.transmitted, "010000110111");
}

// compliance-kit/ADA5B61C8C7FDFFBE89AF7FFFFFFFF.json
// Standard Location (Long), Aircraft Operator (5-bit Baudot designator).
// Colombia (730).
#[test]
fn std_location_aircraft_operator() {
    let b = decode_hex("ADA5B61C8C7FDFFBE89AF7FFFFFFFF").unwrap();
    assert_eq!(b.protocol_type, "Aircraft Operator");
    assert_eq!(b.hex_id, "5B4B6C3918FFBFF");
    assert_eq!(b.country_code, 730);
    assert_eq!(b.aircraft_operator.as_deref(), Some("FAC"));
    assert_eq!(b.aircraft_serial_number, Some(140));
    assert!(b.bch1.ok);
}

// compliance-kit/1C66738928FFBFF.json (15-hex short ID)
// Standard Location, Aircraft Address (24-bit ICAO). Brazil-region (227).
#[test]
fn std_location_aircraft_address_15hex() {
    let b = decode_hex("1C66738928FFBFF").unwrap();
    assert_eq!(b.format, Format::Unknown); // 15-hex: format flag unknown
    assert_eq!(b.protocol_type, "Aircraft Address");
    assert_eq!(b.hex_id, "1C66738928FFBFF");
    assert_eq!(b.country_code, 227);
    assert_eq!(b.aircraft_24bit_address_hex.as_deref(), Some("39C494"));
    assert_eq!(b.aircraft_24bit_address_octal.as_deref(), Some("16342224"));
    // No BCH parity is present in a 15-hex string.
    assert!(!b.bch1.ok);
}

// compliance-kit/1D0E4E9142FFBFF.json (15-hex short ID)
// Standard Location, PLB - Serial. Italy (232).
#[test]
fn std_location_plb_serial_15hex() {
    let b = decode_hex("1D0E4E9142FFBFF").unwrap();
    assert_eq!(b.protocol_type, "PLB - Serial");
    assert_eq!(b.hex_id, "1D0E4E9142FFBFF");
    assert_eq!(b.country_code, 232);
    assert_eq!(b.cs_type_approval, Some(157));
    assert_eq!(b.beacon_serial_number, Some(2209));
}

// compliance-kit/8E0D0990014710021963C85C7009F5.json
// Return Link Service Location, TAC 2153 / RLS id 5, coarse + fine position +
// PDF-2. (224).
#[test]
fn return_link_service_location() {
    let b = decode_hex("8E0D0990014710021963C85C7009F5").unwrap();
    assert_eq!(b.message_type, "Return Link Service Location");
    assert_eq!(b.protocol_type, "Return Link Service");
    assert_eq!(b.hex_id, "1C1A132002BFDFF");
    assert_eq!(b.country_code, 224);
    assert_eq!(b.rls_tac_number.as_deref(), Some("2153"));
    assert_eq!(b.rls_id, Some(5));
    let c = b.coarse_position.unwrap();
    approx(c.latitude, 28.0);
    approx(c.longitude, 0.0);
    let p = b.position.unwrap();
    approx(p.latitude, 27.882_222_222_222_22);
    approx(p.longitude, 0.0);
    assert!(b.bch1.ok);
    assert!(b.bch2.as_ref().unwrap().ok);
}

// compliance-kit/96ED09900149D4D467EE0851A3B2E8.json
// Return Link Service Location, USA (366), western-hemisphere fine position.
#[test]
fn return_link_service_location_west() {
    let b = decode_hex("96ED09900149D4D467EE0851A3B2E8").unwrap();
    assert_eq!(b.protocol_type, "Return Link Service");
    assert_eq!(b.hex_id, "2DDA132002BFDFF");
    assert_eq!(b.country_code, 366);
    assert_eq!(b.rls_tac_number.as_deref(), Some("2153"));
    assert_eq!(b.rls_id, Some(5));
    let c = b.coarse_position.unwrap();
    approx(c.latitude, 39.0);
    approx(c.longitude, -77.0);
    let p = b.position.unwrap();
    approx(p.latitude, 38.926_666_666_666_67);
    approx(p.longitude, -76.977_777_777_777_77);
    assert!(b.bch1.ok && b.bch2.as_ref().unwrap().ok);
}

// compliance-kit/4CB31E0C02A82608F011BE00000000.json
// User (Short), Aviation. Tunisia (203).
#[test]
fn user_aviation_short() {
    let b = decode_hex("4CB31E0C02A82608F011BE00000000").unwrap();
    assert_eq!(b.message_type, "User (Short)");
    assert_eq!(b.format, Format::Short);
    assert_eq!(b.protocol_type, "Aviation");
    assert_eq!(b.hex_id, "99663C1805504C1");
    assert_eq!(b.country_code, 203);
    assert!(b.bch1.ok);
}

// compliance-kit/4E86A265C600146DBC407600000000.json
// User (Short), Serial (Maritime Float-Free). Italy (232).
#[test]
fn user_serial_short() {
    let b = decode_hex("4E86A265C600146DBC407600000000").unwrap();
    assert_eq!(b.message_type, "User (Short)");
    assert_eq!(b.protocol_type, "Serial");
    assert_eq!(b.hex_id, "9D0D44CB8C0028D");
    assert_eq!(b.country_code, 232);
    assert!(b.bch1.ok);
}

// compliance-kit/3EE6F80D1AFFBFF.json (15-hex)
// Standard Location, Aircraft Address with an Australian-registered callsign in
// the oracle; we assert the address + country which are layout-independent.
#[test]
fn std_location_aircraft_address_australia_15hex() {
    let b = decode_hex("3EE6F80D1AFFBFF").unwrap();
    assert_eq!(b.protocol_type, "Aircraft Address");
    assert_eq!(b.hex_id, "3EE6F80D1AFFBFF");
    assert_eq!(b.country_code, 503);
    assert_eq!(b.aircraft_24bit_address_hex.as_deref(), Some("7C068D"));
    assert_eq!(b.aircraft_24bit_address_octal.as_deref(), Some("37003215"));
}

// Layout check (NOT a loopback): the PDF-1 protected field is bits 25-85 and
// the parity is bits 86-106. On a known-good oracle vector the transmitted
// parity matches the recomputed parity; corrupting a parity nibble (hex char
// 16 covers bits 89-92, inside the parity field) must make the BCH flag the
// mismatch.
#[test]
fn bch1_detects_corrupted_parity() {
    let good = decode_hex("A3E7B10016150D364D8B3689C09437").unwrap();
    assert!(good.bch1.ok);
    let mut chars: Vec<char> = "A3E7B10016150D364D8B3689C09437".chars().collect();
    chars[16] = if chars[16] == '0' { '8' } else { '0' };
    let mutated: String = chars.into_iter().collect();
    let bad = decode_hex(&mutated).unwrap();
    assert!(!bad.bch1.ok, "BCH1 must detect the corrupted parity");
}

#[test]
fn rejects_bad_length() {
    assert!(decode_hex("DEADBEEF").is_err());
    assert!(decode_hex("").is_err());
}

#[test]
fn serializes_to_json() {
    let b = decode_hex("8DA41A02C17FDFF83B4235FFFFFFFF").unwrap();
    let j = serde_json::to_value(&b).unwrap();
    assert_eq!(j["hex_id"], "1B48340582FFBFF");
    assert_eq!(j["country_code"], 218);
    assert_eq!(j["protocol_type"], "ELT - Serial");
}
