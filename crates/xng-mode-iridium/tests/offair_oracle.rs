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

use xng_mode_iridium::{decode_bits, decode_da_bits, frame, lcw_traffic_frame};

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

#[test]
fn offair_ibc_matches_toolkit() {
    // iridium-parser.py: IBC bc:0 sat:013 cell:15 slot:0 sv_blkn:0
    // aq_cl:1111111111111111 aq_sb:20 aq_ch:2 ... max_uplink_pwr + assignment
    const RAW: &str = "0011000000110000111100110000000000111110010011111101110011011100000110111111011110111011011100111101101101010011100011101110010000001010101110111000001000000011001011100100101011011011100011101100111110100010111100000110111100101110011000101101101110001110110011101110001011110010111011";
    let f = decode_bits(&canonical(RAW)).expect("IBC decodes");
    assert_eq!(f.kind, "broadcast");
    let d = &f.details;
    assert_eq!(d["bc_type"], 0);
    assert_eq!(d["sat"], 13);
    assert_eq!(d["beam"], 15);
    assert_eq!(d["slot"], 0);
    assert_eq!(d["sv_blocking"], 0);
    assert_eq!(d["acq_classes"], 65535); // 1111111111111111
    assert_eq!(d["acq_sub_band"], 20);
    assert_eq!(d["acq_channels"], 2);
    assert_eq!(d["info_type"], 0);
    assert_eq!(d["max_uplink_pwr"], 20);
    // Sub-block-1 "unknown" bits the toolkit also surfaces (iridium-parser.py
    // prints the lone `0` after `cell:15` and the trailing `00`). Both are 0
    // for this burst.
    assert_eq!(d["unknown01"], 0);
    assert_eq!(d["unknown02"], 0);
    // Exactly four 42-bit blocks (descriptor + info + 2 assignments) -> no
    // {LONG}/{SHORT} block-count anomaly.
    assert!(d.get("block_trailer").is_none());
    // Channel-assignment block(s) decoded. iridium-parser.py:
    //   [111 Rid:153 ts:4 ul_sb:31 dl_sb:22 access:6 dtoa:212 dfoa:17 10] (x2)
    assert_eq!(d["assignments"].as_array().unwrap().len(), 2);
    let a = &d["assignments"][0];
    assert_eq!(a["random_id"], 153);
    assert_eq!(a["timeslot"], 4);
    assert_eq!(a["uplink_sub_band"], 31);
    assert_eq!(a["downlink_sub_band"], 22);
    assert_eq!(a["access"], 6);
    assert_eq!(a["dtoa"], 212);
    assert_eq!(a["dfoa"], 17);
}

#[test]
fn offair_ibc_tmsi_expiry_time() {
    // iridium-parser.py: IBC ... tmsi_expiry:2014-05-11T15:13:0x
    const RAW: &str = "001100000011000011110011000000000011111001001111110111001101110000011011111101111011101101110000110101110011100110010000000000000000000000000000000110000000000110000011001110100100100110001010110011101100000111110010111011011000001100111010010010011000101011001110110000011111001011101100";
    let f = decode_bits(&canonical(RAW)).expect("IBC decodes");
    assert_eq!(f.kind, "broadcast");
    let d = &f.details;
    assert_eq!(d["info_type"], 2);
    // Oracle: iridium-toolkit extracts tmsi_expiry counter == 32768 from this
    // burst (its bit layout, BCH-corrected). That raw counter is the stable,
    // re-epoch-independent fact this off-air vector pins.
    assert_eq!(d["tmsi_expiry"], 32768);
    // The `_unix` value applies the *currently active* Iridium era (IRID-8:
    // the counter restarts at each re-epoch, so the era is chosen from the
    // receive-time clock, not the counter). Decoded live in the ERA2 window it
    // reproduces toolkit `fmt_iritime(32768)` == 1399821184.12 exactly; under a
    // later era the same counter maps onto that era's base. Assert it equals
    // base + 32768*0.09 for whichever era is in force, never the broken
    // counter-only interpretation.
    let ux = d["tmsi_expiry_unix"].as_f64().unwrap();
    let offset = 32768.0 * 0.09; // 2949.12 s
    let era_bases = [1_399_821_184.12f64, 1_739_491_200.0 + offset, 1_768_414_080.0 + offset];
    assert!(
        era_bases.iter().any(|&b| (ux - b).abs() < 1.0),
        "tmsi_expiry_unix={ux} matched no known Iridium era base + {offset}"
    );
}

#[test]
fn offair_u3_lcw_handoff() {
    // iridium-parser.py: IU3: LCW(3,T:hndof,C:handoff_cand,...)
    const RAW: &str = "001100000011000011110011001100011000000101100001110100010111110100010000101100110110000000000101000000010000011011001100100000100100010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
    let f = lcw_traffic_frame(&canonical(RAW)).expect("U3 LCW frame decodes");
    assert_eq!(f.kind, "u3");
    assert_eq!(f.details["frame_ft"], 3);
    assert_eq!(f.details["lcw"]["type"], "hndof");
    // iridium-parser.py: LCW(3,T:hndof,C:handoff_cand,34c,120,...). The
    // handoff candidate carries an 11-bit + a 10-bit lcw3 field that the
    // toolkit prints as 0x34c / 0x120; surface both rather than dropping them.
    let code = &f.details["lcw"]["code"];
    assert_eq!(code["code"], "handoff_cand");
    assert_eq!(code["cand_a"], 0x34c); // 844
    assert_eq!(code["cand_b"], 0x120); // 288
    // iridium-parser.py decoded this one as IU3 (RS did not correct).
    assert_eq!(f.details["u3_type"], "IU3");
}
