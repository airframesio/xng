//! Aero C-channel (8 400 bps OQPSK voice circuits), ported from JAERO
//! `aerol.cpp::DecodeC` (MIT; see PROVENANCE.md).
//!
//! Frame structure (one ~500 ms superframe = 4 208 bits at 8 400 bps):
//!
//! - 112-bit unique word: two 52-bit rail patterns bit-interleaved
//!   (every detector tries both patterns and their complements, so the
//!   OQPSK 180° ambiguity resolves per rail)
//! - 4 096 coded bits: K=7 rate-1/2 convolutional (the P-channel
//!   polynomials), punctured 3/4 (every 4th coded bit dropped), block
//!   interleaved as 16 consecutive 64×4 row-permuted blocks
//! - decoded 2 730 bits → first 2 714 kept: 25 sub-blocks of
//!   1 + 96 voice + 12 data bits — the 96-bit chunks are AMBE voice
//!   frames (12 bytes each, 20 ms; codec proprietary, bytes surfaced
//!   for external decoding), the 12-bit chunks accumulate into 12-byte
//!   sub-band signal units with a CRC-16/X.25 trailer.

use xng_dsp::scramble::Lfsr15;
use xng_dsp::viterbi::Viterbi;

pub const FRAME_CODED_BITS: usize = 4096;
pub const INFO_BITS: usize = 2714;
const DECODED_BITS: usize = 2730;
const SUBBLOCK_BITS: usize = 109; // 1 + 96 voice + 12 data
const VOICE_FRAMES: usize = 25;
const UW_LEN: usize = 52;
/// The two 52-bit UW rail patterns (JAERO `setPreamble` arguments).
pub const UW_RAIL1: u64 = 216_866_263_330_005;
pub const UW_RAIL2: u64 = 3_012_071_630_031_408;
/// 52-bit sliding correlator over both rail patterns and their
/// complements (JAERO `OQPSKPreambleDetectorAndAmbiguityCorrection`:
/// every rail tries both patterns, so the rail phase is arbitrary and
/// the OQPSK 180° ambiguity resolves from which polarity matched).
struct UwDetector {
    p1: [u8; UW_LEN],
    p2: [u8; UW_LEN],
    buf: [u8; UW_LEN],
    fill: usize,
    tolerance: u32,
    /// Set when a *complement* matched: this rail is BPSK-inverted.
    pub inverted: bool,
}

fn pattern_bits(v: u64) -> [u8; UW_LEN] {
    let mut p = [0u8; UW_LEN];
    for (i, b) in p.iter_mut().enumerate() {
        *b = ((v >> (UW_LEN - 1 - i)) & 1) as u8;
    }
    p
}

impl UwDetector {
    fn new(tolerance: u32) -> Self {
        Self {
            p1: pattern_bits(UW_RAIL1),
            p2: pattern_bits(UW_RAIL2),
            buf: [0; UW_LEN],
            fill: 0,
            tolerance,
            inverted: false,
        }
    }

    fn update(&mut self, bit: u8) -> bool {
        self.buf.rotate_left(1);
        self.buf[UW_LEN - 1] = bit;
        self.fill = (self.fill + 1).min(UW_LEN);
        if self.fill < UW_LEN {
            return false;
        }
        for pat in [&self.p1, &self.p2] {
            let diff: u32 = self
                .buf
                .iter()
                .zip(pat)
                .map(|(&b, &p)| (b ^ p) as u32)
                .sum();
            if diff <= self.tolerance {
                self.inverted = false;
                return true;
            }
            if diff >= UW_LEN as u32 - self.tolerance {
                self.inverted = true;
                return true;
            }
        }
        false
    }
}

/// One decoded C-channel event.
#[derive(Debug, Clone, PartialEq)]
pub enum CChannelEvent {
    /// A 12-byte AMBE voice frame (96 bits, 20 ms of compressed audio).
    Voice([u8; 12]),
    /// A CRC-valid 12-byte sub-band signal unit (CRC trailer included).
    SignalUnit([u8; 12]),
}

