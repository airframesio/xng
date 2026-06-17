//! Iridium ring-alert loopback: build an IRA frame bit-exactly per the
//! iridium-toolkit layout, modulate as a DQPSK burst, and decode through
//! the full chain (burst hunt, tone CFO, coherent UW fit, DQPSK,
//! deinterleave, BCH, field parse).

use num_complex::Complex;
use xng_mode_iridium::{decode_bits, frame, modulate, IridiumChannelDecoder, CHANNEL_RATE};

fn push_field(bits: &mut Vec<u8>, v: u32, n: usize) {
    for k in (0..n).rev() {
        bits.push(((v >> k) & 1) as u8);
    }
}

/// Build the payload data bits of a ring alert (63-bit header + pages).
fn ira_payload(sat: u32, beam: u32, x: i32, y: i32, z: i32, tmsis: &[u32]) -> Vec<u8> {
    let mut d = Vec::new();
    push_field(&mut d, sat, 7);
    push_field(&mut d, beam, 6);
    for v in [x, y, z] {
        let sign = if v < 0 { 1 } else { 0 };
        let mag = if v < 0 { v + (1 << 11) } else { v } as u32;
        push_field(&mut d, sign, 1);
        push_field(&mut d, mag, 11);
    }
    push_field(&mut d, 17, 7); // ra_interval
    push_field(&mut d, 1, 1); // timeslot
    push_field(&mut d, 0, 1); // epi
    push_field(&mut d, 9, 5); // bc sub-band
    for &tmsi in tmsis {
        push_field(&mut d, tmsi, 32);
        push_field(&mut d, 0, 2);
        push_field(&mut d, 14, 5); // msc
        push_field(&mut d, 0, 3);
    }
    d.extend(std::iter::repeat(1).take(42)); // END page
    d
}

/// Encode payload data bits into the transmitted bit stream:
/// 21-bit blocks → BCH(31,21)+parity → interleave (3-way then 2-way) →
/// access code prefix.
fn ira_bits(payload: &[u8]) -> Vec<u8> {
    let mut padded = payload.to_vec();
    while padded.len() % 21 != 0 {
        padded.push(0);
    }
    // Need an even number of blocks beyond the first three for the 2-way
    // interleave; pad with zero blocks.
    let mut nblk = padded.len() / 21;
    while (nblk - 3) % 2 != 0 {
        padded.extend(std::iter::repeat(0).take(21));
        nblk += 1;
    }
    let blocks: Vec<Vec<u8>> = padded
        .chunks_exact(21)
        .map(|d| frame::bch_encode(frame::RINGALERT_BCH_POLY, d))
        .collect();
    let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
    bits.extend(frame::interleave3(&blocks[0], &blocks[1], &blocks[2]));
    for pair in blocks[3..].chunks_exact(2) {
        bits.extend(frame::interleave2(&pair[0], &pair[1]));
    }
    bits
}

#[test]
fn ira_bits_decode() {
    let payload = ira_payload(42, 13, -1200, 800, 1500, &[0xDEADBEEF]);
    let bits = ira_bits(&payload);
    let f = decode_bits(&bits).expect("frame decodes");
    assert_eq!(f.kind, "ring-alert");
    assert_eq!(f.details["sat"], 42);
    assert_eq!(f.details["beam"], 13);
    assert_eq!(f.details["ra_interval"], 17);
    assert_eq!(f.details["bc_sub_band"], 9);
    assert_eq!(f.details["pages"][0]["tmsi"], "deadbeef");
    assert_eq!(f.details["pages"][0]["msc_id"], 14);
    assert_eq!(f.details["pages_complete"], true);
    // Geometry: alt = 4*r km.
    let lat = f.details["lat"].as_f64().unwrap();
    assert!((lat - (1500.0f64).atan2((1200.0f64*1200.0+800.0*800.0).sqrt()).to_degrees()).abs() < 0.01);
}

