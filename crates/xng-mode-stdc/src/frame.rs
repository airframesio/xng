//! STD-C frame layer: UW sync over the 10368-symbol frame, row
//! depermutation, 64×162 deinterleave, Viterbi, byte packing,
//! group descrambler. Constants per docs/notes/STDC.md.

use xng_dsp::viterbi::Viterbi;

pub const FRAME_SYMBOLS: usize = 10368; // 64 rows × 162 cols, 8.64 s
pub const ROWS: usize = 64;
pub const COLS: usize = 162;
pub const FRAME_BYTES: usize = 640;

/// 64-bit unique word; bit j is transmitted twice at the start of
/// transmitted row j.
pub const UW: [u8; 8] = [0x07, 0xEA, 0xCD, 0xDA, 0x4E, 0x2F, 0x28, 0xC2];
/// Minimum matching UW symbol pairs (of 128) to declare sync.
pub const UW_MIN_MATCH: u32 = 121;

#[inline]
fn uw_bit(j: usize) -> u8 {
    (UW[j / 8] >> (7 - j % 8)) & 1
}

/// Score a candidate frame alignment: count matching UW symbols (each UW
/// bit appears at row*162 and row*162+1). Returns (normal, inverted).
pub fn uw_score(hard: &[u8]) -> (u32, u32) {
    debug_assert!(hard.len() >= FRAME_SYMBOLS);
    let mut normal = 0u32;
    let mut inverted = 0u32;
    for j in 0..ROWS {
        let expect = uw_bit(j);
        for k in 0..2 {
            let got = hard[j * COLS + k];
            if got == expect {
                normal += 1;
            } else {
                inverted += 1;
            }
        }
    }
    (normal, inverted)
}

/// 7-bit descrambler LFSR (G = 1 + x^3 + x^4 + x^5 + x^7, init 0x80):
/// one output bit per 4-byte group; bit set → complement the group.
pub fn descramble(frame: &mut [u8; FRAME_BYTES]) {
    let mut reg: u8 = 0x80;
    for group in frame.chunks_exact_mut(4) {
        let out = reg & 1;
        let newbit = out ^ ((reg >> 2) & 1) ^ ((reg >> 3) & 1) ^ ((reg >> 4) & 1);
        reg = (reg >> 1) | (newbit << 7);
        if out == 1 {
            for b in group {
                *b ^= 0xFF;
            }
        }
    }
}

pub struct FrameDecoder {
    viterbi: Viterbi,
}

impl FrameDecoder {
    pub fn new() -> Self {
        // 133-output first in each coded pair — the same on-air order
        // off-air validation established for Aero and HFDL; confirmed
        // for STD-C against the sigidwiki EGC capture.
        Self { viterbi: Viterbi::new(7, 0o133, 0o171) }
    }

    /// Decode one aligned frame of 10368 soft symbols (+1.0 = bit 1).
    /// `invert` complements the symbols (UW matched inverted).
    pub fn decode(&self, soft: &[f32], invert: bool) -> [u8; FRAME_BYTES] {
        debug_assert_eq!(soft.len(), FRAME_SYMBOLS);
        let sign = if invert { -1.0 } else { 1.0 };

        // Depermute rows: original row i was transmitted as row (i*23)%64,
        // then strip the 2 UW columns and read column-wise.
        let mut deleaved = vec![0.0f32; ROWS * (COLS - 2)];
        for i in 0..ROWS {
            let src = ((i * 23) % ROWS) * COLS;
            for col in 0..COLS - 2 {
                deleaved[col * ROWS + i] = soft[src + col + 2] * sign;
            }
        }

        let bits = self.viterbi.decode(&deleaved);
        // Pack LSB-first per byte (KA9Q chainback + per-byte reversal in
        // the reference implementations; flagged in PROVENANCE.md).
        let mut frame = [0u8; FRAME_BYTES];
        for (i, chunk) in bits.chunks_exact(8).take(FRAME_BYTES).enumerate() {
            frame[i] = chunk.iter().enumerate().fold(0u8, |b, (k, &v)| b | (v << k));
        }
        descramble(&mut frame);
        frame
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Transmit side (loopback/testing): 639 payload bytes (+1 flush byte)
/// → scramble → encode → interleave → permute → UW columns.
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= FRAME_BYTES - 1);
    let mut bytes = [0u8; FRAME_BYTES];
    bytes[..payload.len()].copy_from_slice(payload);
    descramble(&mut bytes); // scrambling is its own inverse

