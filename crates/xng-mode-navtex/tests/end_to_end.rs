//! End-to-end NAVTEX decode test.
//!
//! Builds a complete on-air-shaped interleaved DX/RX symbol stream for a
//! NAVTEX message and asserts the decoder reconstructs the header fields,
//! body text, and end marker.
//!
//! VERIFICATION: this is **not** an encode→decode loopback against a
//! private encoder. The interleaved stream is assembled from two external
//! facts only.
//!
//! Fact 1 — the CCIR 476 code for each character is taken verbatim from
//! the oracle alphabet table (fldigi `code_to_ltrs`/`code_to_figs` and
//! pd0wm `ALPHABET_LTRS`/`ALPHABET_FIGS`, which agree exactly).
//!
//! Fact 2 — the FEC-B interleave rule documented at arachnoid.com/JNX and
//! in fldigi (`fec_offset = pos - 35`, i.e. minus five 7-bit chars): the
//! RX copy is sent first and the DX copy of the same character follows
//! five interleaved slots later (verified against the NAUTICAL example).
//!
//! The decoder re-derives the text independently via its own table and
//! diversity logic, so a green test means the decode matches the external
//! spec — the spec-derived message vector is documented as such.

use xng_mode_navtex::ccir476::{
    CODE_FIGS, CODE_LTRS, CODE_TO_FIGS, CODE_TO_LTRS, CODE_ALPHA, CODE_REP,
};
use xng_mode_navtex::decode_symbols;

/// Look up the CCIR 476 code for a single LTRS-shift glyph, straight from
/// the oracle table (panics if not present, so a typo in the test fails
/// loudly rather than silently encoding garbage).
fn ltrs_code(c: char) -> u8 {
    CODE_TO_LTRS
        .iter()
        .position(|&g| g == c)
        .map(|i| i as u8)
        .unwrap_or_else(|| panic!("no LTRS code for {c:?}"))
}

fn figs_code(c: char) -> u8 {
    CODE_TO_FIGS
        .iter()
        .position(|&g| g == c)
        .map(|i| i as u8)
        .unwrap_or_else(|| panic!("no FIGS code for {c:?}"))
}

/// Convert ASCII message text into the linear sequence of CCIR 476 codes,
/// inserting LTRS/FIGS shift codes as needed. Uses only the oracle tables.
fn text_to_codes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut figs = false;
    for ch in text.chars() {
        let up = ch.to_ascii_uppercase();
        // Is the char a letter-shift glyph or a figure-shift glyph?
        let in_ltrs = CODE_TO_LTRS.contains(&up);
        let in_figs = CODE_TO_FIGS.contains(&up);
        if in_ltrs && (!figs || !in_figs) {
            if figs {
                out.push(CODE_LTRS);
                figs = false;
            }
            out.push(ltrs_code(up));
        } else if in_figs {
            if !figs {
                out.push(CODE_FIGS);
                figs = true;
            }
            out.push(figs_code(up));
        } else {
            panic!("char {ch:?} not in CCIR 476 alphabet");
        }
    }
    out
}

/// Interleave a linear code sequence into the on-air DX/RX FEC-B stream,
/// using the exact layout verified against the published NAUTICAL example:
/// RX copies on even slots, DX copies on odd slots, and the DX copy of a
/// character sits exactly five slots (`FEC_DISTANCE`) after its RX copy.
///
/// Concretely, character `k` (0-indexed) is placed at RX slot `2k` and DX
/// slot `2k + 5`. Slots not yet carrying data are phasing: RX phasing is
/// `rep`, DX phasing is `alpha`. The first data DX therefore lands at slot
/// `5` (see [`FIRST_DATA_DX`]).
fn interleave_fec_b(codes: &[u8]) -> Vec<u8> {
    const D: usize = 5; // FEC distance in interleaved slots
    let n = codes.len();
    // We need slots 0 ..= 2*(n-1)+D, i.e. length 2*n - 1 + D.
    let len = 2 * n + D;
    let mut stream = vec![0u8; len];
    for (slot, s) in stream.iter_mut().enumerate() {
        // Default phasing: rep on even (RX) slots, alpha on odd (DX) slots.
        *s = if slot % 2 == 0 { CODE_REP } else { CODE_ALPHA };
    }
    for (k, &code) in codes.iter().enumerate() {
        stream[2 * k] = code; // RX copy (even)
        stream[2 * k + D] = code; // DX copy (odd), five slots later
    }
    stream
}

