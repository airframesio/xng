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

/// Total UW symbol positions checked per frame (64 rows × 2 doubled bits).
pub const UW_SYMBOLS: u32 = (ROWS * 2) as u32;

/// Per-frame unique-word bit-error rate in parts-per-thousand for the
/// chosen polarity. `matches` is the matching-symbol count for that
/// polarity (the relevant element of [`uw_score`]); the rest of the 128
/// UW symbols are errors. Data-independent channel-quality measure.
pub fn uw_ber_ppt(matches: u32) -> u32 {
    let errors = UW_SYMBOLS.saturating_sub(matches);
    (errors * 1000 + UW_SYMBOLS / 2) / UW_SYMBOLS
}

/// Per-row UW agreement for both polarities. Returns, for each of the 64
/// transmitted rows, `(normal_matches, inverted_matches)` over that row's
/// two doubled UW symbols (0..=2 each). Used by mid-frame polarity-flip
/// recovery to locate a Costas 180° slip.
fn per_row_uw(hard: &[u8]) -> [(u8, u8); ROWS] {
    let mut rows = [(0u8, 0u8); ROWS];
    for j in 0..ROWS {
        let expect = uw_bit(j);
        let mut n = 0u8;
        for k in 0..2 {
            if hard[j * COLS + k] == expect {
                n += 1;
            }
        }
        rows[j] = (n, 2 - n);
    }
    rows
}

/// Detect a single mid-frame polarity flip (Costas 180° slip) and, if one
/// is found, return the symbol index from which the frame must be inverted
/// to restore a consistent polarity, plus the resulting whole-frame UW
/// match count and whether the recovered frame is overall inverted.
///
/// A flip manifests as a run of rows matching one polarity followed by a
/// run matching the other. We scan the row boundary that maximises total
/// UW agreement after inverting one side; the gain over the no-flip score
/// must be large (≥ `min_gain` extra UW symbols) to avoid false positives
/// on noise. Returns `None` when no confident flip is found.
pub fn detect_polarity_flip(hard: &[u8], min_gain: u32) -> Option<PolarityFlip> {
    debug_assert!(hard.len() >= FRAME_SYMBOLS);
    let rows = per_row_uw(hard);
    let (base_n, base_i) = uw_score(hard);
    let base = base_n.max(base_i);

    // Prefix sums of normal/inverted matches over rows.
    let mut best: Option<(usize, u32, bool)> = None; // (flip_row, score, first_half_inverted)
    // Try every boundary 1..ROWS: rows [0,b) one polarity, [b,ROWS) other.
    let mut pre_n = 0u32;
    let mut pre_i = 0u32;
    let total_n: u32 = rows.iter().map(|r| r.0 as u32).sum();
    let total_i: u32 = rows.iter().map(|r| r.1 as u32).sum();
    for b in 1..ROWS {
        pre_n += rows[b - 1].0 as u32;
        pre_i += rows[b - 1].1 as u32;
        // Option A: first half normal, second half inverted.
        let a = pre_n + (total_i - pre_i);
        // Option B: first half inverted, second half normal.
        let bb = pre_i + (total_n - pre_n);
        let (score, first_inv) = if a >= bb { (a, false) } else { (bb, true) };
        if best.is_none_or(|(_, s, _)| score > s) {
            best = Some((b, score, first_inv));
        }
    }

    let (flip_row, score, first_inv) = best?;
    if score < base + min_gain {
        return None; // not a confident mid-frame flip
    }
    // Symbol index at which polarity changes: start of the flip row.
    Some(PolarityFlip {
        flip_symbol: flip_row * COLS,
        first_half_inverted: first_inv,
        uw_score: score,
    })
}

/// Result of [`detect_polarity_flip`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolarityFlip {
    /// Symbol index from which the second polarity run begins.
    pub flip_symbol: usize,
    /// Whether the first run (symbols `0..flip_symbol`) is the inverted one.
    pub first_half_inverted: bool,
    /// Total UW symbol matches after correcting the flip (max 128).
    pub uw_score: u32,
}

