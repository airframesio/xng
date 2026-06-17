//! Frame / sub-block decode tests against the rs1729/RS `rs41.txt` worked
//! example sample frames.
//!
//! The two sample frames are real RS41 frames published in rs1729/RS
//! (`rs41/rs41.txt`). The asserted field values (serial, frame number,
//! battery, GPS week/TOW, ECEF->lat/lon/alt, raw PTU channels) are derived
//! directly from the published frame bytes via the documented sub-block
//! offsets and the ECEF formula — pinning the decode of real frames, not an
//! encode/decode loopback.

use xng_mode_sonde::{decode_dewhitened, decode_on_air, ecef_to_geodetic};

/// rs41.txt example (1): clean 320-byte standard frame, sonde K1930293.
const FRAME1_HEX: &str = "8635f44093df1a602c87e0fa0521e8943d9cef4c7a67393f6d39fb546461f2111b6447ab79a746c80350cda5344157f8c0c12234f46902220f792816174b313933303239331a00000300000a00002f0007322ce53e31991abf12dada3eb68468c16755d51c7a2a15310216060245f302000d08a31607821e08bb210219060243f302000000000000000000000000000000220d7c1e0807d03cdc071fd81ddb19d70a8d0eb602b60cb518d40692ff00ff00ff001c277d59b8d83301ff0f881f0f38f4fe18b283038735ff000000003eb8ff4947201e6e3aff55415f13fc6e005440440cf100009e9f7406f85800832b631719d70010bebc172a8b00000000000000000000000000000000000000000000a48b7b15366181193ef05d07e1245b1be0f721f801f60804107b0b76110000000000000000000000000000000000ecc7";

/// rs41.txt [RS-DECODER] input: 518-byte extended frame with two byte
/// errors, sonde K4020244.
const FRAME2_ERR_HEX: &str = "8635f44093df1a608f9b1025bf8ec9e28ad68413c31788307e9881c5cb2f37f754fa09b711c5c39977ed8fbf22377b3e5e1cee59fc644b19f0792896134b343032303234341c00000100000c00007a0007320f00000000008920bac20000000000000092697a2ae9030226fd015de502363208522a075f330874040228fd015de502000000000000000000000000000000e7917c1e4d0750f1921703fb01f8068d1fd811f70bd604d50afa17f913d90c8b20f9a16a7d5921103501ff440000006c1f00cd977e059ab7009566fd191d1affd82fbf143fb8ff5277180991faff9ca1d10d441b01927bf211dd190190999f0553a1ff9120b10c3847ff06eeee0e571301a2c0891c000000cddd1a0882d10011167b153c154217941930005fc50b1eb9fde107d2050902115a537ea6ed343030313030303120313037393020202033312e37203036373520303334392030373030203132383636203630303520313339333120363031342031343038322035383830203738313420383032372031303039203930392039353631353632203935303839323220343238383339313633382032393335383636203539343238203335323439203636393920333738332034363837203637303120363930312037393939049a762d000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f35a";

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn close(a: f64, b: f64, eps: f64) {
    assert!((a - b).abs() <= eps, "{a} not within {eps} of {b}");
}

#[test]
fn decodes_clean_standard_frame() {
    let d = decode_dewhitened(&hex(FRAME1_HEX)).expect("decode");
    assert!(d.rs.ok());
    assert_eq!(d.rs.total_corrected(), 0);

    let f = &d.frame;
    // STATUS sub-block.
    assert_eq!(f.serial, "K1930293");
    assert_eq!(f.frame_num, 5910);
    close(f.battery_v as f64, 2.6, 1e-6);
    assert!(f.crc.status);

    // GPS-INFO sub-block: GPS week 1800 (rs41.txt: "W 1800"), TOW in ms.
    let t = f.gps_time.expect("gps time");
    assert_eq!(t.week, 1800);
    assert_eq!(t.tow_ms, 131_874_000);

    // GPS-POS sub-block: ECEF -> lat/lon/alt near Zagreb, 8 satellites.
    let p = f.gps_pos.as_ref().expect("gps pos");
    close(p.lat, 46.050_263, 1e-5);
    close(p.lon, 16.110_771, 1e-5);
    close(p.alt_m, 28_410.02, 0.1);
    assert_eq!(p.num_sv, 8);

    // PTU sub-block: 12 raw 24-bit channels + this frame's cal sub-frame.
    let ptu = f.ptu.as_ref().expect("ptu");
    assert_eq!(
        ptu.raw,
        [143637, 132630, 193349, 527616, 464547, 532098, 139707, 132633, 193347, 0, 0, 0]
    );
    assert_eq!(ptu.cal_index, 44);
}

