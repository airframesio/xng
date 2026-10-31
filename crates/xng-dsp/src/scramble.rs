//! 15-stage additive LFSR scrambler (x^15 + x + 1) shared by VDL2 and
//! Inmarsat Aero: keystream bit = s0 ⊕ s14, fed back into s0. Both systems
//! use the same initial state.

pub const LFSR15_INIT: [u8; 15] = [1, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1];

/// [`LFSR15_INIT`] bit-packed: stage `i` in bit `i`.
const LFSR15_INIT_PACKED: u16 = {
    let mut v = 0u16;
    let mut i = 0;
    while i < 15 {
        v |= (LFSR15_INIT[i] as u16) << i;
        i += 1;
    }
    v
};

pub struct Lfsr15 {
    /// Stage `i` in bit `i` (bit 15 always clear).
    state: u16,
}

impl Lfsr15 {
    pub fn new() -> Self {
        Self { state: LFSR15_INIT_PACKED }
    }

    #[inline]
    pub fn next_bit(&mut self) -> u8 {
        let out = (self.state ^ (self.state >> 14)) & 1;
        self.state = ((self.state << 1) & 0x7FFF) | out;
        out as u8
    }

    /// XOR the keystream over `bits` in place.
    pub fn apply(&mut self, bits: &mut [u8]) {
        for b in bits {
            *b ^= self.next_bit();
        }
    }
}

impl Default for Lfsr15 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystream_matches_spec_derivation() {
        // First 48 bits derived from ICAO Annex 10 Vol III Figure 6-2
        // (VDL2); JAERO's AeroLScrambler produces the identical stream.
        let expected = "000100110001101111000100001001010000111110001100";
        let mut s = Lfsr15::new();
        let got: String = (0..48).map(|_| char::from(b'0' + s.next_bit())).collect();
        assert_eq!(got, expected);
    }

    /// The packed register reproduces the stage-array formulation (out =
    /// s0 ⊕ s14, shift up, feed back into s0) over two full periods.
    #[test]
    fn packed_matches_array_reference() {
        let mut reference = LFSR15_INIT;
        let mut s = Lfsr15::new();
        for i in 0..2 * 32_767 {
            let want = reference[0] ^ reference[14];
            reference.rotate_right(1);
            reference[0] = want;
            assert_eq!(s.next_bit(), want, "bit {i}");
        }
    }
}
