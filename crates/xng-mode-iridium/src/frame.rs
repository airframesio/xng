//! Iridium layer 2 (ported from BSD-2-licensed iridium-toolkit — see PROVENANCE.md): access codes, symbol-pair deinterleaving,
//! BCH(31,21) blocks with even parity, FILL removal, and frame
//! classification (IRA ring alert, IBC broadcast, IMS messaging header).

/// 24-bit access codes (differential decode of the 12-symbol UW).
pub const ACCESS_DL: &[u8; 24] = &[
    0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1,
];
pub const ACCESS_UL: &[u8; 24] = &[
    1, 1, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0,
];

/// IMS (messaging) 32-bit header.
pub const HEADER_MESSAGING: &[u8; 32] = &[
    0, 0, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1,
    1, 1, 1, 1, 0, 0, 1, 1,
];

/// BCH generator polynomials (toolkit integer convention).
pub const RINGALERT_BCH_POLY: u32 = 1207;
pub const MESSAGING_BCH_POLY: u32 = 1897;
pub const HDR_POLY: u32 = 29;

/// The 64-bit FILL pattern that pads RA/MS frames (two 32-bit halves).
pub const FILL_A: u32 = 0b1010_0010_0111_0011_1011_1111_0110_1101;
pub const FILL_B: u32 = 0b0101_0100_0100_0101_1100_0010_1110_0110;

/// GF(2) polynomial remainder of a bit slice by `poly`.
pub fn ndivide(poly: u32, bits: &[u8]) -> u32 {
    let mut num: u64 = 0;
    for &b in bits {
        num = (num << 1) | b as u64;
    }
    let pbits = 32 - poly.leading_zeros();
    let nbits = bits.len() as u32;
    if nbits < pbits {
        return num as u32;
    }
    let mut shift = nbits - pbits;
    loop {
        if num >> (shift + pbits - 1) & 1 == 1 {
            num ^= (poly as u64) << shift;
        }
        if shift == 0 {
            break;
        }
        shift -= 1;
    }
    num as u32
}

/// Repair a 31-bit BCH block (≤2 bit flips searched); returns the number
/// of corrected bits, or None when uncorrectable. `block` is fixed up in
/// place.
pub fn bch_repair(poly: u32, block: &mut [u8]) -> Option<u32> {
    if ndivide(poly, block) == 0 {
        return Some(0);
    }
    for i in 0..block.len() {
        block[i] ^= 1;
        if ndivide(poly, block) == 0 {
            return Some(1);
        }
        for j in i + 1..block.len() {
            block[j] ^= 1;
            if ndivide(poly, block) == 0 {
                return Some(2);
            }
            block[j] ^= 1;
        }
        block[i] ^= 1;
    }
    None
}

/// 2-way symbol-pair deinterleave: 64 bits → two 32-bit blocks.
/// Operates on QPSK symbol pairs with the two bits of each pair swapped,
/// reading symbols from the end backwards (toolkit `de_interleave`).
pub fn de_interleave2(group: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let symbols: Vec<[u8; 2]> = group
        .chunks_exact(2)
        .map(|p| [p[1], p[0]])
        .collect();
    let n = symbols.len();
    let mut even = Vec::with_capacity(n);
    let mut odd = Vec::with_capacity(n);
    let mut x = n as isize - 2;
    while x >= 0 {
        even.extend_from_slice(&symbols[x as usize]);
        x -= 2;
    }
    let mut x = n as isize - 1;
    while x >= 0 {
        odd.extend_from_slice(&symbols[x as usize]);
        x -= 2;
    }
    (odd, even)
}