/// Sub-band SU message types (JAERO `AEROTypeC`).
pub fn su_type_name(t: u8) -> &'static str {
    match t {
        0x01 => "fill",
        0x30 => "call-progress",
        0x60 => "telephony-acknowledge",
        _ => "other",
    }
}

pub struct CChannelDeframer {
    det_a: UwDetector,
    det_b: UwDetector,
    rail: bool,
    sync_arm: bool,
    synced: bool,
    /// Soft bits since the UW (only the first FRAME_CODED_BITS kept).
    frame: Vec<f32>,
    viterbi: Viterbi,
    inv_a: bool,
    inv_b: bool,
}

impl Default for CChannelDeframer {
    fn default() -> Self {
        Self::new()
    }
}

impl CChannelDeframer {
    pub fn new() -> Self {
        Self {
            det_a: UwDetector::new(6),
            det_b: UwDetector::new(6),
            rail: false,
            sync_arm: false,
            synced: false,
            frame: Vec::with_capacity(FRAME_CODED_BITS),
            viterbi: Viterbi::k7(),
            inv_a: false,
            inv_b: false,
        }
    }

    /// Push one soft bit (sign = decision, magnitude = confidence);
    /// bits alternate OQPSK rails. Returns decoded events as frames
    /// complete.
    pub fn push(&mut self, soft: f32) -> Vec<CChannelEvent> {
        let mut out = Vec::new();
        let hard = (soft > 0.0) as u8;

        // UW search runs continuously on both rail detectors; sync
        // fires on hits in two consecutive bit slots (one per rail),
        // mirroring JAERO's gotsync_last handshake.
        self.rail = !self.rail;
        let hit = if self.rail {
            let h = self.det_a.update(hard);
            if h {
                self.inv_a = self.det_a.inverted;
            }
            h
        } else {
            let h = self.det_b.update(hard);
            if h {
                self.inv_b = self.det_b.inverted;
            }
            h
        };
        if hit && self.sync_arm {
            self.sync_arm = false;
            self.synced = true;
            self.frame.clear();
            return out;
        }
        self.sync_arm = hit;

        if self.synced && self.frame.len() < FRAME_CODED_BITS {
            // Per-rail BPSK ambiguity correction from the UW match.
            let inv = if self.rail { self.inv_a } else { self.inv_b };
            self.frame.push(if inv { -soft } else { soft });
            if self.frame.len() == FRAME_CODED_BITS {
                out.extend(self.decode_frame());
            }
        }
        out
    }

