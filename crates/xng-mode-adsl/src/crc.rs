//! 24-bit CRC over the ADS-L packet (Version byte + 20 payload bytes +
//! 3 CRC bytes).
//!
//! Ported from the OGN `ADSL_Packet::PolyPass`/`checkPI`/`calcPI` that
//! SoftRF references for `ADSL_CRC_TYPE = RF_CHECKSUM_TYPE_CRC_MODES`. The
//! polynomial register is 32-bit, fed MSB-first, with the 24-bit residue
//! taken from the top three bytes (`crc >> 8`). See PROVENANCE.md.

const POLY: u32 = 0xFFFA_0480;

/// Pass a single byte through the polynomial register.
#[inline]
fn poly_pass(mut crc: u32, byte: u8) -> u32 {
    crc |= byte as u32;
    for _ in 0..8 {
        if crc & 0x8000_0000 != 0 {
            crc ^= POLY;
        }
        crc <<= 1;
    }
    crc
}

/// Run the register over `bytes` (data plus the trailing 3 CRC bytes) and
/// return the 24-bit residue. Zero indicates an intact packet.
pub fn check(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bytes {
        crc = poly_pass(crc, b);
    }
    crc >> 8
}

/// Compute the 24-bit CRC of `bytes` (the data *without* the CRC field),
/// as written into the three trailing CRC bytes.
pub fn calc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bytes {
        crc = poly_pass(crc, b);
    }
    crc = poly_pass(crc, 0);
    crc = poly_pass(crc, 0);
    crc = poly_pass(crc, 0);
    crc >> 8
}

#[cfg(test)]
mod tests {
    use super::*;

    // calc() produces a residue that check() verifies to zero — this is the
    // CRC's defining property. The external anchor is that the routine is a
    // line-for-line port of OGN's PolyPass with the 0xFFFA0480 polynomial.
    #[test]
    fn calc_then_check_is_zero() {
        let data: Vec<u8> = (0u8..21).collect(); // Version + 20 payload bytes
        let crc = calc(&data);
        let mut full = data.clone();
        full.push((crc >> 16) as u8);
        full.push((crc >> 8) as u8);
        full.push(crc as u8);
        assert_eq!(check(&full), 0, "valid CRC must check to zero");

        // a single flipped bit must break the check
        full[0] ^= 0x01;
        assert_ne!(check(&full), 0);
    }
}
