//! OFF-AIR validation against a real RTL-SDR FLEX capture (the oracle).
//!
//! `/tmp/flex_930.cu8` is a cu8 capture at 2.4 MS/s centered on 930.000 MHz with
//! two live FLEX paging channels:
//!   - **929.6125 MHz** (−387.5 kHz offset), 6400-bps 4-level (A-code `0xDEA0`),
//!   - **929.9375 MHz** (−62.5 kHz offset), 3200-bps 4-level (A-code `0xB068`).
//!
//! The 929.9375 channel historically FLOODED garbage through the auto decoder:
//! ~148 frames/pass with all-ones-fill capcodes in the `0xFFFF_xxxx` band,
//! non-printable "alpha" text, and the same fill body attributed to many
//! addresses (idle/fill words misread as ADDRESS words, BCH false-correcting
//! fill into plausible-looking codewords). These tests are the STRICT acceptance
//! gate that proves the hardened decoder emits only REAL pages, never garbage.
//!
//! Gated on the file's presence (skip cleanly when absent so CI without the
//! capture stays green).

use num_complex::Complex;
use std::path::Path;
use xng_mode_flex::{FlexChannelDecoder, FlexFrame, FlexKind};

const CAP: &str = "/tmp/flex_930.cu8";
const INPUT_RATE: f64 = 2_400_000.0;
/// 929.6125 MHz − 930.000 MHz center.
const OFFSET_6125_HZ: f64 = -387_500.0;
/// 929.9375 MHz − 930.000 MHz center.
const OFFSET_9375_HZ: f64 = -62_500.0;

/// The all-ones-fill garbage capcode band (idle/fill ADDRESS-word misreads).
const GARBAGE_LO: u32 = 0xFFFF_0000;