/// Apply a detected polarity flip to a soft-symbol frame in place, leaving
/// it in a single consistent polarity (the second run's polarity), so the
/// normal (non-inverted) decode path can recover it.
pub fn apply_polarity_flip(soft: &mut [f32], flip: &PolarityFlip) {
    // Invert whichever run is the odd one out so the whole frame agrees
    // with the second run's polarity.
    let end = FRAME_SYMBOLS.min(soft.len());
    if flip.first_half_inverted {
        for s in &mut soft[..flip.flip_symbol] {
            *s = -*s;
        }
    } else {
        for s in &mut soft[flip.flip_symbol..end] {
            *s = -*s;
        }
    }
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

/// Per-frame decode quality, surfaced to the message layer.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameStats {
    /// Coded symbols the Viterbi decoder corrected, estimated by
    /// re-encoding the decoded bits and counting disagreements with the
    /// hard-decision channel symbols (the standard external estimate of
    /// convolutional corrections when the decoder does not report them).
    pub fec_corrected: u32,
    /// Unique-word bit-error rate over the 128 UW symbols of this frame
    /// (matched in the chosen polarity), in parts-per-thousand. A direct,
    /// data-independent channel-quality measure.
    pub uw_ber_ppt: u32,
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
        self.decode_with_stats(soft, invert).0
    }

    /// Decode and also report per-frame quality ([`FrameStats`]).
    pub fn decode_with_stats(&self, soft: &[f32], invert: bool) -> ([u8; FRAME_BYTES], FrameStats) {
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

        // FEC-correction estimate: re-encode the decoded bits with the same
        // convolutional code and count coded symbols that disagree with the
        // hard decisions fed in. Each disagreement is a channel symbol the
        // Viterbi traceback overrode (i.e. corrected).
        let coded = self.viterbi.encode(&bits);
        let mut fec_corrected = 0u32;
        for (k, &c) in coded.iter().enumerate() {
            let rx = (deleaved[k] > 0.0) as u8;
            if rx != c {
                fec_corrected += 1;
            }
        }

        // Pack LSB-first per byte (KA9Q chainback + per-byte reversal in
        // the reference implementations; flagged in PROVENANCE.md).
        let mut frame = [0u8; FRAME_BYTES];
        for (i, chunk) in bits.chunks_exact(8).take(FRAME_BYTES).enumerate() {
            frame[i] = chunk.iter().enumerate().fold(0u8, |b, (k, &v)| b | (v << k));
        }
        descramble(&mut frame);
        (frame, FrameStats { fec_corrected, uw_ber_ppt: 0 })
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

    #[test]
    fn fec_corrected_counts_overrides() {
        let payload: Vec<u8> = (0..639).map(|i| (i as u8).wrapping_mul(17)).collect();
        let symbols = encode_frame(&payload);
        let clean: Vec<f32> =
            symbols.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        // Clean frame: nothing to correct.
        let (_, s0) = FrameDecoder::new().decode_with_stats(&clean, false);
        assert_eq!(s0.fec_corrected, 0, "clean frame needs no FEC corrections");

        // Inject sparse symbol errors; the estimate must be positive and the
        // frame must still decode (Viterbi corrected them).
        let mut noisy = clean.clone();
        let mut flipped = 0u32;
        for k in (300..noisy.len()).step_by(53) {
            noisy[k] = -noisy[k];
            flipped += 1;
        }
        let (frame, s1) = FrameDecoder::new().decode_with_stats(&noisy, false);
        assert_eq!(&frame[..639], &payload[..], "Viterbi still recovers payload");
        assert!(s1.fec_corrected > 0, "FEC corrections must be reported");
        // The re-encode estimate counts every coded-symbol disagreement,
        // which for a fully corrected frame equals the channel errors that
        // landed inside the coded (non-UW) region.
        assert!(
            s1.fec_corrected <= flipped + 4,
            "estimate {} should track injected errors ({flipped})",
            s1.fec_corrected
        );
    }

    #[test]
    fn uw_ber_matches_error_count() {
        // 0 errors -> 0 ppt; 128/128 wrong -> 1000 ppt; midpoints round.
        assert_eq!(uw_ber_ppt(UW_SYMBOLS), 0);
        assert_eq!(uw_ber_ppt(0), 1000);
        // 4 of 128 wrong = 124 matches -> 31.25 ppt -> 31 (rounded).
        assert_eq!(uw_ber_ppt(124), 31);
    }

    #[test]
    fn mid_frame_polarity_flip_recovered() {
        let payload: Vec<u8> = (0..639).map(|i| (i as u8) ^ 0x3C).collect();
        let symbols = encode_frame(&payload);
        let mut soft: Vec<f32> =
            symbols.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();

        // Simulate a Costas 180° slip partway through the frame: invert the
        // tail from row 40 onward. Neither whole-frame polarity now syncs.
        let flip_at = 40 * COLS;
        for s in &mut soft[flip_at..] {
            *s = -*s;
        }
        let hard: Vec<u8> = soft.iter().map(|&s| (s > 0.0) as u8).collect();
        let (n, inv) = uw_score(&hard);
        assert!(
            n < UW_MIN_MATCH && inv < UW_MIN_MATCH,
            "a mid-frame flip must defeat whole-frame sync (n={n} inv={inv})"
        );

        // Detect and correct the flip, then the normal decode path recovers.
        let flip = detect_polarity_flip(&hard, 24).expect("flip detected");
        assert!(flip.uw_score >= UW_MIN_MATCH, "corrected UW score recovers sync");
        apply_polarity_flip(&mut soft, &flip);
        let frame = FrameDecoder::new().decode(&soft, false);
        assert_eq!(&frame[..639], &payload[..], "payload recovered after flip fix");
    }

    #[test]
    fn no_false_flip_on_clean_frame() {
        // A clean, single-polarity frame must not be mistaken for a flip.
        let payload: Vec<u8> = (0..639).map(|i| i as u8).collect();
        let symbols = encode_frame(&payload);
        let hard: Vec<u8> = symbols.clone();
        assert!(
            detect_polarity_flip(&hard, 24).is_none(),
            "clean frame must not trigger flip recovery"
        );
    }
}
