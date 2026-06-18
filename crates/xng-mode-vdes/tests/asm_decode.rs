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
//!   - IMO SN.1/Circ.289 Annex: DAC=1 FID=11 (met/hydro IMO236), FID=16
//!     (persons on board), FID=17 (VTS-generated/synthetic targets), FID=18
//!     (clearance time to enter port), FID=31 (met/hydro IMO289).
//!   - UNECE Inland AIS / RIS (ES-TRIN): DAC=200 FID=10 (inland static &
//!     voyage), FID=55 (inland number of persons on board).
//!
//! INDEPENDENT ORACLES (not encoder↔decoder loopback):
//!   - Bit offsets / widths / scaling / N/A sentinels for every field below
//!     are taken from gpsd's `driver_ais.c` + `gps.h` (BSD-licensed FACT
//!     reference) and the GPSd AIVDM/AIVDO field tables. The decoder reads by
//!     absolute (offset, width); these tests pack the SAME fields by an
//!     independent MSB-first packer in document order — a wrong offset/width
//!     mismatches.
//!   - `inland_static_voyage_matches_pyais` decodes two REAL AIVDM-armored
//!     payloads and asserts the values published by pyais 2.x's own test
//!     suite (`test_msg_type_8_inland`, `test_msg_type_8_inland_2`): a true
//!     third-party decode oracle, no fixture built by our own encoder.

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

/// AIVDM 6-bit-armor → MSB-first bit vector (ITU-R M.1371-5 Table 47 armor):
/// each printable char carries 6 payload bits. Independent of the decoder and
/// of the `pack` helpers above — used only to feed REAL third-party vectors.
fn unarmor(payload: &str) -> Vec<u8> {
    let mut bits = Vec::new();
    for c in payload.bytes() {
        let mut v = c as i32 - 48;
        if v > 40 {
            v -= 8;
        }
        for k in (0..6).rev() {
            bits.push(((v >> k) & 1) as u8);
        }
    }
    bits
}

#[test]
fn inland_static_voyage_matches_pyais() {
    // ORACLE: pyais 2.x own test suite. These are real Inland-AIS Message 8
    // DAC=200 FID=10 sentences; the expected values are exactly what pyais
    // decodes (test_msg_type_8_inland / _2). Our decoder must agree.
    //   sentence 1: !BSVDM,1,1,,B,83m;Fa0j2d<<<<<<<0@pUg`50000,0*11
    //   sentence 2: !AIVDO,1,1,,A,85M67F@j2U=7EW=RAkQkBDITMV=e,0*51
    let a = asm::decode(&unarmor("83m;Fa0j2d<<<<<<<0@pUg`50000")).expect("ASM");
    assert_eq!(a.msg_id, 8);
    assert_eq!(a.source_mmsi, 257_087_140);
    assert_eq!(a.dac, 200);
    assert_eq!(a.fid, 10);
    // pyais: beam 7.5 m. (length 13.5 m here, raw 135.)
    assert_eq!(a.app["beam_m"], 7.5);
    assert_eq!(a.app["length_m"], 13.5);

    let b = asm::decode(&unarmor("85M67F@j2U=7EW=RAkQkBDITMV=e")).expect("ASM");
    assert_eq!(b.msg_id, 8);
    assert_eq!(b.source_mmsi, 366_053_209);
    assert_eq!(b.dac, 200);
    assert_eq!(b.fid, 10);
    // pyais: length 180.6 m, beam 42 m, loaded NotAvailable (omitted).
    assert_eq!(b.app["length_m"], 180.6);
    assert_eq!(b.app["beam_m"], 42.0);
    assert!(!b.app.as_object().unwrap().contains_key("loaded"), "loaded=0 (N/A) omitted");
}

/// Build a Message 8 broadcast header (msg ID 8 .. DAC .. FID) and return the
/// bit vector ready for the application body to be appended.
fn msg8_header(bits: &mut Vec<u8>, mmsi: u64, dac: u64, fid: u64) {
    pack(bits, 8, 6);
    pack(bits, 0, 2); // repeat
    pack(bits, mmsi, 30);
    pack(bits, 0, 2); // spare
    pack(bits, dac, 10);
    pack(bits, fid, 6);
}

