//! VDL2 burst header: reserved symbol (3 bits) + 17-bit transmission
//! length (LSB first) + 5-bit (25,20) header FEC (Annex 10 Table 6-2).

/// H matrix (5×20) over [R1 R2 R3, TL1..TL17] in transmission order.
const H: [[u8; 20]; 5] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 0, 0, 0, 1, 1, 1, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1],
    [1, 1, 0, 1, 1, 0, 1, 1, 0, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 1],
    [0, 1, 1, 0, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 0, 1, 0, 1, 0, 1],
];

pub const HEADER_BITS: usize = 25;
pub const MAX_TL: u32 = 131_071;

fn parity(bits20: &[u8; 20]) -> [u8; 5] {
    let mut p = [0u8; 5];
    for (i, row) in H.iter().enumerate() {
        p[i] = bits20.iter().zip(row).fold(0, |acc, (&b, &h)| acc ^ (b & h));
    }
    p
}

/// Encode the 25 header bits in transmission order for `tl` data bits.
pub fn encode(tl: u32) -> [u8; HEADER_BITS] {
    debug_assert!(tl <= MAX_TL);
    let mut bits20 = [0u8; 20];
    // Reserved symbol bits R1..R3 are 0.
    for i in 0..17 {
        bits20[3 + i] = ((tl >> i) & 1) as u8; // LSB first
    }
    let p = parity(&bits20);
    let mut out = [0u8; HEADER_BITS];
    out[..20].copy_from_slice(&bits20);
    out[20..].copy_from_slice(&p);
    out
}

/// Decode 25 received (descrambled) header bits; corrects a single bit
/// error by exhaustive flip. Returns the transmission length.
pub fn decode(bits: &[u8; HEADER_BITS]) -> Option<u32> {
    let check = |b: &[u8; HEADER_BITS]| -> bool {
        let mut bits20 = [0u8; 20];
        bits20.copy_from_slice(&b[..20]);
        parity(&bits20)[..] == b[20..]
    };
    let tl_of = |b: &[u8; HEADER_BITS]| -> u32 {
        (0..17).fold(0u32, |acc, i| acc | ((b[3 + i] as u32) << i))
    };
    if check(bits) {
        return Some(tl_of(bits));
    }
    for i in 0..HEADER_BITS {
        let mut t = *bits;
        t[i] ^= 1;
        if check(&t) {
            return Some(tl_of(&t));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec-derived parity vectors (R = 000).
    #[test]
    fn parity_vectors() {
        for (tl, p) in [(1u32, [0, 1, 0, 1, 0]), (100, [0, 1, 1, 1, 0]), (1000, [1, 0, 1, 1, 1]), (131071, [0, 1, 1, 0, 1])] {
            let enc = encode(tl);
            assert_eq!(&enc[20..], &p, "TL={tl}");
            assert_eq!(decode(&enc), Some(tl));
        }
    }

    #[test]
    fn corrects_single_bit_error() {
        let mut enc = encode(4242);
        enc[7] ^= 1;
        assert_eq!(decode(&enc), Some(4242));
    }
}
