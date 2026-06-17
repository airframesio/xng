//! IMS pager ("messaging") frames — ported from BSD-licensed
//! iridium-toolkit (bitsparser.py: IridiumMSMessage / -Body /
//! -Ascii / -BCD; see PROVENANCE.md). Ref: US 5,596,315.
//!
//! Input: the 21-bit BCH data blocks (messaging polynomial) of a frame
//! classified as MS. Block 0 is the header (super-frame block/frame
//! counters, length, group); acquisition-group frames carry 2–4 extra
//! header blocks; up to two all-ones trailer blocks pad the end. The
//! body interleaves an "odd bit" stream (first bit of each block) with
//! 20-bit payload slices carrying the pager RIC, format, sequence, and
//! either 7-bit ASCII text or BCD digits.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsFrame {
    /// Block number in the super frame.
    pub block: u8,
    /// Frame (or cell) number.
    pub frame: u8,
    /// Group: "A" (acquisition) or "0".."3".
    pub group: String,
    /// Acquisition-group ("A") header fields, present only when the frame is
    /// an acquisition message (toolkit `IridiumMSMessage` group-A path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acq: Option<MsAcq>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<MsBody>,
}

/// Acquisition-group ("AQ") header carried by group-"A" messaging frames
/// (IRID-1). Toolkit `IridiumMSMessage`: `unknown1`/`secondary` live in the
/// header block (bits 19/20); the two-block pre-message header carries a
/// 12-bit counter `ctr1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsAcq {
    /// Header bit 19 (toolkit `unknown1`).
    pub unknown1: u8,
    /// Header bit 20 (toolkit `secondary`: something like a secondary SV).
    pub secondary: u8,
    /// 12-bit counter from the first pre-message block (toolkit `ctr1`).
    pub ctr1: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsBody {
    /// Pager Radio Identity Code.
    pub ric: u32,
    pub format: u8,
    pub seq: u8,
    #[serde(flatten)]
    pub content: MsContent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "content", rename_all = "snake_case")]
pub enum MsContent {
    Ascii {
        text: String,
        /// Part counter / highest part for multi-part messages.
        ctr: u8,
        ctr_max: u8,
        csum_ok: bool,
    },
    Bcd {
        digits: String,
    },
    Raw {
        hex: String,
    },
}

fn int(bits: &[u8]) -> u32 {
    bits.iter().fold(0u32, |v, &b| (v << 1) | b as u32)
}

/// Parse an MS frame from its 21-bit BCH data blocks.
pub fn parse(blocks: &[Vec<u8>]) -> Option<MsFrame> {
    if blocks.is_empty() || blocks[0].len() != 21 {
        return None;
    }
    let h = &blocks[0];
    let ms_type = h[0];
    if int(&h[1..5]) != 0 {
        return None; // zero1 must be 0000
    }
    let block = int(&h[5..9]) as u8;
    let frame = int(&h[9..15]) as u8;
    let bch_blocks = int(&h[15..19]) as usize;
    if bch_blocks < 2 {
        return None;
    }
    let group = if ms_type == 1 { "A".to_string() } else { int(&h[19..21]).to_string() };

    // Trim to the declared length (each "BCH block" is two 21-bit halves).
    let mut blocks: Vec<&Vec<u8>> = blocks.iter().take(2 * bch_blocks).collect();
    blocks.remove(0);

    // Acquisition group ("AQ"): the header block carries unknown1/secondary in
    // bits 19/20, and a 2-block pre-message header whose first block starts
    // with a 12-bit counter (toolkit `ctr1`). Capture those, then drain the
    // 2 (or 4 when present) block-header blocks.
    let mut acq: Option<MsAcq> = None;
    if group == "A" {
        if blocks.len() < 2 {
            return None;
        }
        let ctr1 = int(&blocks[0][..12]) as u16;
        acq = Some(MsAcq { unknown1: h[19], secondary: h[20], ctr1 });
        let n = if blocks.len() >= 4 { 4 } else { 2 };
        blocks.drain(..n);
    }
    // Up to two all-ones trailer blocks.
    for _ in 0..2 {
        if blocks.last().is_some_and(|b| b.iter().all(|&x| x == 1)) {
            blocks.pop();
        }
    }
    let mut out = MsFrame { block, frame, group, acq, body: None };
    if blocks.is_empty() {
        return Some(out);
    }

    // Body: strip the leading "odd bit" of each block, concatenate the
    // 20-bit remainders.
    let rest: Vec<u8> = blocks.iter().flat_map(|b| b[1..].iter().copied()).collect();
    if rest.len() <= 27 + 16 {
        return Some(out);
    }
    // RIC is transmitted LSB-first.
    let ric = rest[..22].iter().rev().fold(0u32, |v, &b| (v << 1) | b as u32);
    let format = int(&rest[22..27]) as u8;
    let seq = int(&rest[27..33]) as u8;
    if int(&rest[33..37]) != 0 {
        return Some(out); // zero1 violation in body
    }
    let data = &rest[43..];

    let content = match format {
        5 => ascii_content(data, &blocks),
        3 => bcd_content(data),
        _ => None,
    }
    .unwrap_or_else(|| MsContent::Raw {
        hex: data.chunks(8).map(|c| format!("{:02x}", int(c))).collect(),
    });
    out.body = Some(MsBody { ric, format, seq, content });
    Some(out)
}