/// Build a Message 6 addressed header (msg ID 6 .. dest MMSI .. DAC .. FID).
fn msg6_header(bits: &mut Vec<u8>, src: u64, dest: u64, dac: u64, fid: u64) {
    pack(bits, 6, 6);
    pack(bits, 0, 2); // repeat
    pack(bits, src, 30);
    pack(bits, 0, 2); // sequence number
    pack(bits, dest, 30);
    pack(bits, 0, 1); // retransmit
    pack(bits, 0, 1); // spare
    pack(bits, dac, 10);
    pack(bits, fid, 6);
}

/// 6-bit-ASCII pack of a string (ITU-R M.1371-5 Table 47), `chars` long,
/// '@'-padded. Independent of the decoder's `sixbit` reader.
fn pack_str(bits: &mut Vec<u8>, s: &str, chars: usize) {
    let bytes = s.as_bytes();
    for k in 0..chars {
        let c = if k < bytes.len() { bytes[k] } else { b'@' };
        let v = if c == b'@' { 0u64 } else { (c as u64) & 0x3f };
        // Map ASCII '@'..'_' (0x40..0x5f) -> 0..31, ' '..'?' (0x20..0x3f) -> 32..63.
        let sixbit = if (0x40..=0x5f).contains(&c) {
            (c - 0x40) as u64
        } else if (0x20..=0x3f).contains(&c) {
            c as u64
        } else {
            v
        };
        pack(bits, sixbit, 6);
    }
}

#[test]
fn dac1_fid11_met_hydro_imo236_layout() {
    // IMO236 met/hydro (DAC=1 FID=11): LATITUDE 24 FIRST, then LONGITUDE 25,
    // packed ddhhmm, UNSIGNED air temp (raw-600)/10 and dew point (raw-200)/10,
    // pressure raw+800. Offsets per gpsd driver_ais.c dac1fid11.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 244_000_000, 1, 11);
    // App data (offset 56): lat first.
    pack_i(&mut bits, 3_120_000, 24); // lat 52.0° N (×0.001 min)
    pack_i(&mut bits, 240_000, 25); // lon 4.0° E
    pack(&mut bits, 14, 5); // day
    pack(&mut bits, 9, 5); // hour
    pack(&mut bits, 30, 6); // minute
    pack(&mut bits, 12, 7); // avg wind 12 kt
    pack(&mut bits, 18, 7); // gust 18 kt
    pack(&mut bits, 270, 9); // wind dir 270°
    pack(&mut bits, 290, 9); // wind gust dir 290°
    pack(&mut bits, 753, 11); // air temp raw 753 -> (753-600)/10 = 15.3 °C
    pack(&mut bits, 80, 7); // humidity 80%
    pack(&mut bits, 320, 10); // dew point raw 320 -> (320-200)/10 = 12.0 °C
    pack(&mut bits, 213, 9); // pressure raw 213 -> 213+800 = 1013 hPa
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.dac, 1);
    assert_eq!(a.fid, 11);
    let app = &a.app;
    assert_eq!(app["lat"], 52.0);
    assert_eq!(app["lon"], 4.0);
    assert_eq!(app["day"], 14);
    assert_eq!(app["hour"], 9);
    assert_eq!(app["minute"], 30);
    assert_eq!(app["wind_speed_kt"], 12);
    assert_eq!(app["wind_gust_kt"], 18);
    assert_eq!(app["wind_dir_deg"], 270);
    assert_eq!(app["wind_gust_dir_deg"], 290);
    assert_eq!(app["air_temp_c"], 15.3);
    assert_eq!(app["humidity_pct"], 80);
    assert_eq!(app["dew_point_c"], 12.0);
    assert_eq!(app["pressure_hpa"], 1013);
}

#[test]
fn dac1_fid11_distinct_from_fid31_position_order() {
    // Guard against the FID 11/31 confusion: the SAME raw position bits decode
    // differently because FID 11 is lat-first(24)/lon-first... wait, FID 11 is
    // lat(24) then lon(25); FID 31 is lon(25) then lat(24). Pack a vector that
    // is valid as FID 11 but whose bits, read as FID 31, would give a
    // different (here out-of-range, hence omitted) position.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 244_000_000, 1, 11);
    pack_i(&mut bits, 3_120_000, 24); // lat 52.0
    pack_i(&mut bits, 240_000, 25); // lon 4.0
    octet_pad(&mut bits);
    let a = asm::decode(&bits).unwrap();
    assert_eq!(a.app["lat"], 52.0);
    assert_eq!(a.app["lon"], 4.0);
}

