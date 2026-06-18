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
    /// Human-readable clearing/reset/restart cause.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause_text: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<u8>,
    /// Human-readable diagnostic-code name (X.25 Annex E / ICAO Doc 9705).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_text: Option<&'static str>,
    /// Negotiated facilities on call packets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facilities: Vec<Value>,
    /// SNDCF compression negotiation on call packets (ICAO Doc 9705 §5.7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sndcf: Option<Value>,
    /// Payload (data packets: user data; call packets: CUD).
    #[serde(skip)]
    pub payload: Vec<u8>,
}

/// X.25 clearing-cause name (ITU-T X.25 Table 5-7).
fn x25_clear_cause(c: u8) -> Option<&'static str> {
    Some(match c {
        0x00 => "DTE originated",
        0x01 => "Number busy",
        0x03 => "Invalid facility request",
        0x05 => "Network congestion",
        0x09 => "Remote procedure error",
        0x0D => "Not obtainable",
        0x13 => "Local procedure error",
        0x15 => "ROA out of order",
        0x19 => "Reverse charging acceptance not subscribed",
        0x21 => "Incompatible destination",
        0x29 => "Fast select acceptance not subscribed",
        0x39 => "Ship absent",
        _ => return None,
    })
}

/// X.25 reset-cause name (ITU-T X.25 Table 5-7).
fn x25_reset_cause(c: u8) -> Option<&'static str> {
    Some(match c {
        0x00 => "DTE originated",
        0x01 => "Out of order",
        0x03 => "Remote procedure error",
        0x05 => "Local procedure error",
        0x07 => "Network congestion",
        0x09 => "Remote DTE operational",
        0x0F => "Network operational",
        0x11 => "Incompatible destination",
        0x1D => "Network out of order",
        _ => return None,
    })
}

/// X.25 restart-cause name (ITU-T X.25 Table 5-7).
fn x25_restart_cause(c: u8) -> Option<&'static str> {
    Some(match c {
        0x01 => "Local procedure error",
        0x03 => "Network congestion",
        0x07 => "Network operational",
        _ => return None,
    })
}

/// X.25 diagnostic-code name (X.25 Annex E + ISO 8208 + ICAO Doc 9705
/// Table 5.7-3 / Doc 9880 extensions).
fn x25_diagnostic(d: u8) -> Option<&'static str> {
    Some(match d {
        0x00 => "Cleared by system management",
        0x01 => "Invalid P(S)",
        0x02 => "Invalid P(R)",
        0x10 => "Packet type invalid",
        0x11 => "Packet type invalid for state r1",
        0x12 => "Packet type invalid for state r2",
        0x13 => "Packet type invalid for state r3",
        0x14 => "Packet type invalid for state p1",
        0x15 => "Packet type invalid for state p2",
        0x16 => "Packet type invalid for state p3",
        0x17 => "Packet type invalid for state p4",
        0x18 => "Packet type invalid for state p5",
        0x19 => "Packet type invalid for state p6",
        0x1A => "Packet type invalid for state p7",
        0x1B => "Packet type invalid for state d1",
        0x1C => "Packet type invalid for state d2",
        0x1D => "Packet type invalid for state d3",
        0x20 => "Packet not allowed",
        0x21 => "Unidentifiable packet",
        0x22 => "Call on one-way logical channel",
        0x23 => "Invalid packet type on a PVC",
        0x24 => "Packet on unassigned logical channel",
        0x25 => "Reject not subscribed to",
        0x26 => "Packet too short",
        0x27 => "Packet too long",
        0x28 => "Invalid general format identifier",
        0x29 => "Restart packet with non-zero reserved bits",
        0x2A => "Packet type not compatible with facility",
        0x2B => "Unauthorized interrupt confirmation",
        0x2C => "Unauthorized interrupt",
        0x2D => "Unauthorized reject",
        0x2E => "TOA/NPI address subscription facility not subscribed to",
        0x30 => "Time expired",
        0x31 => "Time expired for incoming call",
        0x32 => "Time expired for clear indication",
        0x33 => "Time expired for reset indication",
        0x34 => "Time expired for restart indication",
        0x35 => "Time expired for call deflection",
        0x40 => "Call setup or call clearing problem",
        0x41 => "Facility code not allowed",
        0x42 => "Facility parameter not allowed",
        0x43 => "Invalid called DTE address",
        0x44 => "Invalid calling DTE address",
        0x45 => "Invalid facility length",
        0x46 => "Incoming call barred",
        0x47 => "No logical channel available",
        0x48 => "Call collision",
        0x49 => "Duplicate facility requested",
        0x4A => "Non-zero address length",
        0x4B => "Non-zero facility length",
        0x4C => "Facility not provided when expected",
        0x4D => "Invalid ITU-T specified DTE facility",
        0x4E => "Max number of call redirections or deflections exceeded",
        0x50 => "Miscellaneous",
        0x51 => "Improper cause code from DTE",
        0x52 => "Not aligned octet",
        0x53 => "Inconsistent Q-bit setting",
        0x54 => "NUI problem",
        0x55 => "ICRD problem",
        0x70 => "International problem",
        0x71 => "Remote network problem",
        0x72 => "International protocol problem",
        0x73 => "International link out of order",
        0x74 => "International link busy",
        0x75 => "Transit network facility problem",
        0x76 => "Remote network facility problem",
        0x77 => "International routing problem",
        0x78 => "Temporary routing problem",
        0x79 => "Unknown called DNIC",
        0x7A => "Maintenance action",
        // ICAO Doc 9705 Table 5.7-3
        0x80 => "Version number not supported",
        0x81 => "Invalid length field",
        0x82 => "Call collision resolution",
        0x83 => "Proposed directory size too large",
        0x84 => "LREF cancellation not supported",
        0x85 => "Received DTE refused, received NET refused or invalid NET selector",
        0x86 => "Invalid SNCR field",
        0x87 => "ACA compression not supported",
        0x88 => "LREF compression not supported",
        0x8F => "Deflate compression not supported",
        0x90 => "Idle timer expired",
        0x91 => "Need to reuse the circuit",
        0x92 => "System local error",
        0x93 => "Invalid SEL field value in received NET",
        // ISO 8208
        0xE1 => "OSI network disconnect (transient)",
        0xE2 => "OSI network disconnect (permanent)",
        0xE3 => "OSI network reject - reason unspecified (transient)",
        0xE4 => "OSI network reject - reason unspecified (permanent)",
        0xE5 => "OSI network reject - QoS not available (transient)",
        0xE6 => "OSI network reject - QoS not available (permanent)",
        0xE7 => "OSI network reject - NSAP unreachable (transient)",
        0xE8 => "OSI network reject - NSAP unreachable (permanent)",
        0xE9 => "OSI network reset - no reason given",
        0xEA => "OSI network reset - congestion",
        0xEB => "OSI network reject - NSAP address unknown (permanent)",
        0xF0 => "System lack of resources",
        0xF1 => "Higher level initiated disconnect (normal)",
        0xF2 => "Incompatible information in user data",
        0xF3 => "Higher level initiated disconnect - incompatible data",
        0xF4 => "Higher level initiated reject - no reason given (transient)",
        0xF5 => "Higher level initiated reject - no reason given (permanent)",
        0xF6 => "Higher level initiated reject - QoS not available (transient)",
        0xF7 => "Higher level initiated reject - QoS not available (permanent)",
        0xF8 => "Higher level initiated reject - incompatible data",
        0xF9 => "Unrecognized protocol ID",
        0xFA => "Higher level initiated reset - user resync",
        _ => return None,
    })
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
        cause_text: None,
        diagnostic: None,
        diagnostic_text: None,
        facilities: Vec::new(),
        sndcf: None,
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
            let is_request = t == 0x0B;
            pkt.kind = if is_request { "call-request" } else { "call-accepted" };
            // Address block: BCD digit counts (called, calling), then the
            // digits, facilities length + facilities, then the SNDCF field
            // (ATN profile, ICAO Doc 9705 §5.7) and finally any CUD.
            if b.len() > 3 {
                let called_len = (b[3] & 0x0F) as usize;
                let calling_len = (b[3] >> 4) as usize;
                let addr_octets = (called_len + calling_len).div_ceil(2);
                let fac_pos = 4 + addr_octets;
                if b.len() > fac_pos {
                    let fac_len = b[fac_pos] as usize;
                    let mut pos = fac_pos + 1 + fac_len;
                    if fac_len > 0 && pos <= b.len() {
                        pkt.facilities =
                            parse_facilities(&b[fac_pos + 1..fac_pos + 1 + fac_len]);
                    }
                    // SNDCF: on a Call-Request the field is identifier 0xC1,
                    // length, then a value whose 4th octet (after version 1)
                    // is the compression-support bitfield; on a Call-Accept
                    // a single compression octet follows the facilities.
                    if is_request {
                        if let Some((sndcf, consumed)) = parse_sndcf_field(&b[pos.min(b.len())..]) {
                            pkt.sndcf = Some(sndcf);
                            pos += consumed;
                        }
                    } else if pos < b.len() {
                        pkt.sndcf = Some(decode_x25_compression(b[pos]));
                        pos += 1;
                    }
                    if pos < b.len() {
                        pkt.payload = b[pos..].to_vec();
                    }
                }
            }
        }
        0x13 | 0x1B | 0xFB => {
            // CLEAR / RESET / RESTART request: cause octet then diagnostic.
            let (kind, namer): (&str, fn(u8) -> Option<&'static str>) = match t {
                0x13 => ("clear-request", x25_clear_cause),
                0x1B => ("reset-request", x25_reset_cause),
                _ => ("restart-request", x25_restart_cause),
            };
            pkt.kind = kind;
            if let Some(&raw) = b.get(3) {
                // X.25 Table 5-7: bit 8 set means the lower bits are the
                // remote DTE's cause; normalise to 0 for the dictionary.
                let cause = if raw & 0x80 != 0 { 0 } else { raw };
                pkt.cause = Some(cause);
                pkt.cause_text = namer(cause);
            }
            if let Some(&d) = b.get(4) {
                pkt.diagnostic = Some(d);
                pkt.diagnostic_text = x25_diagnostic(d);
            }
        }
        0x17 | 0x1F | 0xFF => {
            // CLEAR / RESET / RESTART confirmation: no cause/diagnostic.
            pkt.kind = match t {
                0x17 => "clear-confirmation",
                0x1F => "reset-confirmation",
                _ => "restart-confirmation",
            };
        }
        0xF1 => {
            pkt.kind = "diagnostic";
            if let Some(&d) = b.get(3) {
                pkt.diagnostic = Some(d);
                pkt.diagnostic_text = x25_diagnostic(d);
            }
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

/// The segmentation fields of a CLNP derived PDU (ISO/IEC 8473 §6.7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClnpSegment {
    /// Data-unit identifier (groups the derived PDUs of one initial PDU).
    pub pdu_id: u16,
    /// Offset of this segment's data within the reassembled data unit.
    pub offset: u16,
    /// Total length (header + complete data) of the initial PDU.
    pub total_len: u16,
    /// More-segments flag: another segment follows.
    pub more: bool,
    /// Header length (octets) — the data part begins here.
    pub hdr_len: usize,
}

