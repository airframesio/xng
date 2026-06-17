//! Externally-verified UAT decode tests.
//!
//! Vectors come from FlightAware dump978's published `sample-data.txt`
//! (real off-air UAT captures, distributed with the dump978 source). Each
//! expected decode was produced by running the corresponding dump978
//! reference decoder on the same payload on this machine:
//!
//! * downlink fields/JSON — the maintained `uat_message.cc`
//!   (`AdsbMessage::ToJson`) decoder;
//! * uplink site / FIS-B framing / DLAC text — `legacy/uat_decode.c`
//!   (`uat2text`), which is the reference that decodes FIS-B contents.
//!
//! The RS parity vectors are the exact check octets libfec emits for the
//! same payloads (`init_rs_char(8, 0x187, 120, 1, nroots, pad)` /
//! `encode_rs_char`), so the FEC layer is verified against the reference
//! implementation, not a self-consistency loop.

use xng_mode_uat::{decode_frame, fec, modulate, UatChannelDecoder, UatMessage};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Reed-Solomon FEC vs. dump978/libfec
// ---------------------------------------------------------------------------

#[test]
fn rs_downlink_short_parity_matches_libfec() {
    // Payload = first 18 bytes of sample-data.txt downlink
    // `-00a66ef135445d525a0c0519119021204800`.
    // libfec `encode_rs_char` (nroots=12, pad=225) emitted this parity.
    let payload = hex("00a66ef135445d525a0c0519119021204800");
    assert_eq!(payload.len(), 18);
    let parity = fec::encode_downlink_short(&payload);
    assert_eq!(to_hex(&parity), "6cb82bc4d53a5b2bb0a8ec6e");
}

#[test]
fn rs_downlink_long_parity_matches_libfec() {
    // Payload = the 34-byte long downlink for N5130E from sample-data.txt;
    // libfec `encode_rs_char` (nroots=14, pad=207) emitted this parity.
    let payload = hex("08a66ef1353e2d525fd4050911882aa038101d06b85d440be2a4c2a0000590000000");
    assert_eq!(payload.len(), 34);
    let parity = fec::encode_downlink_long(&payload);
    assert_eq!(to_hex(&parity), "d0e3c7ccb1fed50a5afd9d6aa963");
}

#[test]
fn rs_downlink_short_corrects_six_errors() {
    // RS(30,18) corrects up to (nroots/2)=6 symbol errors.
    let payload = hex("00a66ef135445d525a0c0519119021204800");
    let parity = fec::encode_downlink_short(&payload);
    let mut block: Vec<u8> = payload.iter().chain(parity.iter()).copied().collect();
    let clean = block.clone();
    for k in 0..6usize {
        block[k * 4] ^= 0x5a + k as u8;
    }
    let c = fec::correct_downlink(&block).expect("must correct 6 errors");
    assert_eq!(c.errors, 6);
    assert_eq!(c.payload, payload);
    // The first 18 bytes of the corrected block equal the original payload.
    assert_eq!(&clean[..18], &c.payload[..]);
}

#[test]
fn rs_downlink_long_corrects_seven_errors() {
    // RS(48,34) corrects up to (nroots/2)=7 symbol errors.
    let payload = hex("08a66ef1353e2d525fd4050911882aa038101d06b85d440be2a4c2a0000590000000");
    let parity = fec::encode_downlink_long(&payload);
    let mut block: Vec<u8> = payload.iter().chain(parity.iter()).copied().collect();
    for k in 0..7usize {
        block[k * 6] ^= 0xa5 ^ (k as u8);
    }
    let c = fec::correct_downlink(&block).expect("must correct 7 errors");
    assert_eq!(c.errors, 7);
    assert_eq!(c.payload, payload);
}