    fn decode_frame(&mut self) -> Vec<CChannelEvent> {
        // 16 deinterleave blocks of 256 (64 rows × 4 cols, row
        // permutation (27·i) mod 64, column-major readout on TX).
        let mut deleaved = Vec::with_capacity(FRAME_CODED_BITS);
        for blk in self.frame.chunks_exact(256) {
            let mut block = [0.0f32; 256];
            let mut k = 0;
            for j in 0..4 {
                for i in 0..64 {
                    block[k] = blk[depermute(i) * 4 + j];
                    k += 1;
                }
            }
            deleaved.extend_from_slice(&block);
        }

        // Depuncture 3/4: a neutral bit after every 3rd received bit
        // (the final source bit is dropped, matching JAERO). Frames
        // decode independently — the unflushed Viterbi tail only
        // degrades the dropped 2 714..2 730 dummy region.
        let mut depunct = Vec::with_capacity(5460);
        let mut ptr = 0usize;
        for &s in &deleaved[..deleaved.len() - 1] {
            ptr += 1;
            depunct.push(s);
            if ptr >= 3 {
                depunct.push(0.0);
                ptr = 0;
            }
        }
        let mut bits = self.viterbi.decode(&depunct);
        bits.truncate(INFO_BITS);
        if bits.len() < INFO_BITS {
            return Vec::new();
        }

        // Descramble (LFSR reset per frame, like the P-channel).
        Lfsr15::new().apply(&mut bits);

        let mut out = Vec::new();

        // Voice: from bit 1, runs of 96 bits separated by 13 skipped.
        let mut voice = Vec::with_capacity(VOICE_FRAMES * 12);
        let mut h = 1usize;
        let mut acc = 0u8;
        let mut nb = 0u8;
        let mut bitsin = 0usize;
        while h < INFO_BITS {
            acc |= bits[h] << 7;
            nb += 1;
            if nb == 8 {
                voice.push(acc);
                acc = 0;
                nb = 0;
            } else {
                acc >>= 1;
            }
            bitsin += 1;
            h += 1;
            if bitsin == 96 {
                bitsin = 0;
                h += 13;
            }
        }
        for f in voice.chunks_exact(12).take(VOICE_FRAMES) {
            out.push(CChannelEvent::Voice(f.try_into().unwrap()));
        }

        // Data: 12 bits per sub-block at offset 97..109, accumulated
        // LSB-first into 12-byte signal units, CRC-16/X.25 checked.
        let mut su = Vec::with_capacity(12);
        let mut acc = 0u8;
        let mut nb = 0u8;
        for y in 0..24 {
            let offset = y * SUBBLOCK_BITS;
            for h in offset + 97..offset + 109 {
                acc |= bits[h] << 7;
                nb += 1;
                if nb == 8 {
                    su.push(acc);
                    acc = 0;
                    nb = 0;
                } else {
                    acc >>= 1;
                }
            }
            if su.len() == 12 {
                if crate::su::su_crc_ok(&su) {
                    out.push(CChannelEvent::SignalUnit(su[..].try_into().unwrap()));
                }
                su.clear();
            }
        }

        self.synced = false; // re-arm on the next UW
        out
    }
}

#[inline]
fn permute(i: usize) -> usize {
    (27 * i) % 64
}

#[inline]
fn depermute(i: usize) -> usize {
    // inverse of (27·i) mod 64: 27·19 = 513 ≡ 1 (mod 64)
    (19 * i) % 64
}

/// Mirror encoder for loopback testing: builds the UW + 4 096 coded
/// bits for one frame from 2 714 info bits (bit 0 and the per-frame
/// tail are zero-filled).
pub struct CChannelEncoder {
    viterbi: Viterbi,
}

impl Default for CChannelEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CChannelEncoder {
    pub fn new() -> Self {
        Self { viterbi: Viterbi::k7() }
    }

