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
    /// Negotiated facilities on call packets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facilities: Vec<Value>,
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
        facilities: Vec::new(),
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
                    if fac_len > 0 && fac_pos + 1 + fac_len <= b.len() {
                        pkt.facilities =
                            parse_facilities(&b[fac_pos + 1..fac_pos + 1 + fac_len]);
                    }
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
        0x82 => Some(parse_esis(b)),
        0x83 => Some(parse_idrp(b)),
        // ICAO 9705 LREF/deflate-compressed CLNP: leading octet is the
        // local-reference type. Layout not yet verified — label only.
        _ => Some(json!({
            "protocol": "clnp-compressed?",
            "first": format!("{:#04x}", b[0]),
            "payload_len": b.len(),
        })),
    }
}

/// ES-IS (ISO 9542) option-TLV name (the parameters profiled for ATN).
fn esis_option_name(t: u8) -> &'static str {
    match t {
        0x81 => "mobile-subnetwork-capabilities",
        0x88 => "atn-data-link-capabilities",
        0xCF => "priority",
        0xC5 => "security",
        _ => "unknown",
    }
}

/// ES-IS (ISO 9542) header: type and the advertised network entity
/// titles / addresses (hex NSAPs), plus the trailing option TLVs.
fn parse_esis(b: &[u8]) -> Value {
    let mut out = json!({ "protocol": "ES-IS", "payload_len": b.len() });
    if b.len() < 9 {
        return out;
    }
    let type_code = b[4] & 0x1F;
    out["type"] = json!(match type_code {
        2 => "ESH",
        4 => "ISH",
        6 => "RD",
        _ => "?",
    });
    out["holding_time_s"] = json!(u16::from_be_bytes([b[5], b[6]]));
    // ESH: count + SA(s); ISH: single NET. Both length-prefixed.
    let mut pos = 9usize;
    let mut addrs = Vec::new();
    if type_code == 2 {
        if let Some(&n) = b.get(pos) {
            pos += 1;
            for _ in 0..n {
                let Some(&len) = b.get(pos) else { break };
                let len = len as usize;
                pos += 1;
                if pos + len > b.len() || len > 20 {
                    break;
                }
                addrs.push(
                    b[pos..pos + len].iter().map(|x| format!("{x:02x}")).collect::<String>(),
                );
                pos += len;
            }
        }
    } else if type_code == 4 {
        if let Some(&len) = b.get(pos) {
            let len = len as usize;
            pos += 1;
            if pos + len <= b.len() && len <= 20 {
                addrs.push(
                    b[pos..pos + len].iter().map(|x| format!("{x:02x}")).collect::<String>(),
                );
                pos += len;
            }
        }
    }
    if !addrs.is_empty() {
        out["addresses"] = json!(addrs);
    }
    // Option TLVs follow the addresses on ESH/ISH PDUs (ISO 9542 + the
    // ATN profile: Mobile-Subnetwork-Capabilities 0x81, ATN-Data-Link-
    // Capabilities 0x88, Priority 0xCF, Security 0xC5).
    if matches!(type_code, 2 | 4) {
        let opts = parse_esis_options(&b[pos..]);
        if !opts.is_empty() {
            out["options"] = json!(opts);
        }
    }
    out
}

/// Parse ES-IS option TLVs: each is `type(1) | length(1) | value`.
fn parse_esis_options(b: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= b.len() {
        let t = b[pos];
        let len = b[pos + 1] as usize;
        pos += 2;
        if pos + len > b.len() {
            break;
        }
        out.push(json!({
            "type": esis_option_name(t),
            "type_code": t,
            "value_hex": b[pos..pos + len].iter().map(|x| format!("{x:02x}")).collect::<String>(),
        }));
        pos += len;
    }
    out
}

