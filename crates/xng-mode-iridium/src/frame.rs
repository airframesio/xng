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
        // Whole-block even parity (over the repaired 31 bits + parity)
        // guards against BCH miscorrection. A weight-1 correction on this
        // d=5 BCH(31,21) is unambiguous (no weight-≤2 error shares a
        // single-flip syndrome), so an odd overall parity after errs≤1
        // means the separate parity bit was the second flipped bit — a
        // harmless error that does not touch the 21 data bits. Only an
        // errs==2 correction (BCH at its t=2 limit) with bad parity
        // signals a likely >2-error miscorrection worth truncating on.
        let ones: u32 =
            b31.iter().map(|&v| v as u32).sum::<u32>() + block[31] as u32;
        if ones % 2 == 1 && errs >= 2 {
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
    /// Time-Location ("TL", satellite ranging broadcast). 96-bit header
    /// `11` + 94 zeros; the payload is descrambled, not BCH-coded.
    Itl,
    /// LCW-bearing duplex frame (DA/voice/IP/sync by frame type).
    Lw,
    Unknown,
}

/// Classify the post-access bit stream (downlink heuristics from the
/// toolkit, without frequency classing).
pub fn classify(data: &[u8]) -> FrameKind {
    if data.len() >= 32 && data[..32] == HEADER_MESSAGING[..] {
        return FrameKind::Ms;
    }
    // ITL ("TL", Time-Location): 96-bit header is `11` followed by 94
    // zeros (toolkit `header_time_location`). Match tolerantly — the
    // 24-bit access + UW fit already confirmed a real burst, so a handful
    // of off-air bit errors in the header must not let an ITL frame fall
    // through to the IRA classifier, where its near-all-zero header is a
    // valid (degenerate) BCH codeword and would mis-decode as an all-zero
    // ring alert.
    if data.len() >= 96 {
        let diff = (data[0] == 0) as u32
            + (data[1] == 0) as u32
            + data[2..96].iter().map(|&b| b as u32).sum::<u32>();
        if diff <= 3 {
            return FrameKind::Itl;
        }
    }
    if data.len() > 6 + 64 && ndivide(HDR_POLY, &data[..6]) == 0 {
        let (b1, b2) = de_interleave2(&data[6..6 + 64]);
        if ndivide(RINGALERT_BCH_POLY, &b1[..31]) == 0
            && ndivide(RINGALERT_BCH_POLY, &b2[..31]) == 0
        {
            return FrameKind::Bc;
        }
    }
    if data.len() > 64 {
        // LCW: zero-syndrome check on its three BCH components.
        let lcw: Vec<u8> = LCW_TABLE.iter().map(|&i| data[i - 1]).collect();
        if ndivide(HDR_POLY, &lcw[..7]) == 0 && ndivide(LCW3_POLY, &lcw[20..]) == 0 {
            let mut b2a: Vec<u8> = lcw[7..20].to_vec();
            b2a.push(0);
            let mut b2b: Vec<u8> = lcw[7..20].to_vec();
            b2b.push(1);
            if ndivide(LCW2_POLY, &b2a) == 0 || ndivide(LCW2_POLY, &b2b) == 0 {
                return FrameKind::Lw;
            }
        }
    }
    if data.len() >= 96 {
        let (mut b1, mut b2, mut b3) = de_interleave3(&data[..96]);
        let zeros = (ndivide(RINGALERT_BCH_POLY, &b1[..31]) == 0) as u32
            + (ndivide(RINGALERT_BCH_POLY, &b2[..31]) == 0) as u32
            + (ndivide(RINGALERT_BCH_POLY, &b3[..31]) == 0) as u32;
        // Accept BCH-*correctable* headers, not just all-zero syndromes:
        // a real off-air RA burst routinely carries one correctable bit
        // error in the header, which the downstream ecc_blocks fixes. The
        // 24-bit access code already gated this as a genuine burst, so
        // requiring >=1 clean block + a small total error budget keeps
        // false classifications away while not dropping real frames.
        if let (Some(e1), Some(e2), Some(e3)) = (
            bch_repair(RINGALERT_BCH_POLY, &mut b1[..31]),
            bch_repair(RINGALERT_BCH_POLY, &mut b2[..31]),
            bch_repair(RINGALERT_BCH_POLY, &mut b3[..31]),
        ) {
            if zeros >= 1 && e1 + e2 + e3 <= 3 {
                return FrameKind::Ra;
            }
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

/// BCH-encode data bits with `check` check bits (no parity bit).
pub fn bch_encode_raw(poly: u32, data: &[u8], check: usize) -> Vec<u8> {
    let mut padded: Vec<u8> = data.to_vec();
    padded.extend(std::iter::repeat(0).take(check));
    let rem = ndivide(poly, &padded);
    let mut block: Vec<u8> = data.to_vec();
    for k in (0..check).rev() {
        block.push(((rem >> k) & 1) as u8);
    }
    block
}

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

    #[test]
    fn itl_header_classifies_as_itl_not_ra() {
        // ITL ("TL"): 96-bit header `11` + 94 zeros, then payload.
        let mut data = vec![0u8; 96 + 64];
        data[0] = 1;
        data[1] = 1;
        for i in 96..data.len() {
            data[i] = ((i * 7) % 3 == 0) as u8;
        }
        assert_eq!(classify(&data), FrameKind::Itl);
        // A couple of off-air bit errors in the zero run still match ITL.
        let mut noisy = data.clone();
        noisy[40] = 1;
        noisy[71] = 1;
        assert_eq!(classify(&noisy), FrameKind::Itl);
        // A degenerate all-zero header must never become a ring alert (its
        // blocks are the trivially-valid all-zero BCH codeword).
        assert_ne!(classify(&vec![0u8; 96 + 64]), FrameKind::Ra);
    }

    #[test]
    fn ira_header_still_classifies_as_ra() {
        // A real IRA header (nonzero data) is well clear of the ITL
        // tolerance and must still classify as RA.
        let b1 = bch_encode(RINGALERT_BCH_POLY, &rand_bits(21, 101));
        let b2 = bch_encode(RINGALERT_BCH_POLY, &rand_bits(21, 102));
        let b3 = bch_encode(RINGALERT_BCH_POLY, &rand_bits(21, 103));
        let mut data = interleave3(&b1, &b2, &b3);
        data.extend(std::iter::repeat(0u8).take(64));
        assert_eq!(classify(&data), FrameKind::Ra);
    }

    #[test]
    fn ecc_blocks_tolerates_flipped_parity_bit() {
        // Three valid RA header blocks; block 2 carries one correctable
        // BCH error AND a flipped overall-parity bit. The weight-1 BCH
        // correction is unambiguous, so the block (and the whole frame)
        // must survive rather than truncate at the parity mismatch.
        let mut blocks = vec![
            bch_encode(RINGALERT_BCH_POLY, &rand_bits(21, 11)),
            bch_encode(RINGALERT_BCH_POLY, &rand_bits(21, 12)),
            bch_encode(RINGALERT_BCH_POLY, &rand_bits(21, 13)),
        ];
        blocks[2][5] ^= 1; // BCH-correctable bit error
        blocks[2][31] ^= 1; // overall even-parity bit flipped
        let (payload, fixed) = ecc_blocks(&blocks, RINGALERT_BCH_POLY);
        assert_eq!(payload.len(), 63, "all three blocks must survive");
        assert!(fixed >= 1);
    }
}

// ── LCW (link control word) + DA frames (iridium-toolkit, BSD-2) ────────

/// LCW deinterleave permutation (1-based bit indices into the 46-bit
/// post-access stream; toolkit `de_interleave_lcw`).
const LCW_TABLE: [usize; 46] = [
    40, 39, 36, 35, 32, 31, 28, 27, 24, 23, 20, 19, 16, 15, 12, 11, 8, 7, 4, 3,
    41, 38, 37, 34, 33, 30, 29, 26, 25, 22, 21, 18, 17, 14, 13, 10, 9, 6, 5, 2,
    1, 46, 45, 44, 43, 42,
];

pub const LCW2_POLY: u32 = 465;
pub const LCW3_POLY: u32 = 41;
pub const ACCH_BCH_POLY: u32 = 3545;

/// Decode the 46-bit LCW: returns (frame type, lcw2 data, lcw3 data,
/// corrected bits) or None when any component is uncorrectable.
pub fn decode_lcw(bits: &[u8]) -> Option<(u8, u32, u32, u32)> {
    if bits.len() < 46 {
        return None;
    }
    let lcw: Vec<u8> = LCW_TABLE.iter().map(|&i| bits[i - 1]).collect();
    let (o1, o2, o3) = (&lcw[..7], &lcw[7..20], &lcw[20..]);

    let mut b1 = o1.to_vec();
    let e1 = bch_repair(HDR_POLY, &mut b1)?;
    // lcw2 is transmitted with one bit missing; try both completions.
    let mut b2a: Vec<u8> = o2.to_vec();
    b2a.push(0);
    let r2a = bch_repair(LCW2_POLY, &mut b2a);
    let mut b2b: Vec<u8> = o2.to_vec();
    b2b.push(1);
    let r2b = bch_repair(LCW2_POLY, &mut b2b);
    let (e2, b2) = match (r2a, r2b) {
        (Some(a), Some(b)) if b < a => (b, b2b),
        (Some(a), _) => (a, b2a),
        (None, Some(b)) => (b, b2b),
        (None, None) => return None,
    };
    let mut b3 = o3.to_vec();
    let e3 = bch_repair(LCW3_POLY, &mut b3)?;

    let ft = b1[..3].iter().fold(0u8, |v, &b| (v << 1) | b);
    let lcw2 = b2[..6].iter().fold(0u32, |v, &b| (v << 1) | b as u32);
    let lcw3 = b3[..21].iter().fold(0u32, |v, &b| (v << 1) | b as u32);
    Some((ft, lcw2, lcw3, e1 + e2 + e3))
}

/// A decoded DA (SBD data) frame.
#[derive(Debug, Clone, PartialEq)]
pub struct DaFrame {
    /// More fragments follow.
    pub continuation: bool,
    /// 3-bit fragment counter.
    pub ctr: u8,
    /// Used payload bytes (≤ 20).
    pub len: u8,
    /// The 20 payload bytes.
    pub data: [u8; 20],
    pub crc_ok: bool,
    pub bch_corrected: u32,
}

/// Decode the post-LCW payload of an ft==2 frame (312 bits) into a DA
/// frame: 124-bit chunks → 2-way deinterleave → 31-bit BCH blocks in
/// order [b4,b2,b3,b1] (final 64-bit chunk: two blocks, first bit
/// dropped) → BCH(31,21) poly 3545 → 200 data bits.
pub fn decode_da(data: &[u8]) -> Option<DaFrame> {
    if data.len() < 312 {
        return None;
    }
    let data = &data[..312];
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for chunk in data[..248].chunks_exact(124) {
        let (b1, b2) = de_interleave2(chunk);
        let all: Vec<u8> = b1.iter().chain(&b2).copied().collect();
        let q: Vec<Vec<u8>> = all.chunks_exact(31).map(|c| c.to_vec()).collect();
        blocks.push(q[3].clone());
        blocks.push(q[1].clone());
        blocks.push(q[2].clone());
        blocks.push(q[0].clone());
    }
    let (b1, b2) = de_interleave2(&data[248..312]);
    blocks.push(b2[1..].to_vec());
    blocks.push(b1[1..].to_vec());

    let mut bits = Vec::with_capacity(200);
    let mut fixed = 0u32;
    for block in &mut blocks {
        let errs = bch_repair(ACCH_BCH_POLY, block)?;
        if errs > 0 {
            fixed += 1;
        }
        bits.extend_from_slice(&block[..20]);
    }

    let field = |r: std::ops::Range<usize>| bits[r].iter().fold(0u32, |v, &b| (v << 1) | b as u32);
    let continuation = bits[3] == 1;
    let ctr = field(5..8) as u8;
    let len = field(11..16) as u8;
    if field(17..20) != 0 || field(196..200) != 0 {
        return None;
    }
    let mut payload = [0u8; 20];
    for (i, byte) in bits[20..180].chunks_exact(8).enumerate() {
        payload[i] = byte.iter().fold(0u8, |v, &b| (v << 1) | b);
    }
    // CRC-CCITT-FALSE over bits[0..20] + 12 zero bits + bits[20..196];
    // result must be zero.
    let mut crc_stream: Vec<u8> = bits[..20].to_vec();
    crc_stream.extend(std::iter::repeat(0).take(12));
    crc_stream.extend_from_slice(&bits[20..196]);
    let crc_bytes: Vec<u8> = crc_stream
        .chunks_exact(8)
        .map(|c| c.iter().fold(0u8, |v, &b| (v << 1) | b))
        .collect();
    let crc = crc::Crc::<u16>::new(&crc::CRC_16_IBM_3740);
    let crc_ok = len > 0 && crc.checksum(&crc_bytes) == 0;

    Some(DaFrame { continuation, ctr, len, data: payload, crc_ok, bch_corrected: fixed })
}

/// TX (loopback/testing): inverse LCW interleave — place the three
/// BCH-encoded LCW parts back into transmitted bit positions. lcw2 is
/// transmitted with its final bit dropped.
pub fn encode_lcw(ft: u8, lcw2_data: u32, lcw3_data: u32) -> Vec<u8> {
    let ft_bits: Vec<u8> = (0..3).rev().map(|k| (ft >> k) & 1).collect();
    let p1 = bch_encode_raw(HDR_POLY, &ft_bits, 4);
    let d2: Vec<u8> = (0..6).rev().map(|k| ((lcw2_data >> k) & 1) as u8).collect();
    let mut p2 = bch_encode_raw(LCW2_POLY, &d2, 8);
    p2.pop(); // the missing transmitted bit
    let d3: Vec<u8> = (0..21).rev().map(|k| ((lcw3_data >> k) & 1) as u8).collect();
    let p3 = bch_encode_raw(LCW3_POLY, &d3, 5);
    let lcw: Vec<u8> = p1.into_iter().chain(p2).chain(p3).collect();
    let mut out = vec![0u8; 46];
    for (k, &b) in lcw.iter().enumerate() {
        out[LCW_TABLE[k] - 1] = b;
    }
    out
}

/// TX: build the 312 transmitted bits of a DA frame from its 200 data
/// bits (inverse of `decode_da`'s block mapping).
pub fn encode_da_payload(bits200: &[u8]) -> Vec<u8> {
    debug_assert_eq!(bits200.len(), 200);
    let blocks: Vec<Vec<u8>> = bits200
        .chunks_exact(20)
        .map(|d| bch_encode_raw(ACCH_BCH_POLY, d, 11))
        .collect();
    let mut out = Vec::with_capacity(312);
    // Chunks of 4 blocks → de_interleave order [b4,b2,b3,b1] reversed:
    // stream blocks q0..q3 = [blk3, blk1, blk2, blk0] of each group.
    for grp in blocks[..8].chunks_exact(4) {
        let q: Vec<&Vec<u8>> = vec![&grp[3], &grp[1], &grp[2], &grp[0]];
        let all: Vec<u8> = q.into_iter().flatten().copied().collect();
        out.extend(interleave2(&all[..62], &all[62..]));
    }
    // Final pair: [b2[1:], b1[1:]] with a dropped leading bit each.
    let mut b1 = vec![0u8];
    b1.extend_from_slice(&blocks[9]);
    let mut b2 = vec![0u8];
    b2.extend_from_slice(&blocks[8]);
    out.extend(interleave2(&b1, &b2));
    out
}

/// TX: build a DA frame's 200 data bits from fields + payload, with a
/// valid CRC.
pub fn build_da_bits(cont: bool, ctr: u8, len: u8, payload: &[u8; 20]) -> Vec<u8> {
    let mut bits = vec![0u8; 200];
    bits[3] = cont as u8;
    for k in 0..3 {
        bits[5 + k] = (ctr >> (2 - k)) & 1;
    }
    for k in 0..5 {
        bits[11 + k] = (len >> (4 - k)) & 1;
    }
    for (i, &b) in payload.iter().enumerate() {
        for k in 0..8 {
            bits[20 + i * 8 + k] = (b >> (7 - k)) & 1;
        }
    }
    // CRC over bits[0..20] + 12 zeros + bits[20..180]; stored at 180..196
    // such that the check stream (with the CRC at 180..196 included via
    // bits[20..196]) divides to zero.
    let crc = crc::Crc::<u16>::new(&crc::CRC_16_IBM_3740);
    let mut stream: Vec<u8> = bits[..20].to_vec();
    stream.extend(std::iter::repeat(0).take(12));
    stream.extend_from_slice(&bits[20..180]);
    let bytes: Vec<u8> = stream
        .chunks_exact(8)
        .map(|c| c.iter().fold(0u8, |v, &b| (v << 1) | b))
        .collect();
    let c = crc.checksum(&bytes);
    for k in 0..16 {
        bits[180 + k] = ((c >> (15 - k)) & 1) as u8;
    }
    bits
}
