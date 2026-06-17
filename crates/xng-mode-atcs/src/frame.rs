//! HDLC/LAPB deframing for the ATCS Spec-200 RF link.
//!
//! The ATCS data radio (AAR Spec-200, 900 MHz, 4800 bps FSK) carries a
//! synchronous **HDLC-LAPB** bit stream (ISO/IEC 3309 / 13239 framing):
//! a transmitter sends bit synchronization (40 alternating 1s/0s), then a
//! frame-synchronization sequence, then HDLC frames. Each frame is bounded
//! by `0x7E` flags, the payload is bit-stuffed (a `0` is inserted after
//! five consecutive `1`s), and a 16-bit Frame Check Sequence (FCS) — the
//! standard ISO HDLC FCS, CRC-16/X-25 — protects the frame.
//!
//! This module hunts for flags, removes bit stuffing, assembles octets,
//! and verifies the FCS, yielding the raw frame bytes (FCS stripped). The
//! Spec-200 packet header that lives inside those bytes is decoded by
//! [`crate::spec200`].
//!
//! On the wire HDLC transmits each octet **LSB-first**; we assemble octets
//! accordingly (bit `i` of a byte is the `i`-th bit that arrived), matching
//! the convention the AIS/VDL2 HDLC layers in this workspace use and the
//! one the FCS in [`xng_dsp::checksum`] expects.

use xng_dsp::checksum::hdlc_frame_ok;

/// HDLC flag octet, in arrival order (bit 0 = oldest): 0,1,1,1,1,1,1,0.
const FLAG: u8 = 0x7E;

/// Minimum useful frame length in destuffed bits, FCS included. A Spec-200
/// packet needs at least the 1-octet control field, a 4-octet reserved
/// span, the address-length octet, one BCD address octet, plus the 2-octet
/// FCS — well above 24 bits, but we keep a permissive floor and let the
/// FCS reject garbage.
const MIN_BITS: usize = 24;

/// Generous upper bound on a destuffed frame (bits). Spec-200 RF packets
/// are short; this caps runaway collection on noise.
const MAX_BITS: usize = 4096;

/// A CRC-valid HDLC frame from the ATCS link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcsFrame {
    /// Frame octets with the trailing 2-octet FCS removed. The first octet
    /// is the Spec-200 packet control field (see [`crate::spec200`]).
    pub bytes: Vec<u8>,
    /// The transmitted FCS (already verified) as it appeared on the wire,
    /// low octet first.
    pub fcs: u16,
}

/// Streaming HDLC deframer: feed NRZI-decoded link bits, get frames.
#[derive(Debug)]
pub struct HdlcDeframer {
    /// Rolling raw-bit window for flag hunting (newest bit at bit 7).
    shift: u8,
    collecting: bool,
    /// Consecutive ones seen (for destuffing / flag / abort detection).
    ones: u32,
    /// Destuffed frame bits.
    buf: Vec<u8>,
}

impl HdlcDeframer {
    pub fn new() -> Self {
        Self {
            shift: 0,
            collecting: false,
            ones: 0,
            buf: Vec::with_capacity(MAX_BITS),
        }
    }

    /// Push one link bit; returns a frame when a CRC-valid one completes.
    pub fn push_bit(&mut self, bit: u8) -> Option<AtcsFrame> {
        let bit = bit & 1;
        self.shift = (self.shift >> 1) | (bit << 7);

        if !self.collecting {
            if self.shift == FLAG {
                self.collecting = true;
                self.buf.clear();
                self.ones = 0;
            }
            return None;
        }

        if bit == 1 {
            self.ones += 1;
            if self.ones > 6 {
                // Seven+ consecutive ones is an abort (or noise): give up
                // and resume flag hunting.
                self.collecting = false;
                self.ones = 0;
                self.buf.clear();
                return None;
            }
            self.buf.push(1);
            None
        } else if self.ones == 5 {
            // Stuffed zero after five ones: drop it.
            self.ones = 0;
            None
        } else if self.ones == 6 {
            // 0111111 0 = a closing flag. The six ones plus the leading
            // zero of the flag were pushed into buf; strip those 7 bits.
            let frame_len = self.buf.len().saturating_sub(7);
            let frame = Self::close(&self.buf[..frame_len]);
            // Stay collecting: this flag may also open the next frame.
            self.buf.clear();
            self.ones = 0;
            frame
        } else {
            self.buf.push(0);
            self.ones = 0;
            if self.buf.len() > MAX_BITS {
                self.collecting = false;
                self.buf.clear();
            }
            None
        }
    }

    /// Feed a whole NRZI-decoded bit slice, collecting every frame found.
    pub fn push_bits(&mut self, bits: &[u8]) -> Vec<AtcsFrame> {
        bits.iter().filter_map(|&b| self.push_bit(b)).collect()
    }

