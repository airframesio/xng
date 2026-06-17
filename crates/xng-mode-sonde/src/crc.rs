//! RS41 sub-block CRC-16.
//!
//! Each variable-length sub-block (`ID | LEN | DATA[LEN] | CRC16`) is
//! protected by a CRC-16 with polynomial 0x1021 and init 0xFFFF, no final
//! XOR, no reflection — the "CCITT-FALSE" variant (rs1729/RS `rs41mod.c`
//! `crc16()`). The stored CRC is little-endian (low byte first).

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflect, no xorout).
pub fn crc16(data: &[u8]) -> u16 {
    let mut rem: u16 = 0xFFFF;
    for &byte in data {
        rem ^= (byte as u16) << 8;
        for _ in 0..8 {
            if rem & 0x8000 != 0 {
                rem = (rem << 1) ^ 0x1021;
            } else {
                rem <<= 1;
            }
        }
    }
    rem
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CRC-16/CCITT-FALSE check value for the ASCII string "123456789" is
    /// 0x29B1 (the standard reference vector for this CRC variant).
    #[test]
    fn check_vector_29b1() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }
}