/// Extract the segmentation part from a raw CLNP PDU (ISO/IEC 8473 §6.7).
/// Returns `None` when the PDU is not a segmentation-permitted CLNP PDU or
/// is too short to carry the 6-octet segmentation part. The data part of
/// the PDU is `b[hdr_len..]`.
pub fn clnp_segment(b: &[u8]) -> Option<ClnpSegment> {
    if b.len() < 9 || b[0] != 0x81 {
        return None;
    }
    let hdr_len = b[1] as usize;
    let flags = b[4];
    // SP (segmentation permitted) must be set for the segmentation part to
    // be present.
    if flags & 0x80 == 0 || hdr_len < 9 || hdr_len > b.len() {
        return None;
    }
    let more = flags & 0x40 != 0;
    // Walk past the two NSAP address fields to reach the segmentation part.
    let mut pos = 9usize;
    for _ in 0..2 {
        let len = *b.get(pos)? as usize;
        pos += 1 + len;
    }
    if pos + 6 > b.len() {
        return None;
    }
    Some(ClnpSegment {
        pdu_id: u16::from_be_bytes([b[pos], b[pos + 1]]),
        offset: u16::from_be_bytes([b[pos + 2], b[pos + 3]]),
        total_len: u16::from_be_bytes([b[pos + 4], b[pos + 5]]),
        more,
        hdr_len,
    })
}

/// Reassembles segmented CLNP data units (ISO/IEC 8473 §6.7). Derived PDUs
/// of one initial PDU share a data-unit identifier; each carries a fragment
/// of the data part at `segment offset`, and the initial PDU's `total
/// length` (header + complete data) bounds the reassembled data. The first
/// segment's header (initial-PDU header, but with offset 0) is preserved so
/// a complete CLNP PDU can be reconstructed for downstream (COTP) parsing.
pub struct ClnpReassembler {
    /// Keyed by (src_nsap, dst_nsap, pdu_id): the assembled data buffer, the
    /// expected data length (total_len − hdr_len), the first segment's full
    /// header bytes, and the last-seen timestamp.
    pending: HashMap<(Vec<u8>, Vec<u8>, u16), ClnpPending>,
}

struct ClnpPending {
    data: Vec<u8>,
    /// Bytes of `data` actually filled (tracked because segments can arrive
    /// out of order, leaving holes).
    filled: usize,
    data_len: Option<usize>,
    header: Option<Vec<u8>>,
    last: f64,
}

const CLNP_TIMEOUT_SECS: f64 = 60.0;

impl ClnpReassembler {
    pub fn new() -> Self {
        Self { pending: HashMap::new() }
    }

    /// Push one raw CLNP PDU. Returns the reassembled, de-segmented CLNP PDU
    /// (a complete single PDU: first-segment header with SP cleared of the
    /// more-segments flag, followed by the full data part) when the data
    /// unit completes. Unsegmented PDUs pass straight through.
    pub fn push(&mut self, b: &[u8], now: f64) -> Option<Vec<u8>> {
        let seg = match clnp_segment(b) {
            Some(s) => s,
            // Not a segmentation-permitted PDU: nothing to reassemble.
            None => return Some(b.to_vec()),
        };
        // A lone, complete PDU (offset 0, no more segments) needs no work.
        if seg.offset == 0 && !seg.more {
            return Some(b.to_vec());
        }
        self.pending.retain(|_, p| now - p.last < CLNP_TIMEOUT_SECS);

        // Address fields key the data unit alongside the data-unit id.
        let (dst, src) = clnp_addresses(b)?;
        let key = (src, dst, seg.pdu_id);
        let data = &b[seg.hdr_len..];
        let data_len = (seg.total_len as usize).checked_sub(seg.hdr_len)?;
        let offset = seg.offset as usize;

        let entry = self.pending.entry(key.clone()).or_insert_with(|| ClnpPending {
            data: Vec::new(),
            filled: 0,
            data_len: None,
            header: None,
            last: now,
        });
        entry.last = now;
        if entry.data_len.is_none() {
            entry.data_len = Some(data_len);
        }
        let total = entry.data_len.unwrap_or(data_len);
        if total > 0 && entry.data.len() < total {
            entry.data.resize(total, 0);
        }
        // Place this fragment's data at its offset.
        if offset + data.len() <= entry.data.len() {
            entry.data[offset..offset + data.len()].copy_from_slice(data);
            entry.filled += data.len();
        }
        // Capture the first segment's header (offset 0) to rebuild the PDU.
        if offset == 0 {
            entry.header = Some(b[..seg.hdr_len].to_vec());
        }

        // Complete when every byte of the data unit is filled.
        if entry.filled >= total && entry.header.is_some() {
            let ClnpPending { data, header, .. } = self.pending.remove(&key)?;
            let mut hdr = header?;
            // Clear the more-segments flag in the reconstructed PDU so it is
            // treated as a complete data unit downstream.
            if hdr.len() > 4 {
                hdr[4] &= !0x40;
            }
            let mut full = hdr;
            full.extend_from_slice(&data);
            return Some(full);
        }
        None
    }
}

impl Default for ClnpReassembler {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the (dst, src) NSAP addresses of a raw CLNP PDU as octet vectors.
fn clnp_addresses(b: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut pos = 9usize;
    let read = |pos: &mut usize| -> Option<Vec<u8>> {
        let len = *b.get(*pos)? as usize;
        *pos += 1;
        if *pos + len > b.len() || len > 20 {
            return None;
        }
        let v = b[*pos..*pos + len].to_vec();
        *pos += len;
        Some(v)
    };
    let dst = read(&mut pos)?;
    let src = read(&mut pos)?;
    Some((dst, src))
}

/// Return the COTP TPDU (the CLNP data part) of a complete, unsegmented CLNP
/// DT PDU, or `None` when `b` is not such a PDU. Used to feed the COTP TSDU
/// reassembler (ISO/IEC 8073 §6.6) before the upper-layer (ULCS/CPDLC) decode.
pub fn clnp_cotp_tpdu(b: &[u8]) -> Option<&[u8]> {
    if b.len() < 9 || b[0] != 0x81 {
        return None;
    }
    let hdr_len = b[1] as usize;
    if hdr_len < 9 || hdr_len > b.len() {
        return None;
    }
    let flags = b[4];
    // Only DT PDUs carry COTP; a segmented PDU (SP set with MS or non-zero
    // offset) is not a complete data unit here.
    if flags & 0x1F != 0x1C {
        return None;
    }
    if flags & 0x80 != 0 {
        // SP set: check the segmentation part for more-segments / offset.
        if let Some(seg) = clnp_segment(b) {
            if seg.more || seg.offset != 0 {
                return None;
            }
        }
    }
    b.get(hdr_len..)
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

/// Decode an X.25 SNDCF compression-support octet into the ATN algorithm
/// set (ICAO Doc 9705 §5.7): ACA 0x40, DEFLATE 0x20, LREF 0x02,
/// LREF-CAN 0x01, plus the M/I (maintenance/initialisation) bit 0x10.
fn decode_x25_compression(byte: u8) -> Value {
    let mut algos = Vec::new();
    if byte & 0x40 != 0 {
        algos.push("ACA");
    }
    if byte & 0x20 != 0 {
        algos.push("DEFLATE");
    }
    if byte & 0x02 != 0 {
        algos.push("LREF");
    }
    if byte & 0x01 != 0 {
        algos.push("LREF-CAN");
    }
    json!({
        "compression_options": byte,
        "compression_algos": algos,
        "maintenance": byte & 0x10 != 0,
    })
}

/// Parse the X.25 Call-Request SNDCF field (ATN profile, ICAO Doc 9705
/// §5.7 / ISO 8208): identifier 0xC1, length octet, then the value whose
/// first octet is the SNDCF version (must be 1) and whose 4th octet is the
/// compression-support bitfield. Returns the decoded value and the number
/// of octets consumed (`2 + length`). Returns None when the identifier,
/// version or length do not match (so the caller leaves the bytes as CUD).
fn parse_sndcf_field(b: &[u8]) -> Option<(Value, usize)> {
    if b.len() < 2 || b[0] != 0xC1 {
        return None;
    }
    let len = b[1] as usize;
    // ATN SNDCF value is at least 4 octets (version + 3) and version == 1.
    if len < 4 || 2 + len > b.len() {
        return None;
    }
    let val = &b[2..2 + len];
    if val[0] != 0x01 {
        return None;
    }
    let mut out = decode_x25_compression(val[3]);
    out["version"] = json!(val[0]);
    Some((out, 2 + len))
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

/// CLNP option-part parameter name (ISO/IEC 8473 / X.233 §7.5).
fn clnp_option_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0x05 => "lref",
        0xC1 => "discard_reason",
        0xC3 => "qos_maintenance",
        0xC4 => "prefix_scope_control",
        0xC5 => "security",
        0xC6 => "radius_scope_control",
        0xC8 => "source_routing",
        0xCB => "record_route",
        0xCC => "padding",
        0xCD => "priority",
        _ => return None,
    })
}

