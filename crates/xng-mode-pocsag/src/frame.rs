//! POCSAG batch / codeword framing and payload decoding.
//!
//! # Spec provenance
//!
//! ITU-R Recommendation M.584-2, Annex 1 ("The radiopaging code No.1"):
//!
//! - §2.1 **Preamble**: at least 576 bits of alternating 1/0 (the reversal
//!   sequence) precede the first batch, for bit-clock recovery.
//! - §2.4 **Batch**: each batch begins with the synchronisation codeword
//!   `0x7CD215D8` followed by **8 frames**, each frame being **2 codewords**
//!   of 32 bits → 16 codewords per batch, 17 words including sync.
//! - §2.2 **Address codeword** (flag bit = 0): bits 2..=19 carry the most
//!   significant 18 bits of the address; the receiver's *frame position*
//!   (0..=7 — which of the 8 frames the codeword fell in) supplies the 3 least
//!   significant address bits. The full pager number ("capcode") is therefore
//!   `(address18 << 3) | frame_position`. Bits 20..=21 are the 2 **function
//!   bits** (selecting one of four message types / tone alerts).
//! - §1.3.3 **Message codeword** (flag bit = 1): bits 2..=21 are 20 message
//!   bits. Consecutive message codewords (until the next address codeword,
//!   idle codeword, or end of transmission) are concatenated MSB-first into a
//!   single bit stream, then decoded as either:
//!     * **Numeric** (§2.1, Table 3) — 4 bits per character. The four bits of
//!       each character are *transmitted bit-No.1 first* (LSB first), and the
//!       table value `V = (b4 b3 b2 b1)` maps `0-9`, then spare(10)=`.`,
//!       `U`(11), space(12), hyphen(13), `]`(14), `[`(15).
//!     * **Alphanumeric** (§2.2) — 7 bits per character, **bit-No.1 first**
//!       (LSB first), ISO 646 (ASCII). A character may be split across the
//!       20-bit codeword boundary (3 chars = 21 bits > 20), so the whole
//!       message is partitioned into contiguous 20-bit blocks and 7-bit
//!       characters are read straight across those blocks.
//!
//! The numeric/alphanumeric choice is signalled by the address codeword's
//! function bits (§2.1: numeric uses function `00`; §2.2: alphanumeric uses
//! function `11`); this decoder exposes both decodings and the caller selects
//! per the function code.

use crate::bch;

/// Codewords following the sync word in one batch (8 frames × 2).
pub const CODEWORDS_PER_BATCH: usize = 16;
/// Minimum POCSAG preamble length in bits (ITU-R M.584-2 §2.1).
pub const PREAMBLE_MIN_BITS: usize = 576;

/// One decoded codeword's role within a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Codeword {
    /// Address codeword: full capcode and 2 function bits.
    Address { capcode: u32, function: u8 },
    /// Message (text/data) codeword carrying 20 payload bits (MSB-first in the
    /// low 20 bits).
    Message { payload20: u32 },
    /// Idle codeword (`0x7A89C197`): no information.
    Idle,
}

/// Classify a *corrected* 32-bit codeword by its flag bit, knowing which frame
/// position (0..=7) it occupied (needed for the 3 low address bits).
pub fn classify(cw: u32, frame_position: u8) -> Codeword {
    if cw == bch::IDLE_CODEWORD {
        return Codeword::Idle;
    }
    let flag = (cw >> 31) & 1;
    if flag == 0 {
        // Address codeword. Bits 31..=14 (18 bits) are address[17:0] MSBs;
        // bits 13..=12 (2 bits) are the function bits.
        let address18 = (cw >> 13) & 0x3_FFFF;
        let function = ((cw >> 11) & 0x3) as u8;
        let capcode = (address18 << 3) | (frame_position as u32 & 0x7);
        Codeword::Address { capcode, function }
    } else {
        // Message codeword. Bits 30..=11 (20 bits) are payload, MSB-first.
        let payload20 = (cw >> 11) & 0xF_FFFF;
        Codeword::Message { payload20 }
    }
}