#[test]
fn decodes_extended_frame_after_rs_correction() {
    // Decoding the errored 518-byte frame must succeed (RS fixes 2 errors)
    // and yield the K4020244 fields.
    let d = decode_dewhitened(&hex(FRAME2_ERR_HEX)).expect("decode");
    assert!(d.rs.ok());
    assert_eq!(d.rs.total_corrected(), 2);

    let f = &d.frame;
    assert_eq!(f.serial, "K4020244");
    assert_eq!(f.frame_num, 5014);
    close(f.battery_v as f64, 2.8, 1e-6);

    let t = f.gps_time.expect("gps time");
    assert_eq!(t.week, 1869);
    assert_eq!(t.tow_ms, 395_506_000);

    let p = f.gps_pos.as_ref().expect("gps pos");
    close(p.lat, 52.442_021, 1e-5);
    close(p.lon, 0.462_852, 1e-5);
    close(p.alt_m, 10_021.71, 0.1);
    assert_eq!(p.num_sv, 9);

    let ptu = f.ptu.as_ref().expect("ptu");
    assert_eq!(
        ptu.raw,
        [132073, 130342, 189789, 537142, 469586, 537439, 132212, 130344, 189789, 0, 0, 0]
    );
    assert_eq!(ptu.cal_index, 15);
}

#[test]
fn on_air_path_dewhitens_then_decodes() {
    // Re-whiten the de-whitened oracle frame to reconstruct the on-air byte
    // stream, then decode through the full on-air path. The whitening
    // transform is independently anchored to the published header
    // (whitening::tests::dewhiten_published_header); the asserted serial is
    // the oracle value, so this exercises the pipeline end to end without
    // being a pure self-consistency loop.
    let mut on_air = hex(FRAME1_HEX);
    xng_mode_sonde::whitening::dewhiten_frame(&mut on_air); // de-whiten == whiten (involution)
    let d = decode_on_air(&on_air).expect("decode on-air");
    assert_eq!(d.frame.serial, "K1930293");
    assert_eq!(d.frame.frame_num, 5910);
}

#[test]
fn ecef_formula_matches_oracle_position() {
    // The exact ECEF coordinates from FRAME1 (bytes at 0x114/0x118/0x11C,
    // i32 little-endian centimetres) convert to the published position.
    let (lat, lon, alt) = ecef_to_geodetic(4_279_094.30, 1_235_968.62, 4_589_580.49);
    close(lat, 46.050_263, 1e-5);
    close(lon, 16.110_771, 1e-5);
    close(alt, 28_410.02, 0.1);
}

#[test]
fn rejects_short_frame() {
    use xng_mode_sonde::DecodeError;
    assert_eq!(
        decode_dewhitened(&[0u8; 100]).unwrap_err(),
        DecodeError::TooShort(100)
    );
}

#[test]
fn rejects_uncorrectable_frame() {
    // Overwrite the whole STATUS region with garbage RS cannot repair (far
    // more than 12 byte errors per codeword). Decode must error rather than
    // emit a fabricated serial.
    let mut bad = hex(FRAME1_HEX);
    for b in bad[8..200].iter_mut() {
        *b = 0xAA;
    }
    assert!(decode_dewhitened(&bad).is_err());
}
