//! FLEX frame structure, Frame Information Word, block layout, and page decode.
//!
//! # Spec provenance
//!
//! Motorola **FLEX** one-way paging air interface. Constants and field layouts
//! below are cited to the public FLEX protocol description as implemented in
//! multimon-ng `demod_flex.c` (the de-facto open reference for the FLEX
//! protocol; values reproduced from Motorola's FLEX protocol technical
//! summary).
//!
//! A FLEX **frame** lasts 1.875 s and is structured as:
//!
//! ```text
//!   Sync 1   : BS1 dotting | A (32b) | B (16b) | inverted-A (32b)
//!   FIW      : 32-bit Frame Information Word (BCH(31,21)+parity protected)
//!   Sync 2   : bit/frame fine sync
//!   Data     : 11 blocks, each 8 words of 32 bits  (= 88 words / "phase")
//! ```
//!
//! At 1600 bps 2-level FSK there is a single phase of 88 words per frame.
//! (multimon-ng `struct Flex_Phase { uint32_t buf[88]; }`; "88 words per
//! phase".)
//!
//! The 88 data words begin with the **Block Information Word (BIW)** at index 0,
//! which gives the address-field start (`address offset`) and vector-field
//! start (`vector offset`). Address words follow; each address word's matching
//! **Vector Information Word (VIW)** lives at `voffset + i - aoffset`; the VIW's
//! type field selects how the page body (message words) is decoded.

use crate::bch;

/// Words per FLEX phase at 1600 bps (11 blocks x 8 words).
/// (multimon-ng `Flex_Phase::buf[88]`.)
pub const WORDS_PER_PHASE: usize = 88;
/// Blocks per frame.
pub const BLOCKS_PER_FRAME: usize = 11;
/// Words per block.
pub const WORDS_PER_BLOCK: usize = 8;

/// Fixed middle ("B") field of the 64-bit FLEX Sync 1 word.
/// (multimon-ng `#define FLEX_SYNC_MARKER 0xA6C6AAAAul`.)
pub const SYNC_MARKER_B: u32 = 0xA6C6_AAAA;

/// A FLEX page-vector type, from VIW bits 4..=6.
/// (multimon-ng `FLEX_PAGETYPE_*`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageType {
    /// 0 — secure.
    Secure,
    /// 1 — short instruction (group message header).
    ShortInstruction,
    /// 2 — tone-only.
    Tone,
    /// 3 — standard numeric.
    StandardNumeric,
    /// 4 — special-format numeric.
    SpecialNumeric,
    /// 5 — alphanumeric (7-bit characters).
    Alphanumeric,
    /// 6 — binary.
    Binary,
    /// 7 — numbered numeric.
    NumberedNumeric,
}

impl PageType {
    /// Decode the 3-bit VIW type field `(viw >> 4) & 0x7`.
    pub fn from_viw(viw: u32) -> Self {
        match (viw >> 4) & 0x7 {
            0 => PageType::Secure,
            1 => PageType::ShortInstruction,
            2 => PageType::Tone,
            3 => PageType::StandardNumeric,
            4 => PageType::SpecialNumeric,
            5 => PageType::Alphanumeric,
            6 => PageType::Binary,
            _ => PageType::NumberedNumeric,
        }
    }

    /// The bus message-class string emitted in [`crate::MessageBody::Flex`].
    pub fn kind_str(self) -> &'static str {
        match self {
            PageType::Tone | PageType::ShortInstruction | PageType::Secure => "tone",
            PageType::Alphanumeric | PageType::Binary => "alpha",
            PageType::StandardNumeric | PageType::SpecialNumeric | PageType::NumberedNumeric => {
                "numeric"
            }
        }
    }
}

/// Parsed Frame Information Word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fiw {
    /// Cycle number 0..=14 (FIW bits 4..=7).
    pub cycle: u8,
    /// Frame number 0..=127 (FIW bits 8..=14).
    pub frame: u8,
    /// True iff the FLEX FIW mod-16 checksum verified.
    pub checksum_ok: bool,
}

/// Parse a *BCH-corrected* 32-bit Frame Information Word.
///
/// Layout (multimon-ng `decode_fiw`, after masking `fiw & 0x001FFFFF`):
/// checksum = bits 0..=3, cycle = bits 4..=7, frame = bits 8..=14,
/// "fix3" = bits 15..=20.
///
/// Checksum: sum of the 4-bit nibbles [0..=3],[4..=7],[8..=11],[12..=15],
/// [16..=19] plus bit 20, taken mod 16, must equal `0xF`.
pub fn parse_fiw(fiw_word: u32) -> Fiw {
    let fiw = fiw_word & 0x001F_FFFF;
    let cycle = ((fiw >> 4) & 0xF) as u8;
    let frame = ((fiw >> 8) & 0x7F) as u8;
    let sum = (fiw & 0xF)
        + ((fiw >> 4) & 0xF)
        + ((fiw >> 8) & 0xF)
        + ((fiw >> 12) & 0xF)
        + ((fiw >> 16) & 0xF)
        + ((fiw >> 20) & 0x1);
    let checksum_ok = (sum & 0xF) == 0xF;
    Fiw {
        cycle,
        frame,
        checksum_ok,
    }
}

