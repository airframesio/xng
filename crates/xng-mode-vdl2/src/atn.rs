//! ATN transport decoding under AVLC information frames — clean-room
//! from the public ISO specs (ISO/IEC 8208 X.25 packet layer, ISO/IEC
//! 8473 CLNP, ISO/IEC 8073 COTP) as profiled by ICAO Doc 9776/9705.
//! GPL implementations (dumpvdl2) were not consulted for this module.
//!
//! Scope: X.25 packet identification with call/clear semantics and
//! M-bit data reassembly; full (uncompressed) CLNP header parse; COTP
//! TPDU identification. ATN's deflate/LREF-compressed CLNP variants
//! are labeled but not expanded (layouts not yet verified against the
//! spec — hex is preserved).

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// A decoded X.25 packet (ISO 8208 packet layer over AVLC).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct X25Packet {
    /// Logical channel (group:number).
    pub lcn: u16,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ps: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u8>,
    /// More-data bit (data packets).
    pub more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<u8>,
    /// Payload (data packets: user data; call packets: CUD).
    #[serde(skip)]
    pub payload: Vec<u8>,
}

/// Parse one X.25 packet (modulo-8 sequencing, the VDL2 profile).
pub fn parse_x25(b: &[u8]) -> Option<X25Packet> {
    if b.len() < 3 {
        return None;
    }
    let gfi = b[0] >> 4;
    // VDL2 uses modulo-8 (GFI 0bxx01); tolerate Q/D bits.
    if gfi & 0b0011 != 0b0001 {
        return None;
    }
    let lcn = ((b[0] as u16 & 0x0F) << 8) | b[1] as u16;
    let t = b[2];
    let mut pkt = X25Packet {
        lcn,
        kind: "?",
        ps: None,
        pr: None,
        more: false,
        cause: None,
        diagnostic: None,
        payload: Vec::new(),
    };
    if t & 0x01 == 0 {
        // DATA: P(R) M P(S) 0.
        pkt.kind = "data";
        pkt.ps = Some((t >> 1) & 7);
        pkt.pr = Some((t >> 5) & 7);
        pkt.more = (t >> 4) & 1 == 1;
        pkt.payload = b[3..].to_vec();
        return Some(pkt);
    }
    match t {
        0x0B | 0x0F => {
            pkt.kind = if t == 0x0B { "call-request" } else { "call-accepted" };
            // Address block: BCD digit counts (called, calling), then the
            // digits, facilities length + facilities, then CUD.
            if b.len() > 3 {
                let called_len = (b[3] & 0x0F) as usize;
                let calling_len = (b[3] >> 4) as usize;
                let addr_octets = (called_len + calling_len).div_ceil(2);
                let fac_pos = 4 + addr_octets;
                if b.len() > fac_pos {
                    let fac_len = b[fac_pos] as usize;
                    let cud_pos = fac_pos + 1 + fac_len;
                    if b.len() > cud_pos {
                        pkt.payload = b[cud_pos..].to_vec();
                    }
                }
            }
        }
        0x13 | 0x17 => {
            pkt.kind = if t == 0x13 { "clear-request" } else { "clear-confirmation" };
            pkt.cause = b.get(3).copied();
            pkt.diagnostic = b.get(4).copied();
        }
        0x1B | 0x1F => pkt.kind = if t == 0x1B { "reset-request" } else { "reset-confirmation" },
        0xFB | 0xFF => return None, // restart: not expected on VDL2 SVCs
        0xF1 => {
            pkt.kind = "diagnostic";
            pkt.diagnostic = b.get(3).copied();
        }
        _ if t & 0x1F == 0x01 => {
            pkt.kind = "rr";
            pkt.pr = Some((t >> 5) & 7);
        }
        _ if t & 0x1F == 0x05 => {
            pkt.kind = "rnr";
            pkt.pr = Some((t >> 5) & 7);
        }
        _ if t & 0x1F == 0x09 => {
            pkt.kind = "rej";
            pkt.pr = Some((t >> 5) & 7);
        }
        _ => return None,
    }
    Some(pkt)
}