/// 3-way symbol-pair deinterleave: 96 bits → three 32-bit blocks
/// (toolkit `de_interleave3`).
pub fn de_interleave3(group: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let symbols: Vec<[u8; 2]> = group
        .chunks_exact(2)
        .map(|p| [p[1], p[0]])
        .collect();
    let n = symbols.len() as isize;
    let collect = |start: isize| -> Vec<u8> {
        let mut out = Vec::new();
        let mut x = start;
        while x >= 0 {
            out.extend_from_slice(&symbols[x as usize]);
            x -= 3;
        }
        out
    };
    (collect(n - 1), collect(n - 2), collect(n - 3))
}

fn bits_to_u32(bits: &[u8]) -> u32 {
    bits.iter().fold(0u32, |v, &b| (v << 1) | b as u32)
}

/// Decode a sequence of 32-bit interleaver-output blocks through
/// BCH(31,21) + even parity; returns (data bits, corrected blocks) and
/// stops at the first uncorrectable block (toolkit `IridiumECCMessage`).
pub fn ecc_blocks(blocks: &[Vec<u8>], poly: u32) -> (Vec<u8>, u32) {
    let mut data = Vec::new();
    let mut fixed = 0u32;
    for block in blocks {
        if block.len() != 32 {
            break;
        }
        let mut b31: Vec<u8> = block[..31].to_vec();
        let Some(errs) = bch_repair(poly, &mut b31) else {
            break;
        };
        // Whole-block even parity (over the repaired 31 bits + parity).
        let ones: u32 =
            b31.iter().map(|&v| v as u32).sum::<u32>() + block[31] as u32;
        if ones % 2 == 1 && errs > 0 {
            break;
        }
        if errs > 0 {
            fixed += 1;
        }
        data.extend_from_slice(&b31[..21]);
    }
    (data, fixed)
}

/// Remove trailing FILL pattern pairs from the interleaved block list.
pub fn strip_fill(blocks: &mut Vec<Vec<u8>>) -> u32 {
    let mut fill = 0;
    while blocks.len() >= 2 {
        let a = bits_to_u32(&blocks[blocks.len() - 2]);
        let b = bits_to_u32(&blocks[blocks.len() - 1]);
        if (a ^ FILL_A).count_ones() <= 2 && (b ^ FILL_B).count_ones() <= 2 {
            blocks.pop();
            blocks.pop();
            fill += 1;
        } else {
            break;
        }
    }
    fill
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Ring alert (simplex downlink).
    Ra,
    /// Broadcast (IBC).
    Bc,
    /// Messaging header seen (payload not parsed in v1).
    Ms,
    Unknown,
}

/// Classify the post-access bit stream (downlink heuristics from the
/// toolkit, without frequency classing).
pub fn classify(data: &[u8]) -> FrameKind {
    if data.len() >= 32 && data[..32] == HEADER_MESSAGING[..] {
        return FrameKind::Ms;
    }
    if data.len() > 6 + 64 && ndivide(HDR_POLY, &data[..6]) == 0 {
        let (b1, b2) = de_interleave2(&data[6..6 + 64]);
        if ndivide(RINGALERT_BCH_POLY, &b1[..31]) == 0
            && ndivide(RINGALERT_BCH_POLY, &b2[..31]) == 0
        {
            return FrameKind::Bc;
        }
    }
    if data.len() >= 96 {
        let (b1, b2, b3) = de_interleave3(&data[..96]);
        if ndivide(RINGALERT_BCH_POLY, &b1[..31]) == 0
            && ndivide(RINGALERT_BCH_POLY, &b2[..31]) == 0
            && ndivide(RINGALERT_BCH_POLY, &b3[..31]) == 0
        {
            return FrameKind::Ra;
        }
    }
    FrameKind::Unknown
}

/// Deinterleave an RA frame's bit stream: leading 3-way triple, then
/// 2-way per following 64-bit chunk.
pub fn ra_blocks(data: &[u8]) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    let (b1, b2, b3) = de_interleave3(&data[..96]);
    blocks.push(b1);
    blocks.push(b2);
    blocks.push(b3);
    for chunk in data[96..].chunks_exact(64) {
        let (o, e) = de_interleave2(chunk);
        blocks.push(o);
        blocks.push(e);
    }
    blocks
}

