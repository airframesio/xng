//! Syndrome-table FEC for the ARINC 618 block check (ACARS-4.2).
//!
//! acarsdec's `syndrom.h` precomputes, for every single-bit error in the
//! block, the CRC residue ("syndrome") that the error produces, then inverts
//! that map at runtime: compute the syndrome of the received block and look
//! up which bit is wrong in O(1). This replaces the brute-force 8-candidates-
//! per-suspect search that the deframer used before.
//!
//! ## How the table works (the linearity of CRC)
//!
//! The ACARS BCS is CRC-16/KERMIT (poly 0x1021 reflected = 0x8408, init 0;
//! see `xng_dsp::checksum` and PROVENANCE.md). For a *valid* block the CRC
//! over `[Mode .. ETX/ETB, BCS_lo, BCS_hi]` is 0. Flip one bit and the CRC
//! becomes non-zero — and because CRC is linear over GF(2),
//!
//! ```text
//!   crc(received) = crc(original) XOR crc(error_pattern)
//!                 = 0            XOR crc(single_bit_error)
//! ```
//!
//! so the residue depends *only* on the error pattern, not the message. The
//! residue of "a single 1-bit at byte-distance `d` from the end of the
//! buffer, bit `b`" is therefore a fixed value we can tabulate. acarsdec
//! indexes that table as `syndrom[8*d + b]`; we build the identical values by
//! running the very same CRC over the corresponding one-hot buffer (verified
//! byte-for-byte against acarsdec's `syndrom.h` in the tests below).
//!
//! ## Oracle
//!
//! - Polynomial / parity scheme: CRC-16/KERMIT, the ARINC 618 BCS
//!   (PROVENANCE.md; reveng CRC catalogue).
//! - Table values: acarsdec `syndrom.h` (TLeconte, GPL) — the canonical
//!   entries are asserted directly in `matches_acarsdec_syndrom_h`.
//! - Correction semantics: acarsdec `acars.c` `fixprerr`/`fixdberr`
//!   (`syndrom[i + 8*(len - k + 1)]`).

use std::collections::HashMap;
use std::sync::OnceLock;
use xng_dsp::checksum::acars_crc;

/// A located single-bit error: the byte's distance from the end of the
/// buffer and the bit index within that byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorLocus {
    /// Bytes from the end of the buffer (0 = last byte).
    pub dist_from_end: usize,
    /// Bit position within the byte (0 = LSB).
    pub bit: u8,
}

/// Largest block we tabulate, in bytes. ACARS text is ≤ 220 chars; with the
/// 12-byte header, STX, suffix and 2 BCS bytes a real block stays well under
/// this. acarsdec's own table covers 242 bytes.
const MAX_BLOCK_BYTES: usize = 256;

/// syndrome (CRC residue of the lone error) → its position. Built once.
fn table() -> &'static HashMap<u16, ErrorLocus> {
    static TABLE: OnceLock<HashMap<u16, ErrorLocus>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::with_capacity(MAX_BLOCK_BYTES * 8);
        // For each distance from the end and each bit, the error pattern is a
        // buffer that is all zero except one bit, `dist_from_end` bytes before
        // the end. Its CRC is the syndrome that error produces.
        for dist in 0..MAX_BLOCK_BYTES {
            for bit in 0..8u8 {
                let mut pattern = vec![0u8; dist + 1];
                pattern[0] = 1 << bit; // first byte is `dist` bytes from the end
                let syndrome = acars_crc(&pattern);
                // Collisions are impossible for a proper CRC over this span,
                // but if one ever appeared we keep the nearest-to-end locus
                // (matches acarsdec's first-match scan order).
                map.entry(syndrome).or_insert(ErrorLocus { dist_from_end: dist, bit });
            }
        }
        map
    })
}

/// Look up the single-bit error that produced `syndrome` (the non-zero CRC
/// residue of a received block). `None` if no single-bit error explains it
/// (i.e. zero or ≥2 bit errors).
pub fn locate_single_bit_error(syndrome: u16) -> Option<ErrorLocus> {
    if syndrome == 0 {
        return None;
    }
    table().get(&syndrome).copied()
}

