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

/// True iff `s` begins with a clock time of the form `H:MM` or `HH:MM`
/// (optionally followed by ` AM`/` PM`), i.e. a real page that opens with a
/// timestamp. Used to assert the leading FLEX signature byte was stripped so the
/// page starts at the first real digit, not at the header/signature symbol.
fn starts_with_time(s: &str) -> bool {
    let b = s.as_bytes();
    // 1 or 2 leading digits, then ':', then 2 digits.
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=2).contains(&digits) {
        return false;
    }
    b.get(digits) == Some(&b':')
        && b.get(digits + 1).is_some_and(|c| c.is_ascii_digit())
        && b.get(digits + 2).is_some_and(|c| c.is_ascii_digit())
}

/// STRICT ACCEPTANCE for the FLEX **alphanumeric leading-character fix** on the
/// live 929.9375 (3200-bps) channel.
///
/// Background: real FLEX alpha messages begin with a per-message header word and
/// a signature word whose low 7 bits are a non-display **signature/checksum**
/// byte. The decoder previously emitted that signature byte (and, when the vector
/// pointed one word early, the whole header word) as a spurious leading symbol —
/// e.g. `"□Subj:…"`, `":1:34 AM…"`, `"H2.KEN NAG…"`. The fix strips that
/// message-level header/signature from the START so text begins at the first real
/// character, WITHOUT blindly dropping the first char of every word. A small
/// number of fully-garbled pages (random punctuation/case) are dropped by the
/// tightened content gate rather than emitted as fake text.
///
/// This test asserts, on the real capture:
///   1. Every emitted alpha page starts with a PRINTABLE character — none begins
///      with a non-printable / obviously-spurious signature byte.
///   2. The known real pages decode with the right leading text: a page starting
///      `"Subj:"`, a page opening with a clock time `H:MM`, and a page starting
///      `"KEN NAG"` — each at the message START, no leading junk.
///   3. The aggregate alpha printable ratio is ≥ 0.95 (cleaner now that garbled
///      pages are dropped).
///   4. At least a few genuinely-readable multi-word pages still decode (the fix
///      is not "drop everything").
///
/// Skips cleanly if the capture is absent.
#[test]
fn offair_9375_alpha_leading_char_and_gate() {
    if !Path::new(CAP).exists() {
        eprintln!("offair: {CAP} absent — skipping alpha leading-char acceptance");
        return;
    }
    let iq = load_capture();
    let (baud, frames) = run_auto(&iq, OFFSET_9375_HZ);
    assert_eq!(baud, Some(3200), "929.9375 must auto-detect 3200 bps");

    let alpha: Vec<&FlexFrame> = frames
        .iter()
        .filter(|f| f.kind == FlexKind::Alpha && !f.text.is_empty())
        .collect();

    for f in &alpha {
        eprintln!("alpha cap={} text={:?}", f.capcode, f.text);
    }

    // (1) No emitted alpha page may start with a non-printable / spurious char —
    // the spurious leading FLEX signature byte must be gone.
    for f in &alpha {
        let lead = f.text.chars().next().unwrap();
        assert!(
            (' '..='~').contains(&lead),
            "alpha page starts with a spurious non-printable char {:#x}: {:?}",
            lead as u32,
            f.text
        );
        // A real page never opens on a control/ETX/signature byte.
        assert!(
            !lead.is_control(),
            "alpha page starts with a control char: {:?}",
            f.text
        );
    }

    // (2) Known real pages decode with the correct leading text at the START.
    assert!(
        alpha.iter().any(|f| f.text.starts_with("Subj:")),
        "expected a page starting exactly \"Subj:\" (signature byte stripped); got {:?}",
        alpha.iter().map(|f| &f.text).collect::<Vec<_>>()
    );
    assert!(
        alpha.iter().any(|f| starts_with_time(&f.text)),
        "expected a page opening with a clock time H:MM (leading signature byte stripped); got {:?}",
        alpha.iter().map(|f| &f.text).collect::<Vec<_>>()
    );
    assert!(
        alpha.iter().any(|f| f.text.starts_with("KEN NAG")),
        "expected a page starting \"KEN NAG\" (header word + signature byte stripped); got {:?}",
        alpha.iter().map(|f| &f.text).collect::<Vec<_>>()
    );

    // (3) Aggregate alpha printable ratio ≥ 0.95 (garbled pages dropped → cleaner).
    let printable: usize = alpha
        .iter()
        .flat_map(|f| f.text.chars())
        .filter(|&c| c == '\n' || c == '\r' || c == '\t' || (' '..='~').contains(&c))
        .count();
    let total: usize = alpha.iter().map(|f| f.text.chars().count()).sum();
    let ratio = printable as f64 / total.max(1) as f64;
    assert!(
        ratio >= 0.95,
        "929.9375 aggregate alpha printable ratio {ratio:.4} < 0.95"
    );

    // (4) Still several genuinely-readable multi-word pages (not over-pruned).
    let multiword = alpha
        .iter()
        .filter(|f| f.text.contains(' ') && f.text.chars().count() >= 12)
        .count();
    assert!(
        multiword >= 3,
        "expected several readable multi-word pages; got {multiword}"
    );

    // (5) The fully-garbled random-ASCII pages must NOT survive the gate: no
    // emitted alpha page is a space-less wall of odd punctuation. (The historical
    // garble e.g. "VH=P@3jE6lbAZMhFKVba[4>^>Hnnm99UkHFS`cHm".)
    for f in &alpha {
        let n = f.text.chars().count();
        let junk = f
            .text
            .chars()
            .filter(|&c| {
                !(c.is_ascii_alphanumeric()
                    || c == ' '
                    || c == '\n'
                    || c == '\r'
                    || ".,:;/#-+*&%$@!?()[]'\"".contains(c))
            })
            .count();
        assert!(
            (junk as f64 / n.max(1) as f64) <= 0.15,
            "garbled alpha page survived the gate: {:?}",
            f.text
        );
    }
}