/// POCSAG "numeric-only" character set, ITU-R M.584-2 Annex 1 §2.1, **Table 3**.
///
/// The table is indexed by the 4-bit character *value* `V = (b4 b3 b2 b1)` (the
/// "4-bit Combination" column read as a binary number, bit No.4 = MSB):
///
/// | V  | bits b4 b3 b2 b1 | Table 3 entry          | char |
/// |----|------------------|------------------------|------|
/// | 0–9| `0000`..`1001`   | `0`..`9`               | digit|
/// | 10 | `1010`           | Spare                  | `.`  |
/// | 11 | `1011`           | U (urgency indicator)  | `U`  |
/// | 12 | `1100`           | Space                  | ` `  |
/// | 13 | `1101`           | Hyphen                 | `-`  |
/// | 14 | `1110`           | closing bracket        | `]`  |
/// | 15 | `1111`           | opening bracket        | `[`  |
///
/// Spare (10) has no defined glyph; we render `.` (the de-facto convention, and
/// what the multimon-ng reference decoder emits) so a spare position is visible
/// without being mistaken for a space or a digit.
const NUMERIC_TABLE: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', 'U', ' ', '-', ']', '[',
];

/// Decode a concatenated message-codeword bit stream as **numeric** paging
/// (ITU-R M.584-2 Annex 1 §2.1 + Table 3): 4 bits per character.
///
/// `bits` is the on-air MSB-first concatenation of all message-codeword 20-bit
/// payloads, so within each 4-bit group the bits appear earliest→latest as
/// `b1, b2, b3, b4` (§2.1: "the bits of each character are transmitted … starting
/// with bit No. 1", i.e. LSB first). We rebuild the character value
/// `V = (b4 b3 b2 b1)` by placing the earliest received bit in the LSB, then map
/// it through the `NUMERIC_TABLE` (Table 3). A trailing partial group (< 4 bits)
/// is ignored.
pub fn decode_numeric(bits: &[u8]) -> String {
    bits.chunks_exact(4)
        .map(|g| {
            // g[0] is bit No.1 (LSB), g[3] is bit No.4 (MSB): earliest → LSB.
            let mut v = 0u8;
            for (i, &b) in g.iter().enumerate() {
                if b != 0 {
                    v |= 1 << i;
                }
            }
            NUMERIC_TABLE[v as usize]
        })
        .collect()
}

/// Decode a concatenated message-codeword bit stream as **alphanumeric**
/// paging (ITU-R M.584-2 Annex 1 §2.2): 7 bits per character, transmitted
/// **bit No.1 first** (LSB first), mapped to ISO 646 / ASCII.
///
/// Continuation across codewords is intrinsic: §2.2 says "the complete message
/// is partitioned into contiguous 20-bit blocks … a character may be split
/// between one message codeword and the next." Because [`message_bits`] first
/// concatenates every 20-bit payload into one stream, then this function chunks
/// it into 7-bit characters, a character that straddles the 20-bit boundary
/// (e.g. the 3rd char, since 3·7 = 21 > 20) is reassembled with no special
/// case. A trailing partial group (< 7 bits) is ignored. §2.2 fills the unused
/// tail of the last codeword with non-printing chars (EOT/ETX/NUL); we drop
/// trailing control bytes (< 0x20) so the visible text is clean.
pub fn decode_alpha(bits: &[u8]) -> String {
    let mut out = String::new();
    for g in bits.chunks_exact(7) {
        let mut c = 0u8;
        for (i, &b) in g.iter().enumerate() {
            if b != 0 {
                c |= 1 << i; // LSB-first
            }
        }
        out.push(c as char);
    }
    // Pagers pad the final character(s) with NUL/EOT; trim trailing control
    // bytes so the visible text is clean.
    while matches!(out.chars().last(), Some(c) if (c as u32) < 0x20) {
        out.pop();
    }
    out
}