/// Parsed Block Information Word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Biw {
    /// First word index of the address field (`((biw >> 8) & 3) + 1`).
    pub address_offset: usize,
    /// First word index of the vector field (`(biw >> 10) & 0x3F`).
    pub vector_offset: usize,
}

/// Parse a *BCH-corrected* Block Information Word (the first data word).
///
/// (multimon-ng `decode_biw`: vector offset = `(biw >> 10) & 0x3f`,
/// address offset = `((biw >> 8) & 0x03) + 1`.)
pub fn parse_biw(biw_word: u32) -> Biw {
    let biw = biw_word & 0x001F_FFFF;
    Biw {
        address_offset: (((biw >> 8) & 0x03) + 1) as usize,
        vector_offset: ((biw >> 10) & 0x3F) as usize,
    }
}

/// A FLEX address (capcode) decoded from one or two address words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// Decoded capcode.
    pub capcode: u32,
    /// True for the long (two-word) address form.
    pub long: bool,
}

/// Decode an address word into a capcode and the long-address flag.
///
/// Per multimon-ng `demod_flex.c`:
/// ```c
/// flex->Decode.long_address = (aw1 < 0x008001L) || (aw1 > 0x1E0000L) || (aw1 > 0x1E7FFEL);
/// flex->Decode.capcode = aw1 - 0x8000;
/// ```
/// The active reference emits `aw1 - 0x8000` as the capcode regardless of the
/// long flag; the full TWO-word long-capcode reconstruction is *commented out*
/// in the reference ("Don't ask") and is not reliably specified. This crate
/// therefore reports the documented `long` flag and the `aw1 - 0x8000` capcode
/// (the first word of a long pair); fusing the second long-address word is
/// SKIPPED rather than faked — see crate notes.
pub fn decode_short_address(aw1: u32) -> Address {
    let aw1 = aw1 & 0x001F_FFFF;
    let long = aw1 < 0x0000_8001 || aw1 > 0x001E_0000;
    Address {
        capcode: aw1.wrapping_sub(0x8000),
        long,
    }
}

/// FLEX numeric character table (4-bit BCD groups).
/// (multimon-ng numeric table `"0123456789 U -][ "`.) Index = the 4-bit value.
const NUMERIC_TABLE: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ' ', 'U', ' ', '-', ']', '[',
];

/// Decode an **alphanumeric** page body: 7-bit ASCII characters packed LSB-first
/// into the 21 data bits of consecutive message words (3 chars per word).
///
/// FLEX packs alphanumeric text 7 bits per character, least-significant bit
/// first: char0 = bits 0..=6, char1 = bits 7..=13, char2 = bits 14..=20 of each
/// message word. (multimon-ng `parse_alphanumeric`: `dw & 0x7F`, `(dw>>7)&0x7F`,
/// `(dw>>14)&0x7F`.) `0x03` (ETX) bytes are message-segment terminators and are
/// dropped; trailing control/NUL padding is trimmed.
pub fn decode_alpha(words: &[u32]) -> String {
    let mut out = String::new();
    for &w in words {
        let data = w & 0x001F_FFFF;
        for c in 0..3 {
            let ch = ((data >> (c * 7)) & 0x7F) as u8;
            // 0x03 (ETX) is a FLEX message terminator/separator — skip it
            // (multimon-ng: `if (ch != 0x03)`).
            if ch == 0x03 {
                continue;
            }
            out.push(ch as char);
        }
    }
    while matches!(out.chars().last(), Some(c) if (c as u32) < 0x20) {
        out.pop();
    }
    out
}

