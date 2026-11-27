//! LoRa PHY bit coding: whitening, explicit header, payload CRC, Hamming
//! code, diagonal interleaver and Gray mapping.
//!
//! Symbol values here are "chirp values" (0..2^SF), the cyclic shift of the
//! base upchirp that the radio puts on the air. [`encode`] turns a payload
//! into chirp values and [`decode_header`] / [`decode_payload`] go the other
//! way. The layout follows the published reverse engineering of the Semtech
//! chips (Knight & Seeber 2016, Tapparel et al. 2020); see PROVENANCE.md.

use serde::Serialize;

/// Radio settings that shape the coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CodingParams {
    /// Spreading factor, 7..=12.
    pub sf: u8,
    /// Coding rate index 1..=4 (4/5 .. 4/8).
    pub cr: u8,
    /// Low data rate optimization: payload symbols carry SF-2 bits.
    pub ldro: bool,
}

/// Decoded explicit header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Header {
    pub length: u8,
    pub cr: u8,
    pub has_crc: bool,
}

/// The LoRa whitening sequence (LFSR x^8+x^6+x^5+x^4+1 seeded 0xFF), one byte
/// per payload byte.
pub fn whitening_seq(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut s: u8 = 0xFF;
    for _ in 0..len {
        out.push(s);
        let fb = ((s >> 7) ^ (s >> 5) ^ (s >> 4) ^ (s >> 3)) & 1;
        s = (s << 1) | fb;
    }
    out
}

/// The 16-bit payload CRC the SX127x/SX126x append: CRC-16/CCITT (poly
/// 0x1021, init 0) over all but the last two bytes, XORed with those two.
pub fn payload_crc(payload: &[u8]) -> u16 {
    let n = payload.len();
    if n < 2 {
        return 0;
    }
    let mut crc: u16 = 0;
    for &b in &payload[..n - 2] {
        let mut byte = b;
        for _ in 0..8 {
            let top = ((crc >> 8) as u8 ^ byte) & 0x80 != 0;
            crc <<= 1;
            if top {
                crc ^= 0x1021;
            }
            byte <<= 1;
        }
    }
    crc ^ payload[n - 1] as u16 ^ ((payload[n - 2] as u16) << 8)
}

/// Header checksum (5 bits) over the three header nibbles.
pub fn header_checksum(n0: u8, n1: u8, n2: u8) -> u8 {
    let b = |n: u8, i: u8| (n >> i) & 1;
    let c4 = b(n0, 3) ^ b(n0, 2) ^ b(n0, 1) ^ b(n0, 0);
    let c3 = b(n0, 3) ^ b(n1, 3) ^ b(n1, 2) ^ b(n1, 1) ^ b(n2, 0);
    let c2 = b(n0, 2) ^ b(n1, 3) ^ b(n1, 0) ^ b(n2, 3) ^ b(n2, 1);
    let c1 = b(n0, 1) ^ b(n1, 2) ^ b(n1, 0) ^ b(n2, 2) ^ b(n2, 1) ^ b(n2, 0);
    let c0 = b(n0, 0) ^ b(n1, 1) ^ b(n2, 3) ^ b(n2, 2) ^ b(n2, 1) ^ b(n2, 0);
    (c4 << 4) | (c3 << 3) | (c2 << 2) | (c1 << 1) | c0
}

/// Hamming-encode one nibble at coding rate `cr` (codeword of 4+cr bits,
/// data bits first, LSB of the nibble in the codeword MSB).
pub fn hamming_encode(nibble: u8, cr: u8) -> u8 {
    let d = |i: u8| (nibble >> i) & 1;
    let (b0, b1, b2, b3) = (d(0), d(1), d(2), d(3));
    if cr == 1 {
        let p4 = b0 ^ b1 ^ b2 ^ b3;
        return (b0 << 4) | (b1 << 3) | (b2 << 2) | (b3 << 1) | p4;
    }
    let p0 = b0 ^ b1 ^ b2;
    let p1 = b1 ^ b2 ^ b3;
    let p2 = b0 ^ b1 ^ b3;
    let p3 = b0 ^ b2 ^ b3;
    let full =
        (b0 << 7) | (b1 << 6) | (b2 << 5) | (b3 << 4) | (p0 << 3) | (p1 << 2) | (p2 << 1) | p3;
    full >> (4 - cr)
}