/// Reassembles X.25 data-packet M-bit sequences per logical channel.
pub struct X25Reassembler {
    pending: HashMap<u16, (Vec<u8>, f64)>,
}

const X25_TIMEOUT_SECS: f64 = 60.0;

impl X25Reassembler {
    pub fn new() -> Self {
        Self { pending: HashMap::new() }
    }

    /// Push a data packet; returns the full payload when a sequence
    /// completes (M=0).
    pub fn push(&mut self, pkt: &X25Packet, now: f64) -> Option<Vec<u8>> {
        if pkt.kind != "data" {
            return None;
        }
        self.pending.retain(|_, (_, t)| now - *t < X25_TIMEOUT_SECS);
        if pkt.more {
            let e = self.pending.entry(pkt.lcn).or_insert_with(|| (Vec::new(), now));
            e.0.extend_from_slice(&pkt.payload);
            e.1 = now;
            return None;
        }
        match self.pending.remove(&pkt.lcn) {
            Some((mut buf, _)) => {
                buf.extend_from_slice(&pkt.payload);
                Some(buf)
            }
            None => Some(pkt.payload.clone()),
        }
    }
}

impl Default for X25Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode an ATN network-layer payload (after X.25 reassembly): full
/// CLNP, or labels for the compressed forms and ES-IS/IDRP.
pub fn parse_network(b: &[u8]) -> Option<Value> {
    match b.first()? {
        0x81 => parse_clnp(b),
        0x82 => Some(json!({ "protocol": "ES-IS", "payload_len": b.len() })),
        0x83 => Some(json!({ "protocol": "IDRP", "payload_len": b.len() })),
        // ICAO 9705 LREF/deflate-compressed CLNP: leading octet is the
        // local-reference type. Layout not yet verified — label only.
        _ => Some(json!({
            "protocol": "clnp-compressed?",
            "first": format!("{:#04x}", b[0]),
            "payload_len": b.len(),
        })),
    }
}

/// Full (uncompressed) CLNP header per ISO 8473.
fn parse_clnp(b: &[u8]) -> Option<Value> {
    if b.len() < 9 || b[0] != 0x81 {
        return None;
    }
    let hdr_len = b[1] as usize;
    let version = b[2];
    let lifetime = b[3];
    let flags = b[4];
    let pdu_type = flags & 0x1F;
    let seg_len = u16::from_be_bytes([b[5], b[6]]);
    if hdr_len > b.len() || hdr_len < 9 {
        return None;
    }
    let type_name = match pdu_type {
        0x1C => "DT",
        0x1E => "ERQ",
        0x1F => "ERP",
        0x01 => "ER",
        _ => "?",
    };
    // Address part: dst len + dst, src len + src.
    let mut pos = 9;
    let read_addr = |pos: &mut usize| -> Option<String> {
        let len = *b.get(*pos)? as usize;
        *pos += 1;
        if *pos + len > b.len() || len > 20 {
            return None;
        }
        let s = b[*pos..*pos + len].iter().map(|x| format!("{x:02x}")).collect();
        *pos += len;
        Some(s)
    };
    let dst = read_addr(&mut pos)?;
    let src = read_addr(&mut pos)?;
    let payload = &b[hdr_len..];
    let mut out = json!({
        "protocol": "CLNP",
        "type": type_name,
        "version": version,
        "lifetime_500ms": lifetime,
        "seg_len": seg_len,
        "dst_nsap": dst,
        "src_nsap": src,
    });
    if let Some(cotp) = parse_cotp(payload) {
        out["cotp"] = cotp;
    }
    Some(out)
}

