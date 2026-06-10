//! 15-stage additive LFSR scrambler (x^15 + x + 1) shared by VDL2 and
//! Inmarsat Aero: keystream bit = s0 ⊕ s14, fed back into s0. Both systems
//! use the same initial state.

pub const LFSR15_INIT: [u8; 15] = [1, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1];

pub struct Lfsr15 {
    state: [u8; 15],
}

impl Lfsr15 {
    pub fn new() -> Self {
        Self { state: LFSR15_INIT }
    }

    #[inline]
    pub fn next_bit(&mut self) -> u8 {
        let out = self.state[0] ^ self.state[14];
        self.state.rotate_right(1);
        self.state[0] = out;
        out
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
}
