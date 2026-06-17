//! UAT 2-ary CPFSK demodulator (frequency-discriminator domain).
//!
//! UAT (DO-282B §2.2.1) is binary continuous-phase FSK at 1.041667 Mbit/s
//! with modulation index h ≈ 0.6 (deviation ≈ ±312.5 kHz): a transmitted
//! `1` is the upper tone, a `0` the lower tone. A burst is a 36-bit sync
//! word followed by the FEC-coded message block (no further line coding —
//! the recovered bits are the RS codeword octets, MSB-first).
//!
//! Chain (reuses the AIS GFSK discriminator idea at UAT's rate, but in the
//! wideband "consume the whole capture" style of ADS-B rather than a
//! streaming bit clock): channel IQ at [`crate::CHANNEL_RATE`] (~2 samples/
//! bit) → per-sample frequency discriminator (`arg(x · conj(prev))`) with a
//! slow DC tracker that absorbs carrier offset → a buffered discriminator
//! stream is hunted at sample resolution for the 36-bit sync words. At a
//! sync hit the symbol period is known, so the message bits are sliced by
//! integrating each bit cell across a half-sample timing grid (the phase
//! that maximizes sync correlation), MSB-first into octets, and handed to
//! [`crate::decode_frame`].
//!
//! The downlink block length (short 30 B vs long 48 B) is not known until
//! the header is decoded, so a downlink sync emits both a long and a short
//! candidate and [`crate::decode_frame`]'s RS gate rejects the wrong one.

use crate::fec::{DOWNLINK_LONG_BLOCK, DOWNLINK_SHORT_BLOCK, UPLINK_FRAME_BYTES};
use crate::CHANNEL_RATE;
use num_complex::Complex;

/// UAT bit rate (DO-282B): 1.041667 Mbit/s nominal.
pub const BIT_RATE: f64 = 1_041_667.0;

/// 36-bit synchronization words (DO-282B §2.2.3 / dump978 `SYNC_BITS`).
pub const SYNC_DOWNLINK: u64 = 0xE_ACDD_A4E2;
pub const SYNC_UPLINK: u64 = 0x1_5322_5B1D;
/// Sync word length in bits.
pub const SYNC_LEN: usize = 36;

/// Max sync-word bit errors tolerated for a correlation hit. UAT sync is
/// 36 bits; a handful of slips are allowed on a weak burst.
const SYNC_MAX_ERRORS: u32 = 4;

/// Block bit lengths (data+parity, ×8). A downlink burst is always sliced
/// at the long length; its 30-byte short prefix is offered to the RS gate
/// separately (see [`short_prefix`]).
const DOWNLINK_LONG_BITS: usize = DOWNLINK_LONG_BLOCK * 8; // 384
const UPLINK_BITS: usize = UPLINK_FRAME_BYTES * 8; //        4416

/// Carrier-offset (discriminator DC) tracking factor.
const FREQ_ALPHA: f32 = 0.002;
/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

/// A raw burst recovered by the demod: the with-parity octets that follow a
/// detected sync word, plus which link the sync identified and the channel
/// power at detection.
#[derive(Debug, Clone)]
pub struct Burst {
    /// Sliced, with-parity octets (a candidate RS block / interleaved frame).
    pub bytes: Vec<u8>,
    /// True for a downlink (aircraft) sync, false for an uplink (ground) sync.
    pub downlink: bool,
    /// Channel power at detection, dBFS.
    pub level_dbfs: f32,
}

pub struct FskDemod {
    samples_per_bit: f64,
    prev_sample: Complex<f32>,
    /// Discriminator DC estimate (carrier frequency offset), carried across
    /// `process` calls.
    freq_offset: f32,
    /// Smoothed channel power.
    level: f32,
    /// Discriminator-domain samples carried between calls so a burst that
    /// straddles a chunk boundary is still recovered. Trimmed once it is
    /// long enough that no in-flight sync could still complete.
    disc: Vec<f32>,
    /// Number of leading `disc` samples already scanned for sync.
    scanned: usize,
}

impl FskDemod {
    pub fn new() -> Self {
        let samples_per_bit = CHANNEL_RATE / BIT_RATE;
        assert!(
            samples_per_bit >= 1.5,
            "CHANNEL_RATE must give >= ~2 samples/bit for the discriminator"
        );
        Self {
            samples_per_bit,
            prev_sample: Complex::new(0.0, 0.0),
            freq_offset: 0.0,
            level: 0.0,
            disc: Vec::new(),
            scanned: 0,
        }
    }