    let bits: Vec<u8> =
        bytes.iter().flat_map(|&b| (0..8).map(move |k| (b >> k) & 1)).collect();
    let coded = Viterbi::new(7, 0o133, 0o171).encode(&bits);
    debug_assert_eq!(coded.len(), ROWS * (COLS - 2));

    // Interleave: coded stream was read column-wise on RX, so write
    // column-wise here; then permute rows for transmission and prepend
    // the doubled UW bits per transmitted row.
    let mut matrix = vec![0u8; ROWS * (COLS - 2)];
    for (k, &b) in coded.iter().enumerate() {
        let col = k / ROWS;
        let row = k % ROWS;
        matrix[row * (COLS - 2) + col] = b;
    }
    let mut out = vec![0u8; FRAME_SYMBOLS];
    for j in 0..ROWS {
        // Transmitted row j carries original row i = (j*39) % 64.
        let i = (j * 39) % ROWS;
        out[j * COLS] = uw_bit(j);
        out[j * COLS + 1] = uw_bit(j);
        out[j * COLS + 2..j * COLS + COLS]
            .copy_from_slice(&matrix[i * (COLS - 2)..(i + 1) * (COLS - 2)]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descrambler_table_prefix() {
        // First entries derived from the LFSR in docs/notes/STDC.md.
        let expected = [
            0u8, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1, 0, 1, 1, 1, 0, 0,
            0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 1, 0, 0, 1, 0, 0, 0, 0,
        ];
        let mut reg: u8 = 0x80;
        for (i, &e) in expected.iter().enumerate() {
            let out = reg & 1;
            let newbit = out ^ ((reg >> 2) & 1) ^ ((reg >> 3) & 1) ^ ((reg >> 4) & 1);
            reg = (reg >> 1) | (newbit << 7);
            assert_eq!(out, e, "table entry {i}");
        }
    }

    #[test]
    fn frame_roundtrip_bits() {
        let payload: Vec<u8> = (0..639).map(|i| (i as u8).wrapping_mul(31) ^ 0x5C).collect();
        let symbols = encode_frame(&payload);
        assert_eq!(symbols.len(), FRAME_SYMBOLS);
        let (n, i) = uw_score(&symbols);
        assert_eq!(n, 128, "clean frame must match UW fully (inv {i})");

        let soft: Vec<f32> = symbols.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        let frame = FrameDecoder::new().decode(&soft, false);
        assert_eq!(&frame[..639], &payload[..]);
    }

    #[test]
    fn inverted_frame_roundtrip() {
        let payload: Vec<u8> = (0..639).map(|i| i as u8 ^ 0xA7).collect();
        let symbols = encode_frame(&payload);
        let soft: Vec<f32> = symbols.iter().map(|&b| if b == 1 { -1.0 } else { 1.0 }).collect();
        let hard: Vec<u8> = soft.iter().map(|&s| (s > 0.0) as u8).collect();
        let (n, inv) = uw_score(&hard);
        assert!(inv >= UW_MIN_MATCH && n < 8);
        let frame = FrameDecoder::new().decode(&soft, true);
        assert_eq!(&frame[..639], &payload[..]);
    }

    #[test]
    fn survives_symbol_errors() {
        let payload: Vec<u8> = (0..639).map(|i| (i as u8).rotate_left(3)).collect();
        let symbols = encode_frame(&payload);
        let mut soft: Vec<f32> =
            symbols.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        for k in (200..soft.len()).step_by(97) {
            soft[k] = -soft[k]; // ~1% symbol errors
        }
        let frame = FrameDecoder::new().decode(&soft, false);
        assert_eq!(&frame[..639], &payload[..]);
    }
}
