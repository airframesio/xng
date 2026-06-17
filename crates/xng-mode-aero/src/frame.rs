//! Aero P-channel frame layer (ported from JAERO `aerol.cpp`):
//! UW + 16-bit header + interleaved/convolutionally-coded payload →
//! descrambled Signal Unit bytes.

use xng_dsp::scramble::Lfsr15;
use xng_dsp::viterbi::Viterbi;

/// 32-bit unique word, transmitted MSB-first.
pub const UW: u32 = 0xE15A_E893;
pub const HEADER_BITS: usize = 16;
pub const CODED_BITS: usize = 1152;

/// Parsed 16-bit P-channel frame header (AERO-4).
///
/// Oracle: JAERO `aerol.cpp` `AeroL::Decode` reads the 16 bits that follow
/// the unique word, MSB-first, into `frameinfo` and splits it into four
/// 4-bit nibbles:
///
/// ```text
/// formatid       = (frameinfo >> 12) & 0x000F;   // bits 15..12
/// supfrmaker     = (frameinfo >>  8) & 0x000F;   // bits 11..8  (superframe marker)
/// framecounter1  = (frameinfo >>  4) & 0x000F;   // bits  7..4
/// framecounter2  = (frameinfo >>  0) & 0x000F;   // bits  3..0
/// ```
///
/// The format id selects the frame's content/format, the superframe marker
/// labels a frame's position in the superframe, and the two frame counters
/// track frame sequencing — the fields a superframe-lock / AFC-DCD state
/// machine consumes. (The state machine itself is a documented follow-up;
/// here we parse and expose the header so a consumer can implement it.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Frame format id (JAERO `formatid`, bits 15..12).
    pub format_id: u8,
    /// Superframe marker (JAERO `supfrmaker`, bits 11..8).
    pub superframe: u8,
    /// First frame counter nibble (JAERO `framecounter1`, bits 7..4).
    pub frame_counter1: u8,
    /// Second frame counter nibble (JAERO `framecounter2`, bits 3..0).
    pub frame_counter2: u8,
}

impl FrameHeader {
    /// Split a 16-bit header word (already assembled MSB-first) into its
    /// four JAERO nibbles. Exactly mirrors JAERO's `frameinfo` shifts.
    pub fn from_u16(frameinfo: u16) -> Self {
        Self {
            format_id: ((frameinfo >> 12) & 0x0F) as u8,
            superframe: ((frameinfo >> 8) & 0x0F) as u8,
            frame_counter1: ((frameinfo >> 4) & 0x0F) as u8,
            frame_counter2: (frameinfo & 0x0F) as u8,
        }
    }

    /// Assemble the 16-bit header word from its nibbles (transmit side /
    /// round-trip testing).
    pub fn to_u16(self) -> u16 {
        ((self.format_id as u16 & 0x0F) << 12)
            | ((self.superframe as u16 & 0x0F) << 8)
            | ((self.frame_counter1 as u16 & 0x0F) << 4)
            | (self.frame_counter2 as u16 & 0x0F)
    }

    /// Parse the 16 header bits collected after the UW. Each element is a
    /// soft value; the sign is the demodulated bit (>= 0 ⇒ 1). Bits arrive
    /// MSB-first (the order JAERO shifts them into `frameinfo`).
    pub fn from_soft_bits(bits: &[f32]) -> Self {
        debug_assert_eq!(bits.len(), HEADER_BITS);
        let mut frameinfo: u16 = 0;
        for &b in bits.iter().take(HEADER_BITS) {
            frameinfo = (frameinfo << 1) | u16::from(b >= 0.0);
        }
        Self::from_u16(frameinfo)
    }

    /// Surface the parsed header fields as a JSON object for the message
    /// `details` channel.
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "format_id": self.format_id,
            "superframe": self.superframe,
            "frame_counter1": self.frame_counter1,
            "frame_counter2": self.frame_counter2,
        })
    }
}
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
    match rate_bps {
        10500 => 78,
        r if r >= 1200 => 9,
        _ => 6,
    }
}

/// Coded bits per frame for a rate (600/1200: 1152; 10.5k: 4992).
pub fn coded_bits_for(rate_bps: u32) -> usize {
    if rate_bps == 10500 {
        64 * 78
    } else {
        CODED_BITS
    }
}

/// SU payload bytes per frame.
pub fn frame_bytes_for(rate_bps: u32) -> usize {
    coded_bits_for(rate_bps) / 2 / 8
}

impl FrameDecoder {
    pub fn new(rate_bps: u32) -> Self {
        Self {
            rate_bps,
            // The Aero code transmits the 0o133 output first in each coded
            // pair (libcorrect polynomial order 109/79 in JAERO; confirmed
            // against the off-air 600 bps recording, where this order
            // decodes with zero Viterbi residual and all SU CRCs pass).
            viterbi: Viterbi::new(7, 0o133, 0o171),
            tail: vec![0.0; OVERLAP],
        }
    }

