//! ARINC 618 frame synchronization, character assembly, and parsing.
//!
//! Block layout (after the SYN SYN SOH sync the deframer hunts for):
//! `Mode(1) Address(7) TechAck(1) Label(2) BlockId(1)` then either
//! `STX Text ETX/ETB` or a bare `ETX` (textless uplink), followed by the
//! 2-byte BCS (no parity) and DEL.
//!
//! Characters are LSB-first with odd parity in bit 8. The BCS is
//! CRC-16/KERMIT over the parity-bearing octets from Mode through ETX/ETB
//! inclusive; appending the two received BCS bytes must leave residue 0.
//!
//! Error correction: a single bit error in a character breaks both that
//! character's odd parity and the CRC, so bad-parity characters localize
//! the search — we try one bit flip per suspect character (8 candidates
//! each, up to 3 suspects) and accept the combination that restores CRC
//! residue 0. If the body is parity-clean but the CRC fails, the error may
//! be in the parity-less BCS bytes themselves; those 16 single-bit flips
//! are tried too.

use crate::fec;
use xng_dsp::checksum::acars_crc;

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const ETX: u8 = 0x03;
const NAK: u8 = 0x15;
const SYN: u8 = 0x16;
const ETB: u8 = 0x17;
const DEL: u8 = 0x7F;

/// SYN SYN SOH, LSB-first transmit order, oldest bit in bit 0.
const SYNC_PATTERN: u32 = (SOH as u32) << 16 | (SYN as u32) << 8 | SYN as u32;
const SYNC_MASK: u32 = 0xFF_FFFF;
/// Bit errors tolerated in the sync pattern.
const SYNC_MAX_ERRORS: u32 = 1;
/// Header length: mode + address(7) + ack + label(2) + block id.
const HEADER_LEN: usize = 12;
/// Header + STX + max text (220) + suffix, with margin.
const MAX_CHARS: usize = 250;
/// Most bad-parity characters the corrector will attempt to repair.
const MAX_CORRECTABLE: usize = 3;
/// Frames that fail CRC with more parity errors than this are discarded as
/// noise (tolerant sync makes false frame starts more common).
const MAX_REPORTED_PARITY_ERRORS: usize = 8;

/// A decoded ACARS frame (single block).
#[derive(Debug, Clone, PartialEq)]
pub struct AcarsFrame {
    pub mode: char,
    /// Aircraft registration, dot-padding stripped; `None` for all-NUL
    /// (squitter/all-call) addresses.
    pub tail: Option<String>,
    /// Technical ack; `None` when NAK (no acknowledgement).
    pub ack: Option<char>,
    /// Two label characters; 0x7F rendered as 'd' by display convention.
    pub label: String,
    /// `None` for NUL block id (uplink).
    pub block_id: Option<char>,
    /// True when the block id is a digit (downlink block identifier).
    pub downlink: bool,
    /// Downlink message sequence number (4 chars), when present.
    pub msg_num: Option<String>,
    /// Downlink flight id (6 chars), when present.
    pub flight: Option<String>,
    pub text: String,
    /// True when the block ended with ETB (more blocks follow).
    pub more_to_come: bool,
    pub crc_ok: bool,
    /// Characters with bad parity after correction (excluding BCS, which
    /// carries none).
    pub parity_errors: u32,
    /// Bits repaired by parity+CRC error correction.
    pub fixed_bits: u32,
    /// Octets from Mode through suffix + BCS (parity bits intact,
    /// post-correction when a repair succeeded).
    pub raw: Vec<u8>,
}

#[derive(Clone, Copy)]
enum State {
    Hunt,
    /// Collecting frame characters; `bcs_remaining` counts the two BCS
    /// bytes once the suffix has been seen.
    Collect { suffix_seen: bool, bcs_remaining: u8 },
}

pub struct Deframer {
    shift: u32,
    state: State,
    /// XOR mask resolving the differential-decode polarity ambiguity:
    /// 0 if the sync matched directly, 1 if the inverted stream matched.
    invert: u8,
    cur: u8,
    nbits: u8,
    chars: Vec<u8>,
    /// Indices (into `chars`) of characters that failed odd parity.
    parity_bad: Vec<usize>,
}

impl Deframer {
    pub fn new() -> Self {
        Self {
            shift: 0,
            state: State::Hunt,
            invert: 0,
            cur: 0,
            nbits: 0,
            chars: Vec::with_capacity(MAX_CHARS),
            parity_bad: Vec::new(),
        }
    }

