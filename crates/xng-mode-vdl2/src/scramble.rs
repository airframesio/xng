//! VDL2 scrambler (ICAO Annex 10 Vol III §6.4.3.1.4): 15-stage LFSR,
//! x^15 + x + 1, additive, applied to everything after the unique word.
//! Keystream bit = stage1 ⊕ stage15; that bit also feeds back into stage 1.

const INIT: [u8; 15] = [1, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1];

pub struct Scrambler {
    state: [u8; 15],
}

impl Scrambler {
    pub fn new() -> Self {
        Self { state: INIT }
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

impl Default for Scrambler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystream_matches_spec_derivation() {
        // First 48 bits derived from Annex 10 Figure 6-2.
        let expected = "000100110001101111000100001001010000111110001100";
        let mut s = Scrambler::new();
        let got: String =
            (0..48).map(|_| char::from(b'0' + s.next_bit())).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn first_three_bits_keep_reserved_symbol_zero() {
        let mut s = Scrambler::new();
        let mut reserved = [0u8, 0, 0];
        s.apply(&mut reserved);
        assert_eq!(reserved, [0, 0, 0]);
    }
}
