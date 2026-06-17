//! STD-C packet layer: descriptor parsing, Fletcher-style checksum, EGC
//! and message-data assembly. Layouts per docs/notes/STDC.md.

use serde::Serialize;
use serde_json::json;

/// ISO 8473 / Fletcher-style checksum over a packet with its two
/// checksum bytes zeroed; returns (CB1, CB2) = the trailing two bytes.
pub fn checksum(packet_with_zeroed_cs: &[u8]) -> (u8, u8) {
    let mut c0: u32 = 0;
    let mut c1: u32 = 0;
    for &b in packet_with_zeroed_cs {
        c0 = (c0 + b as u32) & 0xFF;
        c1 = (c1 + c0) & 0xFF;
    }
    let cb1 = (c0 as u8).wrapping_sub(c1 as u8);
    let cb2 = (c1 as u8).wrapping_sub((c0 as u8).wrapping_mul(2));
    (cb1, cb2)
}

fn checksum_ok(packet: &[u8]) -> bool {
    let n = packet.len();
    if n < 3 {
        return false;
    }
    let rx = (packet[n - 2], packet[n - 1]);
    if rx == (0, 0) {
        return true; // accepted in re-encapsulated multiframe content
    }
    let mut zeroed = packet.to_vec();
    zeroed[n - 2] = 0;
    zeroed[n - 1] = 0;
    checksum(&zeroed) == rx
}

/// One parsed packet (descriptor + fields summarized as JSON details).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StdcPacket {
    pub descriptor: u8,
    pub name: &'static str,
    pub checksum_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub details: serde_json::Value,
    #[serde(skip_serializing)]
    pub raw: Vec<u8>,
}

fn sat_les(b: u8) -> serde_json::Value {
    let region = ["AOR-W", "AOR-E", "POR", "IOR"][(b >> 6) as usize];
    json!({ "region": region, "les": (b >> 6) as u16 * 100 + (b & 0x3F) as u16 })
}

/// IA5 text: one character per byte, top bit masked.
fn ia5(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            let c = (b & 0x7F) as char;
            if c.is_ascii_graphic() || c == ' ' || c == '\n' || c == '\r' {
                c
            } else {
                '·'
            }
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// EGC address length by service code.
fn egc_addr_len(service: u8) -> usize {
    match service {
        0x00 => 3,
        0x02 | 0x72 => 5,
        0x04 | 0x14 | 0x24 | 0x34 | 0x44 => 7,
        0x11 | 0x31 => 4,
        0x13 | 0x23 | 0x33 | 0x73 => 6,
        _ => 3,
    }
}

fn egc_service_name(service: u8) -> &'static str {
    match service {
        0x00 => "system/all-ships",
        0x02 => "fleetnet/group-call",
        0x04 => "safetynet/warning-rect",
        0x11 => "system/inmarsat",
        0x13 => "safetynet/coastal-warning",
        0x14 => "safetynet/distress-circ",
        0x23 => "system/egc",
        0x24 => "safetynet/warning-circ",
        0x31 => "safetynet/navarea-warning",
        0x33 => "system/download-group-id",
        0x34 => "safetynet/sar-rect",
        0x44 => "safetynet/sar-circ",
        0x72 => "fleetnet/chart-correction",
        0x73 => "safetynet/chart-correction",
        _ => "unknown",
    }
}

/// Canonical operator-friendly EGC service-code long name. Names follow
/// inmarsatc `getServiceCodeAndAddressName` (IMO SafetyNET manual).
pub fn egc_service_long_name(service: u8) -> Option<&'static str> {
    Some(match service {
        0x00 => "System, All ships (general call)",
        0x02 => "FleetNET, Group Call",
        0x04 => "SafetyNET, Navigational, Meteorological or Piracy Warning to a Rectangular Area",
        0x11 => "System, Inmarsat System Message",
        0x13 => "SafetyNET, Navigational, Meteorological or Piracy Coastal Warning",
        0x14 => "SafetyNET, Shore-to-Ship Distress Alert to Circular Area",
        0x23 => "System, EGC System Message",
        0x24 => "SafetyNET, Navigational, Meteorological or Piracy Warning to a Circular Area",
        0x31 => "SafetyNET, NAVAREA/METAREA Warning, MET Forecast or Piracy Warning to NAVAREA/METAREA",
        0x33 => "System, Download Group Identity",
        0x34 => "SafetyNET, SAR Coordination to a Rectangular Area",
        0x44 => "SafetyNET, SAR Coordination to a Circular Area",
        0x72 => "FleetNET, Chart Correction Service",
        0x73 => "SafetyNET, Chart Correction Service for Fixed Areas",
        _ => return None,
    })
}