/// ATN traffic-type / ATSC-class names (ICAO Doc 9705 §5.6, Tables 5.6-x).
fn atn_traffic_type_name(bit: u8) -> Option<&'static str> {
    Some(match bit {
        1 => "ATS",
        2 => "AOC",
        4 => "ATN Administrative",
        8 => "General Comms",
        16 => "ATN System Mgmt",
        _ => return None,
    })
}

fn atsc_class_name(bit: u8) -> Option<&'static str> {
    Some(match bit {
        1 => "A",
        2 => "B",
        4 => "C",
        8 => "D",
        16 => "E",
        32 => "F",
        64 => "G",
        128 => "H",
        _ => return None,
    })
}

/// Expand a single-octet bitfield into the set of named bits that are set,
/// using the provided bit→name lookup.
fn bitfield_names(byte: u8, namer: fn(u8) -> Option<&'static str>) -> Vec<&'static str> {
    let mut out = Vec::new();
    for i in 0..8 {
        let bit = 1u8 << i;
        if byte & bit != 0 {
            if let Some(name) = namer(bit) {
                out.push(name);
            }
        }
    }
    out
}

fn atn_subnet_name(s: u8) -> Option<&'static str> {
    Some(match s {
        1 => "Mode S",
        2 => "VDL",
        3 => "AMSS",
        4 => "Gatelink",
        5 => "HF",
        _ => return None,
    })
}

fn atn_security_class_name(c: u8) -> Option<&'static str> {
    Some(match c {
        1 => "unclassified",
        2 => "restricted",
        3 => "confidential",
        4 => "secret",
        5 => "top secret",
        _ => return None,
    })
}

/// Decode one ATN security tag (ICAO Doc 9705 §5.6). `name` is the tag-set
/// name octet, `v` the tag-set value.
fn parse_atn_security_tag(name: u8, v: &[u8]) -> Value {
    let mut out = json!({ "tag_set": name });
    match name {
        // Security classification: single octet class id.
        0x03 if v.len() == 1 => {
            out["kind"] = json!("security_classification");
            out["class_id"] = json!(v[0]);
            if let Some(n) = atn_security_class_name(v[0]) {
                out["class_name"] = json!(n);
            }
        }
        // Subnetwork type: subnet id + permitted-traffic-types bitfield.
        0x05 if v.len() == 2 => {
            out["kind"] = json!("subnet_type");
            out["subnet_id"] = json!(v[0]);
            if let Some(n) = atn_subnet_name(v[0]) {
                out["subnet_name"] = json!(n);
            }
            out["permitted_traffic_types"] =
                json!(bitfield_names(v[1], atn_traffic_type_name));
        }
        // Supported ATSC classes: typecode 6 or 7, single bitfield octet.
        0x06 | 0x07 if v.len() == 1 => {
            out["kind"] = json!("supported_atsc_classes");
            out["classes"] = json!(bitfield_names(v[0], atsc_class_name));
        }
        // Traffic type / routing policy.
        0x0F if !v.is_empty() => {
            out["kind"] = json!("traffic_type");
            // High 3 bits select type/category, low 5 are the route policy.
            let (type_name, category) = match v[0] >> 5 {
                0 => ("ATN operational", "ATSC"),
                1 if v[0] == 0x30 => ("ATN administrative", "none"),
                1 => ("ATN operational", "AOC"),
                3 => ("ATN system management", "none"),
                _ => ("unknown", "unknown"),
            };
            out["traffic_type"] = json!(type_name);
            out["category"] = json!(category);
            out["route_policy"] = json!(v[0] & 0x1F);
        }
        _ => {
            out["value_hex"] = json!(v.iter().map(|x| format!("{x:02x}")).collect::<String>());
        }
    }
    out
}

/// Parse the ATN security label (ICAO Doc 9705 §5.6): security-registration
/// ID (length-prefixed octet string), then optional security information —
/// a sequence of tag sets, each `name-len(1)=1 | name(1) | set-len(1) | tags`.
/// `b` is the security label (after the leading 0xC0 security-format octet).
fn parse_atn_security_label(b: &[u8]) -> Value {
    let mut out = json!({});
    let mut pos = 0usize;
    let Some(&srid_len) = b.first() else {
        return out;
    };
    let srid_len = srid_len as usize;
    pos += 1;
    if pos + srid_len > b.len() {
        return out;
    }
    out["reg_id"] = json!(b[pos..pos + srid_len].iter().map(|x| format!("{x:02x}")).collect::<String>());
    pos += srid_len;
    // Security info part (optional): length octet, then tag sets.
    let Some(&sinfo_len) = b.get(pos) else {
        return out;
    };
    let sinfo_len = sinfo_len as usize;
    pos += 1;
    let end = (pos + sinfo_len).min(b.len());
    let mut tags = Vec::new();
    while pos + 3 <= end {
        // In ATN every tag-set name length is 1.
        if b[pos] != 1 {
            break;
        }
        let name = b[pos + 1];
        let set_len = b[pos + 2] as usize;
        pos += 3;
        if pos + set_len > end {
            break;
        }
        tags.push(parse_atn_security_tag(name, &b[pos..pos + set_len]));
        pos += set_len;
    }
    if !tags.is_empty() {
        out["sec_info"] = json!(tags);
    }
    out
}