#[test]
fn ira_rf_loopback() {
    let payload = ira_payload(99, 7, 500, -900, 1300, &[0x12345678, 0x0BADCAFE]);
    let bits = ira_bits(&payload);
    let mut iq = modulate::modulate(&bits, 64, CHANNEL_RATE, 1_500.0, 0.5);
    // Quiet padding before/after.
    let mut sig = vec![Complex::new(0.0f32, 0.0); 4000];
    sig.extend(iq.drain(..));
    sig.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((CHANNEL_RATE * 0.12) as usize));
    let mut noise = 0x1234_5678_9abc_def0u64;
    for s in &mut sig {
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        let n1 = (noise as f32 / u64::MAX as f32) - 0.5;
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        let n2 = (noise as f32 / u64::MAX as f32) - 0.5;
        *s += Complex::new(n1 * 0.02, n2 * 0.02);
    }

    let mut dec = IridiumChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in sig.chunks(65_536) {
        frames.extend(dec.process(chunk));
    }
    let f = frames.first().expect("burst decodes");
    assert_eq!(f.kind, "ring-alert");
    assert_eq!(f.details["sat"], 99);
    assert_eq!(f.details["beam"], 7);
    assert_eq!(f.details["pages"][0]["tmsi"], "12345678");
    assert_eq!(f.details["pages"][1]["tmsi"], "0badcafe");
}

#[test]
fn oracle_validated_vector() {
    // This exact bit stream was decoded by iridium-toolkit's bitsparser
    // (the reference implementation) as:
    //   IRA: sat:075 beam:21 xyz=(-1411,+0263,+1602) pos=(+48.14/+169.44)
    //        alt=2248 RAI:33 ?01 bc_sb:22
    //        P02: PAGE(tmsi:cafed00d msc_id:07) PAGE(tmsi:00c0ffee msc_id:07) {OK}
    let bitstr = "001100000011000011110011111010100110001000001000100110000011110111011000101110101011000001010001001001100010001010011101000110011011111011000010001011111001001101110111100100010000001100000010010110110010000100111111100000000000111110001100110011001111111111111111111111111111111111111111111111111111111111111111";
    let bits: Vec<u8> = bitstr.bytes().map(|b| b - b'0').collect();
    let f = decode_bits(&bits).expect("decodes");
    assert_eq!(f.kind, "ring-alert");
    assert_eq!(f.details["sat"], 75);
    assert_eq!(f.details["beam"], 21);
    assert_eq!(f.details["ra_interval"], 33);
    assert_eq!(f.details["timeslot"], 0);
    assert_eq!(f.details["epi"], 1);
    assert_eq!(f.details["bc_sub_band"], 22);
    assert!((f.details["lat"].as_f64().unwrap() - 48.14).abs() < 0.01);
    assert!((f.details["lon"].as_f64().unwrap() - 169.44).abs() < 0.01);
    assert!((f.details["alt_km"].as_f64().unwrap() - 2248.7).abs() < 0.5);
    assert_eq!(f.details["pages"][0]["tmsi"], "cafed00d");
    assert_eq!(f.details["pages"][1]["tmsi"], "00c0ffee");
    assert_eq!(f.details["pages"][0]["msc_id"], 7);
}

/// Build an IMS pager burst: ACCESS + messaging header + 2-way
/// interleaved BCH(31,21) blocks (messaging polynomial).
fn ims_bits(blocks21: &[Vec<u8>]) -> Vec<u8> {
    let enc: Vec<Vec<u8>> = blocks21
        .iter()
        .map(|d| frame::bch_encode(frame::MESSAGING_BCH_POLY, d))
        .collect();
    let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
    bits.extend(frame::HEADER_MESSAGING.iter().copied());
    for pair in enc.chunks_exact(2) {
        bits.extend(frame::interleave2(&pair[0], &pair[1]));
    }
    bits
}