/// COTP TPDU identification per ISO 8073.
fn parse_cotp(b: &[u8]) -> Option<Value> {
    let li = *b.first()? as usize;
    let code = *b.get(1)?;
    let (name, refs) = match code & 0xF0 {
        0xE0 => ("CR", true),
        0xD0 => ("CC", true),
        0x80 => ("DR", true),
        0xF0 => ("DT", false),
        0x70 => ("ER", false),
        _ => return None,
    };
    let mut out = json!({ "tpdu": name });
    if refs && b.len() >= 6 {
        out["dst_ref"] = json!(u16::from_be_bytes([b[2], b[3]]));
        out["src_ref"] = json!(u16::from_be_bytes([b[4], b[5]]));
    }
    if name == "DT" && b.len() > li + 1 {
        let user = &b[li + 1..];
        out["user_data_len"] = json!(user.len());
        // ATN-B1 applications ride here (via the ULCS null encoding):
        // try protected-mode CPDLC, then CM.
        if let Some(app) = crate::atn_cpdlc::parse_apdu(user)
            .or_else(|| crate::atn_cpdlc::parse_cm_logon(user))
        {
            out["app"] = app;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25_data_packet_with_m_bit_reassembles() {
        // GFI 0001, LCN 0x023: data P(S)=1 P(R)=2, M=1 then M=0.
        let p1 = [0x10, 0x23, 0b010_1_001_0, 0xAA, 0xBB];
        let p2 = [0x10, 0x23, 0b010_0_010_0, 0xCC];
        let d1 = parse_x25(&p1).unwrap();
        assert_eq!(d1.kind, "data");
        assert_eq!(d1.ps, Some(1));
        assert_eq!(d1.pr, Some(2));
        assert!(d1.more);
        let d2 = parse_x25(&p2).unwrap();
        assert!(!d2.more);
        let mut r = X25Reassembler::new();
        assert_eq!(r.push(&d1, 0.0), None);
        assert_eq!(r.push(&d2, 1.0), Some(vec![0xAA, 0xBB, 0xCC]));
    }

    #[test]
    fn x25_call_and_clear_parse() {
        // CALL REQUEST, no addresses, no facilities, CUD = 0x81 ...
        let call = [0x10, 0x01, 0x0B, 0x00, 0x00, 0x81, 0x01];
        let c = parse_x25(&call).unwrap();
        assert_eq!(c.kind, "call-request");
        assert_eq!(c.payload, vec![0x81, 0x01]);
        let clear = [0x10, 0x01, 0x13, 0x05, 0x42];
        let c = parse_x25(&clear).unwrap();
        assert_eq!(c.kind, "clear-request");
        assert_eq!(c.cause, Some(5));
        assert_eq!(c.diagnostic, Some(0x42));
    }

    #[test]
    fn x25_supervisory_parse() {
        let rr = [0x10, 0x01, 0b101_00001];
        let p = parse_x25(&rr).unwrap();
        assert_eq!(p.kind, "rr");
        assert_eq!(p.pr, Some(5));
    }

    #[test]
    fn clnp_dt_with_cotp_parses() {
        // CLNP DT: NLPID 0x81, hdr_len, ver 1, lifetime, type DT(0x1C),
        // seg len, cksum, dst(2) src(2), then COTP DT.
        let mut b = vec![0x81, 15, 1, 0x3F, 0x1C, 0x00, 0x14, 0x00, 0x00];
        b.extend_from_slice(&[2, 0x47, 0x01]); // dst NSAP
        b.extend_from_slice(&[2, 0x47, 0x02]); // src NSAP
        b.extend_from_slice(&[0x02, 0xF0, 0x80, b'H', b'I']); // COTP DT
        let v = parse_network(&b).unwrap();
        assert_eq!(v["protocol"], "CLNP");
        assert_eq!(v["type"], "DT");
        assert_eq!(v["dst_nsap"], "4701");
        assert_eq!(v["src_nsap"], "4702");
        assert_eq!(v["cotp"]["tpdu"], "DT");
    }

    #[test]
    fn esis_and_compressed_labeled() {
        let v = parse_network(&[0x82, 0, 0]).unwrap();
        assert_eq!(v["protocol"], "ES-IS");
        let v = parse_network(&[0x05, 0, 0]).unwrap();
        assert_eq!(v["protocol"], "clnp-compressed?");
    }
}