/// IDRP (ISO 10747) BISPDU header: length, type, sequence numbers.
/// IDRP path-attribute type names (ISO 10747 §7.12).
fn idrp_attr_name(t: u8) -> &'static str {
    match t {
        1 => "route",
        2 => "ext-info",
        3 => "rd-path",
        4 => "next-hop",
        5 => "distribute-list-incl",
        6 => "distribute-list-excl",
        7 => "multi-exit-disc",
        8 => "transit-delay",
        9 => "residual-error",
        10 => "expense",
        11 => "locally-defined-qos",
        12 => "hierarchical-recording",
        13 => "rd-hop-count",
        14 => "security",
        15 => "capacity",
        16 => "priority",
        _ => "unknown",
    }
}

/// IDRP BISPDU type name (ISO/IEC 10747 §7.1).
fn idrp_pdu_type_name(t: u8) -> &'static str {
    match t {
        1 => "OPEN",
        2 => "UPDATE",
        3 => "ERROR",
        4 => "KEEPALIVE",
        5 => "CEASE",
        6 => "RIB-REFRESH",
        _ => "?",
    }
}

/// IDRP ERROR top-level error-code name (ISO/IEC 10747 §7.10).
fn idrp_error_code_name(c: u8) -> &'static str {
    match c {
        1 => "Open PDU error",
        2 => "Update PDU error",
        3 => "Hold timer expired",
        4 => "FSM error",
        5 => "RIB Refresh PDU error",
        _ => "?",
    }
}

/// IDRP ERROR error-subcode name, keyed by the error code.
fn idrp_error_subcode_name(code: u8, sub: u8) -> Option<&'static str> {
    Some(match (code, sub) {
        (1, 1) => "Unsupported version number",
        (1, 2) => "Bad max PDU size",
        (1, 3) => "Bad peer RD",
        (1, 4) => "Unsupported auth code",
        (1, 5) => "Auth failure",
        (1, 6) => "Bad RIB-AttsSet",
        (1, 7) => "RDC Mismatch",
        (2, 1) => "Malformed attribute list",
        (2, 2) => "Unrecognized well-known attribute",
        (2, 3) => "Missing well-known attribute",
        (2, 4) => "Attribute flags error",
        (2, 5) => "Attribute length error",
        (2, 6) => "RD routing loop",
        (2, 7) => "Invalid NEXT_HOP attribute",
        (2, 8) => "Optional attribute error",
        (2, 9) => "Invalid reachability information",
        (2, 10) => "Misconfigured RDCs",
        (2, 11) => "Malformed NLRI",
        (2, 12) => "Duplicated attributes",
        (2, 13) => "Illegal RD path segment",
        (5, 1) => "Invalid opcode",
        (5, 2) => "Unsupported RIB-Atts",
        _ => return None,
    })
}

fn parse_idrp(b: &[u8]) -> Value {
    let mut out = json!({ "protocol": "IDRP", "payload_len": b.len() });
    if b.len() < 4 {
        return out;
    }
    let len = u16::from_be_bytes([b[1], b[2]]);
    out["bispdu_len"] = json!(len);
    let pdu_type = b[3];
    out["type"] = json!(idrp_pdu_type_name(pdu_type));
    if b.len() >= 12 {
        out["sequence"] = json!(u32::from_be_bytes([b[4], b[5], b[6], b[7]]));
        out["ack"] = json!(u32::from_be_bytes([b[8], b[9], b[10], b[11]]));
    }
    if b.len() >= 14 {
        out["credit_offered"] = json!(b[12]);
        out["credit_avail"] = json!(b[13]);
    }
    // BISPDU common header is 30 octets (pid, len(2), type, seq(4),
    // ack(4), credit offered/available, 16-octet validation).
    let body = match b.get(30..) {
        Some(rest) if !rest.is_empty() => rest,
        _ => return out,
    };
    match pdu_type {
        1 => {
            if let Some(v) = parse_idrp_open(body) {
                out["open"] = v;
            }
        }
        2 => {
            if let Some(v) = parse_idrp_update(body) {
                out["update"] = v;
            }
        }
        3 => {
            let code = body[0];
            out["error_code"] = json!(code);
            out["error"] = json!(idrp_error_code_name(code));
            if body.len() >= 2 {
                let sub = body[1];
                out["error_subcode"] = json!(sub);
                if let Some(name) = idrp_error_subcode_name(code, sub) {
                    out["error_subcode_text"] = json!(name);
                }
            }
        }
        _ => {}
    }
    out
}