/// 21-bit pager blocks for a single-part ASCII page (mirrors the ms.rs
/// unit-test builder).
fn pager_blocks(ric: u32, text: &str) -> Vec<Vec<u8>> {
    fn push_int(v: &mut Vec<u8>, val: u32, n: usize) {
        for k in (0..n).rev() {
            v.push(((val >> k) & 1) as u8);
        }
    }
    let mut rest: Vec<u8> = Vec::new();
    for k in 0..22 {
        rest.push(((ric >> k) & 1) as u8);
    }
    push_int(&mut rest, 5, 5);
    push_int(&mut rest, 7, 6);
    push_int(&mut rest, 0, 4);
    push_int(&mut rest, 0, 6);
    push_int(&mut rest, 0, 4);
    rest.push(0);
    rest.push(0);
    push_int(&mut rest, 0, 7);
    for c in text.bytes() {
        push_int(&mut rest, c as u32, 7);
    }
    push_int(&mut rest, 3, 7);
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for chunk in rest.chunks(20) {
        let mut b = vec![0u8];
        b.extend_from_slice(chunk);
        b.resize(21, 0);
        blocks.push(b);
    }
    let total_halves = 1 + blocks.len();
    let bch_blocks = (total_halves + 1) / 2;
    let mut h = Vec::new();
    h.push(0);
    push_int(&mut h, 0, 4);
    push_int(&mut h, 3, 4);
    push_int(&mut h, 9, 6);
    push_int(&mut h, bch_blocks as u32, 4);
    push_int(&mut h, 1, 2);
    let mut out = vec![h];
    out.extend(blocks);
    if out.len() % 2 == 1 {
        out.push(vec![1u8; 21]);
    }
    out
}

#[test]
fn ims_pager_bits_decode() {
    let bits = ims_bits(&pager_blocks(1234567, "CALL OPS +14155550100"));
    let f = xng_mode_iridium::decode_bits(&bits).expect("frame");
    assert_eq!(f.kind, "msg");
    assert_eq!(f.details.pointer("/body/ric").and_then(|v| v.as_u64()), Some(1234567));
    assert_eq!(
        f.details.pointer("/body/text").and_then(|v| v.as_str()),
        Some("CALL OPS +14155550100")
    );
}

/// Oracle-validated IMS vector: iridium-toolkit bitsparser.py parses
/// these exact bits as IridiumMessagingAscii with
/// `3:1:09 len:06 ric:1234567 fmt:05 seq:07 TXT: CALL OPS +14155550100`
/// (run 2026-06-10 with the vendored toolkit harness). Our decode must
/// agree field-for-field.
#[test]
fn oracle_validated_ims_vector() {
    const BITS: &str = "00110000001100001111001100110011111100110011001111110011111001111011000111110011010001111001100011100101001000001100100011100111001000011101000000001000000000110010000000100010000000101101001111001100110110001101110010110100001100000100000101111000101000100100010101100000010001001001100100101101010110001000101010000110101110111101100001101011100110011101011001100100101110000100001011110000010000000011000000000001011010100000001100000000";
    let bits: Vec<u8> = BITS.bytes().map(|b| b - b'0').collect();
    let f = xng_mode_iridium::decode_bits(&bits).expect("frame");
    assert_eq!(f.kind, "msg");
    let d = &f.details;
    assert_eq!(d.pointer("/block").and_then(|v| v.as_u64()), Some(3));
    assert_eq!(d.pointer("/group").and_then(|v| v.as_str()), Some("1"));
    assert_eq!(d.pointer("/frame").and_then(|v| v.as_u64()), Some(9));
    assert_eq!(d.pointer("/body/ric").and_then(|v| v.as_u64()), Some(1234567));
    assert_eq!(d.pointer("/body/format").and_then(|v| v.as_u64()), Some(5));
    assert_eq!(d.pointer("/body/seq").and_then(|v| v.as_u64()), Some(7));
    assert_eq!(
        d.pointer("/body/text").and_then(|v| v.as_str()),
        Some("CALL OPS +14155550100")
    );
}

#[test]
fn rejects_all_zero_ring_alert() {
    // A degenerate all-zero header (an idle/noisy burst whose blocks
    // BCH-correct to the trivially-valid zero codeword) must NOT emit a
    // bogus ring alert at sat 0 / position (0,0,0).
    let zero = vec![0u8; 96];
    assert!(xng_mode_iridium::ira::parse_ra(&zero, 0, &[]).is_none());
}

