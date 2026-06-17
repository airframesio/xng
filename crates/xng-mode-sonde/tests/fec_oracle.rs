//! FEC-layer tests anchored to the rs1729/RS `rs41.txt` worked example.
//!
//! These are external-oracle tests: the frames, the error positions, and
//! the corrected bytes all come from the published RS41 worked example in
//! rs1729/RS (`rs41/rs41.txt`), NOT from an encode->decode loopback.
//!
//! - FRAME1: a clean 320-byte standard frame (rs41.txt §"Beispiele" (1)).
//!   rs1729's RS decoder reports `codeword1 errors: 0`, `codeword2: 0`.
//! - FRAME2_ERR: a 518-byte extended frame as received, with two byte
//!   errors (rs41.txt §"[RS-DECODER]" input). rs1729 reports
//!   `codeword1 errors: 0`, `codeword2 errors: 2, pos: 234 252`.
//! - FRAME2_FIXED: the same frame after rs1729's RS correction
//!   (rs41.txt "frame:" line) — our corrected output must match it byte
//!   for byte.

use xng_mode_sonde::crc::crc16;
use xng_mode_sonde::rs::Rs41Rs;
use xng_mode_sonde::whitening::HEADER;

/// rs41.txt example (1): a clean standard 320-byte frame, serial K1930293.
const FRAME1_HEX: &str = "8635f44093df1a602c87e0fa0521e8943d9cef4c7a67393f6d39fb546461f2111b6447ab79a746c80350cda5344157f8c0c12234f46902220f792816174b313933303239331a00000300000a00002f0007322ce53e31991abf12dada3eb68468c16755d51c7a2a15310216060245f302000d08a31607821e08bb210219060243f302000000000000000000000000000000220d7c1e0807d03cdc071fd81ddb19d70a8d0eb602b60cb518d40692ff00ff00ff001c277d59b8d83301ff0f881f0f38f4fe18b283038735ff000000003eb8ff4947201e6e3aff55415f13fc6e005440440cf100009e9f7406f85800832b631719d70010bebc172a8b00000000000000000000000000000000000000000000a48b7b15366181193ef05d07e1245b1be0f721f801f60804107b0b76110000000000000000000000000000000000ecc7";

/// rs41.txt [RS-DECODER] input: 518-byte extended frame as received, with
/// two byte errors in codeword 2's parity region (serial K4020244).
const FRAME2_ERR_HEX: &str = "8635f44093df1a608f9b1025bf8ec9e28ad68413c31788307e9881c5cb2f37f754fa09b711c5c39977ed8fbf22377b3e5e1cee59fc644b19f0792896134b343032303234341c00000100000c00007a0007320f00000000008920bac20000000000000092697a2ae9030226fd015de502363208522a075f330874040228fd015de502000000000000000000000000000000e7917c1e4d0750f1921703fb01f8068d1fd811f70bd604d50afa17f913d90c8b20f9a16a7d5921103501ff440000006c1f00cd977e059ab7009566fd191d1affd82fbf143fb8ff5277180991faff9ca1d10d441b01927bf211dd190190999f0553a1ff9120b10c3847ff06eeee0e571301a2c0891c000000cddd1a0882d10011167b153c154217941930005fc50b1eb9fde107d2050902115a537ea6ed343030313030303120313037393020202033312e37203036373520303334392030373030203132383636203630303520313339333120363031342031343038322035383830203738313420383032372031303039203930392039353631353632203935303839323220343238383339313633382032393335383636203539343238203335323439203636393920333738332034363837203637303120363930312037393939049a762d000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f35a";

/// rs41.txt [RS-DECODER] corrected output ("frame:" line) for FRAME2_ERR.
const FRAME2_FIXED_HEX: &str = "8635f44093df1a608f9b1025bf8ec9e28ad68413c31788307e9881c5cb2f37f754fa49b711c5c39977ed8fbf22377b3e5e1cee59bc644b19f0792896134b343032303234341c00000100000c00007a0007320f00000000008920bac20000000000000092697a2ae9030226fd015de502363208522a075f330874040228fd015de502000000000000000000000000000000e7917c1e4d0750f1921703fb01f8068d1fd811f70bd604d50afa17f913d90c8b20f9a16a7d5921103501ff440000006c1f00cd977e059ab7009566fd191d1affd82fbf143fb8ff5277180991faff9ca1d10d441b01927bf211dd190190999f0553a1ff9120b10c3847ff06eeee0e571301a2c0891c000000cddd1a0882d10011167b153c154217941930005fc50b1eb9fde107d2050902115a537ea6ed343030313030303120313037393020202033312e37203036373520303334392030373030203132383636203630303520313339333120363031342031343038322035383830203738313420383032372031303039203930392039353631353632203935303839323220343238383339313633382032393335383636203539343238203335323439203636393920333738332034363837203637303120363930312037393939049a762d000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f35a";

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn frame_header_is_rs41_sync() {
    let f1 = hex(FRAME1_HEX);
    assert_eq!(&f1[..8], &HEADER);
}

#[test]
fn clean_frame_decodes_with_zero_errors() {
    let mut f1 = hex(FRAME1_HEX);
    let rs = Rs41Rs::new();
    let r = rs.correct_frame(&mut f1);
    // rs1729: codeword1 errors: 0, codeword2 errors: 0
    assert_eq!(r.errors1, Some(0), "cw1 should be clean");
    assert_eq!(r.errors2, Some(0), "cw2 should be clean");
    assert!(r.ok());
    // A clean frame must be unchanged by correction.
    assert_eq!(f1, hex(FRAME1_HEX));
}

#[test]
fn corrects_two_errors_to_oracle_frame() {
    let mut f2 = hex(FRAME2_ERR_HEX);
    let rs = Rs41Rs::new();
    let r = rs.correct_frame(&mut f2);
    // rs1729: codeword1 errors: 0, codeword2 errors: 2 (pos 234 252).
    assert_eq!(r.errors1, Some(0), "cw1 should be clean");
    assert_eq!(r.errors2, Some(2), "cw2 should correct exactly 2 errors");
    assert_eq!(r.total_corrected(), 2);
    // The corrected frame must equal rs1729's corrected output, byte for byte.
    assert_eq!(f2, hex(FRAME2_FIXED_HEX), "RS output must match oracle");
}

#[test]
fn subblock_crcs_pass_on_oracle_frame() {
    // rs41.txt §"CRC" lists every sub-block of FRAME1 as CRC-OK. Verify our
    // CRC + sub-block layout reproduces that.
    // Packet IDs and offsets per rs41.txt / rs41mod.c (STATUS, PTU, GPS1,
    // GPS2, GPS3).
    let f = hex(FRAME1_HEX);
    for &(pos, pck, name) in &[
        (0x039usize, 0x79u8, "STATUS"),
        (0x065, 0x7A, "PTU"),
        (0x093, 0x7C, "GPS-INFO"),
        (0x0B5, 0x7D, "GPS2"),
        (0x112, 0x7B, "GPS-POS"),
    ] {
        assert_eq!(f[pos], pck, "{name} packet id mismatch at {pos:#x}");
        let len = f[pos + 1] as usize;
        let body = &f[pos + 2..pos + 2 + len];
        let stored = u16::from_le_bytes([f[pos + 2 + len], f[pos + 2 + len + 1]]);
        assert_eq!(crc16(body), stored, "{name} CRC mismatch");
    }
}