/// STD-C TDM frame number (0..9999) → UTC time-of-day. The frame counter
/// resets at UTC midnight and one frame is exactly 8.64 s, so the whole
/// day is 86400 / 8.64 = 10000 frames. seconds_of_day = frame × 8.64,
/// formatted floored to whole-second "HH:MM:SS". Per docs/notes/STDC.md.
pub fn frame_to_utc_hms(frame_number: u16) -> Option<String> {
    if frame_number > 9999 {
        return None;
    }
    let sod = (frame_number as f64 * 8.64).floor() as u32;
    Some(format!(
        "{:02}:{:02}:{:02}",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    ))
}

const PRIORITY: [&str; 4] = ["routine", "safety", "urgency", "distress"];

/// Parsed EGC packet header + payload, pre-assembly.
#[derive(Debug, Clone)]
pub struct EgcPart {
    pub double_header_part: u8, // 0 = single (0xB0), 1 = part 1, 2 = part 2
    pub service: u8,
    pub continuation: bool,
    pub priority: u8,
    pub msg_seq: u16,
    pub pkt_seq: u8,
    pub presentation: u8,
    pub address: Vec<u8>,
    pub payload: Vec<u8>,
}

fn parse_egc(desc: u8, p: &[u8]) -> Option<EgcPart> {
    if p.len() < 10 {
        return None;
    }
    let service = p[2];
    let alen = egc_addr_len(service);
    if p.len() < 8 + alen + 2 {
        return None;
    }
    Some(EgcPart {
        double_header_part: match desc {
            0xB1 => 1,
            0xB2 => 2,
            _ => 0,
        },
        service,
        continuation: p[3] & 0x80 != 0,
        priority: (p[3] >> 5) & 0x3,
        msg_seq: u16::from_be_bytes([p[4], p[5]]),
        pkt_seq: p[6],
        presentation: p[7],
        address: p[8..8 + alen].to_vec(),
        payload: p[8 + alen..p.len() - 2].to_vec(),
    })
}

/// True when the payload is overwhelmingly printable 7-bit text — the
/// same pragmatic test inmarsatc applies (presentation codes are not
/// always trustworthy on air).
fn looks_textual(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let printable = payload
        .iter()
        .filter(|&&c| {
            let c = c & 0x7F;
            (0x20..0x7F).contains(&c) || c == b'\r' || c == b'\n' || c == 0x07
        })
        .count();
    printable * 100 >= payload.len() * 85
}

fn decode_payload(presentation: u8, payload: &[u8]) -> (Option<String>, serde_json::Value) {
    match presentation {
        0 => (Some(ia5(payload)), json!({})),
        // Unknown presentation codes: decode as IA5 when the bytes are
        // overwhelmingly printable, otherwise keep hex.
        _ if looks_textual(payload) => (
            Some(ia5(payload)),
            json!({ "presentation_heuristic": true }),
        ),
        _ => (None, json!({ "payload_hex": hex(payload) })),
    }
}

/// Walks the 640-byte frame body, validating and parsing packets;
/// maintains multiframe (0xBD/0xBE), EGC, and logical-channel assembly.
pub struct PacketParser {
    multiframe: Option<(usize, Vec<u8>)>,
    egc: Vec<EgcAssembly>,
    channels: Vec<LcnAssembly>,
}

struct EgcAssembly {
    msg_seq: u16,
    service: u8,
    priority: u8,
    presentation: u8,
    address: Vec<u8>,
    parts: Vec<(u16, Vec<u8>)>, // key = pkt_seq*2 + (part==2)
    age: u32,
}

struct LcnAssembly {
    lcn: u8,
    parts: Vec<(u8, Vec<u8>)>,
    age: u32,
}

impl PacketParser {
    pub fn new() -> Self {
        Self { multiframe: None, egc: Vec::new(), channels: Vec::new() }
    }