#[test]
fn dac1_fid31_deep_fields() {
    // IMO289 FID=31 extended fields beyond the leading scalars (dew point,
    // pressure, visibility, water level, current, waves, sea state, water
    // temp, salinity, ice). Offsets/scaling/sentinels per gpsd dac1fid31.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 244_000_000, 1, 31);
    pack_i(&mut bits, 240_000, 25); // lon 4.0 (FID 31: lon first)
    pack_i(&mut bits, 3_120_000, 24); // lat 52.0
    pack(&mut bits, 1, 1); // pos accuracy
    pack(&mut bits, 14, 5); // day
    pack(&mut bits, 9, 5); // hour
    pack(&mut bits, 30, 6); // minute
    pack(&mut bits, 12, 7); // wind 12
    pack(&mut bits, 18, 7); // gust 18
    pack(&mut bits, 270, 9); // wind dir 270
    pack(&mut bits, 290, 9); // wind gust dir 290
    pack_i(&mut bits, 153, 11); // air temp signed 15.3
    pack(&mut bits, 80, 7); // humidity 80
    pack_i(&mut bits, 95, 10); // dew point signed 9.5 °C
    pack(&mut bits, 214, 9); // pressure raw 214 -> 214+799 = 1013 hPa
    pack(&mut bits, 2, 2); // pressure tendency increasing
    pack(&mut bits, 0, 1); // visibility ">" flag
    pack(&mut bits, 75, 7); // visibility 7.5 NM
    pack(&mut bits, 1500, 12); // water level raw 1500 -> (1500-1000)/100 = 5.0 m
    pack(&mut bits, 1, 2); // water level trend decreasing
    pack(&mut bits, 23, 8); // surface current 2.3 kt
    pack(&mut bits, 180, 9); // surface current dir 180
    // currents #2/#3 (cspeed2 8, cdir2 9, cdepth2 5, cspeed3 8, cdir3 9,
    // cdepth3 5) = 44 bits we don't decode -> pad as N/A.
    pack(&mut bits, 255, 8); // cspeed2 N/A
    pack(&mut bits, 360, 9); // cdir2 N/A
    pack(&mut bits, 0, 5); // cdepth2
    pack(&mut bits, 255, 8); // cspeed3 N/A
    pack(&mut bits, 360, 9); // cdir3 N/A
    pack(&mut bits, 0, 5); // cdepth3
    pack(&mut bits, 25, 8); // wave height 2.5 m
    pack(&mut bits, 8, 6); // wave period 8 s
    pack(&mut bits, 200, 9); // wave dir 200
    pack(&mut bits, 255, 8); // swell height N/A (not decoded)
    pack(&mut bits, 63, 6); // swell period N/A
    pack(&mut bits, 360, 9); // swell dir N/A
    pack(&mut bits, 4, 4); // sea state 4
    pack_i(&mut bits, 175, 10); // water temp signed 17.5 °C
    pack(&mut bits, 7, 3); // precip N/A
    pack(&mut bits, 350, 9); // salinity 35.0 ‰
    pack(&mut bits, 1, 2); // ice = yes
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.fid, 31);
    let app = &a.app;
    assert_eq!(app["dew_point_c"], 9.5);
    assert_eq!(app["pressure_hpa"], 1013);
    assert_eq!(app["pressure_tendency"], 2);
    assert_eq!(app["visibility_nm"], 7.5);
    assert_eq!(app["water_level_m"], 5.0);
    assert_eq!(app["water_level_trend"], 1);
    assert_eq!(app["surface_current_speed_kt"], 2.3);
    assert_eq!(app["surface_current_dir_deg"], 180);
    assert_eq!(app["wave_height_m"], 2.5);
    assert_eq!(app["wave_period_s"], 8);
    assert_eq!(app["wave_dir_deg"], 200);
    assert_eq!(app["sea_state"], 4);
    assert_eq!(app["water_temp_c"], 17.5);
    assert_eq!(app["salinity_permille"], 35.0);
    assert_eq!(app["ice"], true);
    // the wind_gust_dir is now surfaced too
    assert_eq!(app["wind_gust_dir_deg"], 290);
}