/// OPEN BISPDU body (ISO/IEC 10747 §7.10): version(1), hold-time(2),
/// max-PDU-size(2), source-RDI (length-prefixed), then RIB-Atts-Set /
/// Confed-IDs / auth-mech (variable, complex) — we decode the fixed
/// leading fields and the source RDI, which are the reliably-framed part.
fn parse_idrp_open(b: &[u8]) -> Option<Value> {
    if b.len() < 6 {
        return None;
    }
    let version = b[0];
    let hold_time = u16::from_be_bytes([b[1], b[2]]);
    let max_pdu_size = u16::from_be_bytes([b[3], b[4]]);
    let rdi_len = b[5] as usize;
    let mut out = json!({
        "version": version,
        "hold_time_s": hold_time,
        "max_pdu_size": max_pdu_size,
    });
    if 6 + rdi_len <= b.len() {
        out["src_rdi"] = json!(
            b[6..6 + rdi_len].iter().map(|x| format!("{x:02x}")).collect::<String>()
        );
        // The RIB-Atts-Set, Confed-IDs and auth-mech/auth-data fields
        // follow the source RDI but are variable-length and complex; they
        // remain in the preserved raw hex rather than being guessed at.
    }
    Some(out)
}

/// UPDATE BISPDU body: withdrawn route IDs, path attributes
/// (flag, type, u16 length, value), then NLRI entries.
fn parse_idrp_update(b: &[u8]) -> Option<Value> {
    let mut out = json!({});
    let mut pos = 0usize;
    let num_withdrawn = u16::from_be_bytes([*b.first()?, *b.get(1)?]) as usize;
    pos += 2;
    if num_withdrawn > 0 {
        let mut withdrawn = Vec::new();
        for _ in 0..num_withdrawn {
            let id = b.get(pos..pos + 4)?;
            withdrawn.push(format!(
                "{:08x}",
                u32::from_be_bytes([id[0], id[1], id[2], id[3]])
            ));
            pos += 4;
        }
        out["withdrawn_routes"] = json!(withdrawn);
    }
    let mut attrib_len = u16::from_be_bytes([*b.get(pos)?, *b.get(pos + 1)?]) as usize;
    pos += 2;
    if attrib_len > 0 {
        let mut attrs = Vec::new();
        while attrib_len > 4 {
            // flag octet skipped; type, then u16 value length
            let t = *b.get(pos + 1)?;
            let alen = u16::from_be_bytes([*b.get(pos + 2)?, *b.get(pos + 3)?]) as usize;
            pos += 4;
            attrib_len = attrib_len.saturating_sub(4);
            let val = b.get(pos..pos + alen)?;
            let mut a = json!({
                "type": idrp_attr_name(t),
                "type_code": t,
            });
            match t {
                7..=10 | 13 | 16 if alen == 1 => a["value"] = json!(val[0]),
                _ if !val.is_empty() => {
                    a["value_hex"] =
                        json!(val.iter().map(|x| format!("{x:02x}")).collect::<String>());
                }
                _ => {}
            }
            attrs.push(a);
            pos += alen;
            attrib_len = attrib_len.saturating_sub(alen);
        }
        out["path_attributes"] = json!(attrs);
    }
    // NLRI: proto_type(1), proto id length(1) + id, then address info
    // length (u16) + prefixes (length-prefixed in half-octets).
    let mut nlri = Vec::new();
    while pos < b.len() {
        let rest = &b[pos..];
        if rest.len() < 7 {
            break;
        }
        let proto_type = rest[0];
        let proto_len = rest[1] as usize;
        if 2 + proto_len + 2 > rest.len() {
            break;
        }
        let proto_id = &rest[2..2 + proto_len];
        let addr_len = u16::from_be_bytes([rest[2 + proto_len], rest[3 + proto_len]]) as usize;
        let addr_start = 4 + proto_len;
        if addr_start + addr_len > rest.len() {
            break;
        }
        let addrs = &rest[addr_start..addr_start + addr_len];
        // CLNP NLRI: proto_type 1 with protocol id 0x81.
        let is_clnp = proto_type == 1 && proto_id == [0x81];
        let mut prefixes = Vec::new();
        let mut apos = 0usize;
        while apos < addrs.len() {
            let nbits = addrs[apos] as usize; // prefix length in semi-octets·4
            let nbytes = nbits.div_ceil(8);
            if apos + 1 + nbytes > addrs.len() {
                break;
            }
            prefixes.push(format!(
                "{}/{nbits}",
                addrs[apos + 1..apos + 1 + nbytes]
                    .iter()
                    .map(|x| format!("{x:02x}"))
                    .collect::<String>()
            ));
            apos += 1 + nbytes;
        }
        nlri.push(json!({
            "proto_type": proto_type,
            "clnp": is_clnp,
            "prefixes": prefixes,
        }));
        pos += addr_start + addr_len;
    }
    if !nlri.is_empty() {
        out["nlri"] = json!(nlri);
    }
    Some(out)
}

