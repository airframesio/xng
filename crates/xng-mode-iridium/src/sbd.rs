//! IDA fragment reassembly → SBD transport → ACARS (chain layout from
//! iridium-toolkit's reassembler, BSD-2 — see PROVENANCE.md). The ACARS
//! payload is a standard SOH-prefixed parity ACARS block handled by
//! xng-acars.

use crate::frame::DaFrame;
use serde_json::json;

/// Frequency match window for grouping a channel's fragments (Hz). The
/// duplex channels are ~41.7 kHz apart, so this is comfortably narrow
/// enough not to confuse neighbours while tolerating per-burst CFO drift.
/// (iridium-toolkit uses ±260 Hz on gr-iridium's finer estimates.)
const FREQ_TOL_HZ: f64 = 2000.0;
/// Max gap between consecutive fragments of one message (toolkit 280 ms).
const FRAG_GAP_S: f64 = 0.28;
/// In-flight buffer lifetime before it is abandoned (toolkit 1000 ms).
const EXPIRE_S: f64 = 1.0;

/// One in-flight multi-fragment message.
struct Pending {
    freq: f64,
    ul: bool,
    /// Counter the next fragment must carry ((last + 1) mod 8).
    next_ctr: u8,
    data: Vec<u8>,
    last_time: f64,
}

/// Reassembles DA fragments into L2 byte streams, then parses the SBD
/// transport and extracts ACARS. Fragments are grouped exactly as
/// iridium-toolkit's `ReassembleIDA` does — by frequency (same duplex
/// channel), direction, sequential 3-bit counter, and time proximity —
/// keeping a list of concurrent in-flight messages, which is essential in
/// the wideband path where many channels are active at once (the old
/// single-slot, frequency-blind reassembler interleaved fragments from
/// different channels and almost never completed).
pub struct SbdReassembler {
    buf: Vec<Pending>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SbdMessage {
    pub details: serde_json::Value,
    pub acars: Option<xng_acars::block::AcarsBlock>,
}

impl SbdReassembler {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Feed one CRC-valid DA frame observed at `time` seconds on the burst
    /// `freq` (Hz, any consistent reference) and direction (`ul`).
    pub fn push(&mut self, f: &DaFrame, time: f64, freq: f64, ul: bool) -> Option<SbdMessage> {
        if !f.crc_ok {
            return None;
        }
        self.buf.retain(|p| time - p.last_time < EXPIRE_S);
        let bytes = &f.data[..(f.len as usize).min(20)];

        // Continue an in-flight message: same channel + direction, the
        // expected next counter, and within the inter-fragment window.
        let m = self.buf.iter().position(|p| {
            (p.freq - freq).abs() < FREQ_TOL_HZ
                && p.ul == ul
                && p.next_ctr == f.ctr
                && time >= p.last_time
                && time <= p.last_time + FRAG_GAP_S
        });
        if let Some(i) = m {
            self.buf[i].data.extend_from_slice(bytes);
            self.buf[i].last_time = time;
            if f.continuation {
                self.buf[i].next_ctr = (f.ctr + 1) % 8;
                return None;
            }
            let p = self.buf.remove(i);
            return Self::parse_l2(&p.data);
        }

        // No continuation: a fresh single packet, a new long packet, or an
        // orphan continuation (dropped).
        match (f.ctr, f.continuation) {
            (0, false) => Self::parse_l2(bytes),
            (0, true) => {
                self.buf.push(Pending {
                    freq,
                    ul,
                    next_ctr: 1,
                    data: bytes.to_vec(),
                    last_time: time,
                });
                None
            }
            _ => None,
        }
    }

    /// Parse an assembled L2 stream: SBD transport framing, then ACARS.
    fn parse_l2(data: &[u8]) -> Option<SbdMessage> {
        if data.len() < 5 {
            return None;
        }
        // SBD packet types (toolkit ReassembleIDASBD).
        let (typ, mut rest): (u16, &[u8]) = match (data[0], data[1]) {
            (0x76, t) if t != 5 => (u16::from_be_bytes([data[0], data[1]]), &data[2..]),
            (0x06, 0x00) => (0x0600, &data[2..]),
            _ => return None,
        };
        match typ {
            // HELLO / registration: a 29-byte pre-header (any sub-type —
            // toolkit accepts sub-types 0x10/0x20/0x40/0x50/0x70).
            0x0600 => {
                if rest.len() < 29 {
                    return None;
                }
                rest = &rest[29..];
            }
            // All 76xx SBD subtypes (7608/7609/760a/760c/d/e) carry a
            // 0x26 (7-byte) or 0x20 (5-byte) pre-header.
            t if t >> 8 == 0x76 => {
                let skip = match rest.first() {
                    Some(0x26) => 7,
                    Some(0x20) => 5,
                    _ => 7,
                };
                if rest.len() < skip {
                    return None;
                }
                rest = &rest[skip..];
            }
            _ => {}
        }
        // Optional ack/nack prefix on uplinks and the 0x10 len/cnt header.
        if rest.len() >= 3 && (rest[0] == 0x50 || rest[0] == 0x51) {
            rest = &rest[3..];
        }
        if rest.len() > 3 && rest[0] == 0x10 {
            let len = rest[1] as usize;
            rest = &rest[3..];
            if rest.len() > len {
                rest = &rest[..len];
            }
        }
        Self::parse_acars(typ, rest)
    }

    /// ACARS-over-SBD: payload begins with SOH (0x01); an optional
    /// 8-byte header tagged 0x03 follows; the rest is a standard
    /// parity-bearing ACARS block ending ETX/ETB + CRC + DEL.
    fn parse_acars(typ: u16, payload: &[u8]) -> Option<SbdMessage> {
        if payload.first() != Some(&0x01) || payload.len() < 16 {
            return Some(SbdMessage {
                details: json!({
                    "type": format!("{typ:04x}"),
                    "payload_hex": payload.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                }),
                acars: None,
            });
        }
        // Rebuild a standard block: SOH + (skip the 0x03 header if present).
        let body = if payload.get(1) == Some(&0x03) && payload.len() > 9 {
            let mut b = vec![0x01];
            b.extend_from_slice(&payload[9..]);
            b
        } else {
            payload.to_vec()
        };
        let acars = xng_acars::block::parse(&body);
        Some(SbdMessage {
            details: json!({
                "type": format!("{typ:04x}"),
                "acars_ok": acars.as_ref().map(|a| a.crc_ok),
            }),
            acars,
        })
    }
}

impl Default for SbdReassembler {
    fn default() -> Self {
        Self::new()
    }
}