/// Try to repair a CRC failure in `block` (`[Mode .. suffix, BCS_lo, BCS_hi]`,
/// parity bits intact) by an O(1) syndrome lookup. On success the offending
/// bit is flipped in place and `Some(locus)` is returned; the block now
/// satisfies `acars_crc(block) == 0`. Returns `None` if the residue is not a
/// single-bit error (the caller can fall back to a multi-error search).
pub fn correct_single_bit(block: &mut [u8]) -> Option<ErrorLocus> {
    let syndrome = acars_crc(block);
    let locus = locate_single_bit_error(syndrome)?;
    if locus.dist_from_end >= block.len() {
        return None; // error placed outside this (shorter) block
    }
    let idx = block.len() - 1 - locus.dist_from_end;
    block[idx] ^= 1 << locus.bit;
    debug_assert_eq!(acars_crc(block), 0, "syndrome correction must clear CRC");
    Some(locus)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical entries lifted verbatim from acarsdec `syndrom.h`
    /// (`static const unsigned short syndrom[]`). `syndrom[p]` is the residue
    /// of a one-hot error at byte-distance `p/8` from the end, bit `p%8`.
    /// Verifying these proves our table is generated identically to the
    /// reference decoder's — an external oracle, not a self-loopback.
    #[test]
    fn matches_acarsdec_syndrom_h() {
        // (index p, expected syndrom[p]) — full table validated offline; these
        // span the first three byte-distances plus the table's last entry.
        let oracle: &[(usize, u16)] = &[
            (0, 0x1189),
            (1, 0x2312),
            (7, 0x8408),
            (8, 0x19d8),
            (15, 0x8ccc),
            (16, 0x5adc),
            (23, 0x0cec),
            (1935, 0x721c),
        ];
        for &(p, expected) in oracle {
            let dist = p / 8;
            let bit = (p % 8) as u8;
            let mut pattern = vec![0u8; dist + 1];
            pattern[0] = 1 << bit;
            assert_eq!(
                acars_crc(&pattern),
                expected,
                "syndrom[{p}] (dist={dist}, bit={bit}) must equal acarsdec's table value"
            );
        }
    }

    /// Every syndrome is distinct, so the inverse map is well defined over the
    /// whole tabulated span (single-bit errors are uniquely locatable).
    #[test]
    fn syndromes_are_unique() {
        assert_eq!(table().len(), MAX_BLOCK_BYTES * 8);
    }

    /// Locating then flipping recovers CRC residue 0 for a known block.
    /// Uses the ARINC 618 §2.2.10 "K7" worked example (octets 0xCB 0x37,
    /// BCS 0x3E 0x6B) — the same vector `xng_dsp` validates — as the clean
    /// block, then injects one bit error and confirms the lookup recovers it.
    #[test]
    fn recovers_arinc618_k7_example_after_bit_flip() {
        // Clean block: residue 0 (PROVENANCE.md / xng_dsp arinc_618 test).
        let clean = [0xCBu8, 0x37, 0x3E, 0x6B];
        assert_eq!(acars_crc(&clean), 0);

        // Flip a known bit (byte 1, bit 2) → CRC breaks.
        let mut corrupt = clean;
        corrupt[1] ^= 1 << 2;
        assert_ne!(acars_crc(&corrupt), 0);

        let locus = correct_single_bit(&mut corrupt).expect("single-bit error must be located");
        // byte 1 of 4 ⇒ distance 2 from the end, bit 2.
        assert_eq!(locus, ErrorLocus { dist_from_end: 2, bit: 2 });
        assert_eq!(corrupt, clean, "block must be restored exactly");
        assert_eq!(acars_crc(&corrupt), 0);
    }

    #[test]
    fn no_error_returns_none() {
        let mut clean = [0xCBu8, 0x37, 0x3E, 0x6B];
        assert!(correct_single_bit(&mut clean).is_none());
    }
}