#[test]
fn dac1_fid17_synthetic_target() {
    // IMO289 FID=17 VTS-generated/synthetic target, first 122-bit report.
    // idtype/id/spare/lat/lon/cog/second/sog per gpsd dac1fid17.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 2_073_900, 1, 17);
    pack(&mut bits, 0, 2); // idtype = MMSI
    pack(&mut bits, 366_000_123, 42); // target MMSI
    pack(&mut bits, 0, 4); // spare
    pack_i(&mut bits, 3_120_000, 24); // lat 52.0
    pack_i(&mut bits, 240_000, 25); // lon 4.0
    pack(&mut bits, 90, 9); // COG 90°
    pack(&mut bits, 30, 6); // timestamp sec 30
    pack(&mut bits, 123, 10); // SOG raw 123 -> 12.3 kt
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.fid, 17);
    let app = &a.app;
    assert_eq!(app["target_mmsi"], 366_000_123u64);
    assert_eq!(app["lat"], 52.0);
    assert_eq!(app["lon"], 4.0);
    assert_eq!(app["cog_deg"], 90);
    assert_eq!(app["timestamp_sec"], 30);
    assert_eq!(app["sog_kt"], 12.3);
}

#[test]
fn dac1_fid17_callsign_target() {
    // idtype = 2 (callsign): id is 7×6-bit ASCII, not a 42-bit number.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 2_073_900, 1, 17);
    pack(&mut bits, 2, 2); // idtype = callsign
    pack_str(&mut bits, "ABCD123", 7); // 42 bits of 6-bit ASCII
    pack(&mut bits, 0, 4); // spare
    pack_i(&mut bits, 3_120_000, 24);
    pack_i(&mut bits, 240_000, 25);
    pack(&mut bits, 360, 9); // COG N/A
    pack(&mut bits, 60, 6); // timestamp N/A (>=60)
    pack(&mut bits, 1023, 10); // SOG N/A
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.app["target_callsign"], "ABCD123");
    assert_eq!(a.app["lat"], 52.0);
    let app = a.app.as_object().unwrap();
    assert!(!app.contains_key("cog_deg"), "COG 360 = N/A");
    assert!(!app.contains_key("timestamp_sec"), "second 60 = N/A");
    assert!(!app.contains_key("sog_kt"), "SOG 1023 = N/A");
}

#[test]
fn dac1_fid18_clearance_time_addressed() {
    // IMO289 FID=18 clearance time to enter port (Message 6 addressed).
    // linkage/month/day/hour/minute/portname/destination/lon/lat per gpsd
    // dac1fid18 (data start at bit 88).
    let mut bits = Vec::new();
    msg6_header(&mut bits, 366_000_005, 367_000_010, 1, 18);
    pack(&mut bits, 42, 10); // linkage id
    pack(&mut bits, 6, 4); // month June
    pack(&mut bits, 18, 5); // day 18
    pack(&mut bits, 14, 5); // hour 14
    pack(&mut bits, 30, 6); // minute 30
    pack_str(&mut bits, "ROTTERDAM BERTH 7", 20); // port name 20 chars
    pack_str(&mut bits, "NLRTM", 5); // destination UN/LOCODE
    pack_i(&mut bits, 240_000, 25); // lon 4.0
    pack_i(&mut bits, 3_120_000, 24); // lat 52.0
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.msg_id, 6);
    assert_eq!(a.dest_mmsi, Some(367_000_010));
    assert_eq!(a.dac, 1);
    assert_eq!(a.fid, 18);
    let app = &a.app;
    assert_eq!(app["linkage_id"], 42);
    assert_eq!(app["month"], 6);
    assert_eq!(app["day"], 18);
    assert_eq!(app["hour"], 14);
    assert_eq!(app["minute"], 30);
    assert_eq!(app["port_name"], "ROTTERDAM BERTH 7");
    assert_eq!(app["destination"], "NLRTM");
    assert_eq!(app["lon"], 4.0);
    assert_eq!(app["lat"], 52.0);
}