/// Decode a codeword to the nearest nibble. Returns (nibble, bits corrected).
/// For CR 4/5 and 4/6 the code only detects errors, so the data bits are
/// taken as received unless a unique nearest codeword exists.
pub fn hamming_decode(cw: u8, cr: u8) -> (u8, u32) {
    let mut best = (0u8, u32::MAX, false);
    for n in 0..16u8 {
        let dist = (hamming_encode(n, cr) ^ cw).count_ones();
        if dist < best.1 {
            best = (n, dist, false);
        } else if dist == best.1 {
            best.2 = true;
        }
    }
    if best.2 && cr <= 2 {
        // Ambiguous: keep the received data bits.
        let len = 4 + cr;
        let bit = |i: u8| (cw >> (len - 1 - i)) & 1;
        let n = bit(0) | (bit(1) << 1) | (bit(2) << 2) | (bit(3) << 3);
        return (n, 0);
    }
    (best.0, best.1)
}

fn gray(x: u32) -> u32 {
    x ^ (x >> 1)
}

fn gray_inverse(mut x: u32) -> u32 {
    let mut out = x;
    while x > 0 {
        x >>= 1;
        out ^= x;
    }
    out
}

/// Interleave `sf_app` codewords of `cw_len` bits into `cw_len` symbols of
/// `sf_app` bits (MSB first), the LoRa diagonal interleaver.
fn interleave(cws: &[u8], cw_len: usize, sf_app: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw_len);
    for i in 0..cw_len {
        let mut sym = 0u32;
        for j in 0..sf_app {
            let row = (i as isize - j as isize - 1).rem_euclid(sf_app as isize) as usize;
            let bit = (cws[row] >> (cw_len - 1 - i)) & 1;
            sym = (sym << 1) | bit as u32;
        }
        out.push(sym);
    }
    out
}

fn deinterleave(syms: &[u32], cw_len: usize, sf_app: usize) -> Vec<u8> {
    let mut cws = vec![0u8; sf_app];
    for (i, &s) in syms.iter().enumerate().take(cw_len) {
        for j in 0..sf_app {
            let bit = ((s >> (sf_app - 1 - j)) & 1) as u8;
            let row = (i as isize - j as isize - 1).rem_euclid(sf_app as isize) as usize;
            cws[row] |= bit << (cw_len - 1 - i);
        }
    }
    cws
}

/// Number of chirps (after the 8 header-block symbols) for a payload.
pub fn payload_symbol_count(length: usize, has_crc: bool, p: &CodingParams) -> usize {
    let sf = p.sf as usize;
    let nibbles = 2 * length + if has_crc { 4 } else { 0 };
    let in_first = sf - 7;
    let rest = nibbles.saturating_sub(in_first);
    let sf_app = if p.ldro { sf - 2 } else { sf };
    rest.div_ceil(sf_app) * (4 + p.cr as usize)
}