    /// True while a block is part-way through being collected, i.e. sync has
    /// matched but the BCS has not arrived yet.
    ///
    /// The demod's presence gate is held open whenever this is set, so a
    /// signal that fades part-way through a block is still followed to the end
    /// of it instead of being cut off by the squelch closing underneath.
    pub fn is_collecting(&self) -> bool {
        matches!(self.state, State::Collect { .. })
    }

    /// Push one demodulated bit; returns a frame when one completes.
    pub fn push_bit(&mut self, bit: u8) -> Option<AcarsFrame> {
        self.shift = (self.shift >> 1) | ((bit as u32) << 23);

        match self.state {
            State::Hunt => {
                let w = self.shift & SYNC_MASK;
                if (w ^ SYNC_PATTERN).count_ones() <= SYNC_MAX_ERRORS {
                    self.start_collect(0);
                } else if (w ^ SYNC_PATTERN ^ SYNC_MASK).count_ones() <= SYNC_MAX_ERRORS {
                    // Differential polarity flipped: decode inverted.
                    self.start_collect(1);
                }
                None
            }
            State::Collect { suffix_seen, bcs_remaining } => {
                self.cur |= (bit ^ self.invert) << self.nbits;
                self.nbits += 1;
                if self.nbits < 8 {
                    return None;
                }
                let octet = self.cur;
                self.cur = 0;
                self.nbits = 0;
                self.chars.push(octet);

                if suffix_seen {
                    // Collecting the 2 BCS bytes (no parity check).
                    if bcs_remaining > 1 {
                        self.state = State::Collect { suffix_seen: true, bcs_remaining: 1 };
                        return None;
                    }
                    let frame = self.finish();
                    self.state = State::Hunt;
                    return frame;
                }

                if octet.count_ones() % 2 == 0 {
                    self.parity_bad.push(self.chars.len() - 1);
                }

                let value = octet & 0x7F;
                let idx = self.chars.len() - 1;
                // Suffix can appear at HEADER_LEN (textless uplink: bare ETX
                // in place of STX) or anywhere after the STX.
                if (value == ETX || value == ETB) && idx >= HEADER_LEN {
                    self.state = State::Collect { suffix_seen: true, bcs_remaining: 2 };
                } else if self.chars.len() >= MAX_CHARS {
                    // Runaway (false sync or lost suffix): back to hunting.
                    self.state = State::Hunt;
                }
                None
            }
        }
    }

    fn start_collect(&mut self, invert: u8) {
        self.state = State::Collect { suffix_seen: false, bcs_remaining: 0 };
        self.invert = invert;
        self.cur = 0;
        self.nbits = 0;
        self.chars.clear();
        self.parity_bad.clear();
    }

    fn finish(&mut self) -> Option<AcarsFrame> {
        // chars = Mode .. suffix + 2 BCS bytes; CRC residue over all == 0.
        let n = self.chars.len();
        if n < HEADER_LEN + 1 + 2 {
            return None;
        }
        let mut chars = self.chars.clone();
        let mut parity_errors = self.parity_bad.len() as u32;
        let mut fixed_bits = 0u32;
        let mut crc_ok = acars_crc(&chars) == 0;
        if !crc_ok {
            if let Some(fixed) = correct_errors(&mut chars, &self.parity_bad) {
                fixed_bits = fixed;
                parity_errors = 0;
                crc_ok = true;
            } else if self.parity_bad.len() > MAX_REPORTED_PARITY_ERRORS {
                // Unrecoverable noise (likely a false sync) — drop silently.
                return None;
            }
        }
        let suffix = chars[n - 3] & 0x7F;
        let body: Vec<u8> = chars[..n - 3].iter().map(|c| c & 0x7F).collect();

        let mode = body[0] as char;
        let addr = &body[1..8];
        let tail = if addr.iter().all(|&c| c == 0) {
            None
        } else {
            Some(addr.iter().map(|&c| c as char).skip_while(|&c| c == '.').collect::<String>())
        };
        let ack = match body[8] {
            NAK => None,
            c => Some(c as char),
        };
        let label: String = body[9..11]
            .iter()
            .map(|&c| if c == DEL { 'd' } else { c as char })
            .collect();
        let block_id = match body[11] {
            0 => None,
            c => Some(c as char),
        };
        let downlink = matches!(body[11], b'0'..=b'9');

        // Text section: STX then payload (downlinks lead with MSN + flight).
        let mut msg_num = None;
        let mut flight = None;
        let mut text = String::new();
        if body.len() > HEADER_LEN && body[HEADER_LEN] == STX {
            let mut payload = &body[HEADER_LEN + 1..];
            if downlink && payload.len() >= 10 {
                msg_num = Some(payload[..4].iter().map(|&c| c as char).collect());
                flight = Some(payload[4..10].iter().map(|&c| c as char).collect());
                payload = &payload[10..];
            }
            text = payload.iter().map(|&c| c as char).collect();
        }

        Some(AcarsFrame {
            mode,
            tail,
            ack,
            label,
            block_id,
            downlink,
            msg_num,
            flight,
            text,
            more_to_come: suffix == ETB,
            crc_ok,
            parity_errors,
            fixed_bits,
            raw: chars,
        })
    }
}