    /// Decode one frame's coded soft bits (after UW + header) into
    /// descrambled SU bytes.
    pub fn decode(&mut self, coded_soft: &[f32]) -> Vec<u8> {
        debug_assert_eq!(coded_soft.len(), coded_bits_for(self.rate_bps));
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
        let n_decoded = coded_bits_for(self.rate_bps) / 2;
        let mut bits: Vec<u8> = decoded[skip..skip + n_decoded].to_vec();

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
        Self { rate_bps, viterbi: Viterbi::new(7, 0o133, 0o171), state_bits: vec![0; 6] }
    }

    /// Encode one frame of SU bytes (72 at 600/1200, 312 at 10.5k) into
    /// the frame bit stream (UW + header + interleaved coded bits).
    /// At 10.5k the UW/dummy sections are handled by the OQPSK modulator;
    /// this emits the low-rate layout (32-bit UW + 16-bit header).
    pub fn encode(&mut self, su_bytes: &[u8], frame_counter: u8) -> Vec<u8> {
        debug_assert_eq!(su_bytes.len(), frame_bytes_for(self.rate_bps));
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
        debug_assert_eq!(coded.len(), coded_bits_for(self.rate_bps));

        let cols = cols_for(self.rate_bps);
        let block = 64 * cols;
        let mut out = Vec::with_capacity(FRAME_BITS);
        for i in (0..32).rev() {
            out.push(((UW >> i) & 1) as u8);
        }
        // Header: format id 1, superframe 0, both frame counters = the
        // running frame counter. Assembled via `FrameHeader` so the wire
        // word and the decoder's parse share one definition (AERO-4).
        let header = FrameHeader {
            format_id: 1,
            superframe: 0,
            frame_counter1: frame_counter & 0x0F,
            frame_counter2: frame_counter & 0x0F,
        }
        .to_u16();
        for i in (0..16).rev() {
            out.push(((header >> i) & 1) as u8);
        }
        for chunk in coded.chunks_exact(block) {
            interleave_block(chunk, cols, &mut out);
        }
        debug_assert_eq!(out.len(), 48 + coded_bits_for(self.rate_bps));
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

    /// AERO-4: the 16-bit P-channel frame header splits into JAERO's four
    /// nibbles. Oracle = JAERO `aerol.cpp` `AeroL::Decode`:
    /// formatid=(frameinfo>>12)&0xF, supfrmaker=(frameinfo>>8)&0xF,
    /// framecounter1=(frameinfo>>4)&0xF, framecounter2=frameinfo&0xF.
    #[test]
    fn frame_header_splits_jaero_nibbles() {
        // frameinfo = 0x1234 → formatid 1, superframe 2, fc1 3, fc2 4.
        let h = FrameHeader::from_u16(0x1234);
        assert_eq!(h.format_id, 0x1);
        assert_eq!(h.superframe, 0x2);
        assert_eq!(h.frame_counter1, 0x3);
        assert_eq!(h.frame_counter2, 0x4);
        // Round-trips back to the same 16-bit word.
        assert_eq!(h.to_u16(), 0x1234);

        // All-ones nibbles (each field is a full 4 bits).
        let h = FrameHeader::from_u16(0xFFFF);
        assert_eq!(
            (h.format_id, h.superframe, h.frame_counter1, h.frame_counter2),
            (0xF, 0xF, 0xF, 0xF)
        );

        // Parse from MSB-first soft bits, exactly as collected after the UW.
        // frameinfo 0xABCD = 1010 1011 1100 1101.
        let word = 0xABCDu16;
        let bits: Vec<f32> =
            (0..16).rev().map(|i| if (word >> i) & 1 == 1 { 0.9 } else { -0.9 }).collect();
        let h = FrameHeader::from_soft_bits(&bits);
        assert_eq!(h.format_id, 0xA);
        assert_eq!(h.superframe, 0xB);
        assert_eq!(h.frame_counter1, 0xC);
        assert_eq!(h.frame_counter2, 0xD);
        assert_eq!(h.to_json()["format_id"], 0xA);
        assert_eq!(h.to_json()["superframe"], 0xB);
        assert_eq!(h.to_json()["frame_counter1"], 0xC);
        assert_eq!(h.to_json()["frame_counter2"], 0xD);
    }

    /// AERO-4: the header the encoder writes is recovered by parsing the
    /// 16 header bits the framer collects after the UW. The encoder writes
    /// format id 1, superframe 0, both frame counters = the running counter.
    #[test]
    fn frame_header_roundtrips_through_encoder() {
        let mut enc = FrameEncoder::new(600);
        for fc in [0u8, 3, 9, 15] {
            let bytes = vec![0u8; frame_bytes_for(600)];
            let frame = enc.encode(&bytes, fc);
            // Bits 32..48 are the 16 header bits (after the 32-bit UW).
            let header_soft: Vec<f32> =
                frame[32..48].iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
            let h = FrameHeader::from_soft_bits(&header_soft);
            assert_eq!(h.format_id, 1, "fc={fc}");
            assert_eq!(h.superframe, 0, "fc={fc}");
            assert_eq!(h.frame_counter1, fc & 0x0F, "fc={fc}");
            assert_eq!(h.frame_counter2, fc & 0x0F, "fc={fc}");
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
