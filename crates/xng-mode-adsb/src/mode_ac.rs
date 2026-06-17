//! Mode A/C reply decoding.
//!
//! Mode A/C replies are the legacy (pre-Mode-S) transponder replies: a
//! train of framing + information pulses with no address and no CRC. Mode A
//! carries the 4-digit octal identity (squawk); Mode C carries a Gillham-
//! coded pressure altitude. This module decodes the recovered 12-pulse
//! information word — the part with a deterministic, verifiable mapping —
//! into squawk / SPI / altitude.
//!
//! The recovered pulses are packed into the conventional 16-bit "Mode A"
//! word used by dump1090/readsb, four 4-bit groups (high bit of each group
//! is always zero):
//!
//! ```text
//!   bits 15..12: 0 A4 A2 A1
//!   bits 11.. 8: 0 B4 B2 B1
//!   bits  7.. 4: 0 C4 C2 C1   (plus the SPI/Ident pulse at 0x0080 = X/SPI)
//!   bits  3.. 0: 0 D4 D2 D1
//! ```
//!
//! The Mode-A→squawk and Mode-A→Mode-C altitude mappings follow the
//! published Gillham ladder; the bit positions and the altitude XOR ladder
//! are the documented dump1090 / readsb algorithm (see PROVENANCE.md —
//! protocol facts only, cross-checked against the upstream C compiled as an
//! external oracle for the unit-test vectors).
//!
//! RF demod (framing-pulse detection in the magnitude domain) is a separate
//! signal-processing path and is not implemented here; this module is the
//! decode kernel a future Mode A/C demod would feed.

/// Convert a 16-bit Mode A word to a Mode C altitude in units of 100 ft
/// (so the result × 100 is feet), or `None` when the word is not a valid
/// Mode C code. Port of the documented dump1090 `internalModeAToModeC`
/// ladder.
fn mode_a_to_mode_c(mode_a: u16) -> Option<i32> {
    let m = mode_a as u32;
    // Zero bits must be zero (and D1, 0x0001, is illegal for altitude);
    // the C-group (0x00F0) cannot be all-zero for a valid Mode C.
    if (m & 0xFFFF_8889) != 0 || (m & 0x0000_00F0) == 0 {
        return None;
    }
    let mut hundreds: u32 = 0;
    if m & 0x0010 != 0 {
        hundreds ^= 0x007; // C1
    }
    if m & 0x0020 != 0 {
        hundreds ^= 0x003; // C2
    }
    if m & 0x0040 != 0 {
        hundreds ^= 0x001; // C4
    }
    // Reflect 7→5 / 5→7.
    if hundreds & 5 == 5 {
        hundreds ^= 2;
    }
    if hundreds > 5 {
        return None; // only 1..5 valid
    }
    let mut five_hundreds: u32 = 0;
    if m & 0x0002 != 0 {
        five_hundreds ^= 0x0FF; // D2
    }
    if m & 0x0004 != 0 {
        five_hundreds ^= 0x07F; // D4
    }
    if m & 0x1000 != 0 {
        five_hundreds ^= 0x03F; // A1
    }
    if m & 0x2000 != 0 {
        five_hundreds ^= 0x01F; // A2
    }
    if m & 0x4000 != 0 {
        five_hundreds ^= 0x00F; // A4
    }
    if m & 0x0100 != 0 {
        five_hundreds ^= 0x007; // B1
    }
    if m & 0x0200 != 0 {
        five_hundreds ^= 0x003; // B2
    }
    if m & 0x0400 != 0 {
        five_hundreds ^= 0x001; // B4
    }
    if five_hundreds & 1 != 0 {
        hundreds = 6 - hundreds;
    }
    Some((five_hundreds * 5 + hundreds) as i32 - 13)
}

/// A decoded Mode A/C reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeAc {
    /// 4-digit octal identity code (squawk), e.g. "7700".
    pub squawk: u16,
    /// SPI / Ident pulse present (the X pulse, 0x0080).
    pub spi: bool,
    /// Mode C barometric altitude in feet, when the word is a valid Mode C
    /// code. Mutually meaningful with `squawk`: a single reply is either a
    /// Mode A (identity) or Mode C (altitude) interrogation response — the
    /// caller knows which from the interrogation, but the word decodes both
    /// ways, so both are offered.
    pub altitude_ft: Option<i32>,
}

impl ModeAc {
    /// The squawk as a 4-character octal string ("7700", "0356", …).
    pub fn squawk_str(&self) -> String {
        let s = self.squawk;
        format!("{}{}{}{}", (s >> 12) & 7, (s >> 8) & 7, (s >> 4) & 7, s & 7)
    }
}

/// Decode a 16-bit Mode A/C information word into squawk, SPI, and (when
/// valid) Mode C altitude. The squawk is the four octal digits packed in
/// `mode_a & 0x7777`; SPI is the X/Ident pulse (0x0080).
pub fn decode(mode_a: u16) -> ModeAc {
    ModeAc {
        squawk: mode_a & 0x7777,
        spi: mode_a & 0x0080 != 0,
        altitude_ft: mode_a_to_mode_c(mode_a).map(|c| c * 100),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Oracle: dump1090 `internalModeAToModeC` compiled verbatim as a C
    // program and run to emit (mode_a → altitude) reference pairs — an
    // independent external decoder, not a self-consistency loopback. The
    // squawk/SPI pairs come from dump1090 `decodeModeAMessage`
    // (squawk = ModeA & 0x7777, spi = ModeA & 0x0080).

    #[test]
    fn mode_c_altitude_matches_dump1090() {
        // (mode_a, expected_feet) emitted by the upstream C algorithm.
        let cases = [
            (0x0020u16, -1000i32),
            (0x0320, 1000),
            (0x0620, 0),
            (0x4220, 5000),
            (0x5124, 35000),
            (0x5424, 38000),
            (0x6520, 10000),
        ];
        for (m, ft) in cases {
            assert_eq!(decode(m).altitude_ft, Some(ft), "mode_a 0x{m:04X}");
        }
    }

    #[test]
    fn invalid_mode_c_words_yield_no_altitude() {
        // dump1090 returns INVALID_ALTITUDE for these (C-group zero / bad
        // hundreds ladder).
        assert_eq!(decode(0x1000).altitude_ft, None); // C bits zero
        assert_eq!(decode(0x0050).altitude_ft, None); // illegal hundreds
    }

    #[test]
    fn squawk_extraction_matches_dump1090() {
        assert_eq!(decode(0x7700).squawk_str(), "7700"); // emergency
        assert_eq!(decode(0x7500).squawk_str(), "7500"); // hijack
        assert_eq!(decode(0x7600).squawk_str(), "7600"); // radio failure
        assert_eq!(decode(0x1200).squawk_str(), "1200"); // VFR
        assert_eq!(decode(0x0356).squawk_str(), "0356");
    }

    #[test]
    fn spi_ident_pulse_detected() {
        // SPI bit (0x0080) is masked out of the squawk but flagged.
        let d = decode(0x1200 | 0x0080);
        assert!(d.spi);
        assert_eq!(d.squawk_str(), "1200");
        assert!(!decode(0x1200).spi);
    }
}
