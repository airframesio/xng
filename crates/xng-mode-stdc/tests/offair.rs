//! Off-air validation: a real Inmarsat-C TDM/EGC capture (sigidwiki —
//! see tests/data/README.md) must decode through the full native chain:
//! IQ → DDC → BPSK demod (coarse AFC, Costas, Gardner) → UW frame sync →
//! depermute/deinterleave → Viterbi → descramble → packet parse.

use num_complex::Complex;
use xng_mode_stdc::StdcChannelDecoder;

#[test]
fn decodes_real_egc_frame() {
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/stdc_egc_14s.i16"
    ))
    .expect("fixture present");
    let samples: Vec<Complex<f32>> = raw
        .chunks_exact(4)
        .map(|b| {
            Complex::new(
                i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                i16::from_le_bytes([b[2], b[3]]) as f32 / 32768.0,
            )
        })
        .collect();
    assert_eq!(samples.len(), 14 * 24_000);

    let mut dec = StdcChannelDecoder::new(24_000.0, 216.0).unwrap();
    let mut packets = Vec::new();
    for chunk in samples.chunks(65_536) {
        packets.extend(dec.process(chunk));
    }
    assert!(packets.len() >= 5, "expected a frame's worth of packets, got {}", packets.len());
    let bb = packets
        .iter()
        .find(|p| p.name == "bulletin-board")
        .expect("bulletin board present");
    assert!(bb.checksum_ok);
    assert_eq!(bb.details["frame_number"], 5987);
    // STDC-7: frame number → UTC-of-day (5987 × 8.64 = 51727 s → 14:22:07).
    assert_eq!(bb.details["utc_time"], "14:22:07");
    // STDC-2: the deepened 0x7D fields decode consistently on the real
    // NCS common-channel frame — channel type 1 = NCS, the sat/LES byte
    // resolves to the AOR-E NCS station (les 144), and the station status
    // reads operational/in-service (a healthy NCS). These match the
    // inmarsatc decode_7D field map applied to the real bytes.
    assert_eq!(bb.details["channel_type"], 1);
    assert_eq!(bb.details["channel_type_name"], "NCS");
    assert_eq!(bb.details["sat_les"]["region"], "AOR-E");
    assert_eq!(bb.details["sat_les"]["les"], 144);
    assert_eq!(bb.details["sat_les"]["les_name"], "NCS");
    assert_eq!(bb.details["status"]["operational"], true);
    assert_eq!(bb.details["status"]["in_service"], true);
    // The services bitfield includes the core C-system capabilities.
    let svcs = bb.details["services"].as_array().expect("services array");
    assert!(svcs.iter().any(|s| s == "SafetyNet"));
    assert!(svcs.iter().any(|s| s == "InmarsatC"));
    let ann = packets
        .iter()
        .find(|p| p.name == "announcement" && p.checksum_ok)
        .expect("announcement present");
    // STDC-4: the real announcement's sat/LES byte resolves to AOR-E
    // (the documented capture region) and a named operator.
    assert_eq!(ann.details["sat_les"]["region"], "AOR-E");
    assert_eq!(
        ann.details["sat_les"]["region_long"],
        "Atlantic Ocean Region East (AOR-E)"
    );
    assert_eq!(ann.details["sat_les"]["les_name"], "Vizada-Telenor, Norway");
    // STDC-3: the real 0x6C signalling-channel packet decodes its uplink
    // channel word (0x2748) to 1636.64 MHz, inside the L-band uplink band.
    let sc = packets
        .iter()
        .find(|p| p.name == "signalling-channel")
        .expect("signalling-channel present");
    assert_eq!(sc.details["uplink_mhz"], 1636.64);
    // STDC-2: the same real 0x6C descriptor carries the 8-bit services
    // byte (0xB4) and 28 TDM-slot codes. Service bit names per inmarsatc
    // getServices_short; the slot array is always 28 entries long.
    assert_eq!(
        sc.details["services"],
        serde_json::json!([
            "MaritimeDistressAlerting",
            "InmarsatC",
            "StoreFwd",
            "FullDuplex"
        ])
    );
    assert_eq!(sc.details["tdm_slots"].as_array().unwrap().len(), 28);
}
