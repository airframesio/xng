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
//! - §2.3 **Message codeword** (flag bit = 1): bits 2..=21 are 20 message
//!   bits. Consecutive message codewords (until the next address codeword,
//!   idle codeword, or end of transmission) are concatenated MSB-first into a
//!   single bit stream, then decoded as either:
//!     * **Numeric** — 4 bits per digit, *bit-reversed*, mapping
//!       `0-9`, then spare/`U`(urgency)/space/`-`/`[`/`]` per the §2.3 table;
//!     * **Alphanumeric** — 7 bits per character, **LSB-first**, ASCII.
//!
//! The numeric/alphanumeric choice is signalled out-of-band (by the function
//! bits / paging operator convention); this decoder exposes both decodings and
//! the caller selects per the function code.

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

/// POCSAG numeric character table, ITU-R M.584-2 Annex 1 §2.3 (Table for the
/// 16 numeric code values, after bit reversal of each 4-bit group):
/// 0-9, then spare, `U`, space, `-`, `]`(`)`), `[`(`(`).
/// Index = the bit-reversed nibble value 0..=15.
const NUMERIC_TABLE: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ' ', 'U', ' ', '-', ']', '[',
];

/// Decode a concatenated message-codeword bit stream as **numeric** paging:
/// 4 bits per digit, each 4-bit group bit-reversed, mapped via the §2.3 table.
///
/// `bits` is the MSB-first concatenation of all message-codeword 20-bit
/// payloads. A trailing partial group (< 4 bits) is ignored.
pub fn decode_numeric(bits: &[u8]) -> String {
    bits.chunks_exact(4)
        .map(|g| {
            // Bit-reverse the 4-bit group (POCSAG numeric sends LSB-first within
            // the nibble, so reverse to read it as a normal value).
            let mut v = 0u8;
            for (i, &b) in g.iter().enumerate() {
                if b != 0 {
                    v |= 1 << (3 - i);
                }
            }
            NUMERIC_TABLE[v as usize]
        })
        .collect()
}

/// Decode a concatenated message-codeword bit stream as **alphanumeric**
/// paging: 7 bits per character, **LSB-first**, mapped to ASCII (ITU-R M.584-2
/// §2.3). A trailing partial group (< 7 bits) is ignored. NUL padding and
/// control chars below 0x20 (except none expected) are dropped from the tail.
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

    /// Numeric decode against a hand-built payload. Encode the digits
    /// "12345" by placing each digit's value in a bit-reversed 4-bit group, as
    /// the spec specifies, then assert decode recovers the digits.
    #[test]
    fn numeric_decode_matches_spec_layout() {
        let digits = [1u8, 2, 3, 4, 5];
        let mut bits = Vec::new();
        for &d in &digits {
            // Find the numeric-table index whose char is this digit, then emit
            // its 4 bits LSB-first (decode bit-reverses back).
            let idx = NUMERIC_TABLE
                .iter()
                .position(|&c| c == (b'0' + d) as char)
                .unwrap() as u8;
            // Emit group such that decode's bit-reverse yields `idx`.
            // decode reads g[i] into bit (3-i); so to encode value `idx`,
            // group bit i = (idx >> (3-i)) & 1.
            for i in 0..4 {
                bits.push((idx >> (3 - i)) & 1);
            }
        }
        assert_eq!(decode_numeric(&bits), "12345");
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

    #[test]
    fn message_bits_is_msb_first() {
        // payload 0xF_FFFF is 20 ones; 0x0_0001 is a single LSB one.
        let bits = message_bits(&[0x0_0001]);
        assert_eq!(bits.len(), 20);
        assert_eq!(bits[19], 1);
        assert!(bits[..19].iter().all(|&b| b == 0));
    }
}
