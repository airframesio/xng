//! ASM transport + application-payload decode, verified against
//! spec-constructed ground-truth bit vectors.
//!
//! VERIFICATION (no self-consistency loopback): the test fixtures are built
//! by an *independent* MSB-first bit packer (`pack` / `pack_i` below) that
//! takes `(value, width)` pairs laid down in DOCUMENT ORDER per the cited
//! spec clause. The decoder (`xng_mode_vdes::asm`) reads by absolute
//! `(offset, width)`. The two share no code, so a wrong offset or width in
//! the decoder mismatches the hand-laid packer.
//!
//! Cited sources:
//!   - ITU-R M.2092-1, Annex 1: VDES carries ASM using the AIS Message 6
//!     (addressed) / Message 8 (broadcast) binary transport and the shared
//!     DAC/FID application catalogue.
//!   - ITU-R M.1371-5, Message 6 / Message 8 bit layout (transport header).
//!   - IMO SN.1/Circ.289 Annex: DAC=1 FID=16 (persons on board), FID=31
//!     (meteorological & hydrological data).

use xng_mode_vdes::asm;

/// Append `value` as `width` MSB-first bits (independent of the decoder).
fn pack(bits: &mut Vec<u8>, value: u64, width: usize) {
    for k in (0..width).rev() {
        bits.push(((value >> k) & 1) as u8);
    }
}

/// Append a two's-complement signed value as `width` MSB-first bits.
fn pack_i(bits: &mut Vec<u8>, value: i64, width: usize) {
    let masked = (value as u64) & ((1u64 << width) - 1);
    pack(bits, masked, width);
}

/// Pad to an octet boundary (ASM frames are octet-aligned).
fn octet_pad(bits: &mut Vec<u8>) {
    while bits.len() % 8 != 0 {
        bits.push(0);
    }
}

#[test]
fn broadcast_msg8_header_dac_fid_source() {
    // ITU-R M.1371-5 Message 8 (broadcast ASM) header, document order:
    //   msg ID 6 = 8, repeat 2 = 0, source MMSI 30, spare 2 = 0,
    //   DAC 10, FID 6, then application data.
    let mut bits = Vec::new();
    pack(&mut bits, 8, 6); // message ID 8
    pack(&mut bits, 0, 2); // repeat indicator
    pack(&mut bits, 211_000_001, 30); // source MMSI (German MID 211)
    pack(&mut bits, 0, 2); // spare
    pack(&mut bits, 1, 10); // DAC = 1 (IMO international)
    pack(&mut bits, 16, 6); // FID = 16 (persons on board)
    // Application data: 13-bit persons-on-board count.
    pack(&mut bits, 167, 13);
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("decodes as ASM");
    assert_eq!(a.msg_id, 8);
    assert_eq!(a.source_mmsi, 211_000_001);
    assert_eq!(a.dest_mmsi, None);
    assert_eq!(a.dac, 1);
    assert_eq!(a.fid, 16);
    assert_eq!(a.kind(), "asm-broadcast");
    // FID 16: persons-on-board count round-trips from the packed value.
    assert_eq!(a.app["persons_on_board"], 167);
}

#[test]
fn addressed_msg6_header_with_dest_mmsi() {
    // ITU-R M.1371-5 Message 6 (addressed ASM) header, document order:
    //   msg ID 6 = 6, repeat 2, source MMSI 30, seqno 2, dest MMSI 30,
    //   retransmit 1, spare 1, DAC 10, FID 6, then application data.
    let mut bits = Vec::new();
    pack(&mut bits, 6, 6); // message ID 6
    pack(&mut bits, 0, 2); // repeat
    pack(&mut bits, 366_000_005, 30); // source MMSI (US MID 366)
    pack(&mut bits, 0, 2); // sequence number
    pack(&mut bits, 367_000_010, 30); // destination MMSI
    pack(&mut bits, 0, 1); // retransmit
    pack(&mut bits, 0, 1); // spare
    pack(&mut bits, 1, 10); // DAC = 1
    pack(&mut bits, 16, 6); // FID = 16
    pack(&mut bits, 42, 13); // persons on board
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("decodes as ASM");
    assert_eq!(a.msg_id, 6);
    assert_eq!(a.source_mmsi, 366_000_005);
    assert_eq!(a.dest_mmsi, Some(367_000_010));
    assert_eq!(a.dac, 1);
    assert_eq!(a.fid, 16);
    assert_eq!(a.kind(), "asm-addressed");
    assert_eq!(a.app["persons_on_board"], 42);
}