/// Checksum over the body blocks (excluding the RIC block): the 10-bit
/// value at a fixed position plus the 8/8/5-bit slice sums must total
/// 1023 mod 1024.
fn msg_checksum(blocks: &[&Vec<u8>]) -> bool {
    if blocks.len() < 2 {
        return false;
    }
    let mut cs_bits: Vec<u8> = blocks[0][21 - 3..].to_vec();
    cs_bits.extend_from_slice(&blocks[1][1..8]);
    let csum_val = cs_bits.iter().rev().fold(0u32, |v, &b| (v << 1) | b as u32);
    let mut csum = 0u32;
    for (idx, b) in blocks.iter().enumerate() {
        if idx != 1 {
            csum += int(&b[..8]);
        }
        csum += int(&b[8..16]);
        if idx != 0 {
            csum += int(&b[16..]);
        }
    }
    (csum_val + csum) % 1024 == 1023
}

fn ascii_content(data: &[u8], body_blocks: &[&Vec<u8>]) -> Option<MsContent> {
    if data.len() < 5 {
        return None;
    }
    let csum_ok = msg_checksum(&body_blocks[1..]);
    let len_bit = data[4];
    let mut rest = &data[5..];
    let (mut ctr, mut ctr_max) = (0u8, 0u8);
    if len_bit == 1 {
        if rest.len() < 4 {
            return None;
        }
        let lfl = int(&rest[..4]) as usize;
        if lfl == 0 || rest.len() < 4 + 2 * lfl {
            return None;
        }
        ctr = int(&rest[4..4 + lfl]) as u8;
        ctr_max = int(&rest[4 + lfl..4 + 2 * lfl]) as u8;
        rest = &rest[4 + 2 * lfl..];
    }
    if rest.len() < 8 || rest[0] != 0 {
        return None;
    }
    let chars = &rest[8..];
    let mut text = String::new();
    for ch in chars.chunks_exact(7) {
        let c = int(ch);
        if c == 3 {
            break; // ETX
        }
        if c < 32 || c == 127 {
            text.push_str(&format!("[{c}]"));
        } else {
            text.push(c as u8 as char);
        }
    }
    Some(MsContent::Ascii { text, ctr, ctr_max, csum_ok })
}

fn bcd_content(data: &[u8]) -> Option<MsContent> {
    if data.len() < 5 {
        return None;
    }
    let digits: String =
        data[1..].chunks_exact(4).map(|c| format!("{:x}", int(c))).collect();
    Some(MsContent::Bcd { digits })
}

/// Joins multi-part ASCII pages (parts share a RIC; `ctr` runs to
/// `ctr_max`).
pub struct PagerReassembler {
    pending: HashMap<u32, (Vec<Option<String>>, f64)>,
}

const PAGE_TIMEOUT_SECS: f64 = 60.0;

impl PagerReassembler {
    pub fn new() -> Self {
        Self { pending: HashMap::new() }
    }

    /// Returns the full message text when all parts have arrived.
    pub fn push(&mut self, body: &MsBody, time: f64) -> Option<String> {
        let MsContent::Ascii { text, ctr, ctr_max, .. } = &body.content else {
            return None;
        };
        if *ctr_max == 0 {
            return Some(text.clone()); // single-part page
        }
        self.pending.retain(|_, (_, t)| time - *t < PAGE_TIMEOUT_SECS);
        let entry = self
            .pending
            .entry(body.ric)
            .or_insert_with(|| (vec![None; *ctr_max as usize + 1], time));
        if entry.0.len() != *ctr_max as usize + 1 {
            entry.0 = vec![None; *ctr_max as usize + 1];
        }
        let idx = (*ctr as usize).min(entry.0.len() - 1);
        entry.0[idx] = Some(text.clone());
        entry.1 = time;
        if entry.0.iter().all(|p| p.is_some()) {
            let full: String =
                entry.0.iter().map(|p| p.as_deref().unwrap_or("")).collect();
            self.pending.remove(&body.ric);
            return Some(full);
        }
        None
    }
}

