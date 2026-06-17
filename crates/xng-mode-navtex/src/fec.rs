//! FEC-B (CCIR 476 collective B-mode) time-diversity decode.
//!
//! NAVTEX / SITOR-B sends every character twice over two interleaved
//! channels. In the received symbol stream the two copies alternate
//! (RX on even slots, DX on odd slots once phased):
//!
//! ```text
//!   RX RX  N  A  U  N  T  A  I  U  ...   (rep alpha rep alpha N alpha A ...)
//! ```
//!
//! The RX ("rep") copy of a character is broadcast first; the DX ("alpha")
//! copy of the *same* character follows **five interleaved symbol-slots**
//! later (35 bit periods at 7 bits/symbol). fldigi expresses this as
//! `fec_offset(pos) = pos - 35` (i.e. minus five 7-bit chars);
//! arachnoid.com/JNX documents the same interleave with the worked
//! "NAUTICAL" example used in the test below, where DX 'N' at slot 9 has
//! its RX copy at slot 4 — exactly [`FEC_DISTANCE`] = 5 slots earlier.
//!
//! Decode rule (per CCIR 476 §B / fldigi `process_bytes`):
//! 1. If the DX copy is a valid 4-of-7 code, use it.
//! 2. Otherwise, if the RX copy (five slots earlier) is valid, use it.
//! 3. Otherwise the character is unrecoverable.
//!
//! This module operates on an already-demodulated symbol stream (each
//! symbol is one 7-bit CCIR 476 code). IQ→symbols is out of scope here
//! (see the crate-level TODO).

use crate::ccir476::{self, is_valid_code, CODE_ALPHA, CODE_REP};

/// Interleave distance, in symbol slots, between a DX character and its
/// earlier RX copy. fldigi: 35 bits / 7 bits-per-symbol = 5 char-slots.
/// Verified against the NAUTICAL example: DX 'N' at slot 9, RX 'N' at slot 4.
pub const FEC_DISTANCE: usize = 5;

/// Outcome of recovering one character position via diversity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharSource {
    /// The DX (primary) copy was valid and used.
    Dx,
    /// The DX copy was bad; the RX (earlier) copy was valid and used.
    Rx,
    /// Neither copy was a valid 4-of-7 code.
    Lost,
}

/// One recovered position: the chosen code (if any) and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    pub code: Option<u8>,
    pub source: CharSource,
}

/// Pick the best copy for a single DX position.
///
/// `dx` is the primary copy at the current position; `rx` is its repeat
/// from five slots earlier (`None` if not yet available — i.e. at the
/// very start of the stream).
pub fn recover(dx: u8, rx: Option<u8>) -> Recovered {
    if is_valid_code(dx) {
        return Recovered { code: Some(dx), source: CharSource::Dx };
    }
    if let Some(rx) = rx {
        if is_valid_code(rx) {
            return Recovered { code: Some(rx), source: CharSource::Rx };
        }
    }
    Recovered { code: None, source: CharSource::Lost }
}

/// Find the symbol-slot offset (the first DX position) at which the
/// interleaved stream best lines up: the most valid characters with their
/// expected RX repeats.
///
/// Models fldigi `find_alpha_characters`: the first DX slot lies in one of
/// the first `FEC_DISTANCE * 2` positions. We score each candidate by the
/// number of valid 4-of-7 DX codes whose RX copy (`offset` − `FEC_DISTANCE`
/// slots) also matches. Returns `None` if no offset reaches the minimum
/// confidence.
pub fn find_phase(symbols: &[u8]) -> Option<usize> {
    let mut best_offset = None;
    let mut best_score = 0i32;

    for offset in 0..(FEC_DISTANCE * 2) {
        if offset >= symbols.len() {
            break;
        }
        let mut score = 0i32;
        let mut reps = 0i32;
        // Step by 2: DX copies sit every other slot once phased.
        let mut i = offset;
        while i < symbols.len() {
            let dx = symbols[i];
            if is_valid_code(dx) {
                score += 1;
                if i >= FEC_DISTANCE {
                    let rx = symbols[i - FEC_DISTANCE];
                    // Matching repeat (ignore phasing pairs, which fldigi
                    // explicitly excludes as a false alignment).
                    if rx == dx && dx != CODE_ALPHA && dx != CODE_REP {
                        reps += 1;
                    }
                }
            }
            i += 2;
        }
        let total = score + reps;
        if reps >= 1 && total > best_score {
            best_score = total;
            best_offset = Some(offset);
        }
    }
    best_offset
}

/// Decode an interleaved DX/RX symbol stream into recovered code words.
///
/// `symbols` is the raw interleaved stream (RX and DX copies alternating).
/// `first_dx` is the slot index of the first DX (alpha) symbol — use
/// [`find_phase`] to locate it, or pass a known value.
///
/// For each DX position `p` (stepping by 2, the DX lattice), the RX copy
/// is at `p - FEC_DISTANCE` in the interleaved stream (five symbol-slots
/// earlier, on the RX lattice). Returns one [`Recovered`] per DX position.
pub fn recover_stream(symbols: &[u8], first_dx: usize) -> Vec<Recovered> {
    let mut out = Vec::new();
    let mut p = first_dx;
    while p < symbols.len() {
        let dx = symbols[p];
        let rx_slot = p.checked_sub(FEC_DISTANCE);
        let rx = rx_slot.map(|s| symbols[s]);
        out.push(recover(dx, rx));
        p += 2;
    }
    out
}