/// Concatenate the 20-bit payloads of a sequence of message codewords into one
/// MSB-first bit vector (for [`decode_numeric`] / [`decode_alpha`]).
pub fn message_bits(payloads: &[u32]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(payloads.len() * 20);
    for &p in payloads {
        for i in (0..20).rev() {
            bits.push(((p >> i) & 1) as u8);
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid address codeword from the spec field layout, for a known
    /// capcode + function, and assert [`classify`] recovers them.
    ///
    /// Per ITU-R M.584-2 §2.2: capcode low 3 bits = frame position; the upper
    /// 18 bits go in codeword bits 31..=14; 2 function bits in 13..=12; flag=0.
    #[test]
    fn address_codeword_roundtrips_capcode_and_function() {
        let capcode = 1_234_567u32; // arbitrary in-range pager number
        let function = 0b10u8;
        let frame_position = (capcode & 0x7) as u8;
        let address18 = capcode >> 3;
        // Assemble the 21 data bits: flag(0) | addr18 | fn2.
        let data21 = (address18 << 2) | function as u32;
        let cw = bch::encode(data21);
        assert!(bch::is_valid(cw));
        match classify(cw, frame_position) {
            Codeword::Address { capcode: c, function: f } => {
                assert_eq!(c, capcode, "capcode mismatch");
                assert_eq!(f, function, "function mismatch");
            }
            other => panic!("expected Address, got {other:?}"),
        }
    }

    #[test]
    fn idle_codeword_classifies_as_idle() {
        assert_eq!(classify(bch::IDLE_CODEWORD, 0), Codeword::Idle);
    }

    /// Emit one numeric character's 4 bits onto the on-air bit stream exactly as
    /// ITU-R M.584-2 Annex 1 §2.1 + Table 3 specify: the character value
    /// `V = (b4 b3 b2 b1)` is transmitted **bit No.1 (LSB) first**. This is the
    /// independent, spec-cited encoder used by the numeric decode tests (it is
    /// NOT [`decode_numeric`]'s inverse-by-construction — it is hand-derived from
    /// the spec's transmission order).
    fn spec_emit_numeric_value(bits: &mut Vec<u8>, v: u8) {
        for i in 0..4 {
            bits.push((v >> i) & 1); // bit No.(i+1) first: i=0 is LSB (b1)
        }
    }

    /// Numeric decode against a SPEC-CITED on-air bit layout (ITU-R M.584-2
    /// Annex 1 §2.1 + Table 3). We build the bit stream directly from the spec's
    /// "bit No.1 first" rule for the values of the digits "12345" and assert the
    /// decoder recovers them. Ground truth is the spec ordering, not a decoder
    /// round-trip.
    #[test]
    fn numeric_decode_matches_spec_layout() {
        let mut bits = Vec::new();
        for v in [1u8, 2, 3, 4, 5] {
            spec_emit_numeric_value(&mut bits, v);
        }
        assert_eq!(decode_numeric(&bits), "12345");
    }

    /// Spec ground-truth for the exact Table 3 bit patterns. We assert, per row,
    /// that the 4 on-air bits `b1 b2 b3 b4` decode to the displayed character.
    /// These are the literal Table 3 "4-bit Combination" rows (header `4 3 2 1`)
    /// turned into transmission order (bit No.1 first).
    #[test]
    fn numeric_table3_exact_rows_decode_per_spec() {
        // (value V = b4 b3 b2 b1, expected char) straight from Table 3.
        let rows: [(u8, char); 16] = [
            (0b0000, '0'),
            (0b0001, '1'),
            (0b0010, '2'),
            (0b0011, '3'),
            (0b0100, '4'),
            (0b0101, '5'),
            (0b0110, '6'),
            (0b0111, '7'),
            (0b1000, '8'),
            (0b1001, '9'),
            (0b1010, '.'), // Spare (no glyph in spec); rendered '.'
            (0b1011, 'U'), // U (urgency indicator)
            (0b1100, ' '), // Space
            (0b1101, '-'), // Hyphen
            (0b1110, ']'), // closing bracket
            (0b1111, '['), // opening bracket
        ];
        for (v, ch) in rows {
            let mut bits = Vec::new();
            spec_emit_numeric_value(&mut bits, v);
            assert_eq!(
                decode_numeric(&bits),
                ch.to_string(),
                "Table 3 row V={v:#06b} expected {ch:?}"
            );
        }
    }

    /// Decode the special characters U / space / hyphen / brackets together, as a
    /// realistic numeric page would carry them, from spec-ordered bits.
    #[test]
    fn numeric_special_chars_decode() {
        // "12-34 U[5]" exercising hyphen(13), space(12), U(11), [(15), ](14).
        let seq: [(u8, char); 10] = [
            (1, '1'),
            (2, '2'),
            (13, '-'),
            (3, '3'),
            (4, '4'),
            (12, ' '),
            (11, 'U'),
            (15, '['),
            (5, '5'),
            (14, ']'),
        ];
        let mut bits = Vec::new();
        for (v, _) in seq {
            spec_emit_numeric_value(&mut bits, v);
        }
        let expected: String = seq.iter().map(|&(_, c)| c).collect();
        assert_eq!(decode_numeric(&bits), expected);
    }

    /// Alphanumeric decode: 7-bit LSB-first ASCII. Build "Hi" by hand.
    #[test]
    fn alpha_decode_lsb_first_ascii() {
        let mut bits = Vec::new();
        for &ch in b"Hi" {
            for i in 0..7 {
                bits.push((ch >> i) & 1); // LSB-first
            }
        }
        assert_eq!(decode_alpha(&bits), "Hi");
    }

    /// SPEC-CITED alphanumeric continuation across codeword boundaries
    /// (ITU-R M.584-2 Annex 1 §2.2: "a character may be split between one
    /// message codeword and the next").
    ///
    /// We pack a 5-character string LSB-first (5·7 = 35 bits) into 20-bit
    /// codeword payloads via the real [`message_bits`] path, so the boundary
    /// falls *inside* the 3rd character (chars start at bit 0, 7, 14, 21, 28; the
    /// 3rd char spans bits 14..21, straddling the 20-bit edge). The decoder must
    /// reassemble it. We assert via the full payload→bits→chars pipeline, not a
    /// flat 7-bit buffer, to prove the boundary handling.
    #[test]
    fn alpha_continuation_spans_codewords() {
        let text = b"PAGER"; // 35 bits → spans two 20-bit codewords
        let mut flat = Vec::new();
        for &ch in text {
            for i in 0..7 {
                flat.push((ch >> i) & 1); // §2.2: bit No.1 (LSB) first
            }
        }
        // Partition into contiguous 20-bit codeword payloads (MSB-first within
        // each payload, exactly as message_bits reconstructs them on decode).
        let mut payloads = Vec::new();
        for chunk in flat.chunks(20) {
            let mut p = 0u32;
            for (i, &b) in chunk.iter().enumerate() {
                // message_bits emits bit (19-i) of the payload first, so to put
                // `chunk[i]` at stream position i we set payload bit (19 - i).
                if b != 0 {
                    p |= 1 << (19 - i);
                }
            }
            payloads.push(p);
        }
        // Sanity: 35 bits needs 2 payloads (20 + 15), so the 3rd char straddles.
        assert_eq!(payloads.len(), 2, "fixture must span two codewords");
        let rebuilt = message_bits(&payloads);
        // message_bits zero-pads the 2nd payload's tail; that yields trailing
        // control/NUL chars which decode_alpha trims.
        assert_eq!(decode_alpha(&rebuilt), "PAGER");
    }

    #[test]
    fn message_bits_is_msb_first() {
        // payload 0xF_FFFF is 20 ones; 0x0_0001 is a single LSB one.
        let bits = message_bits(&[0x0_0001]);
        assert_eq!(bits.len(), 20);
        assert_eq!(bits[19], 1);
        assert!(bits[..19].iter().all(|&b| b == 0));
    }
}