/// Encode a payload into chirp values (explicit header, CRC on).
pub fn encode(payload: &[u8], p: &CodingParams) -> Vec<u32> {
    assert!(payload.len() <= 255);
    let sf = p.sf as usize;
    let ws = whitening_seq(payload.len());
    let len = payload.len() as u8;
    let mut nibbles = vec![len >> 4, len & 0x0F, (p.cr << 1) | 1];
    let chk = header_checksum(nibbles[0], nibbles[1], nibbles[2]);
    nibbles.push(chk >> 4);
    nibbles.push(chk & 0x0F);
    for (b, w) in payload.iter().zip(&ws) {
        let x = b ^ w;
        nibbles.push(x & 0x0F);
        nibbles.push(x >> 4);
    }
    let crc = payload_crc(payload);
    for i in 0..4 {
        nibbles.push(((crc >> (4 * i)) & 0x0F) as u8);
    }

    let mut chirps = Vec::new();
    // Header block: SF-2 codewords at CR 4/8, reduced rate.
    let sf_hdr = sf - 2;
    let mut idx = 0;
    let take = |idx: &mut usize, n: usize, nibbles: &[u8]| -> Vec<u8> {
        let v: Vec<u8> = (0..n)
            .map(|k| nibbles.get(*idx + k).copied().unwrap_or(0))
            .collect();
        *idx += n;
        v
    };
    let hdr: Vec<u8> = take(&mut idx, sf_hdr, &nibbles)
        .iter()
        .map(|&n| hamming_encode(n, 4))
        .collect();
    for s in interleave(&hdr, 8, sf_hdr) {
        chirps.push(to_chirp(s, sf, true));
    }
    let sf_app = if p.ldro { sf - 2 } else { sf };
    let cw_len = 4 + p.cr as usize;
    while idx < nibbles.len() {
        let block: Vec<u8> = take(&mut idx, sf_app, &nibbles)
            .iter()
            .map(|&n| hamming_encode(n, p.cr))
            .collect();
        for s in interleave(&block, cw_len, sf_app) {
            chirps.push(to_chirp(s, sf, p.ldro));
        }
    }
    chirps
}

/// Map interleaver output to a chirp value: inverse Gray, ×4 at reduced
/// rate, then the +1 offset the chips apply.
fn to_chirp(s: u32, sf: usize, reduced: bool) -> u32 {
    let n = 1u32 << sf;
    let v = gray_inverse(s);
    let v = if reduced { v << 2 } else { v };
    (v + 1) % n
}

/// Map a demodulated chirp value back to interleaver bits.
fn from_chirp(bin: u32, sf: usize, reduced: bool) -> u32 {
    let n = 1u32 << sf;
    let v = (bin + n - 1) % n;
    let v = if reduced { ((v + 2) / 4) % (n / 4) } else { v };
    gray(v)
}

/// Result of decoding the 8-symbol header block.
#[derive(Debug, Clone)]
pub struct HeaderBlock {
    pub header: Header,
    /// Payload nibbles carried in the header block (SF-7 of them).
    pub extra_nibbles: Vec<u8>,
    pub corrected: u32,
}

/// Decode the first 8 chirps (explicit header). `None` when the header
/// checksum fails.
pub fn decode_header(chirps: &[u32], sf: u8) -> Option<HeaderBlock> {
    if chirps.len() < 8 {
        return None;
    }
    let sf = sf as usize;
    let sf_hdr = sf - 2;
    let syms: Vec<u32> = chirps[..8]
        .iter()
        .map(|&c| from_chirp(c, sf, true))
        .collect();
    let cws = deinterleave(&syms, 8, sf_hdr);
    let mut corrected = 0;
    let nibbles: Vec<u8> = cws
        .iter()
        .map(|&cw| {
            let (n, c) = hamming_decode(cw, 4);
            corrected += c;
            n
        })
        .collect();
    let chk = header_checksum(nibbles[0], nibbles[1], nibbles[2]);
    let got = ((nibbles[3] & 1) << 4) | nibbles[4];
    if chk != got {
        return None;
    }
    let cr = nibbles[2] >> 1;
    if !(1..=4).contains(&cr) {
        return None;
    }
    let length = (nibbles[0] << 4) | nibbles[1];
    Some(HeaderBlock {
        header: Header {
            length,
            cr,
            has_crc: nibbles[2] & 1 == 1,
        },
        extra_nibbles: nibbles[5..].to_vec(),
        corrected,
    })
}

/// A decoded LoRa payload.
#[derive(Debug, Clone)]
pub struct Payload {
    pub bytes: Vec<u8>,
    /// `None` when the header said no CRC.
    pub crc_ok: Option<bool>,
    pub corrected: u32,
}

