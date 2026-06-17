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
    assert!(packets.iter().any(|p| p.name == "announcement" && p.checksum_ok));
}