/// Decode recovered code words to text, tracking LTRS/FIGS shift and
/// dropping phasing / idle codes.
///
/// Returns the decoded text. Lost characters are rendered as the
/// substitution char `*` (matching common NAVTEX viewers) so positional
/// alignment with the source is preserved; pass `drop_lost = true` to omit
/// them instead.
pub fn codes_to_text(recovered: &[Recovered], drop_lost: bool) -> String {
    let mut figs = false;
    let mut s = String::new();
    for r in recovered {
        let code = match r.code {
            Some(c) => c,
            None => {
                if !drop_lost {
                    s.push('*');
                }
                continue;
            }
        };
        match ccir476::decode(code, figs) {
            ccir476::Decoded::Ltrs => figs = false,
            ccir476::Decoded::Figs => figs = true,
            ccir476::Decoded::Alpha | ccir476::Decoded::Rep | ccir476::Decoded::Idle => {}
            ccir476::Decoded::Char(c) => s.push(c),
            ccir476::Decoded::Unmapped(_) => {
                if !drop_lost {
                    s.push('*');
                }
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccir476::{CODE_ALPHA, CODE_REP};

    // CCIR 476 letter codes (from the oracle table).
    const A: u8 = 0x47;
    const C: u8 = 0x1d;
    const I: u8 = 0x4d;
    const L: u8 = 0x65;
    const N: u8 = 0x59;
    const T: u8 = 0x74;
    const U: u8 = 0x4e;

    /// fldigi's documented interleave of the word "NAUTICAL" (the comment
    /// in `src/navtex/navtex.cxx`, which cites arachnoid.com/JNX):
    ///
    ///   rep alpha rep alpha N alpha A alpha U N T A I U C T A I L C _ A _ L
    ///
    /// where '_' is the RX copy of a character whose RX slot predates the
    /// message (no symbol there yet). DX copies sit at odd indices.
    fn nautical_interleave() -> Vec<u8> {
        // Index:  0    1     2    3     4 5     6 7     8 9 10 11 ...
        // Use a sentinel for the two pre-message RX blanks; any *invalid*
        // code works since the DX copy is what we read there. We use 0x00
        // (zero mark bits => invalid, never confused with a real code).
        const BLANK: u8 = 0x00;
        vec![
            CODE_REP,   // 0  RX phasing
            CODE_ALPHA, // 1  DX phasing
            CODE_REP,   // 2  RX phasing
            CODE_ALPHA, // 3  DX phasing
            N,          // 4  RX N
            CODE_ALPHA, // 5  DX phasing
            A,          // 6  RX A
            CODE_ALPHA, // 7  DX phasing
            U,          // 8  RX U
            N,          // 9  DX N
            T,          // 10 RX T
            A,          // 11 DX A
            I,          // 12 RX I
            U,          // 13 DX U
            C,          // 14 RX C
            T,          // 15 DX T
            A,          // 16 RX A
            I,          // 17 DX I
            L,          // 18 RX L
            C,          // 19 DX C
            BLANK,      // 20 RX (none)
            A,          // 21 DX A
            BLANK,      // 22 RX (none)
            L,          // 23 DX L
        ]
    }

    /// End-to-end FEC-B decode of the externally-published "NAUTICAL"
    /// interleave must reconstruct "NAUTICAL". This anchors the diversity
    /// logic, the 5-character (10-slot) FEC distance, and the shift-aware
    /// text builder against an external worked example — not a loopback.
    #[test]
    fn decodes_nautical_example() {
        let stream = nautical_interleave();
        // First DX data symbol "N" is at index 9.
        let recovered = recover_stream(&stream, 9);
        let text = codes_to_text(&recovered, true);
        assert_eq!(text, "NAUTICAL");
    }

    /// The phase finder must locate the first DX symbol of the message.
    /// For the NAUTICAL stream the first valid repeated DX is "N" at 9.
    #[test]
    fn find_phase_locks_to_data() {
        let stream = nautical_interleave();
        let off = find_phase(&stream).expect("should phase-lock");
        // The located offset must be on the DX (odd) lattice and decode the
        // message correctly from there.
        assert_eq!(off % 2, 1);
        let recovered = recover_stream(&stream, off);
        let text = codes_to_text(&recovered, true);
        assert!(text.contains("NAUTICAL"), "got {text:?}");
    }

    /// When DX is corrupt, the RX copy is used.
    #[test]
    fn rx_fallback_on_corrupt_dx() {
        // DX = invalid (3 ones), RX = valid 'A'.
        let r = recover(0b0000111, Some(A));
        assert_eq!(r.source, CharSource::Rx);
        assert_eq!(r.code, Some(A));
    }

    /// When DX is valid it wins even if RX differs.
    #[test]
    fn dx_preferred_when_valid() {
        let r = recover(A, Some(N));
        assert_eq!(r.source, CharSource::Dx);
        assert_eq!(r.code, Some(A));
    }

    /// Both copies bad => lost.
    #[test]
    fn lost_when_both_bad() {
        let r = recover(0b0000111, Some(0b0000011));
        assert_eq!(r.source, CharSource::Lost);
        assert_eq!(r.code, None);
    }
}
