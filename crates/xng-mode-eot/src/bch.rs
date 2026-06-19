//! BCH error-detection check for the AAR S-9152 EOT 2-way telemetry frame.
//!
//! GROUND TRUTH (cited): the reverse-engineered EOT frame uses a systematic
//! binary BCH(63,45) code, narrowed in the field decoders to an 18-bit check
//! word computed by modulo-2 polynomial division of the 45-bit data block by
//! a 19-bit generator, with the resulting remainder XOR-ed against a fixed
//! 18-bit "cipher" key. Both independent public decoders agree byte-for-byte:
//!
//!   - ereuter/PyEOT  `eot_decoder.py` / `helpers.py`
//!     (https://github.com/ereuter/PyEOT)
//!   - russinnes/EOTDecode `eot_decoder.py` / `helpers.py`
//!     (https://github.com/russinnes/EOTDecode)
//!
//! Both define:
//!   GENERATOR = "1111001101000001111"   (19 bits)
//!   CIPHER    = "101011011101110000"     (18 bits)
//! and verify a frame as:
//!   data_block = reverse(packet[11:56])          # the 45 data bits, LSB-first
//!   computed   = checkbits(data_block, GENERATOR) # mod-2 division remainder
//!   valid      = (computed XOR CIPHER) == packet[56:74]
//!
//! `checkbits(data, key)` appends `len(key)-1` zero bits to `data` and returns
//! the modulo-2 division remainder (CRC-style), i.e. the standard systematic
//! polynomial remainder. We reproduce that exact arithmetic here over bit
//! slices so the framing layer can validate a frame against the documented
//! decoders' field map.

/// Generator polynomial as a bit string, MSB-first, per PyEOT/EOTDecode
/// (`'1111001101000001111'`, 19 bits → degree-18 generator).
pub const GENERATOR: &[u8] = &[1, 1, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 0, 0, 0, 1, 1, 1, 1];

/// Fixed 18-bit XOR "cipher" key applied to the computed remainder, per
/// PyEOT/EOTDecode (`'101011011101110000'`).
pub const CIPHER: &[u8] = &[1, 0, 1, 0, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 0, 0, 0];

/// Number of check bits carried on the wire (degree of the generator).
pub const CHECK_BITS: usize = 18;

/// Modulo-2 polynomial division remainder of `data` by `generator`, returning
/// the `generator.len()-1` remainder bits (CRC-style "checkbits").
///
/// Mirrors `helpers.checkbits` / `helpers.mod2div`: append `len(gen)-1` zeros
/// to the data, then long-divide in GF(2), keeping the final remainder.
pub fn checkbits(data: &[u8], generator: &[u8]) -> Vec<u8> {
    let m = generator.len() - 1;
    let mut buf: Vec<u8> = Vec::with_capacity(data.len() + m);
    buf.extend_from_slice(data);
    buf.extend(std::iter::repeat_n(0u8, m));

    // Long division: whenever the leading bit is 1, XOR the generator in.
    for i in 0..(buf.len() - m) {
        if buf[i] == 1 {
            for (j, &g) in generator.iter().enumerate() {
                buf[i + j] ^= g;
            }
        }
    }
    // The remainder is the trailing `m` bits.
    buf[buf.len() - m..].to_vec()
}

/// Compute the 18-bit ciphered check word for a 45-bit data block (MSB-first
/// as carried in `packet[11:56]`).
///
/// Reproduces the decoders' verify path: the 45 data bits are reversed to
/// LSB-first, divided by [`GENERATOR`], and the remainder is XOR-ed with
/// [`CIPHER`]. The returned 18 bits are what a valid frame carries in
/// `packet[56:74]`.
pub fn ciphered_check(data_block_msb_first: &[u8]) -> Vec<u8> {
    let reversed: Vec<u8> = data_block_msb_first.iter().rev().copied().collect();
    let rem = checkbits(&reversed, GENERATOR);
    rem.iter()
        .zip(CIPHER.iter())
        .map(|(&r, &c)| r ^ c)
        .collect()
}

/// True if the 45-bit data block's recomputed ciphered check matches the
/// 18 received check bits.
pub fn verify(data_block_msb_first: &[u8], received_check: &[u8]) -> bool {
    if received_check.len() != CHECK_BITS {
        return false;
    }
    ciphered_check(data_block_msb_first) == received_check
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_and_cipher_match_cited_decoders() {
        // Spec strings from PyEOT/EOTDecode, re-derived as bit vectors.
        let gen_str = "1111001101000001111";
        let cipher_str = "101011011101110000";
        let gen: Vec<u8> = gen_str.bytes().map(|b| b - b'0').collect();
        let cipher: Vec<u8> = cipher_str.bytes().map(|b| b - b'0').collect();
        assert_eq!(GENERATOR, gen.as_slice());
        assert_eq!(CIPHER, cipher.as_slice());
        assert_eq!(GENERATOR.len(), 19);
        assert_eq!(CIPHER.len(), CHECK_BITS);
    }

    #[test]
    fn checkbits_matches_reference_mod2div() {
        // Anchor the GF(2) long division against the cited decoders'
        // `helpers.mod2div` reference, independent of the EOT key.
        //
        // Case 1 — CRC-3, data 1101, generator 1011: helpers.checkbits
        // returns "001" (verified by running the reference mod2div).
        assert_eq!(checkbits(&[1, 1, 0, 1], &[1, 0, 1, 1]), vec![0, 0, 1]);

        // Case 2 — the classic Wikipedia CRC worked example: dividend
        // 11010011101100, generator 1011 -> remainder 100 (matches both the
        // Wikipedia "Computation of CRC" article and helpers.mod2div).
        let data: Vec<u8> = "11010011101100".bytes().map(|b| b - b'0').collect();
        assert_eq!(checkbits(&data, &[1, 0, 1, 1]), vec![1, 0, 0]);
    }

    #[test]
    fn verify_accepts_self_consistent_check_and_rejects_flips() {
        // NOTE: this only exercises the arithmetic's internal consistency
        // (compute then verify). The SPEC-ANCHORED end-to-end frame test in
        // frame.rs builds a packet per the documented field map and asserts
        // both the field extraction AND this BCH verify pass together.
        let data: Vec<u8> = (0..45).map(|i| ((i * 7 + 3) % 2) as u8).collect();
        let check = ciphered_check(&data);
        assert_eq!(check.len(), CHECK_BITS);
        assert!(verify(&data, &check));

        // Any single data-bit flip must break the check (BCH detects it).
        let mut bad = data.clone();
        bad[10] ^= 1;
        assert!(!verify(&bad, &check));

        // Any single check-bit flip must break it too.
        let mut bad_check = check.clone();
        bad_check[5] ^= 1;
        assert!(!verify(&data, &bad_check));
    }
}
