//! CCIR 476 (ITU-R 476) seven-bit constant-ratio code.
//!
//! Every valid code word has exactly four mark (1) bits and three space
//! (0) bits — a 4-of-7 constant-ratio code, which gives single-error
//! *detection* (any single bit flip changes the 1-count away from four).
//!
//! Two oracles were used to populate the tables and both agree exactly on
//! every printable letter/figure and the control codes (see PROVENANCE.md):
//!
//! - fldigi `src/navtex/navtex.cxx` (`code_to_ltrs` / `code_to_figs`).
//! - pd0wm/navtex `navtex.py` (`ALPHABET_LTRS` / `ALPHABET_FIGS`).
//!
//! Bit ordering: a code is packed LSB-first from the seven received bit
//! decisions — bit *i* is set when symbol *i* is a mark. This matches
//! fldigi `bytes_to_code`: `code |= (pos[i] > 0) << i`.

/// LTRS-shift control code (switch to letters).
pub const CODE_LTRS: u8 = 0x5a;
/// FIGS-shift control code (switch to figures/symbols).
pub const CODE_FIGS: u8 = 0x36;
/// Phasing signal 1 ("alpha"); the DX-channel idle/phasing character.
pub const CODE_ALPHA: u8 = 0x0f;
/// Phasing signal 2 ("rep"); the RX-channel idle/phasing character.
pub const CODE_REP: u8 = 0x66;
/// "Beta" idle (signal repetition request in ARQ; idle here).
pub const CODE_BETA: u8 = 0x33;
/// "Char 32" / unperforated tape / idle.
pub const CODE_CHAR32: u8 = 0x6a;

/// LTRS (letters) shift: code (0..128) → ASCII char, `'_'` if not a
/// letter-shift glyph.
///
/// Verbatim from fldigi `code_to_ltrs` (cross-checked against pd0wm
/// `ALPHABET_LTRS`). `\n` / `\r` are real line breaks; `'_'` marks a
/// code that has no letters-shift glyph.
#[rustfmt::skip]
pub const CODE_TO_LTRS: [char; 128] = [
    //0   1    2    3    4    5    6    7    8    9    a    b    c    d    e    f
    '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', // 0
    '_', '_', '_', '_', '_', '_', '_', 'J', '_', '_', '_', 'F', '_', 'C', 'K', '_', // 1
    '_', '_', '_', '_', '_', '_', '_', 'W', '_', '_', '_', 'Y', '_', 'P', 'Q', '_', // 2
    '_', '_', '_', '_', '_', 'G', '_', '_', '_', 'M', 'X', '_', 'V', '_', '_', '_', // 3
    '_', '_', '_', '_', '_', '_', '_', 'A', '_', '_', '_', 'S', '_', 'I', 'U', '_', // 4
    '_', '_', '_', 'D', '_', 'R', 'E', '_', '_', 'N', '_', '_', ' ', '_', '_', '_', // 5
    '_', '_', '_', 'Z', '_', 'L', '_', '_', '_', 'H', '_', '_', '\n', '_', '_', '_', // 6
    '_', 'O', 'B', '_', 'T', '_', '_', '_', '\r', '_', '_', '_', '_', '_', '_', '_', // 7
];

/// FIGS (figures/symbols) shift: code (0..128) → ASCII char, `'_'` if not
/// a figures-shift glyph.
///
/// Verbatim from fldigi `code_to_figs` (cross-checked against pd0wm
/// `ALPHABET_FIGS`). `\x07` is BELL.
#[rustfmt::skip]
pub const CODE_TO_FIGS: [char; 128] = [
    //0   1    2    3    4    5    6    7    8    9    a    b    c    d    e    f
    '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', // 0
    '_', '_', '_', '_', '_', '_', '_', '\'', '_', '_', '_', '!', '_', ':', '(', '_', // 1
    '_', '_', '_', '_', '_', '_', '_', '2', '_', '_', '_', '6', '_', '0', '1', '_', // 2
    '_', '_', '_', '_', '_', '&', '_', '_', '_', '.', '/', '_', ';', '_', '_', '_', // 3
    '_', '_', '_', '_', '_', '_', '_', '-', '_', '_', '_', '\x07', '_', '8', '7', '_', // 4
    '_', '_', '_', '$', '_', '4', '3', '_', '_', ',', '_', '_', ' ', '_', '_', '_', // 5
    '_', '_', '_', '"', '_', ')', '_', '_', '_', '#', '_', '_', '\n', '_', '_', '_', // 6
    '_', '9', '?', '_', '5', '_', '_', '_', '\r', '_', '_', '_', '_', '_', '_', '_', // 7
];

/// True when `code` is a valid CCIR 476 code word (exactly four mark bits).
///
/// This is the constant-ratio parity check. fldigi `check_bits`: a code is
/// valid iff its population count is 4.
#[inline]
pub fn is_valid_code(code: u8) -> bool {
    code.count_ones() == 4
}

/// Pack seven bit-decisions into a CCIR 476 code, LSB-first.
///
/// `bits[i]` is treated as a mark when `> 0` (fldigi passes signed
/// soft-decision accumulators; `bool`/`0|1` work the same). Matches fldigi
/// `bytes_to_code`.
#[inline]
pub fn pack_bits(bits: &[i32; 7]) -> u8 {
    let mut code = 0u8;
    for (i, &b) in bits.iter().enumerate() {
        if b > 0 {
            code |= 1 << i;
        }
    }
    code
}