impl Default for PagerReassembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the 21-bit blocks of a single-part ASCII page.
    fn build_ascii_frame(ric: u32, text: &str) -> Vec<Vec<u8>> {
        // Body bit stream (per-block: odd bit + 20 payload bits).
        let mut rest: Vec<u8> = Vec::new();
        // RIC, LSB first.
        for k in 0..22 {
            rest.push(((ric >> k) & 1) as u8);
        }
        push_int(&mut rest, 5, 5); // format 5 = ASCII
        push_int(&mut rest, 7, 6); // seq
        push_int(&mut rest, 0, 4); // zero1
        push_int(&mut rest, 0, 6); // cs1 (not validated by parser)
        // data: cs2(4) + len_bit(0) + zero2(1) + checksum(7) + chars
        push_int(&mut rest, 0, 4);
        rest.push(0); // single part
        rest.push(0); // zero2
        push_int(&mut rest, 0, 7); // msg checksum (csum_ok may be false)
        for c in text.bytes() {
            push_int(&mut rest, c as u32, 7);
        }
        push_int(&mut rest, 3, 7); // ETX

        // Slice into 20-bit payloads with odd-bit prefix 0.
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        for chunk in rest.chunks(20) {
            let mut b = vec![0u8];
            b.extend_from_slice(chunk);
            b.resize(21, 0);
            blocks.push(b);
        }
        // Header block: type 0, zero1, block 3, frame 9, bch_blocks, group 1.
        let total_halves = 1 + blocks.len();
        let bch_blocks = (total_halves + 1) / 2;
        let mut h = Vec::new();
        h.push(0);
        push_int(&mut h, 0, 4);
        push_int(&mut h, 3, 4);
        push_int(&mut h, 9, 6);
        push_int(&mut h, bch_blocks as u32, 4);
        push_int(&mut h, 1, 2);
        let mut out = vec![h];
        out.extend(blocks);
        // Pad to an even number of halves with an all-ones trailer.
        if out.len() % 2 == 1 {
            out.push(vec![1u8; 21]);
        }
        out
    }

    fn push_int(v: &mut Vec<u8>, val: u32, n: usize) {
        for k in (0..n).rev() {
            v.push(((val >> k) & 1) as u8);
        }
    }

    #[test]
    fn ascii_page_roundtrips() {
        let blocks = build_ascii_frame(1234567, "CALL OPS +14155550100");
        let f = parse(&blocks).expect("frame");
        assert_eq!(f.block, 3);
        assert_eq!(f.frame, 9);
        assert_eq!(f.group, "1");
        let body = f.body.expect("body");
        assert_eq!(body.ric, 1234567);
        assert_eq!(body.format, 5);
        let MsContent::Ascii { text, ctr_max, .. } = body.content else {
            panic!("expected ascii");
        };
        assert_eq!(text, "CALL OPS +14155550100");
        assert_eq!(ctr_max, 0);
    }

    /// IRID-1: acquisition-group ("AQ") header extraction. Layout pinned
    /// against iridium-toolkit `IridiumMSMessage` group-"A" path:
    ///   header bit[0]=ms_type=1 (group A), bits[19]=unknown1, [20]=secondary;
    ///   the two-block pre-message header's first block bits[0:12] = ctr1.
    /// (Verified with the toolkit slicing in the IRID-1 derivation: a header
    /// with ms_type 1 + unknown1 1 + secondary 1 and ctr1=0xABC parses to
    /// exactly those values.)
    #[test]
    fn acquisition_group_header_decodes() {
        // Header block (21 bits): ms_type=1, zero1=0000, block=2, frame=7,
        // bch_blocks=3, unknown1=1, secondary=1.
        let mut h = Vec::new();
        h.push(1); // ms_type == 1 -> group A
        push_int(&mut h, 0, 4); // zero1
        push_int(&mut h, 2, 4); // block
        push_int(&mut h, 7, 6); // frame
        push_int(&mut h, 3, 4); // bch_blocks (>=2)
        h.push(1); // unknown1
        h.push(1); // secondary
        assert_eq!(h.len(), 21);

        // First pre-message block: 12-bit ctr1 = 0xABC, then padding.
        let mut pre0 = Vec::new();
        push_int(&mut pre0, 0xABC, 12);
        pre0.resize(21, 0);

        // parse() takes 2*bch_blocks = 6 blocks: header + (>=4 -> 4 pre) ...
        // give it 6 blocks so the group-A path drains 4 pre-blocks.
        let blocks: Vec<Vec<u8>> = vec![
            h,
            pre0,
            vec![0u8; 21],
            vec![1u8; 21],
            vec![1u8; 21],
            vec![1u8; 21],
        ];
        let f = parse(&blocks).expect("acquisition frame parses");
        assert_eq!(f.group, "A");
        let acq = f.acq.expect("acq header present for group A");
        assert_eq!(acq.unknown1, 1);
        assert_eq!(acq.secondary, 1);
        assert_eq!(acq.ctr1, 0xABC);
        assert_eq!(f.block, 2);
        assert_eq!(f.frame, 7);
    }

    /// Non-acquisition (group "1") frames must NOT carry an acq header — the
    /// field is exclusive to the acquisition path.
    #[test]
    fn non_acquisition_frame_has_no_acq() {
        let blocks = build_ascii_frame(1234567, "HELLO");
        let f = parse(&blocks).expect("frame");
        assert_eq!(f.group, "1");
        assert!(f.acq.is_none());
    }

    #[test]
    fn pager_reassembles_multipart() {
        let mut r = PagerReassembler::new();
        let part = |ctr: u8, text: &str| MsBody {
            ric: 99,
            format: 5,
            seq: 1,
            content: MsContent::Ascii {
                text: text.into(),
                ctr,
                ctr_max: 1,
                csum_ok: true,
            },
        };
        assert_eq!(r.push(&part(0, "HELLO "), 0.0), None);
        assert_eq!(r.push(&part(1, "WORLD"), 1.0), Some("HELLO WORLD".into()));
    }
}