    /// Parse one decoded 640-byte frame; returns events (parsed packets
    /// plus completed EGC/LCN messages).
    pub fn parse_frame(&mut self, frame: &[u8]) -> Vec<StdcPacket> {
        for a in &mut self.egc {
            a.age += 1;
        }
        for c in &mut self.channels {
            c.age += 1;
        }
        self.egc.retain(|a| a.age < 8);
        self.channels.retain(|c| c.age < 8);
        let mut out = Vec::new();
        self.parse_stream(frame, &mut out);
        out
    }

    fn parse_stream(&mut self, buf: &[u8], out: &mut Vec<StdcPacket>) {
        let mut p = 0;
        while p < buf.len() {
            let desc = buf[p];
            if desc == 0x00 {
                break; // padding to end of frame
            }
            let len = if desc & 0x80 == 0 {
                (desc & 0x0F) as usize + 1
            } else if desc & 0x40 == 0 {
                if p + 1 >= buf.len() {
                    break;
                }
                buf[p + 1] as usize + 2
            } else {
                if p + 2 >= buf.len() {
                    break;
                }
                ((buf[p + 1] as usize) << 8 | buf[p + 2] as usize) + 3
            };
            if len < 3 || p + len > buf.len() {
                break;
            }
            let pkt = &buf[p..p + len];
            p += len;
            if !checksum_ok(pkt) {
                continue;
            }
            self.handle_packet(pkt, out);
        }
    }

    fn handle_packet(&mut self, pkt: &[u8], out: &mut Vec<StdcPacket>) {
        let desc = pkt[0];
        let body = &pkt[..pkt.len()];
        let (name, text, details): (&'static str, Option<String>, serde_json::Value) = match desc {
            0x7D if body.len() >= 4 => {
                let frame_number = u16::from_be_bytes([body[2], body[3]]);
                (
                    "bulletin-board",
                    None,
                    json!({
                        "network_version": body.get(1),
                        "frame_number": frame_number,
                        "utc_time": frame_to_utc_hms(frame_number),
                        "channel_type": body.get(6).map(|b| b >> 5),
                    }),
                )
            }
            0x27 if body.len() >= 8 => {
                // The clear terminates the logical channel: emit any
                // message-data assembled on that LCN.
                if let Some(done) = self.finish_lcn(body[5]) {
                    out.push(done);
                }
                ("logical-channel-clear", None, json!({
                    "mes_id": format!("{:02X}{:02X}{:02X}", body[1], body[2], body[3]),
                    "sat_les": sat_les(body[4]),
                    "lcn": body[5],
                }))
            }
            0x81 if body.len() >= 10 => ("announcement", None, json!({
                "mes_id": format!("{:02X}{:02X}{:02X}", body[2], body[3], body[4]),
                "sat_les": sat_les(body[5]),
                "lcn": body.get(9),
            })),
            0x83 if body.len() >= 8 => ("logical-channel-assignment", None, json!({
                "mes_id": format!("{:02X}{:02X}{:02X}", body[2], body[3], body[4]),
                "lcn": body.get(7),
            })),
            0xAA => {
                // Message data: assemble per logical channel.
                if body.len() >= 7 {
                    let lcn = body[3];
                    let seq = body[4];
                    let data = body[5..body.len() - 2].to_vec();
                    let idx = self.channels.iter().position(|c| c.lcn == lcn).unwrap_or_else(|| {
                        self.channels.push(LcnAssembly { lcn, parts: Vec::new(), age: 0 });
                        self.channels.len() - 1
                    });
                    let c = &mut self.channels[idx];
                    c.age = 0;
                    if !c.parts.iter().any(|(s, _)| *s == seq) {
                        c.parts.push((seq, data));
                    }
                }
                ("message-data", None, json!({
                    "sat_les": sat_les(body[2]),
                    "lcn": body[3],
                    "pkt_seq": body[4],
                }))
            }
            0xB0 | 0xB1 | 0xB2 => {
                if let Some(part) = parse_egc(desc, body) {
                    if let Some(done) = self.push_egc(part) {
                        out.push(done);
                    }
                }
                return; // EGC parts surface only as assembled messages
            }
            0xBD => {
                if body.len() > 4 {
                    let inner = &body[2..body.len() - 2];
                    if !inner.is_empty() {
                        let inner_len = if inner[0] & 0x80 == 0 {
                            (inner[0] & 0x0F) as usize + 1
                        } else if inner[0] & 0x40 == 0 {
                            inner.get(1).map(|&l| l as usize + 2).unwrap_or(0)
                        } else {
                            ((inner.get(1).copied().unwrap_or(0) as usize) << 8
                                | inner.get(2).copied().unwrap_or(0) as usize)
                                + 3
                        };
                        self.multiframe = Some((inner_len, inner.to_vec()));
                    }
                }
                return;
            }
            0xBE => {
                if let Some((need, acc)) = &mut self.multiframe {
                    if body.len() > 4 {
                        acc.extend_from_slice(&body[2..body.len() - 2]);
                    }
                    if acc.len() + 2 >= *need {
                        let (_, acc) = self.multiframe.take().unwrap();
                        self.parse_stream(&acc, out);
                    }
                }
                return;
            }
            0x92 => ("login-ack", None, json!({})),
            0xA8 => ("confirmation", None, json!({})),
            0xAB => ("les-list", None, json!({})),
            0x08 => ("ack-request", None, json!({})),
            0x6C => ("signalling-channel", None, json!({})),
            0x2A => ("inbound-message-ack", None, json!({})),
            0x91 => ("distress-alert-ack", None, json!({})),
            0x9A => ("enhanced-data-report-ack", None, json!({})),
            0xA0 => ("distress-test-request", None, json!({})),
            0xA3 if body.len() >= 8 => ("individual-poll", None, json!({
                "mes_id": format!("{:02X}{:02X}{:02X}", body[2], body[3], body[4]),
                "sat_les": sat_les(body[5]),
            })),
            0xAC => ("request-status", None, json!({})),
            0xAD => ("test-result", None, json!({})),
            _ => ("unknown", None, json!({ "hex": hex(body) })),
        };
        out.push(StdcPacket {
            descriptor: desc,
            name,
            checksum_ok: true,
            text,
            details,
            raw: pkt.to_vec(),
        });
    }