/// Load the cu8 capture as complex baseband (RTL-SDR offset-127.5 / 127.5).
fn load_capture() -> Vec<Complex<f32>> {
    let raw = std::fs::read(CAP).expect("read capture");
    raw.chunks_exact(2)
        .map(|c| Complex::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
        .collect()
}

/// Fraction of a string that is printable ASCII (incl. space / newline / CR / tab).
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

/// Run the AUTO decoder over a channel of the capture in realistic chunks.
fn run_auto(iq: &[Complex<f32>], offset_hz: f64) -> (Option<u32>, Vec<FlexFrame>) {
    let mut dec = FlexChannelDecoder::new_auto(INPUT_RATE, offset_hz).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(240_000) {
        frames.extend(dec.process(chunk));
    }
    frames.extend(dec.process(&[]));
    (dec.baud(), frames)
}

/// Count emitted frames whose capcode is in the all-ones-fill garbage band.
fn garbage_capcodes(frames: &[FlexFrame]) -> usize {
    frames.iter().filter(|f| f.capcode >= GARBAGE_LO).count()
}

/// Aggregate printable-ASCII ratio over all non-empty alpha bodies.
fn alpha_aggregate_printable(frames: &[FlexFrame]) -> (usize, f64) {
    let alpha: Vec<_> = frames
        .iter()
        .filter(|f| f.kind == FlexKind::Alpha && !f.text.is_empty())
        .collect();
    let printable: usize = alpha
        .iter()
        .map(|f| {
            f.text
                .chars()
                .filter(|&c| c == '\n' || c == '\r' || c == '\t' || (' '..='~').contains(&c))
                .count()
        })
        .sum();
    let total: usize = alpha.iter().map(|f| f.text.chars().count()).sum();
    // Vacuously fully-printable when a channel emits no alpha text.
    let ratio = if total == 0 {
        1.0
    } else {
        printable as f64 / total as f64
    };
    (alpha.len(), ratio)
}

/// THE ORACLE — STRICT acceptance over BOTH live channels:
///
///  1. ZERO emitted frames with a capcode in the `0xFFFF_0000..` all-ones-fill
///     garbage band, on EITHER channel, AND every emitted capcode inside the
///     sane FLEX bound.
///  2. Emitted ALPHA pages are ≥ 0.90 printable-ASCII in aggregate.
///  3. The 929.9375 channel — the historical garbage flooder (~148 junk
///     frames/pass) — drops drastically to only its real pages.
///  4. At least a few genuinely-readable pages still decode (it is not "fixed"
///     by emitting nothing): the 929.9375 channel yields clean alpha pages.
///
/// Skips cleanly if the capture is not present.
#[test]
fn offair_emits_only_real_pages_no_garbage() {
    if !Path::new(CAP).exists() {
        eprintln!("offair: {CAP} absent — skipping real-capture oracle");
        return;
    }
    let iq = load_capture();

    let (baud_6125, frames_6125) = run_auto(&iq, OFFSET_6125_HZ);
    let (baud_9375, frames_9375) = run_auto(&iq, OFFSET_9375_HZ);

    let g6125 = garbage_capcodes(&frames_6125);
    let g9375 = garbage_capcodes(&frames_9375);
    let (alpha6125, pr6125) = alpha_aggregate_printable(&frames_6125);
    let (alpha9375, pr9375) = alpha_aggregate_printable(&frames_9375);

    let clean_9375 = frames_9375
        .iter()
        .filter(|f| {
            f.kind == FlexKind::Alpha
                && f.text.chars().count() >= 12
                && printable_ratio(&f.text) >= 0.90
        })
        .count();

    eprintln!(
        "offair 929.6125: baud={baud_6125:?} frames={} alpha={alpha6125} garbage={g6125} alpha_printable={pr6125:.3}",
        frames_6125.len()
    );
    eprintln!(
        "offair 929.9375: baud={baud_9375:?} frames={} alpha={alpha9375} garbage={g9375} alpha_printable={pr9375:.3} clean_pages={clean_9375}",
        frames_9375.len()
    );
    for f in frames_9375
        .iter()
        .filter(|f| {
            f.kind == FlexKind::Alpha
                && f.text.chars().count() >= 12
                && printable_ratio(&f.text) >= 0.90
        })
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

    // (1) ZERO all-ones-fill garbage capcodes on EITHER channel.
    assert_eq!(
        g6125, 0,
        "929.6125 emitted {g6125} all-ones-fill garbage capcodes"
    );
    assert_eq!(
        g9375, 0,
        "929.9375 emitted {g9375} all-ones-fill garbage capcodes"
    );
    // Sane FLEX capcode bound (no wraparound / out-of-range addresses).
    for f in frames_6125.iter().chain(frames_9375.iter()) {
        assert!(
            f.capcode >= 1 && f.capcode < GARBAGE_LO,
            "out-of-range capcode emitted: {:#010x} (kind={:?} text={:?})",
            f.capcode,
            f.kind,
            f.text
        );
    }

    // (2) Aggregate alpha must be overwhelmingly printable on EITHER channel
    // that emits alpha (vacuously true at 1.0 when a channel emits none).
    assert!(
        pr9375 >= 0.90,
        "929.9375 alpha not printable enough: {pr9375:.3} (want >= 0.90)"
    );
    assert!(
        pr6125 >= 0.90,
        "929.6125 alpha not printable enough: {pr6125:.3} (want >= 0.90)"
    );

    // (3) The 929.9375 garbage flooder is now bounded to its real pages — far
    // below the ~148-frame garbage baseline.
    assert!(
        frames_9375.len() <= 40,
        "929.9375 still floods frames: {} (was ~148 garbage)",
        frames_9375.len()
    );

    // (4) ...but it is NOT "fixed" by emitting nothing: genuine readable pages
    // still decode.
    assert!(
        clean_9375 >= 3,
        "expected several cleanly-readable 929.9375 alpha pages; got {clean_9375}"
    );
    assert_eq!(
        baud_9375,
        Some(3200),
        "929.9375 must auto-detect its real 3200-bps rate; got {baud_9375:?}"
    );
}