#[test]
fn dac200_fid10_inland_static_voyage_spec_vector() {
    // UNECE Inland-AIS FID=10 hand vector (complements the pyais payload test).
    // eni/length/beam/shiptype/hazard/draught/loaded/quality per gpsd
    // dac200fid10.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 211_000_001, 200, 10);
    pack_str(&mut bits, "02327013", 8); // ENI (8 chars)
    pack(&mut bits, 1100, 13); // length raw 1100 -> 110.0 m
    pack(&mut bits, 115, 10); // beam raw 115 -> 11.5 m
    pack(&mut bits, 8010, 14); // ERI ship type
    pack(&mut bits, 0, 3); // hazard 0 cones
    pack(&mut bits, 250, 11); // draught raw 250 -> 2.50 m
    pack(&mut bits, 1, 2); // loaded
    pack(&mut bits, 1, 1); // speed quality high
    pack(&mut bits, 0, 1); // course quality low
    pack(&mut bits, 1, 1); // heading quality high
    pack(&mut bits, 0, 8); // spare
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.dac, 200);
    assert_eq!(a.fid, 10);
    let app = &a.app;
    assert_eq!(app["eni"], "02327013");
    assert_eq!(app["length_m"], 110.0);
    assert_eq!(app["beam_m"], 11.5);
    assert_eq!(app["eri_ship_type"], 8010);
    assert_eq!(app["hazard_cones"], 0);
    assert_eq!(app["draught_m"], 2.5);
    assert_eq!(app["loaded"], "loaded");
    assert_eq!(app["speed_quality_high"], true);
    assert_eq!(app["course_quality_high"], false);
    assert_eq!(app["heading_quality_high"], true);
}

#[test]
fn dac200_fid55_inland_persons_on_board() {
    // UNECE Inland-AIS FID=55 number of persons on board (Message 6 addressed).
    // crew 8 / passengers 13 / personnel 8 per gpsd dac200fid55 (start 88).
    let mut bits = Vec::new();
    msg6_header(&mut bits, 211_000_001, 211_000_002, 200, 55);
    pack(&mut bits, 5, 8); // crew 5
    pack(&mut bits, 250, 13); // passengers 250
    pack(&mut bits, 3, 8); // shipboard personnel 3
    pack(&mut bits, 0, 51); // spare
    octet_pad(&mut bits);

    let a = asm::decode(&bits).expect("ASM");
    assert_eq!(a.dac, 200);
    assert_eq!(a.fid, 55);
    let app = &a.app;
    assert_eq!(app["crew"], 5);
    assert_eq!(app["passengers"], 250);
    assert_eq!(app["personnel"], 3);
}

#[test]
fn dac200_fid55_unknown_counts_omitted() {
    // 0xFF crew / 0x1FFF passengers / 0xFF personnel = unknown -> omitted.
    let mut bits = Vec::new();
    msg6_header(&mut bits, 211_000_001, 211_000_002, 200, 55);
    pack(&mut bits, 0xFF, 8);
    pack(&mut bits, 0x1FFF, 13);
    pack(&mut bits, 0xFF, 8);
    pack(&mut bits, 0, 51);
    octet_pad(&mut bits);

    let a = asm::decode(&bits).unwrap();
    let app = a.app.as_object().unwrap();
    assert!(!app.contains_key("crew"));
    assert!(!app.contains_key("passengers"));
    assert!(!app.contains_key("personnel"));
    // Nothing decoded -> raw payload preserved, never fabricated.
    assert!(app.contains_key("data_hex"));
}

#[test]
fn dac1_fid11_na_sentinels_omitted() {
    // FID 11 N/A sentinels (distinct from FID 31): air temp 2047, dew point
    // 1023, pressure 511, humidity 127, wind 127, wind dir 511, lat 0x7FFFFF,
    // lon 0xFFFFFF.
    let mut bits = Vec::new();
    msg8_header(&mut bits, 244_000_000, 1, 11);
    pack(&mut bits, 0x7F_FFFF, 24); // lat N/A
    pack(&mut bits, 0xFF_FFFF, 25); // lon N/A (25-bit; 0xFFFFFF is 24 ones)
    pack(&mut bits, 0, 5); // day N/A
    pack(&mut bits, 24, 5); // hour N/A
    pack(&mut bits, 60, 6); // minute N/A
    pack(&mut bits, 127, 7); // wind N/A
    pack(&mut bits, 127, 7); // gust N/A
    pack(&mut bits, 511, 9); // wind dir N/A
    pack(&mut bits, 511, 9); // gust dir N/A
    pack(&mut bits, 2047, 11); // air temp N/A
    pack(&mut bits, 127, 7); // humidity N/A
    pack(&mut bits, 1023, 10); // dew point N/A
    pack(&mut bits, 511, 9); // pressure N/A
    octet_pad(&mut bits);

    let a = asm::decode(&bits).unwrap();
    let app = a.app.as_object().unwrap();
    for k in ["day", "hour", "minute", "wind_speed_kt", "wind_dir_deg", "air_temp_c", "humidity_pct", "dew_point_c", "pressure_hpa"] {
        assert!(!app.contains_key(k), "{k} sentinel must be omitted");
    }
}