    /// Encode one frame; returns hard bits (UW then coded payload).
    pub fn encode(&mut self, info: &[u8]) -> Vec<u8> {
        assert_eq!(info.len(), INFO_BITS);
        let mut bits = info.to_vec();
        bits.resize(DECODED_BITS, 0);
        Lfsr15::new().apply(&mut bits);
        let coded = self.viterbi.encode(&bits); // 5 460 bits

        // Puncture 3/4: drop every 4th coded bit.
        let mut punct = Vec::with_capacity(FRAME_CODED_BITS);
        for (i, &b) in coded.iter().enumerate() {
            if i % 4 != 3 {
                punct.push(b);
            }
        }
        punct.push(0); // the dropped trailing bit
        assert_eq!(punct.len(), FRAME_CODED_BITS);

        // Interleave per 256-bit block (column-major write of the
        // row-permuted 64×4 matrix — the inverse of decode_frame).
        let mut tx = Vec::with_capacity(112 + FRAME_CODED_BITS);
        // UW: rails alternate starting with rail A (detector order):
        // 8 lead-in zeros keep the 112-bit spacing.
        tx.extend([0u8; 8]);
        for i in 0..UW_LEN {
            tx.push(((UW_RAIL1 >> (UW_LEN - 1 - i)) & 1) as u8);
            tx.push(((UW_RAIL2 >> (UW_LEN - 1 - i)) & 1) as u8);
        }
        for blk in punct.chunks_exact(256) {
            let mut block = [0u8; 256];
            for j in 0..4 {
                for i in 0..64 {
                    block[i * 4 + j] = blk[j * 64 + permute(i)];
                }
            }
            tx.extend_from_slice(&block);
        }
        tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(su: &[u8; 12], voice_byte: u8) -> Vec<u8> {
        // Build 2 714 info bits: voice areas filled with a marker
        // byte pattern, the first sub-block data slots carrying `su`.
        let mut bits = vec![0u8; INFO_BITS];
        // voice bits (offset+1..offset+97) — LSB-first byte pattern
        for y in 0..VOICE_FRAMES {
            let offset = y * SUBBLOCK_BITS;
            for (n, b) in bits[offset + 1..]
                .iter_mut()
                .take(96)
                .enumerate()
            {
                if offset + 1 + n >= INFO_BITS {
                    break;
                }
                *b = (voice_byte >> (n % 8)) & 1;
            }
        }
        // SU: 96 bits over the first 8 sub-blocks' 12-bit data slots,
        // LSB-first
        let mut bitstream: Vec<u8> = su
            .iter()
            .flat_map(|&b| (0..8).map(move |i| (b >> i) & 1))
            .collect();
        bitstream.reverse(); // pop() from the front
        for y in 0..8 {
            let offset = y * SUBBLOCK_BITS;
            for h in offset + 97..offset + 109 {
                bits[h] = bitstream.pop().unwrap();
            }
        }
        bits
    }

    #[test]
    fn loopback_voice_and_su() {
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x30; // call-progress
        su10[1..4].copy_from_slice(&[0xAB, 0xCD, 0xEF]); // AES
        su10[4] = 0x44; // GES
        let su: [u8; 12] = crate::su::su_with_crc(su10).try_into().unwrap();

        let info = frame_with(&su, 0x5A);
        let mut enc = CChannelEncoder::new();
        let mut dec = CChannelDeframer::new();

        // Two identical frames: the first sync arms mid-stream, the
        // second decodes (and exercises the continuous-FEC carry).
        let mut events = Vec::new();
        for _ in 0..3 {
            for &b in &enc.encode(&info) {
                let soft = if b == 1 { 0.9f32 } else { -0.9 };
                events.extend(dec.push(soft));
            }
        }
        let voices: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                CChannelEvent::Voice(v) => Some(v),
                _ => None,
            })
            .collect();
        let sus: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                CChannelEvent::SignalUnit(s) => Some(s),
                _ => None,
            })
            .collect();
        assert!(!voices.is_empty(), "no voice frames decoded");
        assert_eq!(voices[0][0], 0x5A, "voice byte pattern survives");
        assert!(!sus.is_empty(), "no signal units decoded");
        assert_eq!(sus[0][0], 0x30);
        assert_eq!(&sus[0][1..4], &[0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn interleaver_roundtrip() {
        let block: Vec<u8> = (0..256u32).map(|i| (i % 2) as u8 ^ ((i / 3) % 2) as u8).collect();
        // encode-side write then decode-side read must be identity
        let mut txb = [0u8; 256];
        for j in 0..4 {
            for i in 0..64 {
                txb[i * 4 + j] = block[j * 64 + permute(i)];
            }
        }
        let mut back = [0u8; 256];
        let mut k = 0;
        for j in 0..4 {
            for i in 0..64 {
                back[k] = txb[depermute(i) * 4 + j];
                k += 1;
            }
        }
        assert_eq!(&back[..], &block[..]);
    }

    #[test]
    fn uw_detector_normal_and_inverted() {
        for raw in [UW_RAIL1, UW_RAIL2] {
            let mut d = UwDetector::new(6);
            for i in 0..UW_LEN {
                let bit = ((raw >> (UW_LEN - 1 - i)) & 1) as u8;
                let hit = d.update(bit);
                assert_eq!(hit, i == UW_LEN - 1, "pattern {raw:#x} bit {i}");
            }
            assert!(!d.inverted);
            let mut d = UwDetector::new(6);
            for i in 0..UW_LEN {
                let bit = 1 - ((raw >> (UW_LEN - 1 - i)) & 1) as u8;
                let hit = d.update(bit);
                assert_eq!(hit, i == UW_LEN - 1);
            }
            assert!(d.inverted);
        }
    }
}