/// X.25 facilities: TLVs with class-coded lengths (bits 7-8 of the
/// code: 1/2/3 parameter bytes, or variable with a length byte).
/// Codes are reported numerically — naming is deferred until the ITU
/// table can be verified.
fn parse_facilities(b: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < b.len() {
        let code = b[pos];
        pos += 1;
        let plen = match code >> 6 {
            0 => 1,
            1 => 2,
            2 => 3,
            _ => match b.get(pos) {
                Some(&l) => {
                    pos += 1;
                    l as usize
                }
                None => break,
            },
        };
        if pos + plen > b.len() {
            break;
        }
        out.push(json!({
            "code": format!("{code:#04x}"),
            "params": b[pos..pos + plen].iter().map(|x| format!("{x:02x}")).collect::<String>(),
        }));
        pos += plen;
    }
    out
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
            .or_else(|| crate::atn_cpdlc::parse_cm_ground(user))
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
    fn esis_ish_parses_net() {
        // NLPID 0x82, hdr_len, version, lifetime, type=ISH(4),
        // holding time 600 s, checksum, then NET length + NET.
        let mut b = vec![0x82, 0x0E, 0x01, 0x00, 0x04, 0x02, 0x58, 0x00, 0x00];
        b.push(3); // NET length
        b.extend([0x47, 0x00, 0x27]);
        let v = parse_network(&b).unwrap();
        assert_eq!(v["protocol"], "ES-IS");
        assert_eq!(v["type"], "ISH");
        assert_eq!(v["holding_time_s"], 600);
        assert_eq!(v["addresses"][0], "470027");
    }

    #[test]
    fn idrp_update_attributes_and_nlri() {
        // 30-octet common header: pid, len, type=UPDATE(2), seq, ack,
        // credits, 16-octet validation; then the UPDATE body.
        let mut b = vec![0x83, 0x00, 0x00, 0x02];
        b.extend(7u32.to_be_bytes()); // seq
        b.extend(3u32.to_be_bytes()); // ack
        b.extend([0, 0]); // credits
        b.extend([0u8; 16]); // validation
        // body: 1 withdrawn route, attrs: multi-exit-disc(7)=5,
        // rd-hop-count(13)=2; NLRI: CLNP, prefix 47.0027 (24 bits)
        b.extend([0x00, 0x01]); // num withdrawn
        b.extend(0xDEADBEEFu32.to_be_bytes());
        let attrs: &[u8] = &[
            0x00, 7, 0x00, 0x01, 5, // flag, type 7, len 1, val 5
            0x00, 13, 0x00, 0x01, 2, // type 13 = rd-hop-count
        ];
        b.extend((attrs.len() as u16).to_be_bytes());
        b.extend(attrs);
        // NLRI: proto_type 1, proto_len 1, id 0x81, addr_len, prefixes
        let prefixes: &[u8] = &[24, 0x47, 0x00, 0x27];
        b.extend([1, 1, 0x81]);
        b.extend((prefixes.len() as u16).to_be_bytes());
        b.extend(prefixes);
        let total = b.len() as u16;
        b[1..3].copy_from_slice(&total.to_be_bytes());

        let v = parse_network(&b).unwrap();
        assert_eq!(v["type"], "UPDATE");
        assert_eq!(v["sequence"], 7);
        assert_eq!(v["ack"], 3);
        let u = &v["update"];
        assert_eq!(u["withdrawn_routes"][0], "deadbeef");
        assert_eq!(u["path_attributes"][0]["type"], "multi-exit-disc");
        assert_eq!(u["path_attributes"][0]["value"], 5);
        assert_eq!(u["path_attributes"][1]["type"], "rd-hop-count");
        assert_eq!(u["path_attributes"][1]["value"], 2);
        assert_eq!(u["nlri"][0]["clnp"], true);
        assert_eq!(u["nlri"][0]["prefixes"][0], "470027/24");
    }

    #[test]
    fn idrp_keepalive_parses() {
        // NLPID 0x83, BISPDU len, type=KEEPALIVE(4), sequence.
        let b = [0x83, 0x00, 0x0C, 0x04, 0x00, 0x00, 0x00, 0x2A, 0, 0, 0, 0];
        let v = parse_network(&b).unwrap();
        assert_eq!(v["protocol"], "IDRP");
        assert_eq!(v["type"], "KEEPALIVE");
        assert_eq!(v["sequence"], 42);
        assert_eq!(v["bispdu_len"], 12);
    }

    /// Build an IDRP BISPDU: 30-octet common header (with the given type
    /// and seq/ack/credit fields zeroed unless noted) followed by `body`.
    fn idrp_pdu(pdu_type: u8, body: &[u8]) -> Vec<u8> {
        let mut b = vec![0x83, 0x00, 0x00, pdu_type];
        b.extend([0u8; 8]); // seq, ack
        b.extend([0u8; 2]); // credit offered/avail
        b.extend([0u8; 16]); // validation
        b.extend_from_slice(body);
        let total = b.len() as u16;
        b[1..3].copy_from_slice(&total.to_be_bytes());
        b
    }

    #[test]
    fn idrp_rib_refresh_is_sixth_type() {
        // PDU type 6 must decode as RIB-REFRESH (ISO/IEC 10747 §7.1).
        let b = idrp_pdu(6, &[]);
        let v = parse_network(&b).unwrap();
        assert_eq!(v["type"], "RIB-REFRESH");
    }

    #[test]
    fn idrp_open_body_fields_decode() {
        // OPEN body: version=1, hold-time=90 s, max-PDU=1024, src-RDI len 3.
        let body: &[u8] = &[
            0x01, 0x00, 0x5A, 0x04, 0x00, 0x03, 0x47, 0x00, 0x27,
        ];
        let v = parse_network(&idrp_pdu(1, body)).unwrap();
        assert_eq!(v["type"], "OPEN");
        let o = &v["open"];
        assert_eq!(o["version"], 1);
        assert_eq!(o["hold_time_s"], 90);
        assert_eq!(o["max_pdu_size"], 1024);
        assert_eq!(o["src_rdi"], "470027");
    }

    #[test]
    fn idrp_error_code_and_subcode_text() {
        // ERROR: code 1 (OPEN PDU error), subcode 2 (Bad max PDU size).
        let v = parse_network(&idrp_pdu(3, &[0x01, 0x02])).unwrap();
        assert_eq!(v["type"], "ERROR");
        assert_eq!(v["error_code"], 1);
        assert_eq!(v["error"], "Open PDU error");
        assert_eq!(v["error_subcode"], 2);
        assert_eq!(v["error_subcode_text"], "Bad max PDU size");
        // UPDATE PDU error (2) / RD routing loop (6).
        let v = parse_network(&idrp_pdu(3, &[0x02, 0x06])).unwrap();
        assert_eq!(v["error"], "Update PDU error");
        assert_eq!(v["error_subcode_text"], "RD routing loop");
        // RIB Refresh PDU error (5) / Unsupported RIB-Atts (2).
        let v = parse_network(&idrp_pdu(3, &[0x05, 0x02])).unwrap();
        assert_eq!(v["error"], "RIB Refresh PDU error");
        assert_eq!(v["error_subcode_text"], "Unsupported RIB-Atts");
    }

    #[test]
    fn esis_options_decode() {
        // ISH (type 4), holding time 600 s, NET, then two option TLVs:
        // Priority (0xCF) len 1, ATN-Data-Link-Capabilities (0x88) len 2.
        let mut b = vec![0x82, 0x00, 0x01, 0x00, 0x04, 0x02, 0x58, 0x00, 0x00];
        b.push(3); // NET length
        b.extend([0x47, 0x00, 0x27]);
        b.extend([0xCF, 0x01, 0x06]); // Priority = 6
        b.extend([0x88, 0x02, 0xAB, 0xCD]); // ATN data-link caps
        let v = parse_network(&b).unwrap();
        assert_eq!(v["type"], "ISH");
        assert_eq!(v["addresses"][0], "470027");
        let opts = v["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["type"], "priority");
        assert_eq!(opts[0]["type_code"], 0xCF);
        assert_eq!(opts[0]["value_hex"], "06");
        assert_eq!(opts[1]["type"], "atn-data-link-capabilities");
        assert_eq!(opts[1]["value_hex"], "abcd");
    }

    #[test]
    fn esis_esh_options_after_address_list() {
        // ESH (type 2): count=1, one SA, then Mobile-Subnetwork-Capabilities
        // (0x81) and Security (0xC5) options.
        let mut b = vec![0x82, 0x00, 0x01, 0x00, 0x02, 0x02, 0x58, 0x00, 0x00];
        b.push(1); // address count
        b.push(3); // SA length
        b.extend([0x47, 0x00, 0x27]);
        b.extend([0x81, 0x01, 0x0F]); // mobile subnetwork caps
        b.extend([0xC5, 0x02, 0x00, 0x01]); // security
        let v = parse_network(&b).unwrap();
        assert_eq!(v["type"], "ESH");
        assert_eq!(v["addresses"][0], "470027");
        let opts = v["options"].as_array().unwrap();
        assert_eq!(opts[0]["type"], "mobile-subnetwork-capabilities");
        assert_eq!(opts[1]["type"], "security");
        assert_eq!(opts[1]["value_hex"], "0001");
    }

    #[test]
    fn facilities_class_lengths() {
        // class 0 (1 param byte): 0x01 0xAA; class 1 (2): 0x42 0x01 0x02;
        // class 3 (length-prefixed): 0xC9 0x02 0xDE 0xAD.
        let f = parse_facilities(&[0x01, 0xAA, 0x42, 0x01, 0x02, 0xC9, 0x02, 0xDE, 0xAD]);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0]["code"], "0x01");
        assert_eq!(f[0]["params"], "aa");
        assert_eq!(f[1]["params"], "0102");
        assert_eq!(f[2]["code"], "0xc9");
        assert_eq!(f[2]["params"], "dead");
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