#[test]
fn dac1_fid31_met_hydro_fields() {
    // IMO SN.1/Circ.289 Annex, DAC=1 FID=31 met/hydro application block,
    // document order from the application-data start:
    //   lon 25 (1/1000 min), lat 24 (1/1000 min), pos-accuracy 1,
    //   day 5, hour 5, minute 6, avg wind 7 (kt), gust 7 (kt),
    //   wind dir 9 (deg), [wind gust dir 9], air temp 11 (0.1 °C),
    //   humidity 7 (%), ...
    // We pack the leading grounded scalar fields and assert their physical
    // values; the WMO weather tail is deferred (decoder doesn't read it).
    let mut bits = Vec::new();
    pack(&mut bits, 8, 6); // Message 8 broadcast
    pack(&mut bits, 0, 2); // repeat
    pack(&mut bits, 244_000_000, 30); // source MMSI (Netherlands MID 244)
    pack(&mut bits, 0, 2); // spare
    pack(&mut bits, 1, 10); // DAC = 1
    pack(&mut bits, 31, 6); // FID = 31

    // Application data (offset 56). Position 4.0° E, 52.0° N at 1/1000 min:
    //   4.0° = 240000 thousandths-of-minute, 52.0° = 3120000.
    pack_i(&mut bits, 240_000, 25); // longitude
    pack_i(&mut bits, 3_120_000, 24); // latitude
    pack(&mut bits, 1, 1); // position accuracy = high
    pack(&mut bits, 14, 5); // day 14
    pack(&mut bits, 9, 5); // hour 09
    pack(&mut bits, 30, 6); // minute 30
    pack(&mut bits, 12, 7); // avg wind 12 kt
    pack(&mut bits, 18, 7); // gust 18 kt
    pack(&mut bits, 270, 9); // wind direction 270°
    pack(&mut bits, 290, 9); // wind gust direction 290° (decoder skips)
    pack_i(&mut bits, 153, 11); // air temp 15.3 °C (raw 153 → /10)
    pack(&mut bits, 80, 7); // humidity 80%
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("decodes as ASM");
    assert_eq!(a.dac, 1);
    assert_eq!(a.fid, 31);
    let app = &a.app;
    assert_eq!(app["lon"], 4.0);
    assert_eq!(app["lat"], 52.0);
    assert_eq!(app["position_accuracy"], true);
    assert_eq!(app["day"], 14);
    assert_eq!(app["hour"], 9);
    assert_eq!(app["minute"], 30);
    assert_eq!(app["wind_speed_kt"], 12);
    assert_eq!(app["wind_gust_kt"], 18);
    assert_eq!(app["wind_dir_deg"], 270);
    assert_eq!(app["air_temp_c"], 15.3);
    assert_eq!(app["humidity_pct"], 80);
}

#[test]
fn na_sentinels_are_omitted_not_emitted_as_junk() {
    // IMO SN.1/Circ.289: FID=31 N/A sentinels — day 0, hour 24, minute 60,
    // wind 127, wind dir 360, air temp raw -1024, humidity 101 — must be
    // OMITTED, not surfaced as bogus values.
    let mut bits = Vec::new();
    pack(&mut bits, 8, 6);
    pack(&mut bits, 0, 2);
    pack(&mut bits, 244_000_000, 30);
    pack(&mut bits, 0, 2);
    pack(&mut bits, 1, 10);
    pack(&mut bits, 31, 6);
    // Position sentinels: longitude 181°, latitude 91° = not available.
    pack_i(&mut bits, 181 * 60_000, 25);
    pack_i(&mut bits, 91 * 60_000, 24);
    pack(&mut bits, 0, 1); // position accuracy low
    pack(&mut bits, 0, 5); // day 0 = N/A
    pack(&mut bits, 24, 5); // hour 24 = N/A
    pack(&mut bits, 60, 6); // minute 60 = N/A
    pack(&mut bits, 127, 7); // wind N/A
    pack(&mut bits, 127, 7); // gust N/A
    pack(&mut bits, 360, 9); // wind dir N/A
    pack(&mut bits, 360, 9); // gust dir N/A
    pack_i(&mut bits, -1024, 11); // air temp N/A
    pack(&mut bits, 101, 7); // humidity N/A
    octet_pad(&mut bits);

    let a = asm::decode(&bits).unwrap();
    let app = a.app.as_object().unwrap();
    assert!(!app.contains_key("lon"), "181° longitude is N/A");
    assert!(!app.contains_key("lat"), "91° latitude is N/A");
    assert!(!app.contains_key("day"));
    assert!(!app.contains_key("hour"));
    assert!(!app.contains_key("minute"));
    assert!(!app.contains_key("wind_speed_kt"));
    assert!(!app.contains_key("wind_dir_deg"));
    assert!(!app.contains_key("air_temp_c"));
    assert!(!app.contains_key("humidity_pct"));
    // The position-accuracy flag is a real boolean, always present.
    assert_eq!(app["position_accuracy"], false);
}

#[test]
fn unknown_dac_fid_preserves_raw_payload() {
    // A DAC/FID with no clean-room layout must NOT be fabricated; the raw
    // application payload is preserved verbatim as data_hex.
    let mut bits = Vec::new();
    pack(&mut bits, 8, 6);
    pack(&mut bits, 0, 2);
    pack(&mut bits, 538_000_000, 30); // Marshall Islands MID 538
    pack(&mut bits, 0, 2);
    pack(&mut bits, 999, 10); // unallocated DAC
    pack(&mut bits, 5, 6);
    // 16 bits of application data: 0xAB 0xCD.
    pack(&mut bits, 0xAB, 8);
    pack(&mut bits, 0xCD, 8);

    let a = asm::decode(&bits).unwrap();
    assert_eq!(a.dac, 999);
    assert_eq!(a.fid, 5);
    assert_eq!(a.app["data_hex"], "abcd");
}