/// Parse the CLNP options part (ISO/IEC 8473 §7.5): a sequence of
/// `type(1) | length(1) | value` options. The Security option (0xC5) is
/// expanded as the ATN Security Label per ICAO Doc 9705.
fn parse_clnp_options(b: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= b.len() {
        let t = b[pos];
        let len = b[pos + 1] as usize;
        pos += 2;
        if pos + len > b.len() {
            break;
        }
        let v = &b[pos..pos + len];
        let mut o = json!({ "code": format!("{t:#04x}") });
        if let Some(name) = clnp_option_name(t) {
            o["name"] = json!(name);
        }
        match t {
            // Priority: single octet.
            0xCD if len == 1 => o["priority"] = json!(v[0]),
            // Discard reason: single octet code (ER PDU option).
            0xC1 if len == 1 => o["reason"] = json!(v[0]),
            // Security: ATN globally-unique format (0xC0) then security label.
            0xC5 if !v.is_empty() && v[0] == 0xC0 => {
                o["security_format"] = json!("globally-unique");
                o["security_label"] = parse_atn_security_label(&v[1..]);
            }
            _ => {
                o["value_hex"] =
                    json!(v.iter().map(|x| format!("{x:02x}")).collect::<String>());
            }
        }
        out.push(o);
        pos += len;
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
    let mut out = json!({
        "protocol": "CLNP",
        "type": type_name,
        "version": version,
        "lifetime_500ms": lifetime,
        "seg_len": seg_len,
        "dst_nsap": dst,
        "src_nsap": src,
    });
    // Flags (ISO/IEC 8473 §6.6): bit 8 SP (segmentation permitted),
    // bit 7 MS (more segments), bit 6 E/R (error report). When SP is set a
    // 6-octet segmentation part precedes the options part (§6.7): data-unit
    // identifier, segment offset, total length.
    let sp = flags & 0x80 != 0;
    let more_segments = flags & 0x40 != 0;
    let mut segment_offset = 0u16;
    if sp {
        out["error_report"] = json!(flags & 0x20 != 0);
        out["more_segments"] = json!(more_segments);
        if pos + 6 <= b.len() {
            segment_offset = u16::from_be_bytes([b[pos + 2], b[pos + 3]]);
            out["pdu_id"] = json!(u16::from_be_bytes([b[pos], b[pos + 1]]));
            out["segment_offset"] = json!(segment_offset);
            out["total_len"] = json!(u16::from_be_bytes([b[pos + 4], b[pos + 5]]));
            // A PDU is segmented when more segments follow or this fragment
            // does not start at offset 0.
            if more_segments || segment_offset != 0 {
                out["segmented"] = json!(true);
            }
        }
        pos += 6;
    }
    // Options part runs from here up to the header length indicator.
    if pos < hdr_len && hdr_len <= b.len() {
        let opts = parse_clnp_options(&b[pos..hdr_len]);
        if !opts.is_empty() {
            out["options"] = json!(opts);
        }
    }
    // COTP rides in the data part — but only the *complete* data unit is a
    // valid TPDU. A non-initial fragment (offset != 0) or a fragment with
    // more segments to follow holds a partial byte stream; parsing it as a
    // TPDU would be wrong, so it is left for the reassembler.
    let payload = &b[hdr_len..];
    if !(more_segments || segment_offset != 0) {
        if let Some(cotp) = parse_cotp(payload) {
            out["cotp"] = cotp;
        }
    }
    Some(out)
}

/// Segmentation view of a COTP DT TPDU (ISO/IEC 8073 §6.6 normal-data TSDU
/// segmentation): the destination reference, the end-of-TSDU flag, the TPDU
/// sequence number, and the user-data slice. Returns `None` when `b` is not a
/// DT TPDU (only DT carries TSDU segmentation; ED is expedited and not
/// segmented this way).
pub fn cotp_dt_segment(b: &[u8]) -> Option<(u16, bool, u32, Vec<u8>)> {
    let li = *b.first()? as usize;
    if li == 0 || li == 0xFF || b.len() < li + 1 {
        return None;
    }
    let code = *b.get(1)?;
    if code & 0xF0 != 0xF0 {
        return None; // not DT
    }
    let dst_ref = u16::from_be_bytes([*b.get(2)?, *b.get(3)?]);
    // Extended format (32-bit seq) is signalled by an odd LI for DT.
    let extended = li & 1 == 1;
    let (eot, seq) = if extended {
        let w = u32::from_be_bytes([*b.get(4)?, *b.get(5)?, *b.get(6)?, *b.get(7)?]);
        (w & 0x8000_0000 != 0, w & 0x7FFF_FFFF)
    } else {
        let w = *b.get(4)?;
        (w & 0x80 != 0, (w & 0x7F) as u32)
    };
    let user = b.get(li + 1..)?.to_vec();
    Some((dst_ref, eot, seq, user))
}

/// Reassembles a COTP normal-data TSDU from its DT TPDU segments (ISO/IEC
/// 8073 §6.6): consecutive DT TPDUs on one connection (keyed by destination
/// reference) carry user-data fragments, with the end-of-TSDU (EOT) bit set
/// on the final DT. Fragments accumulate in TPDU-sequence order until EOT,
/// when the complete TSDU is returned for upper-layer (ULCS/CPDLC) decoding.
pub struct CotpReassembler {
    /// Keyed by destination reference: accumulated user data, the next
    /// expected sequence number, and the last-seen timestamp.
    pending: HashMap<u16, CotpPending>,
}

struct CotpPending {
    data: Vec<u8>,
    next_seq: u32,
    last: f64,
}

const COTP_TIMEOUT_SECS: f64 = 60.0;

impl CotpReassembler {
    pub fn new() -> Self {
        Self { pending: HashMap::new() }
    }

    /// Push one raw COTP TPDU. For a single-segment TSDU (the first DT has EOT
    /// set) the user data passes straight through. For a multi-segment TSDU
    /// the fragments are buffered until the EOT DT arrives, when the complete
    /// TSDU is returned. Non-DT TPDUs and out-of-sequence DTs return `None`.
    pub fn push(&mut self, tpdu: &[u8], now: f64) -> Option<Vec<u8>> {
        let (dst_ref, eot, seq, user) = cotp_dt_segment(tpdu)?;
        self.pending.retain(|_, p| now - p.last < COTP_TIMEOUT_SECS);

        // A lone complete TSDU (seq 0, EOT) with nothing pending needs no work.
        if seq == 0 && eot && !self.pending.contains_key(&dst_ref) {
            return Some(user);
        }
        // Begin or continue a multi-segment TSDU. A fresh seq-0 fragment
        // (re)starts the buffer; otherwise the seq must match what is expected.
        if seq == 0 {
            self.pending.insert(
                dst_ref,
                CotpPending { data: user.clone(), next_seq: 1, last: now },
            );
        } else {
            let entry = self.pending.get_mut(&dst_ref)?;
            if seq != entry.next_seq {
                return None; // gap or duplicate: cannot reassemble safely
            }
            entry.data.extend_from_slice(&user);
            entry.next_seq = entry.next_seq.wrapping_add(1);
            entry.last = now;
        }
        if eot {
            return self.pending.remove(&dst_ref).map(|p| p.data);
        }
        None
    }
}

impl Default for CotpReassembler {
    fn default() -> Self {
        Self::new()
    }
}

/// COTP (ISO/IEC 8073 / ITU-T X.224) DR disconnect-reason name.
fn cotp_dr_reason(c: u8) -> Option<&'static str> {
    Some(match c {
        0 => "Reason not specified",
        1 => "TSAP congestion",
        2 => "Session entity not attached to TSAP",
        3 => "Unknown address",
        128 => "Normal disconnect",
        129 => "Remote transport entity congestion",
        130 => "Connection negotiation failed",
        131 => "Duplicate source reference",
        132 => "Mismatched references",
        133 => "Protocol error",
        135 => "Reference overflow",
        136 => "Connection request refused",
        138 => "Header or parameter length invalid",
        _ => return None,
    })
}

/// COTP ER (Error TPDU) reject-cause name (ISO/IEC 8073).
fn cotp_er_reject_cause(c: u8) -> Option<&'static str> {
    Some(match c {
        0 => "Reason not specified",
        1 => "Invalid parameter code",
        2 => "Invalid TPDU type",
        3 => "Invalid parameter value",
        _ => return None,
    })
}

/// COTP variable-part parameter name (ISO/IEC 8073 plus the ATN checksum
/// 0x08 profiled by ICAO Doc 9705). Only the parameters that occur on
/// ATN connections are named; others are reported by code.
fn cotp_param_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0x08 => "atn_checksum",
        0x85 => "ack_time_ms",
        0x86 => "residual_error_rate",
        0x87 => "priority",
        0x88 => "transit_delay",
        0x89 => "throughput",
        0x8A => "subseq_num",
        0x8B => "reassignment_time_sec",
        0x8C => "flow_control",
        0x8F => "sack",
        0xC0 => "tpdu_size",
        0xC1 => "calling_transport_selector",
        0xC2 => "called_responding_transport_selector",
        0xC3 => "checksum",
        0xC4 => "version",
        0xC5 => "protection_params",
        0xC6 => "additional_options",
        0xC7 => "additional_proto_classes",
        0xE0 => "additional_info",
        0xF0 => "preferred_max_tpdu_size",
        0xF2 => "inactivity_timer_ms",
        _ => return None,
    })
}

