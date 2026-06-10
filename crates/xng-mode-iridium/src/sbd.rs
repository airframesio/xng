//! IDA fragment reassembly → SBD transport → ACARS (chain layout from
//! iridium-toolkit's reassembler, BSD-2 — see PROVENANCE.md). The ACARS
//! payload is a standard SOH-prefixed parity ACARS block handled by
//! xng-acars.

use crate::frame::DaFrame;
use serde_json::json;

/// Reassembles DA fragments (matched by counter continuity and burst
/// time proximity; single-channel, so no frequency matching) into L2
/// byte streams, then parses the SBD transport and extracts ACARS.
pub struct SbdReassembler {
    /// In-flight multi-fragment message: (next expected ctr, data,
    /// last fragment time in seconds).
    pending: Option<(u8, Vec<u8>, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SbdMessage {
    pub details: serde_json::Value,
    pub acars: Option<xng_acars::block::AcarsBlock>,
}

impl SbdReassembler {
    pub fn new() -> Self {
        Self { pending: None }
    }

    /// Feed one CRC-valid DA frame observed at `time` seconds.
    pub fn push(&mut self, f: &DaFrame, time: f64) -> Option<SbdMessage> {
        if !f.crc_ok {
            return None;
        }
        // Expire stale assembly (toolkit: 280 ms between fragments).
        if let Some((_, _, t)) = &self.pending {
            if time - t > 0.3 {
                self.pending = None;
            }
        }
        let bytes = &f.data[..(f.len as usize).min(20)];
        match (&mut self.pending, f.ctr, f.continuation) {
            (None, 0, false) => Self::parse_l2(bytes),
            (None, 0, true) => {
                self.pending = Some((1, bytes.to_vec(), time));
                None
            }
            (Some((next, data, _)), ctr, cont) if ctr == *next => {
                data.extend_from_slice(bytes);
                if cont {
                    let d = data.clone();
                    self.pending = Some(((ctr + 1) % 8, d, time));
                    None
                } else {
                    let d = data.clone();
                    self.pending = None;
                    Self::parse_l2(&d)
                }
            }
            _ => {
                self.pending = None;
                None
            }
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
            0x0600 => {
                if rest.first() != Some(&0x20) || rest.len() < 29 {
                    return None;
                }
                rest = &rest[29..];
            }
            0x7608 => {
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
