//! HDLC framing for AX.25: NRZI decode + bit de-stuffing + flag framing.
//!
//! AX.25 v2.2 §3.6–§3.8 and the underlying ISO 3309 HDLC rules:
//!
//! - Frames are delimited by the **flag** octet `0x7E` (`01111110`). One or
//!   more flags precede and follow each frame.
//! - **Bit stuffing** (§3.7): on transmit, after five consecutive `1` bits in
//!   the data a `0` is inserted; on receive that stuffed `0` is removed. This
//!   guarantees the flag's six-ones pattern never appears inside data.
//! - **NRZI** (§3.6): the line code is NRZI — a `0` bit is encoded as a
//!   *change* of the transmitted tone, a `1` bit as *no change*. The
//!   demodulator delivers raw NRZI symbols; this module differentially
//!   decodes them back to data bits.
//! - The recovered data bits are assembled **LSB-first** into octets (§3.8:
//!   each octet is sent low-order bit first).
//!
//! Input here is the stream of NRZI line symbols (one per bit period) coming
//! out of the AFSK detector. Output is a list of de-stuffed octet frames
//! (address…control PID info FCS) ready for [`crate::ax25::parse_frame`].

/// HDLC flag octet.
pub const FLAG: u8 = 0x7e;

/// Streaming HDLC deframer: consumes NRZI symbols, emits raw frames.
///
/// NRZI differential decode feeds a bit-level state machine that recognizes
/// the `0x7E` flag via a running 1-count, removes stuffed zeros, and
/// accumulates LSB-first octets between flags.
///
/// Flag handling is done on the *bit* level: the flag `01111110` is the only
/// place six consecutive 1s appear. Tracking `ones`, a `0` after exactly six
/// 1s is a flag boundary; a `0` after exactly five 1s is a stuffed bit to
/// drop; seven+ 1s is an abort/idle.
pub struct HdlcDeframer {
    /// Previous NRZI symbol, for differential (NRZI) decoding.
    last_symbol: u8,
    have_last: bool,
    /// Count of consecutive 1 line-bits seen (for stuff/flag detection).
    ones: u8,
    /// Bits accumulated for the current octet (LSB-first).
    bit_buf: u8,
    bit_count: u8,
    /// Octets accumulated for the current frame.
    frame: Vec<u8>,
    /// True once a flag has opened a frame.
    in_frame: bool,
}

impl Default for HdlcDeframer {
    fn default() -> Self {
        Self::new()
    }
}

impl HdlcDeframer {
    pub fn new() -> Self {
        Self {
            last_symbol: 0,
            have_last: false,
            ones: 0,
            bit_buf: 0,
            bit_count: 0,
            frame: Vec::new(),
            in_frame: false,
        }
    }

    /// Feed one NRZI line symbol (0/1). Completed frames (de-stuffed octet
    /// sequences, including the trailing FCS) are pushed to `out`.
    pub fn push_symbol(&mut self, symbol: u8, out: &mut Vec<Vec<u8>>) {
        // NRZI: a 1 data-bit = no change; a 0 data-bit = change of symbol.
        let bit = if !self.have_last {
            self.have_last = true;
            self.last_symbol = symbol;
            // First symbol has no predecessor; treat as a 1 (transitions only
            // carry information once we have a reference).
            1
        } else {
            let same = symbol == self.last_symbol;
            self.last_symbol = symbol;
            u8::from(same)
        };
        self.push_bit(bit, out);
    }

    /// Feed an already-NRZI-decoded data bit directly (used by tests that
    /// build a bit stream from spec octets).
    pub fn push_data_bit(&mut self, bit: u8, out: &mut Vec<Vec<u8>>) {
        self.push_bit(bit & 1, out);
    }

    fn push_bit(&mut self, bit: u8, out: &mut Vec<Vec<u8>>) {
        if bit == 1 {
            // Legitimate data never has six consecutive 1s (the encoder stuffs
            // a 0 after five). So:
            match self.ones {
                0..=4 => {
                    // Still within a data run: store and count.
                    self.ones += 1;
                    self.store_bit(1);
                }
                5 => {
                    // Sixth consecutive 1: this is the flag's body, not data.
                    // Do NOT store it; advance to 6 and await the closing 0.
                    self.ones = 6;
                }
                _ => {
                    // Seven+ consecutive 1s: abort / idle. Drop any partial
                    // frame; stay out of frame until the next flag.
                    self.abort();
                    self.ones = 7;
                }
            }
            return;
        }

        // bit == 0.
        let prev_ones = self.ones;
        self.ones = 0;
        match prev_ones {
            6 => {
                // 0 1 1 1 1 1 1 0 = flag boundary.
                self.handle_flag(out);
            }
            5 => {
                // Stuffed zero after five ones — drop it (do not store).
            }
            _ => {
                self.store_bit(0);
            }
        }
    }

    /// Append a data bit to the current octet (LSB-first) when inside a frame.
    /// When five 1s have just been counted we are still mid-octet; the flag /
    /// stuff decision for the *following* bit is made in [`push_bit`].
    fn store_bit(&mut self, bit: u8) {
        if !self.in_frame {
            return;
        }
        self.bit_buf |= bit << self.bit_count;
        self.bit_count += 1;
        if self.bit_count == 8 {
            self.frame.push(self.bit_buf);
            self.bit_buf = 0;
            self.bit_count = 0;
        }
    }

