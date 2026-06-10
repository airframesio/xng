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
