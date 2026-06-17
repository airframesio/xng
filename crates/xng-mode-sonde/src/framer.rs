//! RS41 bit-stream framer: sync-word correlation + LSB-first byte packing.
//!
//! The demod ([`crate::demod`]) yields a continuous stream of hard NRZ bits.
//! On air the RS41 frame opens with the fixed 8-byte sync header
//! `10 B6 CA 11 22 96 12 F8` (whitened; it de-whitens to the `86 35 F4 40
//! 93 DF 1A 60` constant). Bytes are transmitted **LSB-first**.
//!
//! This framer slides a 64-bit correlator over the bit stream looking for
//! that sync pattern. GFSK has no inherent tone polarity, so it matches both
//! the sync pattern and its bit-wise inverse; on an inverted match every
//! subsequent recovered bit is flipped. Once the sync is found, the next
//! `CAPTURE_LEN` bytes (header included) are packed LSB-first and handed
//! back across however many `process` calls it takes to arrive.
//!
//! The recovered bytes are the **on-air whitened** frame; the caller
//! de-whitens + RS-corrects + parses via [`crate::decode_on_air`].

use crate::frame::STD_FRAME_LEN;

/// On-air whitened sync header (rs1729/RS rs41.txt).
pub const SYNC_BYTES: [u8; 8] = [0x10, 0xB6, 0xCA, 0x11, 0x22, 0x96, 0x12, 0xF8];

/// Bytes to capture per frame once the sync is located (header included).
///
/// We capture the standard 320-byte frame. All decoded sub-blocks (STATUS,
/// PTU, GPS-INFO, GPS-POS) live within the first 320 bytes; the extended
/// (aux-xdata) frame's trailing bytes are not parsed by the decode core, and
/// requiring the full extended length would stall on the far more common
/// standard frame (whose air-burst is only ~320 bytes long).
pub const CAPTURE_LEN: usize = STD_FRAME_LEN;

/// Allowed Hamming-distance slack on the 64-bit sync correlation.
const SYNC_TOL: u32 = 6;

/// Build the 64-bit sync pattern as transmitted (each byte LSB-first), packed
/// so the first transmitted bit is the most significant bit of the u64.
fn sync_u64() -> u64 {
    let mut acc = 0u64;
    for &byte in &SYNC_BYTES {
        for i in 0..8 {
            let bit = (byte >> i) & 1; // LSB first
            acc = (acc << 1) | (bit as u64);
        }
    }
    acc
}

/// An in-progress frame capture, started at a sync match.
struct Pending {
    inverted: bool,
    bits: Vec<u8>,
}

/// Streaming sync hunter + byte assembler.
pub struct Framer {
    /// 64-bit shift register of recent bits (LSB = most recent).
    window: u64,
    /// Filled-bit count, so we don't match on a half-full window and so we
    /// suppress immediate re-triggering right after a match.
    filled: usize,
    /// Sync pattern (first bit = MSB) and its polarity inverse.
    sync_mask: u64,
    sync_inv: u64,
    /// Captures awaiting enough trailing bits to complete a frame.
    pending: Vec<Pending>,
}

impl Framer {
    pub fn new() -> Self {
        let sync_mask = sync_u64();
        Framer {
            window: 0,
            filled: 0,
            sync_mask,
            sync_inv: !sync_mask,
            pending: Vec::new(),
        }
    }

    /// Feed demodulated bits; append each completed on-air whitened byte
    /// frame to `frames`.
    pub fn process(&mut self, bits: &[u8], frames: &mut Vec<Vec<u8>>) {
        for &bit in bits {
            let bit = bit & 1;

            // Extend any in-flight captures with this bit (polarity-adjusted),
            // completing those that have reached a full frame.
            let cap_bits = CAPTURE_LEN * 8;
            let mut i = 0;
            while i < self.pending.len() {
                let b = if self.pending[i].inverted { bit ^ 1 } else { bit };
                self.pending[i].bits.push(b);
                if self.pending[i].bits.len() >= cap_bits {
                    let done = self.pending.swap_remove(i);
                    frames.push(pack_lsb(&done.bits));
                } else {
                    i += 1;
                }
            }

            // Slide the sync correlator.
            self.window = (self.window << 1) | (bit as u64);
            if self.filled < 64 {
                self.filled += 1;
                continue;
            }

            let d_norm = (self.window ^ self.sync_mask).count_ones();
            let d_inv = (self.window ^ self.sync_inv).count_ones();
            let matched = if d_norm <= SYNC_TOL {
                Some(false)
            } else if d_inv <= SYNC_TOL {
                Some(true)
            } else {
                None
            };

            if let Some(inverted) = matched {
                // The 64-bit window holds the sync header bits. Seed a new
                // capture with the canonical sync bytes (header is fixed and
                // RS-protected); body bits stream in on subsequent iterations.
                let mut p = Pending {
                    inverted,
                    bits: Vec::with_capacity(CAPTURE_LEN * 8),
                };
                for &byte in &SYNC_BYTES {
                    for i in 0..8 {
                        p.bits.push((byte >> i) & 1);
                    }
                }
                self.pending.push(p);
                // Suppress overlapping re-triggers on the same header.
                self.filled = 0;
            }
        }
    }
}

/// Pack a bit slice (LSB-first per octet) into bytes.
fn pack_lsb(bits: &[u8]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | ((v & 1) << i)))
        .collect()
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_pattern_roundtrips_to_bytes() {
        // The packed sync pattern, sliced back into LSB-first bytes, must be
        // the on-air SYNC_BYTES.
        let mut bits = Vec::new();
        for &byte in &SYNC_BYTES {
            for i in 0..8 {
                bits.push((byte >> i) & 1);
            }
        }
        assert_eq!(pack_lsb(&bits), SYNC_BYTES);
    }
}