/// Decode a **numeric** page body: 4-bit groups packed LSB-first into the 21
/// data bits of each message word (the low 21 bits), mapped via the FLEX
/// numeric table. (multimon-ng `parse_numeric`.)
pub fn decode_numeric(words: &[u32]) -> String {
    let mut out = String::new();
    for &w in words {
        let data = w & 0x001F_FFFF;
        // Up to 5 nibbles (20 bits) of digit data per word, LSB-first.
        for n in 0..5 {
            let nib = ((data >> (n * 4)) & 0xF) as usize;
            out.push(NUMERIC_TABLE[nib]);
        }
    }
    // Trim trailing pad spaces.
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Build a valid 32-bit FLEX word from 21 data bits, for tests / re-encode.
pub fn encode_word(data21: u32) -> u32 {
    bch::encode(data21)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIW checksum + field extraction against a spec-constructed word.
    ///
    /// Build a FIW with cycle=5, frame=42 and a checksum nibble chosen so the
    /// mod-16 sum equals 0xF (per multimon-ng `decode_fiw`), BCH-encode it, then
    /// assert [`parse_fiw`] recovers the fields and validates the checksum.
    #[test]
    fn fiw_fields_and_checksum_roundtrip() {
        let cycle = 5u32;
        let frame = 42u32;
        // Place fields: cycle bits 4..=7, frame bits 8..=14, fix3 (bits15..=20)=0.
        let body = (cycle << 4) | (frame << 8);
        // Choose checksum nibble c (bits 0..=3) so total nibble sum mod 16 = 0xF.
        let partial = ((body >> 4) & 0xF)
            + ((body >> 8) & 0xF)
            + ((body >> 12) & 0xF)
            + ((body >> 16) & 0xF)
            + ((body >> 20) & 0x1);
        let c = (0xFu32.wrapping_sub(partial)) & 0xF;
        let fiw = body | c;
        let parsed = parse_fiw(fiw);
        assert_eq!(parsed.cycle, 5);
        assert_eq!(parsed.frame, 42);
        assert!(parsed.checksum_ok, "constructed FIW checksum must verify");

        // A corrupted frame number must break the checksum.
        let bad = parse_fiw(fiw ^ (1 << 8));
        assert!(!bad.checksum_ok);
    }

    /// BIW offsets against a spec-constructed word: address offset = (bits8..9)+1,
    /// vector offset = bits 10..15.
    #[test]
    fn biw_offsets_match_spec_layout() {
        // address offset field = 0b10 -> aoffset = 3; vector offset = 9.
        let biw = (0b10u32 << 8) | (9u32 << 10);
        let parsed = parse_biw(biw);
        assert_eq!(parsed.address_offset, 3);
        assert_eq!(parsed.vector_offset, 9);
    }

    /// Address decode against the multimon-ng formula `capcode = aw1 - 0x8000`
    /// and the documented `long_address` window condition.
    #[test]
    fn short_address_capcode() {
        let aw1 = 0x8000 + 1_234_567;
        let a = decode_short_address(aw1);
        assert_eq!(a.capcode, 1_234_567);
        assert!(!a.long, "value inside short window must be short address");
        // Below 0x8001 flags long (aw1 < 0x008001).
        assert!(decode_short_address(0x0010).long);
        // Above 0x1E0000 flags long; capcode is still aw1 - 0x8000.
        let high = decode_short_address(0x1E_0001);
        assert!(high.long);
        assert_eq!(high.capcode, 0x1E_0001 - 0x8000);
    }

    /// Alphanumeric: pack "Hi!" as 7-bit LSB-first, 3 chars in one 21-bit word,
    /// and assert decode recovers it.
    #[test]
    fn alpha_decode_lsb_first_7bit() {
        let chars = b"Hi!";
        let mut data = 0u32;
        for (i, &ch) in chars.iter().enumerate() {
            data |= ((ch as u32) & 0x7F) << (i * 7);
        }
        assert_eq!(decode_alpha(&[data]), "Hi!");
    }

    /// Alphanumeric across two words ("HELLO" = 5 chars -> word0 holds HEL,
    /// word1 holds LO + pad).
    #[test]
    fn alpha_decode_two_words() {
        let msg = b"HELLO";
        let mut words = [0u32; 2];
        for (i, &ch) in msg.iter().enumerate() {
            let w = i / 3;
            let slot = i % 3;
            words[w] |= ((ch as u32) & 0x7F) << (slot * 7);
        }
        assert_eq!(decode_alpha(&words), "HELLO");
    }

    /// Numeric: pack digits "12345" as 4-bit LSB-first groups via the FLEX
    /// numeric table, one word, and assert decode.
    #[test]
    fn numeric_decode_4bit_table() {
        let digits = [1u32, 2, 3, 4, 5];
        let mut data = 0u32;
        for (i, &d) in digits.iter().enumerate() {
            data |= d << (i * 4);
        }
        assert_eq!(decode_numeric(&[data]), "12345");
    }

    #[test]
    fn page_type_from_viw() {
        assert_eq!(PageType::from_viw(2 << 4), PageType::Tone);
        assert_eq!(PageType::from_viw(3 << 4), PageType::StandardNumeric);
        assert_eq!(PageType::from_viw(5 << 4), PageType::Alphanumeric);
        assert_eq!(PageType::Alphanumeric.kind_str(), "alpha");
        assert_eq!(PageType::Tone.kind_str(), "tone");
        assert_eq!(PageType::StandardNumeric.kind_str(), "numeric");
    }
}
