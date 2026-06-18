//! OFF-AIR validation against a real RTL-SDR FLEX capture (the oracle).
//!
//! `/tmp/flex_930.cu8` is a cu8 capture at 2.4 MS/s centered on 930.000 MHz; the
//! active FLEX paging signal sits at 929.6125 MHz (−387.5 kHz offset). It is
//! **6400-bps 4-level FLEX** (Sync 1 A-code `0xDEA0`, 3200 sym/s) — i.e. NOT the
//! 1600-bps base rate the runtime historically opened FLEX at.
//!
//! These tests are GATED on the file's presence (they skip cleanly if absent so
//! CI without the capture stays green) and assert the AUTO decoder
//! ([`FlexChannelDecoder::new_auto`]) materially out-performs a forced-1600 open:
//! it auto-detects 6400, recovers pages with capcodes in the valid FLEX range
//! (NOT the ~u32::MAX garbage a 1600 misread of 4-level data produces), and
//! decodes alphanumeric text that is overwhelmingly printable ASCII.

use num_complex::Complex;
use std::path::Path;
use xng_mode_flex::{FlexChannelDecoder, FlexKind};

const CAP: &str = "/tmp/flex_930.cu8";
const INPUT_RATE: f64 = 2_400_000.0;
/// 929.6125 MHz − 930.000 MHz center.
const OFFSET_HZ: f64 = -387_500.0;

/// Load the cu8 capture as complex baseband (RTL-SDR offset-127.5 / 127.5).
fn load_capture() -> Vec<Complex<f32>> {
    let raw = std::fs::read(CAP).expect("read capture");
    raw.chunks_exact(2)
        .map(|c| {
            Complex::new(
                (c[0] as f32 - 127.5) / 127.5,
                (c[1] as f32 - 127.5) / 127.5,
            )
        })
        .collect()
}

/// Fraction of a string that is printable ASCII (incl. space/newline/tab).
fn printable_ratio(s: &str) -> f64 {
    let total = s.chars().count();
    if total == 0 {
        return 1.0;
    }
    let ok = s
        .chars()
        .filter(|&c| c == '\n' || c == '\r' || c == '\t' || (' '..='~').contains(&c))
        .count();
    ok as f64 / total as f64
}

/// Run a decoder over the capture in realistic chunks; collect all frames.
fn run(mut dec: FlexChannelDecoder, iq: &[Complex<f32>]) -> Vec<xng_mode_flex::FlexFrame> {
    let mut frames = Vec::new();
    for chunk in iq.chunks(240_000) {
        frames.extend(dec.process(chunk));
    }
    // Drain any tail.
    frames.extend(dec.process(&[]));
    frames
}

