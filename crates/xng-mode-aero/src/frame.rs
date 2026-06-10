//! Aero P-channel frame layer (ported from JAERO `aerol.cpp`):
//! UW + 16-bit header + interleaved/convolutionally-coded payload →
//! descrambled Signal Unit bytes.

use xng_dsp::scramble::Lfsr15;
use xng_dsp::viterbi::Viterbi;

/// 32-bit unique word, transmitted MSB-first.
pub const UW: u32 = 0xE15A_E893;
pub const HEADER_BITS: usize = 16;
pub const CODED_BITS: usize = 1152;
pub const FRAME_BITS: usize = 32 + HEADER_BITS + CODED_BITS; // 1200
/// Decoded bits per frame (rate 1/2).
pub const DECODED_BITS: usize = CODED_BITS / 2; // 576 = 72 bytes = 6 SUs
/// Coded-bit overlap carried between frames for Viterbi continuity.
const OVERLAP: usize = 62;

pub struct FrameDecoder {
    rate_bps: u32,
    viterbi: Viterbi,
    /// Last OVERLAP deinterleaved soft bits of the previous frame.
    tail: Vec<f32>,
}

/// Deinterleave one 64×cols block: output index (j*64 + i) reads input
/// at ((27*i) % 64) * cols + j.
fn deinterleave_block(soft: &[f32], cols: usize, out: &mut Vec<f32>) {
    debug_assert_eq!(soft.len(), 64 * cols);
    for j in 0..cols {
        for i in 0..64 {
            out.push(soft[((27 * i) % 64) * cols + j]);
        }
    }
}

/// Interleave (transmit side).
fn interleave_block(bits: &[u8], cols: usize, out: &mut Vec<u8>) {
    debug_assert_eq!(bits.len(), 64 * cols);
    let mut block = vec![0u8; 64 * cols];
    for (k, &b) in bits.iter().enumerate() {
        let j = k / 64;
        let i = k % 64;
        block[((27 * i) % 64) * cols + j] = b;
    }
    out.extend_from_slice(&block);
}

pub(crate) fn cols_for(rate_bps: u32) -> usize {
    if rate_bps >= 1200 {
        9
    } else {
        6
    }
}

impl FrameDecoder {
    pub fn new(rate_bps: u32) -> Self {
        Self { rate_bps, viterbi: Viterbi::k7(), tail: vec![0.0; OVERLAP] }
    }

    /// Decode one frame's 1152 coded soft bits (after UW + header) into
    /// 72 descrambled SU bytes.
    pub fn decode(&mut self, coded_soft: &[f32]) -> Vec<u8> {
        debug_assert_eq!(coded_soft.len(), CODED_BITS);
        let cols = cols_for(self.rate_bps);
        let block = 64 * cols;

        let mut deleaved = Vec::with_capacity(CODED_BITS);
        for chunk in coded_soft.chunks_exact(block) {
            deinterleave_block(chunk, cols, &mut deleaved);
        }

        // Viterbi with overlap carry: prepend the previous frame's tail,
        // decode, drop the tail's decoded bits.
        let mut input = self.tail.clone();
        input.extend_from_slice(&deleaved);
        self.tail.copy_from_slice(&deleaved[deleaved.len() - OVERLAP..]);
        let decoded = self.viterbi.decode(&input);
        let skip = OVERLAP / 2;
        let mut bits: Vec<u8> = decoded[skip..skip + DECODED_BITS].to_vec();

        // Descramble (LFSR reset per frame) and pack LSB-first.
        Lfsr15::new().apply(&mut bits);
        bits.chunks_exact(8)
            .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << i)))
            .collect()
    }
}

/// Transmit side, mirroring `FrameDecoder` (loopback/testing).
pub struct FrameEncoder {
    rate_bps: u32,
    viterbi: Viterbi,
    /// Convolutional encoder state carried across frames (as the last
    /// K-1 data bits).
    state_bits: Vec<u8>,
}

impl FrameEncoder {
    pub fn new(rate_bps: u32) -> Self {
        Self { rate_bps, viterbi: Viterbi::k7(), state_bits: vec![0; 6] }
    }

    /// Encode 72 SU bytes into a full 1200-bit frame (UW + header +
    /// interleaved coded bits), hard bits.
    pub fn encode(&mut self, su_bytes: &[u8], frame_counter: u8) -> Vec<u8> {
        debug_assert_eq!(su_bytes.len(), DECODED_BITS / 8);
        // Bytes → bits LSB-first, scramble (reset per frame).
        let mut bits: Vec<u8> =
            su_bytes.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)).collect();
        Lfsr15::new().apply(&mut bits);

        // Convolutional encode with state continuity: prepend the carried
        // state bits, encode, drop their coded output.
        let mut input = self.state_bits.clone();
        input.extend_from_slice(&bits);
        self.state_bits = bits[bits.len() - 6..].to_vec();
        let coded_all = self.viterbi.encode(&input);
        let coded = &coded_all[6 * 2..];
        debug_assert_eq!(coded.len(), CODED_BITS);

        let cols = cols_for(self.rate_bps);
        let block = 64 * cols;
        let mut out = Vec::with_capacity(FRAME_BITS);
        for i in (0..32).rev() {
            out.push(((UW >> i) & 1) as u8);
        }
        // Header: format id 1, superframe 0, two frame counters.
        let header: u16 = (1 << 12) | ((frame_counter as u16 & 0xF) << 4) | (frame_counter as u16 & 0xF);
        for i in (0..16).rev() {
            out.push(((header >> i) & 1) as u8);
        }
        for chunk in coded.chunks_exact(block) {
            interleave_block(chunk, cols, &mut out);
        }
        debug_assert_eq!(out.len(), FRAME_BITS);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_both_rates() {
        for rate in [600u32, 1200] {
            let mut enc = FrameEncoder::new(rate);
            let mut dec = FrameDecoder::new(rate);
            // Several consecutive frames to exercise the overlap carry.
            let mut payloads = Vec::new();
            for f in 0..4u8 {
                let bytes: Vec<u8> = (0..72).map(|i| (i as u8).wrapping_mul(7) ^ (f * 31)).collect();
                payloads.push(bytes);
            }
            for (f, bytes) in payloads.iter().enumerate() {
                let frame = enc.encode(bytes, f as u8);
                assert_eq!(frame.len(), FRAME_BITS);
                let soft: Vec<f32> = frame[48..]
                    .iter()
                    .map(|&b| if b == 1 { 1.0 } else { -1.0 })
                    .collect();
                let out = dec.decode(&soft);
                assert_eq!(&out, bytes, "rate={rate} frame={f}");
            }
        }
    }

    #[test]
    fn survives_sparse_coded_errors() {
        let mut enc = FrameEncoder::new(1200);
        let mut dec = FrameDecoder::new(1200);
        let bytes: Vec<u8> = (0..72).map(|i| i as u8 ^ 0x5A).collect();
        // Warm-up frame so overlap state is realistic.
        let _ = dec.decode(&vec![-1.0; CODED_BITS]);
        let frame = enc.encode(&bytes, 0);
        let mut soft: Vec<f32> =
            frame[48..].iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        for i in (5..soft.len()).step_by(40) {
            soft[i] = -soft[i]; // ~2.5% hard errors
        }
        // The first decode after a cold/garbage frame may straddle the
        // overlap; decode the same frame twice to settle the carry.
        let out = dec.decode(&soft);
        assert_eq!(&out, &bytes);
    }
}