/// IRID-5 end-to-end: a ring-alert burst whose first BCH header block carries
/// a weight-3 error fails hard-decision decode (the hard t=2 BCH truncates the
/// frame at block 0, dropping the satellite/position fields) but is recovered
/// in full by the soft-decision (Chase-2) path when the three error positions
/// are the least-reliable bits — the AWGN-typical near-threshold case.
///
/// Oracle-grounded: the codeword is produced by the published BCH(31,21)
/// generator (poly 1207), and the soft path is verified to reproduce the
/// exact sat/beam/position fields a clean decode yields — not a loopback of
/// the soft decoder against itself.
#[test]
fn soft_decode_recovers_weight3_ring_alert() {
    let payload = ira_payload(73, 21, -1100, 640, 1480, &[0xCAFEF00D]);
    let bits = ira_bits(&payload);

    // The clean decode is the oracle for the field values.
    let clean = decode_bits(&bits).expect("clean RA decodes");
    assert_eq!(clean.kind, "ring-alert");

    // Locate the transmitted positions of header block 0 (the 3-way
    // interleaved triple) via a tag stream, then inject a weight-3 error there.
    let tag = frame::interleave3(&vec![1u8; 32], &vec![0u8; 32], &vec![0u8; 32]);
    // tag is the 96-bit header region; offset by the 24-bit access code.
    let block0_positions: Vec<usize> = tag
        .iter()
        .enumerate()
        .filter(|(_, &v)| v == 1)
        .map(|(i, _)| 24 + i)
        .collect();
    let err_positions = [
        block0_positions[2],
        block0_positions[11],
        block0_positions[19],
    ];

    let mut corrupted = bits.clone();
    // Strong reliability everywhere; the 3 error bits are the weakest.
    let mut rel = vec![6.0f32; bits.len()];
    for &p in &err_positions {
        corrupted[p] ^= 1;
        rel[p] = 0.12;
    }

    // Hard path: block 0 has 3 errors → either no frame, or a frame missing
    // the (truncated) satellite/position fields. It must NOT reproduce them.
    let hard = decode_bits(&corrupted);
    let hard_ok = hard
        .as_ref()
        .map(|f| f.kind == "ring-alert" && f.details["sat"] == 73 && f.details["beam"] == 21)
        .unwrap_or(false);
    assert!(!hard_ok, "hard decode must not recover the weight-3 header block");

    // Soft path: Chase-2 recovers block 0; the full frame matches the oracle.
    let soft = xng_mode_iridium::decode_bits_soft(&corrupted, Some(&rel))
        .expect("soft decode recovers the frame");
    assert_eq!(soft.kind, "ring-alert");
    assert_eq!(soft.details["sat"], 73);
    assert_eq!(soft.details["beam"], 21);
    assert_eq!(soft.details["ra_interval"], clean.details["ra_interval"]);
    assert_eq!(soft.details["pages"][0]["tmsi"], "cafef00d");
    assert_eq!(soft.details["lat"], clean.details["lat"]);
    assert_eq!(soft.details["lon"], clean.details["lon"]);
}

/// IRID-5 UW pre-classify end-to-end: a burst whose differential access code
/// carries 3 bit errors (so the strict prefix-match fails) still decodes on
/// the soft path, because `frame::correct_access` snaps the access field to its
/// exact downlink word before classification. The hard path drops it.
#[test]
fn soft_decode_recovers_corrupted_access_code() {
    let payload = ira_payload(55, 9, 900, -700, 1600, &[0x1357ACE0]);
    let bits = ira_bits(&payload);

    let mut corrupted = bits.clone();
    // Corrupt 3 of the 24 access-code bits (within correct_access's 5-bit reach).
    for &p in &[1usize, 12, 22] {
        corrupted[p] ^= 1;
    }
    // Strong reliabilities (the BCH header itself is intact here).
    let rel = vec![6.0f32; bits.len()];

    // Hard path: the access prefix no longer matches → dropped.
    assert!(
        decode_bits(&corrupted).is_none(),
        "hard decode must reject a non-matching access prefix"
    );

    // Soft path: access code is UW-corrected, frame decodes with right fields.
    let soft = xng_mode_iridium::decode_bits_soft(&corrupted, Some(&rel))
        .expect("soft decode recovers via UW correction");
    assert_eq!(soft.kind, "ring-alert");
    assert_eq!(soft.details["sat"], 55);
    assert_eq!(soft.details["beam"], 9);
}