    fn handle_flag(&mut self, out: &mut Vec<Vec<u8>>) {
        if self.in_frame {
            // Closing flag. The in-progress partial octet (`bit_buf` /
            // `bit_count`) holds only the flag's own leading zero + ones — it
            // is never data — so it is discarded; the completed octets in
            // `self.frame` are the frame. A real frame is at least
            // dest(7)+source(7)+control(1)+pid(1)+fcs(2) = 18 octets, but we
            // emit anything plausible and let the AX.25 parser + FCS reject
            // junk; require a minimum so stray flag pairs do not emit empties.
            if self.frame.len() >= 3 {
                out.push(std::mem::take(&mut self.frame));
            }
        }
        // Either way, a flag opens a (new) frame.
        self.in_frame = true;
        self.frame.clear();
        self.bit_buf = 0;
        self.bit_count = 0;
    }

    fn abort(&mut self) {
        self.in_frame = false;
        self.frame.clear();
        self.bit_buf = 0;
        self.bit_count = 0;
    }
}

/// Encode data octets (LSB-first) into a bit-stuffed, flag-delimited HDLC
/// bit stream (NOT NRZI-encoded). Tests / modulator only.
///
/// Surrounds the frame with `flags` opening flags and one closing flag, and
/// inserts a stuffed 0 after every five consecutive 1 data bits.
pub fn frame_bits(frame: &[u8], flags: usize) -> Vec<u8> {
    let mut bits = Vec::new();
    let flag_bits = [0u8, 1, 1, 1, 1, 1, 1, 0];
    for _ in 0..flags.max(1) {
        bits.extend_from_slice(&flag_bits);
    }
    let mut ones = 0u8;
    for &octet in frame {
        for i in 0..8 {
            let b = (octet >> i) & 1; // LSB-first
            bits.push(b);
            if b == 1 {
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
    bits.extend_from_slice(&flag_bits);
    bits
}

/// NRZI-encode a data bit stream into line symbols (tests / modulator only).
/// A 0 data bit toggles the symbol; a 1 keeps it. Starts from symbol 1.
pub fn nrzi_encode(bits: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bits.len());
    let mut sym = 1u8;
    for &b in bits {
        if b == 0 {
            sym ^= 1;
        }
        out.push(sym);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_dsp::checksum::hdlc_fcs;

    /// SPEC GROUND TRUTH — HDLC bit-stuffing (AX.25 §3.7): five consecutive
    /// 1s in the data force a stuffed 0. Build the stuffed bit stream for an
    /// octet containing 0x7F (0b01111111, LSB-first = 1,1,1,1,1,1,1,0) and
    /// confirm a 0 was inserted after the fifth 1.
    #[test]
    fn bit_stuffing_inserts_zero_after_five_ones() {
        // One data octet, no flags around it so we can read the body.
        let bits = frame_bits(&[0x7F], 1);
        // Drop the 8-bit opening flag and trailing flag, leaving the stuffed
        // data bits.
        let body = &bits[8..bits.len() - 8];
        // 0x7F LSB-first is 1,1,1,1,1,1,1,0. After 5 ones a 0 is stuffed:
        // 1 1 1 1 1 [0] 1 1 0
        assert_eq!(body, &[1, 1, 1, 1, 1, 0, 1, 1, 0]);
    }

    /// Round through the deframer: frame_bits -> push_data_bit recovers the
    /// exact octets and removes the stuffed zeros. (This exercises the
    /// de-stuffing + flag framing against the spec stuffing rule above; the
    /// frame content itself is checked by FCS.)
    #[test]
    fn deframer_destuffs_and_recovers_octets() {
        let payload = vec![0x7Fu8, 0x00, 0xFF, 0x55, 0x7E ^ 0x01];
        let mut data = payload.clone();
        let fcs = hdlc_fcs(&payload);
        data.extend_from_slice(&fcs.to_le_bytes());
        let bits = frame_bits(&data, 3);
        let mut deframer = HdlcDeframer::new();
        let mut out = Vec::new();
        for &b in &bits {
            deframer.push_data_bit(b, &mut out);
        }
        assert_eq!(out.len(), 1, "exactly one frame between the flags");
        assert_eq!(out[0], data);
    }

    /// NRZI: a 0 toggles, a 1 holds. The deframer's NRZI decode must invert
    /// nrzi_encode. (AX.25 §3.6.)
    #[test]
    fn nrzi_round_trips_through_deframer() {
        let payload = vec![0xABu8, 0xCD, 0xEF, 0x12];
        let mut data = payload.clone();
        let fcs = hdlc_fcs(&payload);
        data.extend_from_slice(&fcs.to_le_bytes());
        let bits = frame_bits(&data, 4);
        let symbols = nrzi_encode(&bits);
        let mut deframer = HdlcDeframer::new();
        let mut out = Vec::new();
        for &s in &symbols {
            deframer.push_symbol(s, &mut out);
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], data);
    }
}