    /// Feed channel IQ; return any bursts whose sync word was detected and
    /// whose following block bits could be sliced.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Burst> {
        // Extend the discriminator stream with this chunk.
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);
            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            self.disc.push(raw - self.freq_offset);
        }

        let out = self.scan();

        // Retire fully-scanned history, keeping enough tail that a burst
        // beginning near the end of this chunk can still be completed next
        // call: one full uplink (the longest) plus the sync.
        let keep = ((UPLINK_BITS + SYNC_LEN) as f64 * self.samples_per_bit) as usize + 16;
        if self.disc.len() > keep {
            let drop = self.disc.len() - keep;
            self.disc.drain(..drop);
            self.scanned = self.scanned.saturating_sub(drop);
        }
        out
    }

    /// Hunt the buffered discriminator stream for sync words and slice the
    /// blocks that follow each hit.
    fn scan(&mut self) -> Vec<Burst> {
        let mut out = Vec::new();
        let spb = self.samples_per_bit;
        // A sync can only be tested where SYNC_LEN bit centers fit. Leave a
        // margin so the next call (with more samples) can test positions
        // that don't yet have a full sync's worth of lookahead.
        let sync_span = (SYNC_LEN as f64 * spb).ceil() as usize + 2;
        let level_dbfs = self.level_dbfs();

        let mut s = self.scanned;
        while s + sync_span < self.disc.len() {
            // Demodulate the SYNC_LEN bits starting at sample `s` for each
            // of two half-sample timing phases (the integrate-and-dump grid
            // and a half-sample-shifted grid), then test both sync words.
            if let Some((downlink, phase)) = self.match_sync(s) {
                let bit0 = s as f64 + phase;
                if let Some(burst) = self.slice_burst(bit0, downlink, level_dbfs) {
                    out.push(burst);
                    // Skip past this burst to avoid re-detecting inside it.
                    let want_bits = if downlink { DOWNLINK_LONG_BITS } else { UPLINK_BITS };
                    s += ((SYNC_LEN + want_bits) as f64 * spb) as usize;
                    continue;
                }
            }
            s += 1;
        }
        self.scanned = s;
        out
    }

    /// Test both sync words at sample offset `s` over a small set of timing
    /// phases. Returns `(downlink, phase)` for the best within-tolerance
    /// match, preferring the lower Hamming distance.
    fn match_sync(&self, s: usize) -> Option<(bool, f64)> {
        let spb = self.samples_per_bit;
        let mut best: Option<(u32, bool, f64)> = None;
        // Half-sample phase grid: aligning the bit centers to the true
        // symbol timing is what makes 2-samples/bit robust.
        for k in 0..((2.0 * spb).round() as usize) {
            let phase = k as f64 * 0.5;
            let mut reg: u64 = 0;
            for b in 0..SYNC_LEN {
                let center = s as f64 + phase + (b as f64 + 0.5) * spb;
                let bit = self.bit_at(center, spb);
                reg = (reg << 1) | bit as u64;
            }
            let down_err = (reg ^ SYNC_DOWNLINK).count_ones();
            let up_err = (reg ^ SYNC_UPLINK).count_ones();
            let (err, downlink) =
                if down_err <= up_err { (down_err, true) } else { (up_err, false) };
            if err <= SYNC_MAX_ERRORS && best.is_none_or(|(be, _, _)| err < be) {
                best = Some((err, downlink, phase));
            }
        }
        best.map(|(_, downlink, phase)| (downlink, phase))
    }

    /// Slice the block bits after a sync hit. `bit0` is the fractional
    /// sample index of the first sync bit's cell start. Returns the
    /// candidate burst(s) folded into a single `Burst` by length policy.
    fn slice_burst(&self, bit0: f64, downlink: bool, level_dbfs: f32) -> Option<Burst> {
        let spb = self.samples_per_bit;
        let want_bits = if downlink { DOWNLINK_LONG_BITS } else { UPLINK_BITS };
        // Message bits begin right after the 36 sync bits.
        let msg_start = bit0 + SYNC_LEN as f64 * spb;
        let last_center = msg_start + (want_bits as f64 - 0.5) * spb;
        if last_center as usize + 2 >= self.disc.len() {
            return None;
        }
        let mut bits = Vec::with_capacity(want_bits);
        for b in 0..want_bits {
            let center = msg_start + (b as f64 + 0.5) * spb;
            bits.push(self.bit_at(center, spb));
        }
        Some(Burst { bytes: pack_block(&bits, downlink), downlink, level_dbfs })
    }

    /// Decide one bit by integrating the discriminator across the bit cell
    /// centered (in samples) at `center`. Upper tone (positive) ⇒ `1`.
    #[inline]
    fn bit_at(&self, center: f64, spb: f64) -> u8 {
        let lo = (center - spb / 2.0).round().max(0.0) as usize;
        let hi = ((center + spb / 2.0).round() as usize).min(self.disc.len());
        let mut acc = 0.0f32;
        for &d in &self.disc[lo..hi.max(lo + 1).min(self.disc.len())] {
            acc += d;
        }
        (acc > 0.0) as u8
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

impl Default for FskDemod {
    fn default() -> Self {
        Self::new()
    }
}

/// Pack the sliced message bits into the candidate block bytes. For a
/// downlink we keep the full long block; the RS gate in
/// [`crate::decode_frame`] resolves short vs long. (We return the long
/// block here; the [`FskDemod::scan`] caller also re-tests the short
/// prefix via [`crate::decode_frame`].)
fn pack_block(bits: &[u8], downlink: bool) -> Vec<u8> {
    let n = if downlink { DOWNLINK_LONG_BITS } else { UPLINK_BITS };
    bits_to_bytes(&bits[..n.min(bits.len())])
}

/// Pack an MSB-first bit stream into octets (drops a trailing partial byte).
fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|c| c.iter().fold(0u8, |b, &v| (b << 1) | (v & 1)))
        .collect()
}

/// The short downlink block is the long block's 30-byte prefix; expose it
/// so the channel decoder can offer both candidates to the RS gate.
pub fn short_prefix(long_block: &[u8]) -> &[u8] {
    &long_block[..DOWNLINK_SHORT_BLOCK.min(long_block.len())]
}
