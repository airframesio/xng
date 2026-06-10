//! HDLC deframing for AIS (ISO/IEC 13239 as profiled by ITU-R M.1371):
//! 0x7E flag hunt, bit destuffing (a 0 after five consecutive 1s is
//! removed), octet assembly, CRC-16/X-25 FCS check, and the per-octet bit
//! reversal that turns wire bytes (LSB-first transmission) into the
//! MSB-first message bit string that AIS fields and NMEA armoring use.

use xng_dsp::checksum::hdlc_frame_ok;

/// Flag pattern in arrival order (bit 0 = oldest): 0,1,1,1,1,1,1,0 = 0x7E.
const FLAG: u8 = 0x7E;
/// Frame length bounds in destuffed bits, FCS included (shortest real AIS
/// payload is 96 bits + 16 FCS; longest multi-slot ~1008 + 16).
const MIN_BITS: usize = 56;
const MAX_BITS: usize = 1280;

/// A CRC-valid AIS transmission.
#[derive(Debug, Clone, PartialEq)]
pub struct AisFrame {
    /// Message type (bits 0..6 of the message).
    pub msg_type: u8,
    /// Source MMSI (bits 8..38).
    pub mmsi: u32,
    /// Wire octets (arrival-LSB-first) including the FCS.
    pub wire_bytes: Vec<u8>,
    /// Message bit string (per-octet reversed, FCS excluded), MSB-first —
    /// the form NMEA armoring consumes.
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
    pub fn push_bit(&mut self, bit: u8) -> Option<AisFrame> {
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

    fn close(bits: &[u8]) -> Option<AisFrame> {
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
        Some(AisFrame { msg_type, mmsi, wire_bytes, message_bits })
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

    fn run(bits: &[u8]) -> Vec<AisFrame> {
        let mut d = HdlcDeframer::new();
        bits.iter().filter_map(|&b| d.push_bit(b)).collect()
    }

    /// 168 message bits with long runs of ones to exercise stuffing.
    fn stuffy_message() -> Vec<u8> {
        let mut bits = vec![0u8; 168];
        // type 1 (000001)
        bits[5] = 1;
        // MMSI with runs of ones
        for b in bits[8..38].iter_mut().step_by(1).take(20) {
            *b = 1;
        }
        for b in bits[100..130].iter_mut() {
            *b = 1; // a 30-bit run of ones → heavy stuffing
        }
        bits
    }

    #[test]
    fn roundtrip_with_stuffing() {
        let msg = stuffy_message();
        let stream = hdlc_bits(&wire_bytes_from_message_bits(&msg));
        let frames = run(&stream);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].message_bits, msg);
        assert_eq!(frames[0].msg_type, 1);
    }

    #[test]
    fn rejects_bad_fcs() {
        let msg = stuffy_message();
        let mut wire = wire_bytes_from_message_bits(&msg);
        let n = wire.len();
        wire[n - 1] ^= 0x01; // corrupt FCS
        assert!(run(&hdlc_bits(&wire)).is_empty());
    }

    #[test]
    fn back_to_back_frames_share_flag() {
        let msg = stuffy_message();
        let wire = wire_bytes_from_message_bits(&msg);
        let mut stream = hdlc_bits(&wire);
        // Append a second frame right after, reusing the closing flag region.
        let second = hdlc_bits(&wire);
        stream.extend(&second[24..]); // skip the training prefix only
        let frames = run(&stream);
        assert_eq!(frames.len(), 2);
    }
}
