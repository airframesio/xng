//! MSB-first bit reader over a UAT payload, using the same 1-based
//! `(byte, bit)` addressing as DO-282B field tables and dump978's
//! `RawMessage::Bits` / `RawMessage::Bit` helpers.
//!
//! Byte 1, bit 1 is the most-significant bit of the first payload octet;
//! ranges are inclusive of both endpoints.

/// A read-only view over a UAT payload with 1-based MSB-first addressing.
pub struct BitReader<'a> {
    payload: &'a [u8],
}

impl<'a> BitReader<'a> {
    pub fn new(payload: &'a [u8]) -> Self {
        Self { payload }
    }

    pub fn len(&self) -> usize {
        self.payload.len()
    }

    pub fn is_empty(&self) -> bool {
        self.payload.is_empty()
    }

    /// Single bit at 1-based (`byte`, `bit`). `bit` is 1..=8, MSB-first.
    pub fn bit(&self, byte: usize, bit: usize) -> bool {
        debug_assert!(byte >= 1 && (1..=8).contains(&bit));
        let bi = (byte - 1) * 8 + bit - 1;
        let by = bi >> 3;
        let mask = 1u8 << (7 - (bi & 7));
        (self.payload[by] & mask) != 0
    }

    /// Inclusive bit range `[first_byte.first_bit .. last_byte.last_bit]`
    /// returned as an unsigned integer, MSB-first. Up to 32 bits.
    pub fn bits(&self, first_byte: usize, first_bit: usize, last_byte: usize, last_bit: usize) -> u32 {
        debug_assert!(first_byte >= 1 && (1..=8).contains(&first_bit));
        debug_assert!(last_byte >= 1 && (1..=8).contains(&last_bit));
        let fbi = (first_byte - 1) * 8 + first_bit - 1;
        let lbi = (last_byte - 1) * 8 + last_bit - 1;
        debug_assert!(fbi <= lbi);
        let nbits = lbi - fbi + 1;
        debug_assert!(nbits <= 32);
        let mut acc: u32 = 0;
        for i in fbi..=lbi {
            let by = i >> 3;
            let mask = 1u8 << (7 - (i & 7));
            acc = (acc << 1) | u32::from((self.payload[by] & mask) != 0);
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_fields_match_dump978_addressing() {
        // HDR of `-00a66ef1...`: MDB type = bits(1,1,1,5)=0, address
        // qualifier = bits(1,6,1,8)=0, address = bits(2,1,4,8)=0xA66EF1
        // (dump978 reports "A66EF1, ICAO via ADS-B").
        let p = [0x00u8, 0xa6, 0x6e, 0xf1, 0x35, 0x44];
        let r = BitReader::new(&p);
        assert_eq!(r.bits(1, 1, 1, 5), 0);
        assert_eq!(r.bits(1, 6, 1, 8), 0);
        assert_eq!(r.bits(2, 1, 4, 8), 0x00a6_6ef1);
        // bit() agrees with bits() of width 1.
        assert!(!r.bit(1, 1));
        assert_eq!(r.bits(5, 1, 5, 8), 0x35);
    }
}
