//! Off-air validation: a real VDL Mode 2 capture (sigidwiki — see
//! tests/data/README.md) must decode through the full native chain:
//! IQ → D8PSK demod (UW hunt, header FEC, RS deinterleave) → AVLC →
//! ACARS-over-AVLC.

use num_complex::Complex;
use xng_mode_vdl2::Vdl2ChannelDecoder;

#[test]
fn decodes_real_acars() {
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/vdl2_offair_6s.i16"
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
    assert_eq!(samples.len(), 6 * 50_000);

    let mut dec = Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
    let mut acars_tails = Vec::new();
    let mut frames = 0usize;
    for chunk in samples.chunks(65_536) {
        for f in dec.process(chunk) {
            frames += 1;
            if let Some(b) = &f.acars {
                if b.crc_ok {
                    acars_tails.extend(b.core.tail.clone());
                }
            }
        }
    }
    assert!(frames >= 2, "expected at least 2 AVLC frames, got {frames}");
    assert!(
        acars_tails.iter().any(|t| t == "HB-IJW"),
        "expected HB-IJW ACARS in the decoded traffic, got {acars_tails:?}"
    );
}