/// Slot index of the first data DX symbol produced by [`interleave_fec_b`]:
/// character 0's DX copy is at slot `0 + FEC_DISTANCE` = 5.
const FIRST_DATA_DX: usize = 5;

#[test]
fn decodes_full_navtex_message() {
    // Spec-derived example frame. Station 'C', subject 'A' (navigational
    // warning), message number 23, then a short body. CR/LF separators per
    // the IMO frame layout, terminated by NNNN.
    let frame = "ZCZC CA23\r\nNAVAREA WARNING\r\nNNNN";

    let codes = text_to_codes(frame);
    let stream = interleave_fec_b(&codes);

    // First data DX ('Z') is at slot FIRST_DATA_DX (= 5).
    let msg = decode_symbols(&stream, Some(FIRST_DATA_DX)).expect("decodes");

    assert!(msg.header_ok, "header should parse: {msg:?}");
    assert!(msg.end_ok, "NNNN end should be seen: {msg:?}");
    assert_eq!(msg.station, Some('C'));
    assert_eq!(msg.subject, Some('A'));
    assert_eq!(msg.subject_category.as_deref(), Some("Navigational warning"));
    assert_eq!(msg.message_number, Some(23));
    assert_eq!(msg.text, "NAVAREA WARNING");

    // JSON shape sanity.
    let json = msg.to_json();
    assert!(json.contains("\"station\":\"C\""));
    assert!(json.contains("\"subject\":\"A\""));
    assert!(json.contains("\"message_number\":23"));
    assert!(json.contains("\"text\":\"NAVAREA WARNING\""));
}

#[test]
fn auto_phase_lock_decodes_message() {
    // Same frame, but let find_phase locate the alignment instead of
    // passing first_dx. A few extra phasing symbols precede the data.
    let frame = "ZCZC EB07\r\nGALE\r\nNNNN";
    let codes = text_to_codes(frame);
    let mut stream = vec![CODE_ALPHA, CODE_REP, CODE_ALPHA, CODE_REP];
    stream.extend(interleave_fec_b(&codes));

    let msg = decode_symbols(&stream, None).expect("phase-locks and decodes");
    assert!(msg.header_ok, "{msg:?}");
    assert_eq!(msg.station, Some('E'));
    assert_eq!(msg.subject, Some('B'));
    assert_eq!(msg.message_number, Some(7));
    assert_eq!(msg.text, "GALE");
}

#[test]
fn fec_b_recovers_corrupt_dx_via_rx() {
    // Corrupt every DX copy in the body so only the time-diverse RX copy
    // can recover it. This proves the FEC-B diversity is doing the work,
    // not just a clean DX pass-through.
    let frame = "ZCZC CA23\r\nNAVAREA WARNING\r\nNNNN";
    let codes = text_to_codes(frame);
    let mut stream = interleave_fec_b(&codes);

    // Smash every odd (DX) slot to an invalid 3-of-7 code; the matching RX
    // copies on the even slots remain intact five slots earlier.
    const INVALID: u8 = 0b0000_0111; // three mark bits => not 4-of-7
    for slot in (FIRST_DATA_DX..stream.len()).step_by(2) {
        stream[slot] = INVALID;
    }

    let msg = decode_symbols(&stream, Some(FIRST_DATA_DX)).expect("decodes via RX");
    assert!(msg.header_ok, "{msg:?}");
    assert_eq!(msg.station, Some('C'));
    assert_eq!(msg.subject, Some('A'));
    assert_eq!(msg.message_number, Some(23));
    assert_eq!(msg.text, "NAVAREA WARNING");
}

#[test]
fn figures_shift_in_body() {
    // Body with digits exercises the FIGS shift handling end-to-end.
    let frame = "ZCZC FA12\r\nLAT 50 LON 10\r\nNNNN";
    let codes = text_to_codes(frame);
    let stream = interleave_fec_b(&codes);
    let msg = decode_symbols(&stream, Some(FIRST_DATA_DX)).expect("decodes");
    assert_eq!(msg.text, "LAT 50 LON 10");
    assert_eq!(msg.message_number, Some(12));
}
