//! XXTEA "scrambling" of the ADS-L payload.
//!
//! ADS-L uses XXTEA (Corrected Block TEA) with an all-zero 128-bit key and
//! 6 mixing rounds to scramble the five 32-bit payload words. This is
//! obfuscation, not security — the key is public (zero). The byte order
//! into the 32-bit words is little-endian (see [`crate::words_from_le`]).
//!
//! Ported from the canonical Wheeler & Needham Corrected Block TEA, matching
//! the OGN `ognconv.cpp` `XXTEA_*_Key0` routines that SoftRF uses for ADS-L
//! (`ADSL_Packet::Descramble`). Key = 0 collapses the mix function's key
//! term. See PROVENANCE.md.

const DELTA: u32 = 0x9e37_79b9;

/// The XXTEA mix function with an all-zero key.
#[inline]
fn mx_key0(y: u32, z: u32, sum: u32) -> u32 {
    (((z >> 5) ^ (y << 2)).wrapping_add((y >> 3) ^ (z << 4))) ^ ((sum ^ y).wrapping_add(z))
}

/// Scramble `data` in place (`loops` rounds). Used by the modulator/encoder.
pub fn encrypt_key0(data: &mut [u32], loops: u32) {
    let n = data.len();
    if n < 2 {
        return;
    }
    let mut sum: u32 = 0;
    let mut z = data[n - 1];
    for _ in 0..loops {
        sum = sum.wrapping_add(DELTA);
        for p in 0..n - 1 {
            let y = data[p + 1];
            data[p] = data[p].wrapping_add(mx_key0(y, z, sum));
            z = data[p];
        }
        let y = data[0];
        data[n - 1] = data[n - 1].wrapping_add(mx_key0(y, z, sum));
        z = data[n - 1];
    }
}

/// Descramble `data` in place (`loops` rounds). Inverse of [`encrypt_key0`].
pub fn decrypt_key0(data: &mut [u32], loops: u32) {
    let n = data.len();
    if n < 2 {
        return;
    }
    let mut sum = (loops).wrapping_mul(DELTA);
    let mut y = data[0];
    for _ in 0..loops {
        for p in (1..n).rev() {
            let z = data[p - 1];
            data[p] = data[p].wrapping_sub(mx_key0(y, z, sum));
            y = data[p];
        }
        let z = data[n - 1];
        data[0] = data[0].wrapping_sub(mx_key0(y, z, sum));
        y = data[0];
        sum = sum.wrapping_sub(DELTA);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // decrypt ∘ encrypt is the identity; this is a property of the codec
    // itself (the external anchor is that the routine matches OGN's
    // ognconv.cpp byte-for-byte — exercised end-to-end against the spec
    // worked-example vector in tests/decode_vectors.rs).
    #[test]
    fn roundtrip() {
        let orig = [
            0x1122_3344u32,
            0xdead_beef,
            0x0000_0001,
            0xffff_ffff,
            0x5555_aaaa,
        ];
        let mut d = orig;
        encrypt_key0(&mut d, 6);
        assert_ne!(d, orig, "scrambling must change the data");
        decrypt_key0(&mut d, 6);
        assert_eq!(d, orig);
    }
}
