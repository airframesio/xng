//! HDLC deframing for VDES ASM (ITU-R M.2092-1 Annex 1, "ASM" — the
//! link layer is HDLC per ISO/IEC 13239, the same profile AIS uses per
//! ITU-R M.1371): 0x7E flag hunt, bit destuffing (a 0 after five consecutive
//! 1s is removed), octet assembly, CRC-16/X-25 FCS check, and the per-octet
//! bit reversal that turns wire bytes (LSB-first transmission) into the
//! MSB-first message bit string that ASM fields use.
//!
//! The ASM payload carried inside the frame is the AIS binary-message
//! format (ITU-R M.2092-1 reuses the M.1371 Message 6 addressed-binary and
//! Message 8 broadcast-binary structures and the shared DAC/FID
//! application-identifier catalogue). The deframer surfaces the message
//! type (bits 0..6) and the source MMSI (bits 8..38); DAC/FID and the
//! application payload are decoded in [`crate::asm`].

use xng_dsp::checksum::hdlc_frame_ok;

/// Flag pattern in arrival order (bit 0 = oldest): 0,1,1,1,1,1,1,0 = 0x7E.
const FLAG: u8 = 0x7E;
/// Frame length bounds in destuffed bits, FCS included. The shortest ASM
/// (a Message-8 broadcast binary with an empty payload: 56 header bits) is
/// 56 + 16 FCS; the longest single-slot ASM payload is well under 1280.
const MIN_BITS: usize = 56;
const MAX_BITS: usize = 1280;

/// A CRC-valid VDES ASM transmission.
#[derive(Debug, Clone, PartialEq)]
pub struct VdesFrame {
    /// AIS-format message type (bits 0..6): 6 = addressed ASM, 8 = broadcast ASM.
    pub msg_type: u8,
    /// Source MMSI (bits 8..38).
    pub mmsi: u32,
    /// Wire octets (arrival-LSB-first) including the FCS.
    pub wire_bytes: Vec<u8>,
    /// Message bit string (per-octet reversed, FCS excluded), MSB-first —
    /// the form ASM field decode consumes.
    pub message_bits: Vec<u8>,
}

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
        Self { shift: 0, collecting: false, ones: 0, buf: Vec::with_capacity(MAX_BITS) }
    }

    /// Push one NRZI-decoded bit; returns a frame when a CRC-valid one
    /// completes.
    pub fn push_bit(&mut self, bit: u8) -> Option<VdesFrame> {
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
                // Seven+ consecutive ones cannot occur in a stuffed frame.
                self.collecting = false;
                return None;
            }
            self.buf.push(1);
            None
        } else if self.ones == 5 {
            // Stuffed zero: drop it.
            self.ones = 0;
            None
        } else if self.ones == 6 {
            // Closing flag: buf ends with the flag's leading 0111111.
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
            }
            None
        }
    }

    fn close(bits: &[u8]) -> Option<VdesFrame> {
        if bits.len() < MIN_BITS || bits.len() % 8 != 0 {
            return None;
        }
        // Assemble wire octets: bit i of each byte = i-th arrived bit.
        let wire_bytes: Vec<u8> = bits
            .chunks_exact(8)
            .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << i)))
            .collect();
        if !hdlc_frame_ok(&wire_bytes) {
            return None;
        }
        // Message bit string: payload octets (FCS dropped), bits reversed
        // per octet (LSB-first wire order → MSB-first field order).
        let payload = &wire_bytes[..wire_bytes.len() - 2];
        let message_bits: Vec<u8> =
            payload.iter().flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1)).collect();
        if message_bits.len() < 38 {
            return None;
        }
        let msg_type = message_bits[..6].iter().fold(0u8, |v, &b| (v << 1) | b);
        let mmsi = message_bits[8..38].iter().fold(0u32, |v, &b| (v << 1) | b as u32);
        Some(VdesFrame { msg_type, mmsi, wire_bytes, message_bits })
    }
}

impl Default for HdlcDeframer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulate::{hdlc_bits, wire_bytes_from_message_bits};

    /// A 256-bit broadcast-ASM (type 8) message with runs of ones to
    /// exercise bit stuffing.
    fn stuffy_message() -> Vec<u8> {
        let mut bits = vec![0u8; 256];
        // type 8 (001000)
        bits[2] = 1;
        // a run of ones in the MMSI field
        for b in bits[8..38].iter_mut().take(20) {
            *b = 1;
        }
        // a 30-bit run of ones in the payload → heavy stuffing
        for b in bits[120..150].iter_mut() {
            *b = 1;
        }
        bits
    }

    fn run(bits: &[u8]) -> Vec<VdesFrame> {
        let mut d = HdlcDeframer::new();
        bits.iter().filter_map(|&b| d.push_bit(b)).collect()
    }

    #[test]
    fn roundtrip_with_stuffing() {
        let msg = stuffy_message();
        let stream = hdlc_bits(&wire_bytes_from_message_bits(&msg));
        let frames = run(&stream);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].message_bits, msg);
        assert_eq!(frames[0].msg_type, 8);
    }

    #[test]
    fn rejects_bad_fcs() {
        let msg = stuffy_message();
        let mut wire = wire_bytes_from_message_bits(&msg);
        let n = wire.len();
        wire[n - 1] ^= 0x01; // corrupt FCS
        assert!(run(&hdlc_bits(&wire)).is_empty());
    }
}