    /// Assemble and emit the message accumulated on a logical channel
    /// (called when the channel is cleared).
    fn finish_lcn(&mut self, lcn: u8) -> Option<StdcPacket> {
        let idx = self.channels.iter().position(|c| c.lcn == lcn)?;
        let mut c = self.channels.swap_remove(idx);
        if c.parts.is_empty() {
            return None;
        }
        c.parts.sort_by_key(|(s, _)| *s);
        let mut payload = Vec::new();
        for (_, p) in &c.parts {
            payload.extend_from_slice(p);
        }
        let (text, extra) = decode_payload(0xFF, &payload);
        let mut details = json!({
            "lcn": lcn,
            "parts": c.parts.len(),
        });
        if let (Some(obj), Some(eobj)) = (details.as_object_mut(), extra.as_object()) {
            for (k, v) in eobj {
                obj.insert(k.clone(), v.clone());
            }
        }
        Some(StdcPacket {
            descriptor: 0xAA,
            name: "message",
            checksum_ok: true,
            text,
            details,
            raw: payload,
        })
    }

    fn push_egc(&mut self, part: EgcPart) -> Option<StdcPacket> {
        let idx = self
            .egc
            .iter()
            .position(|a| a.msg_seq == part.msg_seq)
            .unwrap_or_else(|| {
                self.egc.push(EgcAssembly {
                    msg_seq: part.msg_seq,
                    service: part.service,
                    priority: part.priority,
                    presentation: part.presentation,
                    address: part.address.clone(),
                    parts: Vec::new(),
                    age: 0,
                });
                self.egc.len() - 1
            });
        let a = &mut self.egc[idx];
        a.age = 0;
        let key = part.pkt_seq as u16 * 2 + (part.double_header_part == 2) as u16;
        if !a.parts.iter().any(|(k, _)| *k == key) {
            a.parts.push((key, part.payload.clone()));
        }
        // Complete when a terminating part arrives (single header or
        // part 2) with continuation cleared.
        let terminal = !part.continuation && part.double_header_part != 1;
        if !terminal {
            return None;
        }
        let mut done = self.egc.swap_remove(idx);
        done.parts.sort_by_key(|(k, _)| *k);
        let mut payload = Vec::new();
        for (_, p) in &done.parts {
            payload.extend_from_slice(p);
        }
        let (text, extra) = decode_payload(done.presentation, &payload);
        let mut details = json!({
            "service": egc_service_name(done.service),
            "service_long": egc_service_long_name(done.service),
            "service_code": done.service,
            "priority": PRIORITY[done.priority as usize],
            "msg_seq": done.msg_seq,
            "address_hex": hex(&done.address),
            "parts": done.parts.len(),
        });
        if let (Some(obj), Some(eobj)) = (details.as_object_mut(), extra.as_object()) {
            for (k, v) in eobj {
                obj.insert(k.clone(), v.clone());
            }
        }
        Some(StdcPacket {
            descriptor: 0xB0,
            name: "egc-message",
            checksum_ok: true,
            text,
            details,
            raw: payload,
        })
    }
}