// ── transmit side (loopback/testing) ────────────────────────────────────

/// BCH-encode 21 data bits into a 32-bit block (21 data + 10 check +
/// even parity).
pub fn bch_encode(poly: u32, data21: &[u8]) -> Vec<u8> {
    debug_assert_eq!(data21.len(), 21);
    let mut padded: Vec<u8> = data21.to_vec();
    padded.extend(std::iter::repeat(0).take(10));
    let rem = ndivide(poly, &padded);
    let mut block: Vec<u8> = data21.to_vec();
    for k in (0..10).rev() {
        block.push(((rem >> k) & 1) as u8);
    }
    let ones: u32 = block.iter().map(|&b| b as u32).sum();
    block.push((ones % 2) as u8);
    block
}

/// Inverse of `de_interleave2`: two 32-bit blocks → 64 transmitted bits.
pub fn interleave2(odd: &[u8], even: &[u8]) -> Vec<u8> {
    let n = (odd.len() + even.len()) / 2; // symbol count
    let mut symbols: Vec<[u8; 2]> = vec![[0, 0]; n];
    let mut o = odd.chunks_exact(2);
    let mut x = n as isize - 1;
    while x >= 0 {
        let p = o.next().unwrap();
        symbols[x as usize] = [p[0], p[1]];
        x -= 2;
    }
    let mut e = even.chunks_exact(2);
    let mut x = n as isize - 2;
    while x >= 0 {
        let p = e.next().unwrap();
        symbols[x as usize] = [p[0], p[1]];
        x -= 2;
    }
    symbols.into_iter().flat_map(|[a, b]| [b, a]).collect()
}

/// Inverse of `de_interleave3`: three 32-bit blocks → 96 transmitted bits.
pub fn interleave3(b1: &[u8], b2: &[u8], b3: &[u8]) -> Vec<u8> {
    let n = (b1.len() + b2.len() + b3.len()) / 2;
    let mut symbols: Vec<[u8; 2]> = vec![[0, 0]; n];
    for (start, blk) in [(1usize, b1), (2, b2), (3, b3)] {
        let mut it = blk.chunks_exact(2);
        let mut x = n as isize - start as isize;
        while x >= 0 {
            let p = it.next().unwrap();
            symbols[x as usize] = [p[0], p[1]];
            x -= 3;
        }
    }
    symbols.into_iter().flat_map(|[a, b]| [b, a]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rand_bits(n: usize, mut s: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((s >> 33) & 1) as u8
            })
            .collect()
    }

    #[test]
    fn interleave_roundtrips() {
        let odd = rand_bits(32, 1);
        let even = rand_bits(32, 2);
        let tx = interleave2(&odd, &even);
        let (o, e) = de_interleave2(&tx);
        assert_eq!(o, odd);
        assert_eq!(e, even);

        let b1 = rand_bits(32, 3);
        let b2 = rand_bits(32, 4);
        let b3 = rand_bits(32, 5);
        let tx = interleave3(&b1, &b2, &b3);
        let (r1, r2, r3) = de_interleave3(&tx);
        assert_eq!(r1, b1);
        assert_eq!(r2, b2);
        assert_eq!(r3, b3);
    }

    #[test]
    fn bch_roundtrip_and_repair() {
        let data = rand_bits(21, 7);
        let block = bch_encode(RINGALERT_BCH_POLY, &data);
        assert_eq!(block.len(), 32);
        assert_eq!(ndivide(RINGALERT_BCH_POLY, &block[..31]), 0);
        // Two-bit error repairs.
        let mut b31: Vec<u8> = block[..31].to_vec();
        b31[3] ^= 1;
        b31[17] ^= 1;
        assert_eq!(bch_repair(RINGALERT_BCH_POLY, &mut b31), Some(2));
        assert_eq!(&b31[..21], &data[..]);
    }
}