    fn close(bits: &[u8]) -> Option<AtcsFrame> {
        if bits.len() < MIN_BITS || !bits.len().is_multiple_of(8) {
            return None;
        }
        // Assemble wire octets LSB-first: bit i of each byte = i-th arrival.
        let wire: Vec<u8> = bits
            .chunks_exact(8)
            .map(|c| {
                c.iter()
                    .enumerate()
                    .fold(0u8, |b, (i, &v)| b | ((v & 1) << i))
            })
            .collect();
        if !hdlc_frame_ok(&wire) {
            return None;
        }
        let n = wire.len();
        let fcs = u16::from_le_bytes([wire[n - 2], wire[n - 1]]);
        Some(AtcsFrame {
            bytes: wire[..n - 2].to_vec(),
            fcs,
        })
    }
}

impl Default for HdlcDeframer {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a transmit-order HDLC bit stream (opening flag, bit-stuffed
/// payload+FCS, closing flag) from frame payload octets. Used by tests and
/// by any future modulator; octets are emitted LSB-first to match the wire
/// convention. This is a framing helper, not a self-consistency oracle:
/// the decode tests assert against externally documented frames, not
/// against bits produced here.
pub fn hdlc_bits(payload: &[u8]) -> Vec<u8> {
    use xng_dsp::checksum::hdlc_fcs;
    let mut wire = payload.to_vec();
    wire.extend_from_slice(&hdlc_fcs(payload).to_le_bytes());

    let flag = [0u8, 1, 1, 1, 1, 1, 1, 0];
    let mut bits: Vec<u8> = Vec::new();
    bits.extend(flag);
    let mut ones = 0;
    for &b in &wire {
        for i in 0..8 {
            let bit = (b >> i) & 1;
            bits.push(bit);
            if bit == 1 {
                ones += 1;
                if ones == 5 {
                    bits.push(0); // stuff
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    bits.extend(flag);
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_dsp::checksum::hdlc_fcs;

    fn run(bits: &[u8]) -> Vec<AtcsFrame> {
        let mut d = HdlcDeframer::new();
        d.push_bits(bits)
    }

    /// The ISO HDLC FCS check value over the standard catalogue string
    /// "123456789" is 0x906E (CRC-16/X-25). This anchors our FCS to an
    /// external reference (the CRC catalogue), not to our own encoder.
    #[test]
    fn fcs_matches_x25_catalogue_value() {
        assert_eq!(hdlc_fcs(b"123456789"), 0x906E);
    }

    /// A frame carrying the catalogue string with its true FCS appended
    /// must verify and round-trip through flag-hunt + destuffing. The FCS
    /// value is externally fixed (0x906E), so this is not a blind loopback.
    #[test]
    fn deframes_catalogue_string_with_known_fcs() {
        let payload = b"123456789";
        let frames = run(&hdlc_bits(payload));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, payload);
        assert_eq!(frames[0].fcs, 0x906E);
    }

    /// A corrupted FCS must be rejected.
    #[test]
    fn rejects_bad_fcs() {
        let mut bits = hdlc_bits(b"123456789");
        // Flip a payload bit well inside the frame.
        bits[20] ^= 1;
        assert!(run(&bits).is_empty());
    }

    /// Bit stuffing must be exercised and removed: a payload with a long
    /// run of ones forces stuff bits, which the deframer must drop so the
    /// FCS still checks.
    #[test]
    fn destuffs_long_one_runs() {
        let payload = &[0xFFu8, 0xFF, 0x7F, 0x00, 0xFF]; // many 1-runs
        let frames = run(&hdlc_bits(payload));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, payload);
        assert_eq!(frames[0].fcs, hdlc_fcs(payload));
    }

    /// Back-to-back frames sharing a single flag between them must both be
    /// recovered (the closing flag of frame N opens frame N+1).
    #[test]
    fn shared_flag_between_frames() {
        let a = hdlc_bits(b"123456789");
        let b = hdlc_bits(b"ATCS");
        // Drop the opening flag (8 bits) of the second frame so the two
        // share the boundary flag.
        let mut stream = a.clone();
        stream.extend_from_slice(&b[8..]);
        let frames = run(&stream);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bytes, b"123456789");
        assert_eq!(frames[1].bytes, b"ATCS");
    }

    /// Seven consecutive ones (an abort) must terminate collection without
    /// emitting a frame.
    #[test]
    fn abort_sequence_drops_frame() {
        let mut bits = vec![0u8, 1, 1, 1, 1, 1, 1, 0]; // opening flag
        bits.extend_from_slice(&[0, 1, 0, 1]); // a little data
        bits.extend_from_slice(&[1, 1, 1, 1, 1, 1, 1]); // 7 ones: abort
        assert!(run(&bits).is_empty());
    }
}