#[test]
fn rs_uplink_block_interleave_roundtrips() {
    // Six RS(92,72) blocks, byte-interleaved into a 552-byte frame, with
    // a handful of injected errors per block (≤ 10 = nroots/2) recovered.
    let mut blocks = [[0u8; fec::UPLINK_BLOCK]; fec::UPLINK_BLOCKS_PER_FRAME];
    let mut payloads = Vec::new();
    for (b, block) in blocks.iter_mut().enumerate() {
        let mut data = vec![0u8; fec::UPLINK_BLOCK_DATA];
        for (i, d) in data.iter_mut().enumerate() {
            *d = ((i * 7 + b * 13 + 3) & 0xff) as u8;
        }
        let parity = fec::encode_uplink_block(&data);
        block[..fec::UPLINK_BLOCK_DATA].copy_from_slice(&data);
        block[fec::UPLINK_BLOCK_DATA..].copy_from_slice(&parity);
        payloads.extend_from_slice(&data);
    }
    let mut frame = fec::interleave_uplink(&blocks);
    // Inject 5 errors into each deinterleaved block.
    for b in 0..fec::UPLINK_BLOCKS_PER_FRAME {
        for k in 0..5usize {
            frame[(k * 10) * fec::UPLINK_BLOCKS_PER_FRAME + b] ^= 0x33 + k as u8;
        }
    }
    let (data, errors) = fec::correct_uplink(&frame).expect("uplink must correct");
    assert_eq!(data, payloads);
    assert_eq!(errors, 5 * fec::UPLINK_BLOCKS_PER_FRAME);
}

// ---------------------------------------------------------------------------
// Downlink ADS-B decode vs. dump978 uat_message.cc JSON
// ---------------------------------------------------------------------------

#[test]
fn downlink_short_type0_matches_dump978_json() {
    // sample-data.txt: -00a66ef135445d525a0c0519119021204800
    let payload = hex("00a66ef135445d525a0c0519119021204800");
    let parity = fec::encode_downlink_short(&payload);
    let frame: Vec<u8> = payload.iter().chain(parity.iter()).copied().collect();
    let (msg, errors) = decode_frame(&frame).expect("decode");
    assert_eq!(errors, 0);
    let UatMessage::Downlink(d) = msg else { panic!("expected downlink") };

    // Expected from dump978 `uat_message.cc` AdsbMessage::ToJson():
    let expected = serde_json::json!({
        "address": "a66ef1",
        "address_qualifier": "adsb_icao",
        "airground_state": "airborne",
        "east_velocity": 65,
        "ground_speed": 118,
        "nic": 9,
        "north_velocity": -99,
        "position": { "lat": 37.45338, "lon": -122.09643 },
        "pressure_altitude": 1000,
        "true_track": 146.7,
        "uplink_feedback": 0,
        "utc_coupled": true,
        "vertical_velocity_geometric": -192,
        "vv_src": "geometric",
        "payload_type": 0
    });
    assert_eq!(d.to_json(), expected);
}

#[test]
fn downlink_long_type1_matches_dump978_json() {
    // sample-data.txt N5130E long frame; payload_type 1 (HDR SV MS AUXSV).
    let payload = hex("08a66ef1353e2d525fd4050911882aa038101d06b85d440be2a4c2a0000590000000");
    let parity = fec::encode_downlink_long(&payload);
    let frame: Vec<u8> = payload.iter().chain(parity.iter()).copied().collect();
    let (msg, errors) = decode_frame(&frame).expect("decode");
    assert_eq!(errors, 0);
    let UatMessage::Downlink(d) = msg else { panic!("expected downlink") };

    // Expected from dump978 `uat_message.cc` AdsbMessage::ToJson():
    let expected = serde_json::json!({
        "address": "a66ef1",
        "address_qualifier": "adsb_icao",
        "airground_state": "airborne",
        "callsign": "N5130E",
        "capability_codes": { "es_in": true, "tcas_operational": false, "uat_in": true },
        "east_velocity": 84,
        "emergency": "none",
        "emitter_category": "A2",
        "geometric_altitude": 1200,
        "ground_speed": 128,
        "gva": 2,
        "mops_version": 2,
        "nac_p": 10,
        "nac_v": 2,
        "nic": 9,
        "nic_baro": 0,
        "nic_supplement": false,
        "north_velocity": -97,
        "operational_modes": { "atc_services": false, "ident_active": false, "tcas_ra_active": false },
        "position": { "lat": 37.43639, "lon": -122.08055 },
        "pressure_altitude": 975,
        "sda": 2,
        "sil": 3,
        "sil_supplement": "per_hour",
        "single_antenna": true,
        "transmit_mso": 56,
        "true_track": 139.1,
        "uplink_feedback": 0,
        "utc_coupled": true,
        "vertical_velocity_geometric": -128,
        "vv_src": "geometric",
        "payload_type": 1
    });
    assert_eq!(d.to_json(), expected);
}

// ---------------------------------------------------------------------------
// Uplink + FIS-B decode vs. dump978 legacy uat2text
// ---------------------------------------------------------------------------