/// Decode the payload chirps that follow the header block.
pub fn decode_payload(hb: &HeaderBlock, chirps: &[u32], sf: u8, ldro: bool) -> Payload {
    let sfu = sf as usize;
    let cr = hb.header.cr as usize;
    let cw_len = 4 + cr;
    let sf_app = if ldro { sfu - 2 } else { sfu };
    let mut nibbles = hb.extra_nibbles.clone();
    let mut corrected = hb.corrected;
    for block in chirps.chunks(cw_len) {
        if block.len() < cw_len {
            break;
        }
        let syms: Vec<u32> = block.iter().map(|&c| from_chirp(c, sfu, ldro)).collect();
        for cw in deinterleave(&syms, cw_len, sf_app) {
            let (n, c) = hamming_decode(cw, cr as u8);
            corrected += c;
            nibbles.push(n);
        }
    }
    let len = hb.header.length as usize;
    // Too few chirps for the advertised length: never fill in zeros and
    // call the result verified.
    let need = 2 * len + if hb.header.has_crc { 4 } else { 0 };
    let complete = nibbles.len() >= need;
    let ws = whitening_seq(len);
    let bytes: Vec<u8> = (0..len)
        .map(|i| {
            let lo = nibbles.get(2 * i).copied().unwrap_or(0);
            let hi = nibbles.get(2 * i + 1).copied().unwrap_or(0);
            ((hi << 4) | lo) ^ ws[i]
        })
        .collect();
    let crc_ok = if hb.header.has_crc {
        let c = |k: usize| nibbles.get(2 * len + k).copied().unwrap_or(0) as u16;
        let rx = c(0) | (c(1) << 4) | (c(2) << 8) | (c(3) << 12);
        Some(complete && rx == payload_crc(&bytes))
    } else {
        None
    };
    Payload {
        bytes,
        crc_ok,
        corrected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitening_matches_published_prefix() {
        assert_eq!(
            whitening_seq(12),
            vec![0xFF, 0xFE, 0xFC, 0xF8, 0xF0, 0xE1, 0xC2, 0x85, 0x0B, 0x17, 0x2F, 0x5E]
        );
    }

    #[test]
    fn hamming_roundtrip_and_correct() {
        for cr in 1..=4 {
            for n in 0..16 {
                assert_eq!(hamming_decode(hamming_encode(n, cr), cr).0, n);
            }
        }
        for n in 0..16u8 {
            let cw = hamming_encode(n, 4);
            for bit in 0..8 {
                assert_eq!(hamming_decode(cw ^ (1 << bit), 4).0, n);
            }
        }
    }

    #[test]
    fn coding_roundtrip_all_settings() {
        let payload: Vec<u8> = (0..57u8).map(|i| i.wrapping_mul(37) ^ 0x5A).collect();
        for sf in 7..=12 {
            for cr in 1..=4 {
                for ldro in [false, true] {
                    let p = CodingParams { sf, cr, ldro };
                    let chirps = encode(&payload, &p);
                    assert_eq!(
                        chirps.len(),
                        8 + payload_symbol_count(payload.len(), true, &p)
                    );
                    let hb = decode_header(&chirps, sf).expect("header");
                    assert_eq!(hb.header.length as usize, payload.len());
                    assert_eq!(hb.header.cr, cr);
                    let out = decode_payload(&hb, &chirps[8..], sf, ldro);
                    assert_eq!(out.bytes, payload, "sf{sf} cr{cr} ldro{ldro}");
                    assert_eq!(out.crc_ok, Some(true));
                }
            }
        }
    }

    #[test]
    fn reduced_rate_symbols_are_multiples_of_four_plus_one() {
        let p = CodingParams {
            sf: 9,
            cr: 1,
            ldro: false,
        };
        for c in &encode(b"hello", &p)[..8] {
            assert_eq!((c + 511) % 4, 0);
        }
    }
}