/// Parse the COTP variable part: a sequence of `type(1) | length(1) | value`
/// parameters (ISO/IEC 8073). `b` is the variable-part slice only.
/// The TPDU-size (0xC0) parameter decodes to bytes via `1 << value`
/// (ISO/IEC 8073 §13.3.4: values 0x07..0x0D), and the ATN checksum (0x08)
/// is surfaced as a named octet string.
fn parse_cotp_params(b: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= b.len() {
        let t = b[pos];
        let len = b[pos + 1] as usize;
        pos += 2;
        if pos + len > b.len() {
            break;
        }
        let val = &b[pos..pos + len];
        let mut p = json!({ "code": format!("{t:#04x}") });
        if let Some(name) = cotp_param_name(t) {
            p["name"] = json!(name);
        }
        match t {
            // TPDU size: 2^value bytes (valid value range 0x07..0x0D).
            0xC0 if len == 1 && (0x07..=0x0D).contains(&val[0]) => {
                p["tpdu_size"] = json!(1u32 << val[0]);
            }
            // Ack time / priority / subseq / reassignment: u16.
            0x85 | 0x87 | 0x8A | 0x8B if len == 2 => {
                p["value"] = json!(u16::from_be_bytes([val[0], val[1]]));
            }
            // Version: u8.
            0xC4 if len == 1 => p["value"] = json!(val[0]),
            // Inactivity timer: u32 ms.
            0xF2 if len == 4 => {
                p["value"] = json!(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            _ => {
                p["value_hex"] =
                    json!(val.iter().map(|x| format!("{x:02x}")).collect::<String>());
            }
        }
        out.push(p);
        pos += len;
    }
    out
}

/// COTP TPDU decode per ISO/IEC 8073 / ITU-T X.224, all ten TPDU types,
/// including the variable part (TPDU-size, ATN checksum 0x08, credit,
/// EOT and extended sequence numbers). `b` is the COTP TPDU starting with
/// the length-indicator (LI) octet; the header occupies `b[0..=li]` and
/// user data (DT/ED) follows at `b[li + 1..]`.
fn parse_cotp(b: &[u8]) -> Option<Value> {
    let li = *b.first()? as usize;
    if li == 0 || li == 0xFF || b.len() < 2 + 2 {
        // Need at least LI + code + dst_ref (X.224 minimum header).
        return None;
    }
    let code = *b.get(1)?;
    // dst_ref occupies b[2..4] for every COTP TPDU.
    let dst_ref = if b.len() >= 4 {
        Some(u16::from_be_bytes([b[2], b[3]]))
    } else {
        None
    };
    // Classify by the high nibble; CR/CC/AK/RJ carry credit in the low
    // nibble (normal format), DT carries the ROA bit in bit 0.
    let high = code & 0xF0;
    let (name, base_code, credit_nibble): (&str, u8, Option<u8>) = match high {
        0xE0 => ("CR", 0xE0, Some(code & 0x0F)),
        0xD0 => ("CC", 0xD0, Some(code & 0x0F)),
        0x80 => ("DR", 0x80, None),
        0xC0 => ("DC", 0xC0, None),
        0xF0 => ("DT", 0xF0, None),
        0x10 => ("ED", 0x10, None),
        0x60 => ("AK", 0x60, Some(code & 0x0F)),
        0x20 => ("EA", 0x20, None),
        0x50 => ("RJ", 0x50, Some(code & 0x0F)),
        0x70 => ("ER", 0x70, None),
        _ => return None,
    };
    let mut out = json!({ "tpdu": name });
    out["dst_ref"] = json!(dst_ref?);

    // Variable-part offset measured from the LI octet (i.e. into `b`);
    // the variable part is b[var_off..=li]. Per X.224 the extended format
    // (32-bit sequence numbers) is signalled by an odd LI for DT/ED/AK/EA/RJ.
    let extended = matches!(name, "DT" | "ED" | "AK" | "EA" | "RJ") && (li & 1 == 1);
    let var_off: usize;
    match name {
        "CR" | "CC" => {
            // code, dst_ref(2), src_ref(2), class/options(1).
            if b.len() < 7 {
                return None;
            }
            out["src_ref"] = json!(u16::from_be_bytes([b[4], b[5]]));
            out["class"] = json!(b[6] >> 4);
            out["options"] = json!(b[6] & 0x0F);
            var_off = 7;
        }
        "DR" => {
            // code, dst_ref(2), src_ref(2), reason(1).
            if b.len() < 7 {
                return None;
            }
            out["src_ref"] = json!(u16::from_be_bytes([b[4], b[5]]));
            let reason = b[6];
            out["reason"] = json!(reason);
            if let Some(t) = cotp_dr_reason(reason) {
                out["reason_text"] = json!(t);
            }
            var_off = 7;
        }
        "DC" => {
            // code, dst_ref(2), src_ref(2).
            if b.len() < 6 {
                return None;
            }
            out["src_ref"] = json!(u16::from_be_bytes([b[4], b[5]]));
            var_off = 6;
        }
        "ER" => {
            // code, dst_ref(2), reject-cause(1).
            if b.len() < 5 {
                return None;
            }
            let cause = b[4];
            out["reject_cause"] = json!(cause);
            if let Some(t) = cotp_er_reject_cause(cause) {
                out["reject_cause_text"] = json!(t);
            }
            var_off = 5;
        }
        "DT" | "ED" => {
            // ROA bit is bit 0 of the code for DT.
            if base_code == 0xF0 {
                out["roa"] = json!(code & 1 == 1);
            }
            if extended {
                // code, dst_ref(2), EOT|seq(4).
                if b.len() < 8 {
                    return None;
                }
                out["eot"] = json!(b[4] & 0x80 != 0);
                out["tpdu_seq"] =
                    json!(u32::from_be_bytes([b[4], b[5], b[6], b[7]]) & 0x7FFF_FFFF);
                var_off = 8;
            } else {
                // code, dst_ref(2), EOT|seq(1).
                if b.len() < 5 {
                    return None;
                }
                out["eot"] = json!(b[4] & 0x80 != 0);
                out["tpdu_seq"] = json!((b[4] & 0x7F) as u32);
                var_off = 5;
            }
        }
        "AK" => {
            if extended {
                // code, dst_ref(2), seq(4), credit(2).
                if b.len() < 10 {
                    return None;
                }
                out["tpdu_seq"] =
                    json!(u32::from_be_bytes([b[4], b[5], b[6], b[7]]) & 0x7FFF_FFFF);
                out["credit"] = json!(u16::from_be_bytes([b[8], b[9]]));
                var_off = 10;
            } else {
                // code(credit low nibble), dst_ref(2), seq(1).
                if b.len() < 5 {
                    return None;
                }
                out["credit"] = json!(credit_nibble.unwrap_or(0));
                out["tpdu_seq"] = json!((b[4] & 0x7F) as u32);
                var_off = 5;
            }
        }
        "EA" => {
            if extended {
                // code, dst_ref(2), seq(4).
                if b.len() < 8 {
                    return None;
                }
                out["tpdu_seq"] =
                    json!(u32::from_be_bytes([b[4], b[5], b[6], b[7]]) & 0x7FFF_FFFF);
                var_off = 8;
            } else {
                // code, dst_ref(2), seq(1).
                if b.len() < 5 {
                    return None;
                }
                out["tpdu_seq"] = json!((b[4] & 0x7F) as u32);
                var_off = 5;
            }
        }
        "RJ" => {
            if extended {
                // code, dst_ref(2), seq(4), credit(2).
                if b.len() < 10 {
                    return None;
                }
                out["tpdu_seq"] =
                    json!(u32::from_be_bytes([b[4], b[5], b[6], b[7]]) & 0x7FFF_FFFF);
                out["credit"] = json!(u16::from_be_bytes([b[8], b[9]]));
                var_off = 10;
            } else {
                // code(credit low nibble), dst_ref(2), seq(1).
                if b.len() < 5 {
                    return None;
                }
                out["credit"] = json!(credit_nibble.unwrap_or(0));
                out["tpdu_seq"] = json!((b[4] & 0x7F) as u32);
                var_off = 5;
            }
        }
        _ => return None,
    }
    if extended {
        out["extended"] = json!(true);
    }

    // Variable part: b[var_off..=li] (header runs through index `li`).
    let hdr_end = li + 1; // one past the last header octet
    if var_off > 0 && hdr_end > var_off && hdr_end <= b.len() {
        let params = parse_cotp_params(&b[var_off..hdr_end]);
        if !params.is_empty() {
            out["params"] = json!(params);
        }
    }

    if matches!(name, "DT" | "ED") && b.len() > li + 1 {
        let user = &b[li + 1..];
        out["user_data_len"] = json!(user.len());
        // ATN-B1 applications ride here (via the ULCS null encoding). A
        // single-segment TSDU (DT with EOT set, seq 0, or any ED) decodes
        // directly; a partial DT (EOT clear or seq > 0) carries only a TSDU
        // fragment and must be reassembled (CotpReassembler) before decode, so
        // it is flagged rather than mis-parsed.
        let partial = name == "DT"
            && (out.get("eot") == Some(&json!(false))
                || out.get("tpdu_seq").and_then(|v| v.as_u64()).unwrap_or(0) != 0);
        if partial {
            out["tsdu_segment"] = json!(true);
        } else if let Some(app) = parse_cotp_user_app(user) {
            out["app"] = app;
        }
    }
    Some(out)
}

/// Dispatch a complete COTP TSDU's user data to the ATN-B1 application
/// decoders (ULCS null encoding): protected-mode CPDLC, then CM. Used both
/// for single-segment TSDUs inline and for TSDUs reassembled from multiple DT
/// TPDUs by [`CotpReassembler`].
pub fn parse_cotp_user_app(user: &[u8]) -> Option<Value> {
    crate::atn_cpdlc::parse_apdu(user)
        .or_else(|| crate::atn_cpdlc::parse_cm_logon(user))
        .or_else(|| crate::atn_cpdlc::parse_cm_ground(user))
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
    fn x25_call_request_sndcf_compression() {
        // Call-Request (0x0B): no addresses, one fast-select facility, then
        // the SNDCF field (id 0xC1, len 4, version 1, .., compression 0x70 =
        // ACA + DEFLATE + M/I bit), then CUD = 0x81 (CLNP). The SNDCF field
        // sits between facilities and CUD per ICAO Doc 9705 §5.7.
        let pkt = [
            0x10, 0x01, 0x0B, 0x00, // GFI/LCN, call-request, no addresses
            0x02, 0x01, 0x80, // facility length 2, fast-select facility
            0xC1, 0x04, 0x01, 0x00, 0x00, 0x70, // SNDCF: version 1, comp 0x70
            0x81, // CUD: CLNP NLPID
        ];
        let p = parse_x25(&pkt).unwrap();
        assert_eq!(p.kind, "call-request");
        let s = p.sndcf.as_ref().unwrap();
        assert_eq!(s["version"], 1);
        assert_eq!(s["compression_options"], 0x70);
        assert_eq!(s["compression_algos"][0], "ACA");
        assert_eq!(s["compression_algos"][1], "DEFLATE");
        assert_eq!(s["maintenance"], true);
        // CUD (the network-protocol identifier) is no longer swallowed.
        assert_eq!(p.payload, vec![0x81]);
        // A non-SNDCF byte sequence is left as CUD (identifier mismatch).
        let no_sndcf = [0x10, 0x01, 0x0B, 0x00, 0x00, 0x81, 0x01];
        let p = parse_x25(&no_sndcf).unwrap();
        assert!(p.sndcf.is_none());
        assert_eq!(p.payload, vec![0x81, 0x01]);
    }

    #[test]
    fn x25_call_accept_single_compression_octet() {
        // Call-Accepted (0x0F): no addresses, no facilities, then a single
        // compression octet 0x02 (LREF only), then CUD.
        let pkt = [0x10, 0x01, 0x0F, 0x00, 0x00, 0x02, 0xAA];
        let p = parse_x25(&pkt).unwrap();
        assert_eq!(p.kind, "call-accepted");
        let s = p.sndcf.as_ref().unwrap();
        assert_eq!(s["compression_options"], 0x02);
        assert_eq!(s["compression_algos"][0], "LREF");
        assert_eq!(s["maintenance"], false);
        assert_eq!(p.payload, vec![0xAA]);
    }

    #[test]
    fn x25_supervisory_parse() {
        let rr = [0x10, 0x01, 0b101_00001];
        let p = parse_x25(&rr).unwrap();
        assert_eq!(p.kind, "rr");
        assert_eq!(p.pr, Some(5));
    }

    #[test]
    fn x25_restart_request_and_confirm() {
        // RESTART REQUEST (0xFB): GFI modulo-8, LCN 0, cause 0x07 (Network
        // operational in the restart-cause table), diagnostic 0x34
        // (Time expired for restart indication).
        let req = [0x10, 0x00, 0xFB, 0x07, 0x34];
        let p = parse_x25(&req).unwrap();
        assert_eq!(p.kind, "restart-request");
        assert_eq!(p.cause, Some(0x07));
        assert_eq!(p.cause_text, Some("Network operational"));
        assert_eq!(p.diagnostic, Some(0x34));
        assert_eq!(p.diagnostic_text, Some("Time expired for restart indication"));
        // RESTART CONFIRM (0xFF): no cause/diagnostic body.
        let conf = [0x10, 0x00, 0xFF];
        let p = parse_x25(&conf).unwrap();
        assert_eq!(p.kind, "restart-confirmation");
        assert_eq!(p.cause, None);
        assert_eq!(p.diagnostic, None);
    }

    #[test]
    fn x25_clear_reset_cause_naming() {
        // CLEAR REQUEST cause 0x01 = "Number busy"; reset uses a different
        // table where 0x01 = "Out of order".
        let clear = [0x10, 0x05, 0x13, 0x01, 0x00];
        let p = parse_x25(&clear).unwrap();
        assert_eq!(p.kind, "clear-request");
        assert_eq!(p.cause_text, Some("Number busy"));
        assert_eq!(p.diagnostic_text, Some("Cleared by system management"));
        let reset = [0x10, 0x05, 0x1B, 0x01, 0x26];
        let p = parse_x25(&reset).unwrap();
        assert_eq!(p.kind, "reset-request");
        assert_eq!(p.cause, Some(0x01));
        assert_eq!(p.cause_text, Some("Out of order"));
        assert_eq!(p.diagnostic_text, Some("Packet too short"));
    }

    #[test]
    fn x25_cause_high_bit_masked() {
        // X.25 Table 5-7: when bit 8 of the cause is set, the lower bits
        // are the remote DTE's value; the dictionary lookup uses 0.
        let clear = [0x10, 0x05, 0x13, 0x85, 0x00];
        let p = parse_x25(&clear).unwrap();
        assert_eq!(p.cause, Some(0));
        assert_eq!(p.cause_text, Some("DTE originated"));
    }

    #[test]
    fn x25_diagnostic_packet_names_code() {
        // DIAG (0xF1) with an ICAO Doc 9705 extension code.
        let diag = [0x10, 0x00, 0xF1, 0x88];
        let p = parse_x25(&diag).unwrap();
        assert_eq!(p.kind, "diagnostic");
        assert_eq!(p.diagnostic, Some(0x88));
        assert_eq!(p.diagnostic_text, Some("LREF compression not supported"));
    }

    #[test]
    fn clnp_dt_with_cotp_parses() {
        // CLNP DT: NLPID 0x81, hdr_len, ver 1, lifetime, type DT(0x1C),
        // seg len, cksum, dst(2) src(2), then COTP DT.
        let mut b = vec![0x81, 15, 1, 0x3F, 0x1C, 0x00, 0x14, 0x00, 0x00];
        b.extend_from_slice(&[2, 0x47, 0x01]); // dst NSAP
        b.extend_from_slice(&[2, 0x47, 0x02]); // src NSAP
        // COTP DT, normal format: LI=4, code 0xF0, dst_ref=1, EOT|seq=0x80,
        // then user data "HI" (ISO/IEC 8073 §13.7).
        b.extend_from_slice(&[0x04, 0xF0, 0x00, 0x01, 0x80, b'H', b'I']);
        let v = parse_network(&b).unwrap();
        assert_eq!(v["protocol"], "CLNP");
        assert_eq!(v["type"], "DT");
        assert_eq!(v["dst_nsap"], "4701");
        assert_eq!(v["src_nsap"], "4702");
        assert_eq!(v["cotp"]["tpdu"], "DT");
        assert_eq!(v["cotp"]["dst_ref"], 1);
        assert_eq!(v["cotp"]["eot"], true);
        assert_eq!(v["cotp"]["tpdu_seq"], 0);
        assert_eq!(v["cotp"]["user_data_len"], 2);
    }

    #[test]
    fn esis_and_compressed_labeled() {
        let v = parse_network(&[0x82, 0, 0]).unwrap();
        assert_eq!(v["protocol"], "ES-IS");
        let v = parse_network(&[0x05, 0, 0]).unwrap();
        assert_eq!(v["protocol"], "clnp-compressed?");
    }

    // --- COTP (ISO/IEC 8073 / ITU-T X.224) TPDU coverage ---
    // The TPDU code values, header layouts, variable-part parameter codes
    // (incl. the ATN checksum 0x08), DR disconnect-reason and ER reject-cause
    // dictionaries are cross-checked against the ISO/IEC 8073 framing as
    // profiled by ICAO Doc 9705 and against dumpvdl2's src/cotp.{c,h}
    // (protocol facts only — no code or formatter text was copied).

    #[test]
    fn cotp_cr_with_tpdu_size_and_atn_checksum() {
        // CR (0xE0 | credit 0): code, dst_ref(2), src_ref(2), class|opt(1),
        // then variable part: TPDU-size 0xC0 len 1 value 0x0A (=1024),
        // ATN checksum 0x08 len 2. LI counts every header octet after LI.
        // header after LI: code(1)+dstref(2)+srcref(2)+class(1)
        //                  + [C0 01 0A] + [08 02 12 34] = 6 + 3 + 4 = 13.
        let b = [
            0x0D, 0xE0, 0x00, 0x05, 0x12, 0x34, 0x40, // CR, dst 5, src 0x1234, class 4
            0xC0, 0x01, 0x0A, // TPDU size 2^10
            0x08, 0x02, 0x12, 0x34, // ATN checksum
        ];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "CR");
        assert_eq!(v["dst_ref"], 5);
        assert_eq!(v["src_ref"], 0x1234);
        assert_eq!(v["class"], 4);
        let params = v["params"].as_array().unwrap();
        assert_eq!(params[0]["name"], "tpdu_size");
        assert_eq!(params[0]["tpdu_size"], 1024);
        assert_eq!(params[1]["name"], "atn_checksum");
        assert_eq!(params[1]["value_hex"], "1234");
    }

    #[test]
    fn cotp_cc_decodes_class_and_refs() {
        // CC (0xD0): same fixed layout as CR. LI=6 (no variable part).
        let b = [0x06, 0xD0, 0x12, 0x34, 0x00, 0x07, 0x40];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "CC");
        assert_eq!(v["dst_ref"], 0x1234);
        assert_eq!(v["src_ref"], 7);
        assert_eq!(v["class"], 4);
        assert_eq!(v["options"], 0);
    }

    #[test]
    fn cotp_dr_disconnect_reason_named() {
        // DR (0x80): code, dst_ref(2), src_ref(2), reason(1)=128 "Normal".
        let b = [0x06, 0x80, 0x00, 0x09, 0x12, 0x34, 128];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "DR");
        assert_eq!(v["dst_ref"], 9);
        assert_eq!(v["src_ref"], 0x1234);
        assert_eq!(v["reason"], 128);
        assert_eq!(v["reason_text"], "Normal disconnect");
    }

    #[test]
    fn cotp_dc_decodes_refs() {
        // DC (0xC0): code, dst_ref(2), src_ref(2). LI=5.
        let b = [0x05, 0xC0, 0x00, 0x09, 0x12, 0x34];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "DC");
        assert_eq!(v["dst_ref"], 9);
        assert_eq!(v["src_ref"], 0x1234);
    }

    #[test]
    fn cotp_er_reject_cause_named() {
        // ER (0x70): code, dst_ref(2), reject-cause(1)=2 "Invalid TPDU type".
        let b = [0x04, 0x70, 0x00, 0x09, 0x02];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "ER");
        assert_eq!(v["dst_ref"], 9);
        assert_eq!(v["reject_cause"], 2);
        assert_eq!(v["reject_cause_text"], "Invalid TPDU type");
    }

    #[test]
    fn cotp_ed_normal_format() {
        // ED (0x10): like DT, EOT|seq(1). LI=4, normal format, user data.
        let b = [0x04, 0x10, 0x00, 0x03, 0x85, 0xAB, 0xCD];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "ED");
        assert_eq!(v["dst_ref"], 3);
        assert_eq!(v["eot"], true);
        assert_eq!(v["tpdu_seq"], 5);
        assert_eq!(v["user_data_len"], 2);
    }

    #[test]
    fn cotp_dt_extended_seq() {
        // DT extended format: odd LI (=7) → 32-bit EOT|seq at b[4..8].
        // code, dst_ref(2), EOT(1)|seq(31). EOT set, seq=0x000000AA.
        let b = [0x07, 0xF0, 0x00, 0x02, 0x80, 0x00, 0x00, 0xAA, b'X'];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "DT");
        assert_eq!(v["extended"], true);
        assert_eq!(v["eot"], true);
        assert_eq!(v["tpdu_seq"], 0xAA);
        assert_eq!(v["user_data_len"], 1);
        assert_eq!(v["roa"], false);
    }

    #[test]
    fn cotp_ak_normal_credit_in_nibble() {
        // AK (0x60 | credit): normal format, code(credit low nibble),
        // dst_ref(2), seq(1). LI=4 (even). credit=5, seq=3.
        let b = [0x04, 0x65, 0x00, 0x01, 0x03];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "AK");
        assert_eq!(v["dst_ref"], 1);
        assert_eq!(v["credit"], 5);
        assert_eq!(v["tpdu_seq"], 3);
    }

    #[test]
    fn cotp_ak_extended_credit_field() {
        // AK extended: odd LI (=9) → code, dst_ref(2), seq(4), credit(2).
        let b = [0x09, 0x60, 0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x00, 0x0A];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "AK");
        assert_eq!(v["extended"], true);
        assert_eq!(v["tpdu_seq"], 7);
        assert_eq!(v["credit"], 10);
    }

    #[test]
    fn cotp_ea_and_rj() {
        // EA (0x20) normal: code, dst_ref(2), seq(1). LI=4.
        let ea = [0x04, 0x20, 0x00, 0x02, 0x09];
        let v = parse_cotp(&ea).unwrap();
        assert_eq!(v["tpdu"], "EA");
        assert_eq!(v["tpdu_seq"], 9);
        // RJ (0x50 | credit) normal: code, dst_ref(2), seq(1). credit=2.
        let rj = [0x04, 0x52, 0x00, 0x02, 0x06];
        let v = parse_cotp(&rj).unwrap();
        assert_eq!(v["tpdu"], "RJ");
        assert_eq!(v["credit"], 2);
        assert_eq!(v["tpdu_seq"], 6);
    }

    // --- CLNP options + ATN security label (ISO/IEC 8473 / ICAO Doc 9705) ---
    // The CLNP option codes, the ATN security-label structure, the security
    // tag-set codes and the traffic-type/ATSC-class/subnet/security-class
    // dictionaries are cross-checked against ISO/IEC 8473 (X.233) and ICAO
    // Doc 9705 and against dumpvdl2's src/clnp.c / src/atn.c (protocol facts
    // only). Vectors are spec-derived, built octet-by-octet (no loopback).

    /// Build a length-prefixed ATN security tag set: `1 | name | len | value`.
    fn sec_tagset(name: u8, value: &[u8]) -> Vec<u8> {
        let mut v = vec![1u8, name, value.len() as u8];
        v.extend_from_slice(value);
        v
    }

    /// Build a CLNP Security option (0xC5) carrying an ATN security label.
    fn clnp_security_option(srid: &[u8], tagsets: &[u8]) -> Vec<u8> {
        // label = srid_len | srid | sinfo_len | tagsets
        let mut label = vec![srid.len() as u8];
        label.extend_from_slice(srid);
        label.push(tagsets.len() as u8);
        label.extend_from_slice(tagsets);
        // option value = security-format (0xC0 = globally unique) + label
        let mut val = vec![0xC0u8];
        val.extend_from_slice(&label);
        let mut opt = vec![0xC5u8, val.len() as u8];
        opt.extend_from_slice(&val);
        opt
    }

    /// Build a CLNP DT PDU with the given option bytes and no payload.
    fn clnp_with_options(opts: &[u8]) -> Vec<u8> {
        // fixed(9) + dst(1+2) + src(1+2) = 15, plus options.
        let hdr_len = 15 + opts.len();
        let mut b = vec![0x81, hdr_len as u8, 1, 0x3F, 0x1C, 0x00, 0x00, 0x00, 0x00];
        b.extend_from_slice(&[2, 0x47, 0x01]); // dst NSAP
        b.extend_from_slice(&[2, 0x47, 0x02]); // src NSAP
        b.extend_from_slice(opts);
        b
    }

    #[test]
    fn clnp_security_label_traffic_type_and_class() {
        // Two tag sets: traffic-type (0x0F) value 0x00 → ATN operational /
        // ATSC / route-policy 0; security-classification (0x03) value 2 →
        // restricted. SRID = 47 00 27.
        let mut tagsets = sec_tagset(0x0F, &[0x00]);
        tagsets.extend(sec_tagset(0x03, &[0x02]));
        let opt = clnp_security_option(&[0x47, 0x00, 0x27], &tagsets);
        let v = parse_network(&clnp_with_options(&opt)).unwrap();
        assert_eq!(v["protocol"], "CLNP");
        let opts = v["options"].as_array().unwrap();
        assert_eq!(opts[0]["name"], "security");
        assert_eq!(opts[0]["security_format"], "globally-unique");
        let label = &opts[0]["security_label"];
        assert_eq!(label["reg_id"], "470027");
        let info = label["sec_info"].as_array().unwrap();
        assert_eq!(info[0]["kind"], "traffic_type");
        assert_eq!(info[0]["traffic_type"], "ATN operational");
        assert_eq!(info[0]["category"], "ATSC");
        assert_eq!(info[0]["route_policy"], 0);
        assert_eq!(info[1]["kind"], "security_classification");
        assert_eq!(info[1]["class_id"], 2);
        assert_eq!(info[1]["class_name"], "restricted");
    }

    #[test]
    fn clnp_security_label_subnet_and_atsc_classes() {
        // Subnet-type (0x05): subnet 2 (VDL), permitted ATS+AOC (0x03);
        // supported ATSC classes (0x06): A+B+C (0x07).
        let mut tagsets = sec_tagset(0x05, &[0x02, 0x03]);
        tagsets.extend(sec_tagset(0x06, &[0x07]));
        let opt = clnp_security_option(&[0xAB], &tagsets);
        let v = parse_network(&clnp_with_options(&opt)).unwrap();
        let info = &v["options"][0]["security_label"]["sec_info"];
        assert_eq!(info[0]["kind"], "subnet_type");
        assert_eq!(info[0]["subnet_id"], 2);
        assert_eq!(info[0]["subnet_name"], "VDL");
        assert_eq!(info[0]["permitted_traffic_types"][0], "ATS");
        assert_eq!(info[0]["permitted_traffic_types"][1], "AOC");
        assert_eq!(info[1]["kind"], "supported_atsc_classes");
        assert_eq!(info[1]["classes"][0], "A");
        assert_eq!(info[1]["classes"][1], "B");
        assert_eq!(info[1]["classes"][2], "C");
    }

    #[test]
    fn clnp_priority_option_decodes() {
        // Priority option (0xCD) value 6, plus a padding option (0xCC).
        let opts = [0xCD, 0x01, 0x06, 0xCC, 0x02, 0x00, 0x00];
        let v = parse_network(&clnp_with_options(&opts)).unwrap();
        let opts = v["options"].as_array().unwrap();
        assert_eq!(opts[0]["name"], "priority");
        assert_eq!(opts[0]["priority"], 6);
        assert_eq!(opts[1]["name"], "padding");
    }

    #[test]
    fn cotp_dt_with_inactivity_and_priority_params() {
        // DT extended with variable part: priority 0x87 (u16) and inactivity
        // timer 0xF2 (u32 ms). LI = 7 (extended fixed) + 4 (priority) + 6
        // (inactivity) = 17 (odd → extended).
        let b = [
            0x11, 0xF0, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, // ext DT, seq 1
            0x87, 0x02, 0x00, 0x06, // priority = 6
            0xF2, 0x04, 0x00, 0x00, 0x75, 0x30, // inactivity 30000 ms
            0x42, // user data
        ];
        let v = parse_cotp(&b).unwrap();
        assert_eq!(v["tpdu"], "DT");
        assert_eq!(v["extended"], true);
        assert_eq!(v["tpdu_seq"], 1);
        let params = v["params"].as_array().unwrap();
        assert_eq!(params[0]["name"], "priority");
        assert_eq!(params[0]["value"], 6);
        assert_eq!(params[1]["name"], "inactivity_timer_ms");
        assert_eq!(params[1]["value"], 30000);
        assert_eq!(v["user_data_len"], 1);
    }

    // --- CLNP segmentation + multipart reassembly (ISO/IEC 8473 §6.6/6.7) ---
    // The flags byte (SP 0x80 / MS 0x40 / E-R 0x20 / type), the 6-octet
    // segmentation part layout (data-unit id, segment offset, total length),
    // and the reassembly-by-offset rule are taken from ISO/IEC 8473 (X.233)
    // as profiled by ICAO Doc 9705. Vectors are built octet-by-octet from the
    // header layout (no encode→decode loopback).

    /// Build one CLNP DT derived PDU (SP set) carrying `data` at
    /// `seg_offset`, with the given more-segments flag and total length.
    /// dst NSAP = 47 01, src NSAP = 47 02. Header = 9 fixed + 3 + 3 + 6 = 21.
    fn clnp_segment_pdu(
        pdu_id: u16,
        seg_offset: u16,
        total_len: u16,
        more: bool,
        data: &[u8],
    ) -> Vec<u8> {
        let hdr_len = 21u8;
        let mut flags = 0x1C | 0x80; // DT type + SP
        if more {
            flags |= 0x40; // MS
        }
        let seg_len = hdr_len as u16 + data.len() as u16;
        let mut b = vec![
            0x81,
            hdr_len,
            0x01,
            0x3F,
            flags,
            (seg_len >> 8) as u8,
            seg_len as u8,
            0x00,
            0x00, // checksum (0 = unused)
        ];
        b.extend_from_slice(&[2, 0x47, 0x01]); // dst NSAP
        b.extend_from_slice(&[2, 0x47, 0x02]); // src NSAP
        // segmentation part: pdu_id, segment offset, total length.
        b.extend_from_slice(&pdu_id.to_be_bytes());
        b.extend_from_slice(&seg_offset.to_be_bytes());
        b.extend_from_slice(&total_len.to_be_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn clnp_segment_fields_parse() {
        // A single segmented DT exposes the segmentation fields + flags.
        let pdu = clnp_segment_pdu(0x1234, 0, 100, true, &[0xAA, 0xBB]);
        let v = parse_network(&pdu).unwrap();
        assert_eq!(v["protocol"], "CLNP");
        assert_eq!(v["type"], "DT");
        assert_eq!(v["more_segments"], true);
        assert_eq!(v["error_report"], false);
        assert_eq!(v["segmented"], true);
        assert_eq!(v["pdu_id"], 0x1234);
        assert_eq!(v["segment_offset"], 0);
        assert_eq!(v["total_len"], 100);
        // A non-initial fragment must NOT be parsed as a COTP TPDU.
        let frag2 = clnp_segment_pdu(0x1234, 2, 100, false, &[0xCC]);
        let v2 = parse_network(&frag2).unwrap();
        assert!(v2.get("cotp").is_none());
        assert_eq!(v2["segment_offset"], 2);
    }

    #[test]
    fn clnp_reassembles_two_segments_into_cotp() {
        // Two DT segments of one data unit reassemble into a complete COTP DT.
        // Complete data part: a COTP DT (LI=4, code 0xF0, dst_ref=1,
        // EOT|seq=0x80, then "HI"). Total data = 7 octets; header = 21;
        // total length = 28. Segment 1: offset 0, data[0..4], MS=1.
        // Segment 2: offset 4, data[4..7], MS=0.
        let cotp: [u8; 7] = [0x04, 0xF0, 0x00, 0x01, 0x80, b'H', b'I'];
        let total_len = 21 + cotp.len() as u16; // header + full data
        let seg1 = clnp_segment_pdu(0x55AA, 0, total_len, true, &cotp[0..4]);
        let seg2 = clnp_segment_pdu(0x55AA, 4, total_len, false, &cotp[4..7]);

        let mut r = ClnpReassembler::new();
        // First segment: not yet complete.
        assert_eq!(r.push(&seg1, 0.0), None);
        // Second segment: completes the data unit; returns a full CLNP PDU.
        let full = r.push(&seg2, 1.0).expect("reassembled PDU");
        // The reconstructed PDU has the more-segments flag cleared.
        assert_eq!(full[4] & 0x40, 0);
        let v = parse_network(&full).unwrap();
        assert_eq!(v["protocol"], "CLNP");
        assert_eq!(v["cotp"]["tpdu"], "DT");
        assert_eq!(v["cotp"]["dst_ref"], 1);
        assert_eq!(v["cotp"]["eot"], true);
        assert_eq!(v["cotp"]["user_data_len"], 2);
    }

    #[test]
    fn clnp_reassembles_out_of_order() {
        // The same two segments arriving in reverse order still reassemble.
        let payload: Vec<u8> = (0..10u8).collect();
        let total_len = 21 + payload.len() as u16;
        let seg1 = clnp_segment_pdu(0x0001, 0, total_len, true, &payload[0..6]);
        let seg2 = clnp_segment_pdu(0x0001, 6, total_len, false, &payload[6..10]);
        let mut r = ClnpReassembler::new();
        // Last segment first.
        assert_eq!(r.push(&seg2, 0.0), None);
        let full = r.push(&seg1, 1.0).expect("reassembled");
        // Data part (after the 21-octet header) equals the original payload.
        assert_eq!(&full[21..], &payload[..]);
    }

    #[test]
    fn clnp_unsegmented_passes_through() {
        // A plain (unsegmented) CLNP DT passes straight through the
        // reassembler unchanged.
        let mut b = vec![0x81, 15, 1, 0x3F, 0x1C, 0x00, 0x14, 0x00, 0x00];
        b.extend_from_slice(&[2, 0x47, 0x01]);
        b.extend_from_slice(&[2, 0x47, 0x02]);
        let mut r = ClnpReassembler::new();
        assert_eq!(r.push(&b, 0.0), Some(b.clone()));
    }

    // --- VDL2-2.2: COTP normal-data TSDU reassembly (ISO/IEC 8073 §6.6) ---
    // The DT TPDU framing (LI, code 0xF0, dst_ref, EOT|seq) and the EOT-driven
    // TSDU segmentation rule are taken from ISO/IEC 8073 / ITU-T X.224 as
    // profiled by ICAO Doc 9705. Vectors are built octet-by-octet (no loopback).

    /// Build a normal-format COTP DT TPDU: LI=4, code 0xF0, dst_ref, EOT|seq,
    /// then user data.
    fn cotp_dt(dst_ref: u16, eot: bool, seq: u8, user: &[u8]) -> Vec<u8> {
        let mut b = vec![0x04, 0xF0];
        b.extend_from_slice(&dst_ref.to_be_bytes());
        b.push(if eot { 0x80 } else { 0x00 } | (seq & 0x7F));
        b.extend_from_slice(user);
        b
    }

    #[test]
    fn cotp_dt_segment_extracts_fields() {
        let dt = cotp_dt(0x1234, false, 3, &[0xAA, 0xBB]);
        let (dst, eot, seq, user) = cotp_dt_segment(&dt).unwrap();
        assert_eq!(dst, 0x1234);
        assert!(!eot);
        assert_eq!(seq, 3);
        assert_eq!(user, vec![0xAA, 0xBB]);
        // A CC TPDU is not a DT and must not be treated as a segment.
        let cc = [0x06, 0xD0, 0x12, 0x34, 0x00, 0x07, 0x40];
        assert!(cotp_dt_segment(&cc).is_none());
    }

    #[test]
    fn cotp_single_segment_passes_through() {
        // A lone DT with EOT set (seq 0) is a complete TSDU.
        let dt = cotp_dt(0x0009, true, 0, &[1, 2, 3]);
        let mut r = CotpReassembler::new();
        assert_eq!(r.push(&dt, 0.0), Some(vec![1, 2, 3]));
    }

    #[test]
    fn cotp_reassembles_three_dt_segments() {
        // Three DTs (seq 0/1/2), EOT on the last, reassemble in order.
        let s0 = cotp_dt(0x0042, false, 0, &[0xAA, 0xBB]);
        let s1 = cotp_dt(0x0042, false, 1, &[0xCC]);
        let s2 = cotp_dt(0x0042, true, 2, &[0xDD, 0xEE]);
        let mut r = CotpReassembler::new();
        assert_eq!(r.push(&s0, 0.0), None);
        assert_eq!(r.push(&s1, 1.0), None);
        assert_eq!(r.push(&s2, 2.0), Some(vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE]));
    }

    #[test]
    fn cotp_out_of_sequence_dt_rejected() {
        // A gap in the sequence (seq 2 after seq 0) cannot reassemble safely.
        let s0 = cotp_dt(0x0001, false, 0, &[0x11]);
        let s2 = cotp_dt(0x0001, true, 2, &[0x22]);
        let mut r = CotpReassembler::new();
        assert_eq!(r.push(&s0, 0.0), None);
        assert_eq!(r.push(&s2, 1.0), None);
    }

    #[test]
    fn clnp_cotp_tpdu_extracts_data_part() {
        // A complete CLNP DT exposes its COTP TPDU (the data part).
        let mut b = vec![0x81, 15, 1, 0x3F, 0x1C, 0x00, 0x14, 0x00, 0x00];
        b.extend_from_slice(&[2, 0x47, 0x01]);
        b.extend_from_slice(&[2, 0x47, 0x02]);
        let dt = cotp_dt(0x0005, true, 0, b"HI");
        b.extend_from_slice(&dt);
        assert_eq!(clnp_cotp_tpdu(&b), Some(&dt[..]));
    }
}