/// Try to repair a frame that failed its CRC.
///
/// Fast path (ACARS-4.2): a single bit error anywhere in the block —
/// including a parity-less BCS byte — is located in O(1) via the syndrome
/// table (`fec::correct_single_bit`), the acarsdec `syndrom.h` approach.
/// This subsumes the old per-character / per-BCS brute-force scan for the
/// common single-error case and works even when the flipped bit landed in a
/// position that *didn't* break odd parity.
///
/// Slow path: when the syndrome is not a single-bit error, fall back to the
/// parity-guided multi-error search. `suspects` are indices of characters
/// with bad parity; each is assumed to hold exactly one flipped bit, searched
/// jointly (8 candidates per suspect) until the CRC residue is 0. Returns the
/// number of repaired bits, with `chars` left corrected.
fn correct_errors(chars: &mut [u8], suspects: &[usize]) -> Option<u32> {
    // O(1) single-bit lookup first: covers a lone error in the body or in a
    // parity-less BCS byte without any search.
    if fec::correct_single_bit(chars).is_some() {
        return Some(1);
    }
    if suspects.is_empty() {
        // Parity-clean body and not a single-bit error: nothing the
        // parity-guided search can localize. (The single-bit BCS case was
        // already handled by the syndrome lookup above.)
        return None;
    }
    if suspects.len() > MAX_CORRECTABLE {
        return None;
    }
    // One flipped bit per suspect: walk the 8^k combinations.
    let k = suspects.len();
    let mut bits = vec![0u8; k];
    loop {
        for (i, &idx) in suspects.iter().enumerate() {
            chars[idx] ^= 1 << bits[i];
        }
        if acars_crc(chars) == 0 {
            return Some(k as u32);
        }
        for (i, &idx) in suspects.iter().enumerate() {
            chars[idx] ^= 1 << bits[i];
        }
        // Increment the base-8 counter.
        let mut i = 0;
        loop {
            if i == k {
                return None;
            }
            bits[i] += 1;
            if bits[i] < 8 {
                break;
            }
            bits[i] = 0;
            i += 1;
        }
    }
}

