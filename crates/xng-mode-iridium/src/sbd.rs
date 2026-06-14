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
    /// Frame kind for the emitted message: "sbd", "gsm", or "mt-position".
    pub kind: &'static str,
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
            return Self::parse_l2(&p.data, p.ul);
        }

        // No continuation: a fresh single packet, a new long packet, or an
        // orphan continuation (dropped).
        match (f.ctr, f.continuation) {
            (0, false) => Self::parse_l2(bytes, ul),
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

    /// Parse an assembled L2 stream. Three parallel decoders run on the
    /// reassembled IDA packet: a mobile-terminal position (paging frames),
    /// GSM CC/MM/SMS signalling, and the SBD transport → ACARS path.
    fn parse_l2(data: &[u8], ul: bool) -> Option<SbdMessage> {
        if data.len() < 5 {
            return None;
        }
        // Mobile-terminal position embedded in paging/uplink frames.
        if let Some(pos) = crate::mtpos::extract(data, ul) {
            return Some(SbdMessage { kind: "mt-position", details: pos, acars: None });
        }
        // GSM call-control / mobility / SMS signalling.
        if let Some(mut g) = crate::gsm::decode(data) {
            // Carry the raw L2 bytes + direction for GSMTAP/Wireshark export.
            g["raw_l2_hex"] = json!(data.iter().map(|b| format!("{b:02x}")).collect::<String>());
            g["ul"] = json!(ul);
            return Some(SbdMessage { kind: "gsm", details: g, acars: None });
        }
        // SBD packet types (toolkit ReassembleIDASBD).
        let (typ, mut rest): (u16, &[u8]) = match (data[0], data[1]) {
            (0x76, t) if t != 5 => (u16::from_be_bytes([data[0], data[1]]), &data[2..]),
            (0x06, 0x00) => (0x0600, &data[2..]),
            _ => return None,
        };
        // Decode and expose the SBD transport pre-header rather than just
        // stripping it (toolkit ReassembleIDASBD / ReassembleIDAPP).
        let mut hdr = serde_json::Map::new();
        match typ {
            // Mobile-originated registration ("HELLO"): a 29-byte pre-header.
            // Only the 0x20 sub-type lays out an IMEI + MO sequence number
            // there; 0x10/0x40/0x50/0x70 reuse those bytes for other fields
            // (toolkit reassembler.py). The message count (byte 15) and the
            // registration timestamp (bytes 25..29) are common to all.
            0x0600 => {
                if rest.len() < 29 {
                    return None;
                }
                let h = &rest[..29];
                if h[0] == 0x20 {
                    if let Some(imei) = imei_bcd(&h[5..13]) {
                        hdr.insert("imei".into(), json!(imei));
                    }
                    hdr.insert("momsn".into(), json!(u16::from_be_bytes([h[13], h[14]])));
                }
                hdr.insert("msg_count".into(), json!(h[15]));
                let it = u32::from_be_bytes([h[25], h[26], h[27], h[28]]);
                if it != 0 {
                    hdr.insert("time_unix".into(), json!(crate::ira::iri_time_unix(it)));
                }
                rest = &rest[29..];
            }
            // SBD transfer: a recognised 0x26 (7-byte) or 0x20 (5-byte)
            // pre-header (toolkit DL 7608/7609/760a). The 0x26 form carries
            // the mobile-terminated sequence number, this transfer's packet
            // count and the queued backlog. An unrecognised first byte (e.g.
            // a UL 760c/d/e 0x50 echo-back) is left for the generic strips
            // below — never blindly skip a fixed count, which would corrupt a
            // header-less body.
            t if t >> 8 == 0x76 => match rest.first() {
                Some(0x26) if rest.len() >= 7 => {
                    hdr.insert("mtmsn".into(), json!(u16::from_be_bytes([rest[1], rest[2]])));
                    hdr.insert("packets".into(), json!(rest[3]));
                    hdr.insert("backlog".into(), json!(rest[4]));
                    rest = &rest[7..];
                }
                Some(0x20) if rest.len() >= 5 => {
                    rest = &rest[5..];
                }
                _ => {}
            },
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
        Self::parse_acars(typ, rest, hdr)
    }

    /// ACARS-over-SBD: payload begins with SOH (0x01); an optional
    /// 8-byte header tagged 0x03 follows; the rest is a standard
    /// parity-bearing ACARS block ending ETX/ETB + CRC + DEL. The decoded
    /// transport header (`hdr`) is merged into the emitted details.
    fn parse_acars(
        typ: u16,
        payload: &[u8],
        mut hdr: serde_json::Map<String, serde_json::Value>,
    ) -> Option<SbdMessage> {
        hdr.insert("type".into(), json!(format!("{typ:04x}")));
        if payload.first() != Some(&0x01) || payload.len() < 16 {
            hdr.insert(
                "payload_hex".into(),
                json!(payload.iter().map(|b| format!("{b:02x}")).collect::<String>()),
            );
            return Some(SbdMessage {
                kind: "sbd",
                details: serde_json::Value::Object(hdr),
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
        hdr.insert("acars_ok".into(), json!(acars.as_ref().map(|a| a.crc_ok)));
        Some(SbdMessage { kind: "sbd", details: serde_json::Value::Object(hdr), acars })
    }
}

/// Decode an Iridium 15-digit IMEI from 8 BCD bytes (low nibble then high
/// nibble per byte; the first nibble is a leading type indicator, so the
/// IMEI proper is the next 15 digits). Returns None unless every nibble is
/// a decimal digit — i.e. the bytes really are a BCD identity, not some
/// other 0x0600 sub-type that happens to reach here.
fn imei_bcd(b: &[u8]) -> Option<String> {
    if b.len() < 8 {
        return None;
    }
    let mut digits = Vec::with_capacity(16);
    for &x in &b[..8] {
        digits.push(x & 0xf);
        digits.push(x >> 4);
    }
    if digits.iter().any(|&d| d > 9) {
        return None;
    }
    Some(digits[1..16].iter().map(|d| char::from(b'0' + d)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn imei_bcd_decodes_15_digits() {
        // Nibbles (low then high per byte): leading 3, IMEI "300234032197210".
        let bytes = [0x33, 0x00, 0x32, 0x04, 0x23, 0x91, 0x27, 0x01];
        assert_eq!(imei_bcd(&bytes).as_deref(), Some("300234032197210"));
        // A non-decimal nibble (0xA) means this isn't a BCD identity.
        let bad = [0xa3, 0x00, 0x32, 0x04, 0x23, 0x91, 0x27, 0x01];
        assert_eq!(imei_bcd(&bad), None);
        assert_eq!(imei_bcd(&[0u8; 4]), None);
    }

    #[test]
    fn sbd_0600_registration_exposes_imei_momsn_count() {
        let mut data = vec![0x06, 0x00];
        let mut h = vec![0u8; 29];
        h[0] = 0x20; // sub-type (not an mt-position uplink marker)
        h[5..13].copy_from_slice(&[0x33, 0x00, 0x32, 0x04, 0x23, 0x91, 0x27, 0x01]);
        h[13] = 0x01; // MOMSN 300 = 0x012c
        h[14] = 0x2c;
        h[15] = 5; // message count
        data.extend_from_slice(&h);
        data.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // non-ACARS payload
        let m = SbdReassembler::parse_l2(&data, true).expect("sbd message");
        assert_eq!(m.kind, "sbd");
        assert_eq!(m.details["type"], json!("0600"));
        assert_eq!(m.details["imei"], json!("300234032197210"));
        assert_eq!(m.details["momsn"], json!(300));
        assert_eq!(m.details["msg_count"], json!(5));
        assert_eq!(m.details["payload_hex"], json!("deadbeef"));
        // Time field absent when the timestamp is zero.
        assert!(m.details.get("time_unix").is_none());
    }

    #[test]
    fn sbd_0600_non_0x20_subtype_omits_imei() {
        // A non-0x20 sub-type (here 0x10) does not lay out IMEI/MOMSN at
        // those offsets, even though the bytes happen to be valid BCD; only
        // the common message count must surface.
        let mut data = vec![0x06, 0x00];
        let mut h = vec![0u8; 29];
        h[0] = 0x10; // not the 0x20 IMEI-bearing sub-type
        h[5..13].copy_from_slice(&[0x33, 0x00, 0x32, 0x04, 0x23, 0x91, 0x27, 0x01]);
        h[13] = 0x01;
        h[14] = 0x2c;
        h[15] = 9;
        data.extend_from_slice(&h);
        data.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let m = SbdReassembler::parse_l2(&data, false).expect("sbd message");
        assert_eq!(m.details["type"], json!("0600"));
        assert!(m.details.get("imei").is_none(), "imei must be 0x20-only");
        assert!(m.details.get("momsn").is_none(), "momsn must be 0x20-only");
        assert_eq!(m.details["msg_count"], json!(9));
    }

    #[test]
    fn sbd_7608_transfer_exposes_mtmsn_packets_backlog() {
        // 7608 with a 0x26 (7-byte) pre-header then a non-ACARS payload.
        let data = [
            0x76, 0x08, // SBD transfer type
            0x26, 0x00, 0x07, // pre-header tag + MTMSN 7
            0x01, 0x02, // packets 1, backlog 2
            0x00, 0x00, // remainder of the 7-byte pre-header
            0xca, 0xfe, // payload
        ];
        let m = SbdReassembler::parse_l2(&data, false).expect("sbd message");
        assert_eq!(m.details["type"], json!("7608"));
        assert_eq!(m.details["mtmsn"], json!(7));
        assert_eq!(m.details["packets"], json!(1));
        assert_eq!(m.details["backlog"], json!(2));
        assert_eq!(m.details["payload_hex"], json!("cafe"));
    }
}

impl Default for SbdReassembler {
    fn default() -> Self {
        Self::new()
    }
}