/// One decoded CCIR 476 code in the current shift, or a recognised control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// A printable glyph in the active shift.
    Char(char),
    /// Switch to letters shift.
    Ltrs,
    /// Switch to figures shift.
    Figs,
    /// Phasing signal 1 (alpha) — DX idle.
    Alpha,
    /// Phasing signal 2 (rep) — RX idle.
    Rep,
    /// Other idle / control with no text effect (beta, char32, ...).
    Idle,
    /// Valid 4-of-7 code with no glyph in either shift.
    Unmapped(u8),
}

/// Decode a (valid) code word to text/control given the current shift.
///
/// `figs_shift` selects the figures table; `false` selects letters.
/// Control codes are returned as their [`Decoded`] variants regardless of
/// shift. A glyph that exists only in the *other* shift is still returned
/// (callers manage shift via [`Decoded::Ltrs`]/[`Decoded::Figs`]); a code
/// with no glyph in the active table falls back to [`Decoded::Unmapped`].
pub fn decode(code: u8, figs_shift: bool) -> Decoded {
    match code {
        CODE_LTRS => return Decoded::Ltrs,
        CODE_FIGS => return Decoded::Figs,
        CODE_ALPHA => return Decoded::Alpha,
        CODE_REP => return Decoded::Rep,
        CODE_BETA | CODE_CHAR32 => return Decoded::Idle,
        _ => {}
    }
    let table = if figs_shift { &CODE_TO_FIGS } else { &CODE_TO_LTRS };
    let glyph = table[code as usize];
    if glyph != '_' {
        Decoded::Char(glyph)
    } else {
        Decoded::Unmapped(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code that maps to a glyph (in either shift) must be a valid
    /// 4-of-7 constant-ratio code word, and conversely the control codes
    /// are all valid too. Anchored to the oracle tables.
    #[test]
    fn glyph_codes_are_constant_ratio() {
        for code in 0u8..128 {
            let l = CODE_TO_LTRS[code as usize];
            let f = CODE_TO_FIGS[code as usize];
            if l != '_' || f != '_' {
                assert!(
                    is_valid_code(code),
                    "code {code:#x} maps to a glyph but is not 4-of-7"
                );
            }
        }
        for c in [CODE_LTRS, CODE_FIGS, CODE_ALPHA, CODE_REP, CODE_BETA, CODE_CHAR32] {
            assert!(is_valid_code(c), "control {c:#x} not 4-of-7");
        }
    }

    /// Spot-check the letter codes against the published CCIR 476 values
    /// (fldigi / pd0wm). These are externally-pinned facts, not derived.
    #[test]
    fn known_letter_codes() {
        assert_eq!(CODE_TO_LTRS[0x47], 'A');
        assert_eq!(CODE_TO_LTRS[0x72], 'B');
        assert_eq!(CODE_TO_LTRS[0x1d], 'C');
        assert_eq!(CODE_TO_LTRS[0x56], 'E');
        assert_eq!(CODE_TO_LTRS[0x59], 'N');
        assert_eq!(CODE_TO_LTRS[0x74], 'T');
        assert_eq!(CODE_TO_LTRS[0x4e], 'U');
        assert_eq!(CODE_TO_LTRS[0x63], 'Z');
        assert_eq!(CODE_TO_LTRS[0x5c], ' ');
    }

    /// Figures-shift digits, pinned to the oracle tables.
    #[test]
    fn known_figure_codes() {
        assert_eq!(CODE_TO_FIGS[0x2d], '0');
        assert_eq!(CODE_TO_FIGS[0x2e], '1');
        assert_eq!(CODE_TO_FIGS[0x27], '2');
        assert_eq!(CODE_TO_FIGS[0x56], '3');
        assert_eq!(CODE_TO_FIGS[0x55], '4');
        assert_eq!(CODE_TO_FIGS[0x74], '5');
        assert_eq!(CODE_TO_FIGS[0x2b], '6');
        assert_eq!(CODE_TO_FIGS[0x4e], '7');
        assert_eq!(CODE_TO_FIGS[0x4d], '8');
        assert_eq!(CODE_TO_FIGS[0x71], '9');
    }

    /// LSB-first bit packing matches fldigi `bytes_to_code`.
    #[test]
    fn pack_is_lsb_first() {
        // 0x47 = 0b1000111 -> bits[0,1,2]=1, bits[6]=1.
        let bits = [1, 1, 1, 0, 0, 0, 1];
        assert_eq!(pack_bits(&bits), 0x47);
        assert_eq!(decode(pack_bits(&bits), false), Decoded::Char('A'));
    }

    #[test]
    fn shift_switches_table() {
        // 0x47 is 'A' in letters, '-' in figures.
        assert_eq!(decode(0x47, false), Decoded::Char('A'));
        assert_eq!(decode(0x47, true), Decoded::Char('-'));
    }

    #[test]
    fn controls_decode() {
        assert_eq!(decode(CODE_LTRS, true), Decoded::Ltrs);
        assert_eq!(decode(CODE_FIGS, false), Decoded::Figs);
        assert_eq!(decode(CODE_ALPHA, false), Decoded::Alpha);
        assert_eq!(decode(CODE_REP, false), Decoded::Rep);
    }
}