/// A real uplink MDB (432-byte corrected payload) from sample-data.txt.
/// The dump978 `+` text format is the *corrected* MDB (parity stripped),
/// so it is decoded directly. Contains a NOTAM APDU followed by three
/// product-413 (Generic Textual, DLAC) Winds Aloft reports.
const UPLINK_HEX: &str = concat!(
    "3514c952d65ca0b0118000210dc6c082102cd3c4000611e808012cd3c4000000000000000f0900a9fd03583000318006",
    "7408605c93844e048b4e0cb5c30c306a080651c439c30c1c0f1cb0c30707c78c30c1c0f2d30c30703cf0c30c1c133d30",
    "c30820cf9c30c1c65e710cf2cf3704cf2cb1b70d60cf3d39b71d60cf4db1b72e20cf5db4d36830c78e75da0cf3d79db0",
    "79d03200067408605c93844e0081360cb5c30c306a080651c439c30c1c0f1cb0c30707c78c30c1c0f2d30c30703cf0c3",
    "0c1c133d30c30820cf9c30c1c65e710cf5c75af0ce0cf5cb9af0c20cf5d32b71d20cf4cf3b72da0cf6d38d34833db4e3",
    "5ce0cf5d71db179d3200067408605c93844e04120e0cb5c30c306a080651c439c30c1c0f1cb0c30707c78c30c1c0f2d3",
    "0c30703cf0c30c1c133d30c30820cf9c30c1c65e710c34c75af0d60c35c76af0c20c36cf4b71ce0c35d74b72e20c35df",
    "1d36830db2df5ce0c31cb7d7779d00000000000000000000000000000000000000000000000000000000000000000000",
    "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);

#[test]
fn uplink_site_and_framing_matches_dump978() {
    let mdb = hex(UPLINK_HEX);
    assert_eq!(mdb.len(), 432, "uplink MDB must be 432 bytes");
    let u = xng_mode_uat::UatUplink::decode(&mdb).expect("decode uplink");

    // dump978 uat2text:
    //   Site Latitude: +37.3227, Site Longitude: -121.7550 (possibly invalid)
    //   UTC coupled: yes, Slot ID: 0, TIS-B Site ID: 11
    assert!(!u.site.position_valid);
    assert!((u.site.lat - 37.3227).abs() < 0.001, "lat={}", u.site.lat);
    assert!((u.site.lon - (-121.7550)).abs() < 0.001, "lon={}", u.site.lon);
    assert!(u.utc_coupled);
    assert_eq!(u.slot_id, 0);
    assert_eq!(u.tisb_site_id, 11);
    assert!(u.app_data_valid);

    // First info frame: FIS-B APDU, product 8 (NOTAM), length 35.
    let f0 = &u.info_frames[0];
    assert_eq!(f0.frame_type, 0);
    assert_eq!(f0.length, 35);
    let p0 = f0.fisb.as_ref().expect("FIS-B");
    assert_eq!(p0.product_id, 8);
    assert_eq!(p0.product_name, "NOTAM (Including TFRs) and Service Status");
    // Product time 1/23 03:24.
    assert_eq!(p0.time.month, Some(1));
    assert_eq!(p0.time.day, Some(23));
    assert_eq!(p0.time.hours, 3);
    assert_eq!(p0.time.minutes, 24);
    assert!(p0.reports.is_empty()); // NOTAM is not DLAC-text in dump978.
}

#[test]
fn uplink_fisb_dlac_winds_text_matches_dump978() {
    let mdb = hex(UPLINK_HEX);
    let u = xng_mode_uat::UatUplink::decode(&mdb).expect("decode uplink");

    // Collect the product-413 (Generic Textual / DLAC) frames.
    let text_frames: Vec<_> = u
        .info_frames
        .iter()
        .filter_map(|f| f.fisb.as_ref())
        .filter(|p| p.product_id == 413)
        .collect();
    assert_eq!(text_frames.len(), 3, "expected 3 product-413 APDUs");

    // dump978 decodes these as Winds Aloft reports for RKS, BAM, PRC, all
    // at 250000Z, time 02:06; first report's body is the FT header line.
    let expected = [
        ("RKS", "3233    3221-05 3349-15 3461-28 356446 018956 335960"),
        ("BAM", "3515+03 3529+00 3542-14 3433-26 364844 364853 355161"),
        ("PRC", "0415+05 0516+00 0634-13 0554-28 057146 062753 012757"),
    ];

    for (frame, (loc, body)) in text_frames.iter().zip(expected.iter()) {
        // Product time: 02:06 (no month/day for this t_opt).
        assert_eq!(frame.time.hours, 2);
        assert_eq!(frame.time.minutes, 6);
        assert_eq!(frame.reports.len(), 1, "one report per APDU");
        let r = &frame.reports[0];
        assert_eq!(r.report_type.as_deref(), Some("WINDS"));
        assert_eq!(r.location.as_deref(), Some(*loc));
        assert_eq!(r.time.as_deref(), Some("250000Z"));
        // The body text contains the FT header then the data line.
        assert!(r.text.contains("FT"), "missing FT header: {:?}", r.text);
        assert!(r.text.contains(body), "missing data line {body:?} in {:?}", r.text);
    }
}

// ---------------------------------------------------------------------------
// IQ DEMOD end-to-end (self-generated modulate → demod path)
// ---------------------------------------------------------------------------
//
// These validate the CPFSK front-end + sync hunt + bit slicing in
// `UatChannelDecoder`. The IQ is SYNTHETIC: a KNOWN with-parity frame
// (whose decode is pinned above against dump978) is CPFSK-modulated by this
// crate's `modulate` module and demodulated by the channel decoder. The
// modulate→demod loop is self-generated; the DECODE core remains
// oracle-anchored by the dump978 vectors above. See PROVENANCE.md.

fn frame_short_type0() -> Vec<u8> {
    // The pinned short type-0 payload + its libfec parity.
    let payload = hex("00a66ef135445d525a0c0519119021204800");
    let parity = fec::encode_downlink_short(&payload);
    payload.iter().chain(parity.iter()).copied().collect()
}

fn frame_long_type1() -> Vec<u8> {
    // The pinned long type-1 payload (N5130E) + its libfec parity.
    let payload = hex("08a66ef1353e2d525fd4050911882aa038101d06b85d440be2a4c2a0000590000000");
    let parity = fec::encode_downlink_long(&payload);
    payload.iter().chain(parity.iter()).copied().collect()
}

/// Drive a synthetic burst (with quiet padding either side) through a fresh
/// decoder and return the recovered frames.
fn demod_synth(frame: &[u8], input_rate: f64) -> Vec<xng_mode_uat::UatFrame> {
    let chan = xng_mode_uat::CHANNEL_RATE;
    // At the channel rate, generate the burst at offset 0 (wideband UAT).
    let burst = modulate::burst_iq(frame, /*downlink=*/ true, input_rate, 0.0, 0.4);
    // Pad with quiet so the demod's DC tracker settles and the trailing
    // bits flush out of the integrate-and-dump.
    let pad = (input_rate / 1000.0) as usize; // ~1 ms
    let mut iq = vec![num_complex::Complex::new(0.0f32, 0.0); pad];
    iq.extend_from_slice(&burst);
    iq.extend(std::iter::repeat(num_complex::Complex::new(0.0f32, 0.0)).take(pad));
    let _ = chan;
    let mut dec = UatChannelDecoder::new(input_rate).unwrap();
    dec.process(&iq)
}

#[test]
fn demod_downlink_short_type0_synth_iq() {
    // Modulate at the native channel rate (DDC bypassed) — exercises the
    // discriminator, sync hunt, and bit slicing directly.
    let frame = frame_short_type0();
    let frames = demod_synth(&frame, xng_mode_uat::CHANNEL_RATE);
    let f = frames
        .iter()
        .find(|f| matches!(f.message, UatMessage::Downlink(_)))
        .expect("recovered a downlink frame from synthetic IQ");
    assert_eq!(f.wire_bytes, frame, "recovered the exact with-parity frame");
    let UatMessage::Downlink(d) = &f.message else { unreachable!() };
    // The recovered decode equals the dump978-pinned known-good values.
    assert_eq!(d.address, "a66ef1");
    assert_eq!(d.payload_type, 0);
    assert_eq!(d.ground_speed, Some(118));
    assert_eq!(d.true_track, Some(146.7));
    let pos = d.position.as_ref().expect("position");
    assert!((pos.lat - 37.45338).abs() < 1e-4, "lat {}", pos.lat);
    assert!((pos.lon - (-122.09643)).abs() < 1e-4, "lon {}", pos.lon);
    assert_eq!(f.kind(), "adsb");
}

#[test]
fn demod_downlink_long_type1_synth_iq() {
    let frame = frame_long_type1();
    let frames = demod_synth(&frame, xng_mode_uat::CHANNEL_RATE);
    let f = frames
        .iter()
        .find(|f| match &f.message {
            UatMessage::Downlink(d) => d.payload_type == 1,
            _ => false,
        })
        .expect("recovered the long type-1 frame from synthetic IQ");
    let UatMessage::Downlink(d) = &f.message else { unreachable!() };
    assert_eq!(d.callsign.as_deref(), Some("N5130E"));
    assert_eq!(d.address, "a66ef1");
    assert_eq!(d.emitter_category.as_deref(), Some("A2"));
    assert_eq!(d.ground_speed, Some(128));
    assert_eq!(f.kind(), "adsb");
}

#[test]
fn demod_downlink_short_noisy_synth_iq() {
    // Add deterministic additive noise so the validation is not a clean
    // loopback: the discriminator, DC tracker, and sync error tolerance
    // must still recover the known frame.
    let frame = frame_short_type0();
    let rate = xng_mode_uat::CHANNEL_RATE;
    let burst = modulate::burst_iq(&frame, true, rate, 0.0, 0.5);
    let pad = (rate / 1000.0) as usize;
    let mut iq = vec![num_complex::Complex::new(0.0f32, 0.0); pad];
    iq.extend_from_slice(&burst);
    iq.extend(std::iter::repeat(num_complex::Complex::new(0.0f32, 0.0)).take(pad));
    // Cheap deterministic PRNG (xorshift) → ~0.1 RMS complex noise.
    let mut st: u32 = 0x1234_5678;
    let mut rng = || {
        st ^= st << 13;
        st ^= st >> 17;
        st ^= st << 5;
        (st as f32 / u32::MAX as f32) - 0.5
    };
    for s in iq.iter_mut() {
        *s += num_complex::Complex::new(rng() * 0.2, rng() * 0.2);
    }
    let mut dec = UatChannelDecoder::new(rate).unwrap();
    let frames = dec.process(&iq);
    let f = frames
        .iter()
        .find(|f| matches!(f.message, UatMessage::Downlink(_)))
        .expect("recovered the downlink frame under additive noise");
    let UatMessage::Downlink(d) = &f.message else { unreachable!() };
    assert_eq!(d.address, "a66ef1");
    assert_eq!(d.ground_speed, Some(118));
    // Level estimate is a finite, reasonable dBFS value.
    assert!(dec.level_dbfs().is_finite());
}

#[test]
fn to_message_emits_uat_adsb_variant() {
    // The wideband interface contract: a recovered downlink frame maps to
    // MessageBody::Uat{kind:"adsb", details=<UatDownlink JSON>} with the
    // mode, crc_ok, raw wire bytes, and rssi set.
    use xng_types::{MessageBody, Mode};
    let frame = frame_short_type0();
    let frames = demod_synth(&frame, xng_mode_uat::CHANNEL_RATE);
    let f = frames
        .iter()
        .find(|f| matches!(f.message, UatMessage::Downlink(_)))
        .expect("downlink frame");
    let source = xng_types::Provenance {
        station: xng_types::StationIdentity::new("TEST"),
        app: xng_types::AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = xng_mode_uat::to_message(f, xng_mode_uat::UAT_FREQUENCY_HZ, source);
    assert_eq!(msg.mode, Mode::Uat);
    assert_eq!(msg.frequency_hz, 978_000_000);
    assert!(msg.decode.crc_ok);
    assert_eq!(msg.raw.as_deref(), Some(&frame[..]));
    assert!(msg.signal.rssi_db.is_some());
    match &msg.body {
        MessageBody::Uat { kind, details } => {
            assert_eq!(kind, "adsb");
            assert_eq!(details["address"], "a66ef1");
        }
        other => panic!("expected MessageBody::Uat, got {other:?}"),
    }
}

#[test]
fn demod_downlink_short_through_ddc_synth_iq() {
    // A non-integer capture rate exercises the DDC (decimate + resample)
    // ahead of the demod, the way a real SDR feeds the wideband decoder.
    let frame = frame_short_type0();
    let frames = demod_synth(&frame, 8_000_000.0);
    let f = frames
        .iter()
        .find(|f| matches!(f.message, UatMessage::Downlink(_)))
        .expect("recovered a downlink frame through the DDC");
    assert_eq!(f.wire_bytes, frame);
    let UatMessage::Downlink(d) = &f.message else { unreachable!() };
    assert_eq!(d.address, "a66ef1");
    assert_eq!(d.ground_speed, Some(118));
}