impl Default for PacketParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a packet with a valid checksum (testing/modulation).
pub fn build_packet(descriptor_and_body: &[u8]) -> Vec<u8> {
    let mut p = descriptor_and_body.to_vec();
    p.extend([0, 0]);
    let (cb1, cb2) = checksum(&p);
    let n = p.len();
    p[n - 2] = cb1;
    p[n - 1] = cb2;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_roundtrip() {
        let pkt = build_packet(&[0xAA, 0x09, 0x10, 0x02, 0x01, b'H', b'I']);
        assert!(checksum_ok(&pkt));
        let mut bad = pkt.clone();
        bad[4] ^= 1;
        assert!(!checksum_ok(&bad));
    }

    /// Build a medium-format EGC packet (0xB0/B1/B2).
    fn egc_packet(desc: u8, service: u8, cont: bool, prio: u8, seq: u16, pkt_no: u8, text: &[u8]) -> Vec<u8> {
        let alen = egc_addr_len(service);
        let mut body = vec![desc, 0u8, service, ((cont as u8) << 7) | (prio << 5) | 1];
        body.extend(seq.to_be_bytes());
        body.push(pkt_no);
        body.push(0); // IA5
        body.extend(std::iter::repeat(0xAB).take(alen));
        body.extend(text);
        body[1] = (body.len() + 2 - 2) as u8; // medium length = total-2
        build_packet(&body)
    }

    #[test]
    fn lcn_message_assembled_on_channel_clear() {
        let mut parser = PacketParser::new();
        // Two message-data packets on LCN 7 (out of order), then the
        // channel clear that terminates the message.
        // Medium format: b[1] = total length - 2 (incl. checksum).
        let p2 = build_packet(&[&[0xAA, 11, 0x65, 7, 1][..], b" WORLD"].concat());
        let p1 = build_packet(&[&[0xAA, 10, 0x65, 7, 0][..], b"HELLO"].concat());
        // Short format: total length = (desc & 0x0F) + 1 = 8 incl. checksum.
        let clear = build_packet(&[0x27, 0x01, 0x02, 0x03, 0x65, 7]);
        let frame: Vec<u8> = [p2, p1, clear].concat();
        let mut padded = frame.clone();
        padded.resize(640, 0);
        let out = parser.parse_frame(&padded);
        let msg = out.iter().find(|p| p.name == "message").expect("assembled message");
        assert_eq!(msg.text.as_deref(), Some("HELLO WORLD"));
        assert_eq!(msg.details["lcn"], 7);
        assert_eq!(msg.details["parts"], 2);
        assert!(out.iter().any(|p| p.name == "logical-channel-clear"));
    }

    #[test]
    fn individual_poll_fields() {
        let mut parser = PacketParser::new();
        // Medium format: b[1] = total length - 2.
        let poll = build_packet(&[0xA3, 8, 0xC1, 0x24, 0xBB, 0x55, 0x01, 0x03]);
        let mut padded = poll.clone();
        padded.resize(640, 0);
        let out = parser.parse_frame(&padded);
        let p = out.iter().find(|p| p.name == "individual-poll").expect("poll parses");
        assert_eq!(p.details["mes_id"], "C124BB");
    }

    #[test]
    fn egc_single_packet_message() {
        let mut p = PacketParser::new();
        let frame = egc_packet(0xB0, 0x31, false, 1, 777, 1, b"NAVAREA XII WARNING TEST");
        let events = p.parse_frame(&frame);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.name, "egc-message");
        assert_eq!(e.text.as_deref(), Some("NAVAREA XII WARNING TEST"));
        assert_eq!(e.details["priority"], "safety");
        assert_eq!(e.details["service"], "safetynet/navarea-warning");
        assert_eq!(e.details["msg_seq"], 777);
    }

    #[test]
    fn egc_multi_packet_assembly() {
        let mut p = PacketParser::new();
        let f1 = egc_packet(0xB0, 0x31, true, 0, 42, 1, b"PART ONE ");
        let f2 = egc_packet(0xB0, 0x31, false, 0, 42, 2, b"PART TWO");
        assert!(p.parse_frame(&f1).is_empty());
        let events = p.parse_frame(&f2);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text.as_deref(), Some("PART ONE PART TWO"));
        assert_eq!(events[0].details["parts"], 2);
    }

    #[test]
    fn multiple_packets_one_frame_with_padding() {
        let mut p = PacketParser::new();
        let mut frame = Vec::new();
        // Bulletin board: 0x7D is short format, total length 14.
        frame.extend(build_packet(&[0x7D, 1, 0x03, 0xE8, 0, 0, 1, 0x10, 0, 0, 0, 0]));
        frame.extend(egc_packet(0xB0, 0x00, false, 3, 9, 1, b"DISTRESS RELAY"));
        frame.extend([0u8; 32]); // padding
        let events = p.parse_frame(&frame);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "bulletin-board");
        assert_eq!(events[1].details["priority"], "distress");
    }

    #[test]
    fn frame_to_utc_matches_offair_oracle() {
        // The vendored off-air capture (tests/offair.rs) carries TDM frame
        // number 5987. 5987 × 8.64 = 51727.68 s → 14:22:07 UTC-of-day.
        assert_eq!(frame_to_utc_hms(5987).as_deref(), Some("14:22:07"));
        // Frame 0 = UTC midnight; the day is exactly 10000 frames.
        assert_eq!(frame_to_utc_hms(0).as_deref(), Some("00:00:00"));
        // 9999 is the last valid frame before the next midnight reset.
        // 9999 × 8.64 = 86391.36 s → 23:59:51.
        assert_eq!(frame_to_utc_hms(9999).as_deref(), Some("23:59:51"));
        assert_eq!(frame_to_utc_hms(10000), None);
        // One-hour boundary: 3600 / 8.64 = 416.66… frames.
        assert_eq!(frame_to_utc_hms(417).as_deref(), Some("01:00:02"));
    }

    #[test]
    fn bulletin_board_surfaces_utc_time() {
        let mut p = PacketParser::new();
        // 0x7D short, frame number 5987 = 0x1763 at body[2..4].
        let bb = build_packet(&[0x7D, 1, 0x17, 0x63, 0, 0, 1, 0x10, 0, 0, 0, 0]);
        let mut frame = bb;
        frame.resize(640, 0);
        let out = p.parse_frame(&frame);
        let b = out.iter().find(|p| p.name == "bulletin-board").unwrap();
        assert_eq!(b.details["frame_number"], 5987);
        assert_eq!(b.details["utc_time"], "14:22:07");
    }

    #[test]
    fn egc_service_long_names_match_inmarsatc() {
        // Verbatim from inmarsatc getServiceCodeAndAddressName (oracle).
        assert_eq!(
            egc_service_long_name(0x31),
            Some("SafetyNET, NAVAREA/METAREA Warning, MET Forecast or Piracy Warning to NAVAREA/METAREA")
        );
        assert_eq!(
            egc_service_long_name(0x14),
            Some("SafetyNET, Shore-to-Ship Distress Alert to Circular Area")
        );
        assert_eq!(egc_service_long_name(0x00), Some("System, All ships (general call)"));
        assert_eq!(egc_service_long_name(0x99), None);
    }

    #[test]
    fn egc_message_carries_service_long_name() {
        let mut p = PacketParser::new();
        let frame = egc_packet(0xB0, 0x31, false, 1, 777, 1, b"NAVAREA XII WARNING TEST");
        let e = &p.parse_frame(&frame)[0];
        assert_eq!(
            e.details["service_long"],
            "SafetyNET, NAVAREA/METAREA Warning, MET Forecast or Piracy Warning to NAVAREA/METAREA"
        );
    }

    #[test]
    fn bad_checksum_packet_skipped() {
        let mut p = PacketParser::new();
        let mut frame = egc_packet(0xB0, 0x31, false, 1, 5, 1, b"GOOD");
        let n = frame.len();
        frame[n - 1] ^= 0xFF;
        assert!(p.parse_frame(&frame).is_empty());
    }
}
