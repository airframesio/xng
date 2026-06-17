//! Off-air validation: a real 21 931 kHz HFDL capture (sigidwiki — see
//! tests/data/README.md) must decode through the full native chain:
//! IQ → DDC (+1440 Hz subcarrier) → A1/A2/M1 acquisition → T-segment
//! phase tracking → descramble → deinterleave → Viterbi → SPDU.
//!
//! The expected fields are dumphfdl 1.7.0's decode of the same capture.

use num_complex::Complex;
use xng_mode_hfdl::HfdlChannelDecoder;

#[test]
fn decodes_real_squitter() {
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/hfdl_21931khz_8s.i16"
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
    assert_eq!(samples.len(), 8 * 24_000);

    let mut dec = HfdlChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut squitters = Vec::new();
    for chunk in samples.chunks(65_536) {
        for e in dec.process(chunk) {
            if e.kind == "squitter" {
                squitters.push(e.clone());
            }
        }
    }
    assert!(!squitters.is_empty(), "no squitter decoded from the off-air capture");
    // Ground truth from dumphfdl 1.7.0 on the same recording.
    let s = &squitters[0].details;
    assert_eq!(s["gs_id"], 4, "ground station (Riverhead)");
    assert_eq!(s["frame_index"], 2397);
    assert_eq!(s["frame_offset"], 1);
    assert_eq!(s["systable_version"], 52);
    assert_eq!(s["utc_sync"], true);
    // HFDL-6 first-octet squitter flags, decoded from this real capture's
    // own first octet (byte0 = 0x10): rls/iso clear, version 0, change_note
    // 0 — the bit math matches dumphfdl spdu.c spdu_parse() on the same air.
    assert_eq!(squitters[0].raw[0], 0x10, "real squitter first octet");
    assert_eq!(s["rls_in_use"], false);
    assert_eq!(s["iso8208_supported"], false);
    assert_eq!(s["spdu_version"], 0);
    assert_eq!(s["change_note"], 0);
    // HFDL-6 TDMA reservation / per-slot assignment region (buf[4..52), the
    // largest span dumphfdl leaves opaque) is surfaced raw, 48 octets wide,
    // taken verbatim from this off-air frame's own bytes.
    let slots = s["slot_assignment_hex"].as_str().expect("slot_assignment_hex");
    assert_eq!(slots.len(), 96, "48 octets of reservation data");
    assert_eq!(
        slots,
        squitters[0].raw[4..52]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        "assignment region surfaced verbatim from the off-air bytes"
    );
    // HFDL-5: the demod path stamps the Viterbi corrected-symbol count on
    // every real-signal event (it CRC-validated, so the count is bounded).
    assert!(
        squitters[0].fec_corrected.is_some(),
        "fec_corrected populated on the off-air squitter"
    );
}