impl Default for Deframer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulate::{frame_octets, FrameSpec};

    fn bits_lsb_first(octets: &[u8]) -> Vec<u8> {
        octets.iter().flat_map(|o| (0..8).map(move |i| (o >> i) & 1)).collect()
    }

    fn run(bits: &[u8]) -> Vec<AcarsFrame> {
        let mut d = Deframer::new();
        bits.iter().filter_map(|&b| d.push_bit(b)).collect()
    }

    fn downlink_spec() -> FrameSpec<'static> {
        FrameSpec {
            mode: '2',
            tail: "N471XG",
            ack: None,
            label: "Q0",
            block_id: '5',
            msg_num: Some("M01A"),
            flight: Some("XG0042"),
            text: "TEST MESSAGE",
            etb: false,
        }
    }

    #[test]
    fn decodes_downlink() {
        // ones (idle) + sync + frame
        let mut bits = vec![1u8; 64];
        bits.extend(bits_lsb_first(&[0xAB, 0x2A, SYN, SYN, SOH]));
        bits.extend(bits_lsb_first(&frame_octets(&downlink_spec())));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(f.crc_ok, "CRC must verify");
        assert_eq!(f.parity_errors, 0);
        assert_eq!(f.mode, '2');
        assert_eq!(f.tail.as_deref(), Some("N471XG"));
        assert_eq!(f.label, "Q0");
        assert_eq!(f.block_id, Some('5'));
        assert!(f.downlink);
        assert_eq!(f.msg_num.as_deref(), Some("M01A"));
        assert_eq!(f.flight.as_deref(), Some("XG0042"));
        assert_eq!(f.text, "TEST MESSAGE");
        assert_eq!(f.ack, None);
        assert!(!f.more_to_come);
    }

    #[test]
    fn decodes_inverted_stream() {
        let mut bits = vec![0u8; 64];
        bits.extend(bits_lsb_first(&[0xAB, 0x2A, SYN, SYN, SOH]));
        bits.extend(bits_lsb_first(&frame_octets(&downlink_spec())));
        let inverted: Vec<u8> = bits.iter().map(|b| b ^ 1).collect();
        let frames = run(&inverted);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].crc_ok);
        assert_eq!(frames[0].text, "TEST MESSAGE");
    }

    #[test]
    fn decodes_textless_uplink() {
        let spec = FrameSpec {
            mode: '2',
            tail: "N471XG",
            ack: Some('5'),
            label: "_\u{7f}",
            block_id: 'A',
            msg_num: None,
            flight: None,
            text: "",
            etb: false,
        };
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&[SYN, SYN, SOH]));
        bits.extend(bits_lsb_first(&frame_octets(&spec)));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(f.crc_ok);
        assert!(!f.downlink);
        assert_eq!(f.label, "_d");
        assert_eq!(f.ack, Some('5'));
        assert_eq!(f.block_id, Some('A'));
        assert_eq!(f.text, "");
    }

    #[test]
    fn corrupted_frame_fails_crc() {
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&[SYN, SYN, SOH]));
        let mut octets = frame_octets(&downlink_spec());
        let text_pos = HEADER_LEN + 2;
        octets[text_pos] ^= 0x05; // corrupt a text char (keep parity rule plausible)
        bits.extend(bits_lsb_first(&octets));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        assert!(!frames[0].crc_ok);
    }

    #[test]
    fn corrects_single_bit_error_in_body() {
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&[SYN, SYN, SOH]));
        let mut octets = frame_octets(&downlink_spec());
        octets[15] ^= 0x04; // one bit in a text char → parity + CRC both break
        bits.extend(bits_lsb_first(&octets));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(f.crc_ok, "single-bit error should be repaired");
        assert_eq!(f.fixed_bits, 1);
        assert_eq!(f.parity_errors, 0);
        assert_eq!(f.text, "TEST MESSAGE", "text must be restored");
    }

    #[test]
    fn corrects_two_separate_single_bit_errors() {
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&[SYN, SYN, SOH]));
        let mut octets = frame_octets(&downlink_spec());
        octets[2] ^= 0x40; // one bit in the address
        octets[16] ^= 0x01; // one bit in the text
        bits.extend(bits_lsb_first(&octets));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(f.crc_ok, "two single-bit errors should be repaired");
        assert_eq!(f.fixed_bits, 2);
        assert_eq!(f.tail.as_deref(), Some("N471XG"));
        assert_eq!(f.text, "TEST MESSAGE");
    }

    #[test]
    fn corrects_bit_error_in_bcs() {
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&[SYN, SYN, SOH]));
        let mut octets = frame_octets(&downlink_spec());
        let n = octets.len();
        octets[n - 1] ^= 0x10; // flip a bit in the (parity-less) BCS
        bits.extend(bits_lsb_first(&octets));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].crc_ok);
        assert_eq!(frames[0].fixed_bits, 1);
    }

    #[test]
    fn tolerates_one_bit_error_in_sync() {
        let mut sync = [SYN, SYN, SOH];
        sync[1] ^= 0x20;
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&sync));
        bits.extend(bits_lsb_first(&frame_octets(&downlink_spec())));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].crc_ok);
        assert_eq!(frames[0].text, "TEST MESSAGE");
    }

    #[test]
    fn etb_marks_more_to_come() {
        let spec = FrameSpec { etb: true, ..downlink_spec() };
        let mut bits = vec![1u8; 40];
        bits.extend(bits_lsb_first(&[SYN, SYN, SOH]));
        bits.extend(bits_lsb_first(&frame_octets(&spec)));
        let frames = run(&bits);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].crc_ok);
        assert!(frames[0].more_to_come);
    }
}