/// THE ORACLE: the auto decoder must recover real 6400-bps FLEX pages from the
/// off-air capture — sane capcodes and printable alpha text — and report the
/// detected rate. Skips cleanly if the capture is not present.
#[test]
fn auto_decodes_real_flex_capture() {
    if !Path::new(CAP).exists() {
        eprintln!("offair: {CAP} absent — skipping real-capture oracle");
        return;
    }
    let iq = load_capture();

    // --- AUTO path: detect the rate from Sync 1 and decode the data phase. ---
    let mut auto = FlexChannelDecoder::new_auto(INPUT_RATE, OFFSET_HZ).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(240_000) {
        frames.extend(auto.process(chunk));
    }
    frames.extend(auto.process(&[]));

    // The signal is 6400-bps 4-level — auto must lock that rate, not 1600.
    assert_eq!(
        auto.baud(),
        Some(6400),
        "auto-detect must lock the on-air 6400-bps 4-level rate; got {:?}",
        auto.baud()
    );

    assert!(
        frames.len() >= 50,
        "auto recovered too few pages from the capture: {}",
        frames.len()
    );

    // Capcodes must be in the valid FLEX range — NOT the ~u32::MAX garbage that a
    // 1600 misread of 4-level data yields. Short addresses are ≤ 0x1F_FFFF; long
    // first-words still land well under 0x00FF_FFFF (multimon `aw1 - 0x8000`).
    let sane_cap = frames
        .iter()
        .filter(|f| f.capcode <= 0x00FF_FFFF)
        .count();
    let sane_frac = sane_cap as f64 / frames.len() as f64;
    assert!(
        sane_frac >= 0.85,
        "too many garbage capcodes: only {sane_cap}/{} in valid range ({sane_frac:.2})",
        frames.len()
    );

    // Alpha pages must carry real text: overwhelmingly printable ASCII, and at
    // least one clearly-readable page (≥90% printable, real length).
    let alpha: Vec<_> = frames
        .iter()
        .filter(|f| f.kind == FlexKind::Alpha && !f.text.is_empty())
        .collect();
    assert!(
        alpha.len() >= 10,
        "expected several alpha pages; got {}",
        alpha.len()
    );
    let printable_chars: usize = alpha
        .iter()
        .map(|f| {
            f.text
                .chars()
                .filter(|&c| c == '\n' || c == '\r' || (' '..='~').contains(&c))
                .count()
        })
        .sum();
    let total_chars: usize = alpha.iter().map(|f| f.text.chars().count()).sum();
    let printable = printable_chars as f64 / total_chars.max(1) as f64;

    let clean = alpha
        .iter()
        .filter(|f| f.text.chars().count() >= 12 && printable_ratio(&f.text) >= 0.90)
        .count();

    // Report a few real decoded pages (visible with `--nocapture`).
    eprintln!(
        "offair AUTO: rate={:?} frames={} alpha={} sane_cap={sane_frac:.2} alpha_printable={printable:.3} clean_pages={clean}",
        auto.baud(),
        frames.len(),
        alpha.len()
    );
    for f in alpha
        .iter()
        .filter(|f| f.text.chars().count() >= 12 && printable_ratio(&f.text) >= 0.90)
        .take(5)
    {
        eprintln!(
            "  cap={} cyc={} frm={} text={:?}",
            f.capcode,
            f.cycle,
            f.frame,
            f.text.chars().take(70).collect::<String>()
        );
    }

    assert!(
        printable >= 0.45,
        "alpha text not printable enough: {printable:.3} (real 4-level decode should be ≫ the 1600 misread)"
    );
    assert!(
        clean >= 1,
        "expected at least one cleanly-readable alpha page; got {clean}"
    );

    // --- BASELINE: the SAME capture forced to 1600 bps garbles 4-level data. ---
    let baseline = FlexChannelDecoder::new(INPUT_RATE, OFFSET_HZ, 1600).unwrap();
    let bframes = run(baseline, &iq);
    let balpha: Vec<_> = bframes
        .iter()
        .filter(|f| f.kind == FlexKind::Alpha && !f.text.is_empty())
        .collect();
    let bprint_chars: usize = balpha
        .iter()
        .map(|f| {
            f.text
                .chars()
                .filter(|&c| c == '\n' || c == '\r' || (' '..='~').contains(&c))
                .count()
        })
        .sum();
    let btotal: usize = balpha.iter().map(|f| f.text.chars().count()).sum();
    let bprintable = bprint_chars as f64 / btotal.max(1) as f64;
    let bclean = balpha
        .iter()
        .filter(|f| f.text.chars().count() >= 12 && printable_ratio(&f.text) >= 0.90)
        .count();
    eprintln!(
        "offair 1600 baseline: alpha={} alpha_printable={bprintable:.3} clean_pages={bclean}",
        balpha.len()
    );

    // The auto (6400) path must be MATERIALLY better than the forced-1600 misread.
    assert!(
        printable > bprintable + 0.08 && clean > bclean,
        "auto path not materially better than forced-1600: \
         auto(printable={printable:.3}, clean={clean}) vs 1600(printable={bprintable:.3}, clean={bclean})"
    );
}
