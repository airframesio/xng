//! Off-air validation: a real Inmarsat Classic Aero recording (JAERO's
//! 600 bps sample, MIT — see tests/data/README.md) must decode through
//! the full native chain: audio → DDC → A-BPSK discriminator → UW →
//! deinterleave → Viterbi → descramble → SU reassembly → ACARS.
//!
//! This guards the conventions that synthetic loopback cannot see
//! (modulation bit mapping, coded pair order, scrambler alignment).

use num_complex::Complex;
use xng_mode_aero::AeroChannelDecoder;

#[test]
fn decodes_real_600bps_recording() {
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/600bps_offair_12s.i16"
    ))
    .expect("fixture present");
    let samples: Vec<Complex<f32>> = raw
        .chunks_exact(2)
        .map(|b| Complex::new(i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0, 0.0))
        .collect();
    assert_eq!(samples.len(), 12 * 48_000);

    // The P channel sits at ~1066 Hz in this recording's audio band.
    let mut dec = AeroChannelDecoder::new(48_000.0, 1_066.0).unwrap();
    let mut acars_ok = 0usize;
    let mut tails: Vec<String> = Vec::new();
    for chunk in samples.chunks(65_536) {
        for e in dec.process(chunk) {
            if let Some(b) = &e.acars {
                if b.crc_ok {
                    acars_ok += 1;
                    if let Some(t) = &b.core.tail {
                        tails.push(t.clone());
                    }
                }
            }
        }
    }
    assert!(acars_ok >= 1, "no CRC-valid ACARS decoded from the off-air recording");
    assert!(
        tails.iter().any(|t| t == "HL8217"),
        "expected HL8217 in the decoded traffic, got {tails:?}"
    );
}
