//! Real off-air regression oracles. These two bursts were captured live
//! at KSMF (Airspy Mini, 1625 MHz center) and decoded by iridium-toolkit's
//! iridium-parser.py as the reference:
//!
//!   IRA: sat:044 beam:25 xyz=(-0738,-0969,+1026) pos=(+40.11/-127.29)
//!        alt=015 RAI:48 ?10 bc_sb:23
//!   IDA: DL LCW(2,T:hndof,C:handoff_cand,25d,3e0,…) — ft=2 SBD carrier
//!
//! They are stored in gr-iridium "RAW" symbol order (what the demod
//! produced before `symbol_reverse`); the live demod now emits the
//! canonical (reversed) order, so the test applies `symbol_reverse` to
//! mirror live output. This guards the bit-order convention that lets the
//! BCH-coded RA / IDA frames decode at all (regression for the fix where
//! only the reverse-invariant ITL/IMS-header frames decoded).

use xng_mode_iridium::{decode_bits, decode_da_bits, frame};

fn canonical(raw: &str) -> Vec<u8> {
    frame::symbol_reverse(&raw.bytes().map(|c| (c == b'1') as u8).collect::<Vec<_>>())
}

#[test]
fn offair_ring_alert_matches_toolkit() {
    const RAW: &str = "0011000000110000111100111111100001001010010011010011101101101100001001101011100001110011001100110000000111100010010011010011101011110101110100010010011010000111000101000111100110001000111111111111111111111111111111111111111111111111111111111111111110010111";
    let f = decode_bits(&canonical(RAW)).expect("decodes");
    assert_eq!(f.kind, "ring-alert");
    let d = &f.details;
    assert_eq!(d["sat"], 44);
    assert_eq!(d["beam"], 25);
    assert_eq!(d["ra_interval"], 48);
    assert_eq!(d["bc_sub_band"], 23);
    assert!((d["lat"].as_f64().unwrap() - 40.11).abs() < 0.01);
    assert!((d["lon"].as_f64().unwrap() - (-127.29)).abs() < 0.01);
    assert_eq!(d["pages"][0]["tmsi"], "071ca54a");
}

#[test]
fn offair_ida_sbd_decodes_with_crc() {
    const RAW: &str = "0011000000110000111100110011000110001101111001011111011101001001100100101110100110001000000101000000000101001100010000001100000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000110000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010001000010010001100000000000000110010001000000010001000110000";
    let (da, _) = decode_da_bits(&canonical(RAW)).expect("ft=2 IDA decodes");
    assert!(da.crc_ok, "DA CRC must validate");
    assert_eq!(da.ctr, 0);
    assert!(!da.continuation);
}
