//! HFDL PDU layer: SPDU (squitters), MPDU/LPDU, HFNPDU (incl. enveloped
//! ACARS), per docs/notes/HFDL.md. All FCS = CRC-16/X-25, LE trailer.

use serde::Serialize;
use serde_json::json;
use xng_acars::block::AcarsBlock;
use xng_dsp::checksum::HDLC_FCS;

fn fcs_ok(span: &[u8], trailer: &[u8]) -> bool {
    trailer.len() >= 2 && HDLC_FCS.checksum(span) == u16::from_le_bytes([trailer[0], trailer[1]])
}

/// ARINC HFDL ground-station names (public station list, as used by
/// the HFDL community and the over-the-air system table assignments).
pub fn gs_name(id: u8) -> Option<&'static str> {
    Some(match id {
        1 => "San Francisco, USA",
        2 => "Molokai, Hawaii",
        3 => "Reykjavik, Iceland",
        4 => "Riverhead, New York",
        5 => "Auckland, New Zealand",
        6 => "Hat Yai, Thailand",
        7 => "Shannon, Ireland",
        8 => "Johannesburg, South Africa",
        9 => "Barrow, Alaska",
        10 => "Muan, South Korea",
        11 => "Albrook, Panama",
        13 => "Santa Cruz, Bolivia",
        14 => "Krasnoyarsk, Russia",
        15 => "Al Muharraq, Bahrain",
        16 => "Agana, Guam",
        17 => "Canarias, Spain",
        _ => return None,
    })
}

/// 20-bit two's-complement coordinate, degrees ×180/2^19.
fn coordinate(v: u32) -> f64 {
    let r = ((v << 12) as i32) >> 12;
    r as f64 * 180.0 / (1 << 19) as f64
}

/// Performance-data "last frequency change cause" code → description
/// (dumphfdl hfnpdu.c `freq_change_code_descriptions`, facts only).
fn freq_change_cause(code: u8) -> &'static str {
    match code {
        0 => "First freq. search in this flight leg",
        1 => "Too many NACKs",
        2 => "SPDUs no longer received",
        3 => "HFDL disabled",
        4 => "GS frequency change",
        5 => "GS down / channel down",
        6 => "Poor uplink channel quality",
        7 => "No change",
        _ => "unknown",
    }
}

/// HFNPDU type byte → description (dumphfdl hfnpdu.c
/// `hfnpdu_type_descriptions`, facts only).
fn hfnpdu_type_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0xD0 => "System table (partial)",
        0xD1 => "Performance data",
        0xD2 => "System table request",
        0xD5 => "Frequency data",
        0xDE => "Delayed echo",
        0xFF => "Enveloped data",
        _ => return None,
    })
}

/// LPDU type byte → description (dumphfdl lpdu.c
/// `lpdu_type_descriptions`, facts only).
fn lpdu_type_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0x0D => "Unnumbered data",
        0x1D => "Unnumbered ack'ed data",
        0x2F => "Logon denied",
        0x3F => "Logoff request",
        0x5F => "Logon resume confirm",
        0x4F => "Logon resume",
        0x8F => "Logon request (normal)",
        0x9F => "Logon confirm",
        0xBF => "Logon request (DLS)",
        _ => return None,
    })
}

/// Logoff-request reason code → text (dumphfdl `logoff_request_reason_codes`).
fn logoff_reason(code: u8) -> &'static str {
    match code {
        0x01 => "Not within slot boundaries",
        0x02 => "Downlink set in uplink slot",
        0x03 => "RLS protocol error",
        0x04 => "Invalid aircraft ID",
        0x05 => "HFDL Ground Station subsystem does not support RLS",
        0x06 => "Other",
        _ => "Reserved",
    }
}

/// Logon-denied reason code → text (dumphfdl `logon_denied_reason_codes`).
fn logon_denied_reason(code: u8) -> &'static str {
    match code {
        0x01 => "Aircraft ID not available",
        0x02 => "HFDL Ground Station subsystem does not support RLS",
        _ => "Reserved",
    }
}

/// Decode the HFNPDU UTC seconds-of-day counter (raw value is half-seconds)
/// into {hour, min, sec}. Mirrors dumphfdl `parse_utc_time(2 * raw)`.
fn utc_hms(raw: u16) -> (u32, u32, u32) {
    let t = raw as u32 * 2;
    (t / 3600, (t % 3600) / 60, t % 60)
}

/// Four per-bitrate MPDU counters {300,600,1200,1800} from a 4-byte run.
fn mpdu_stats(b: &[u8]) -> serde_json::Value {
    json!({ "300bps": b[3], "600bps": b[2], "1200bps": b[1], "1800bps": b[0] })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HfdlEvent {
    pub kind: String,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acars: Option<AcarsBlock>,
    /// Coded symbols the Viterbi decoder corrected for the burst this event
    /// came from (HFDL-5). Set by the demod path; `None` for events built
    /// directly from bytes (tests, reassembled system tables).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fec_corrected: Option<u32>,
    #[serde(skip_serializing)]
    pub raw: Vec<u8>,
}

pub struct PduParser {
    systable: crate::systable::SystableAssembler,
    ac_cache: crate::ac_cache::AcCache,
}

impl PduParser {
    pub fn new() -> Self {
        Self {
            systable: crate::systable::SystableAssembler::new(),
            ac_cache: crate::ac_cache::AcCache::new(),
        }
    }

    /// Construct a parser with a custom aircraft-ID→ICAO cache TTL
    /// (dumphfdl `--aircraft-cache-ttl`; default 3600 s).
    pub fn with_ac_cache_ttl(ttl: std::time::Duration) -> Self {
        Self {
            systable: crate::systable::SystableAssembler::new(),
            ac_cache: crate::ac_cache::AcCache::with_ttl(ttl),
        }
    }

    /// Resolve a channel-local downlink aircraft ID to its cached ICAO,
    /// if a logon-confirm has been heard within the cache TTL.
    pub fn resolve_icao(&self, ac_id: u8) -> Option<&str> {
        self.ac_cache.lookup(ac_id)
    }

    /// Parse one decoded burst payload (one SPDU or MPDU + padding).
    pub fn parse(&mut self, payload: &[u8], bps: u32) -> Vec<HfdlEvent> {
        let mut out = Vec::new();
        if payload.is_empty() {
            return out;
        }
        if payload[0] & 1 == 0 {
            self.parse_spdu(payload, bps, &mut out);
        } else {
            self.parse_mpdu(payload, bps, &mut out);
        }
        out
    }

    fn parse_spdu(&mut self, p: &[u8], bps: u32, out: &mut Vec<HfdlEvent>) {
        if p.len() < 66 || !fcs_ok(&p[..64], &p[64..66]) {
            return;
        }
        let freq_bitmap = |a: u8, b: u8, c: u8| -> u32 {
            // 20 bits starting at a's high nibble, LSB-first fields.
            (a >> 4) as u32 | (b as u32) << 4 | (c as u32) << 12
        };
        out.push(HfdlEvent {
            kind: "squitter".into(),
            details: json!({
                "gs_id": p[1] & 0x7F,
                "gs_name": gs_name(p[1] & 0x7F),
                "utc_sync": p[1] >> 7 == 1,
                "frame_index": (p[2] as u16) | ((p[3] as u16 & 0x0F) << 8),
                "frame_offset": p[3] >> 4,
                "change_note": (p[0] >> 6) & 0x3,
                "min_priority": p[52] & 0x0F,
                "systable_version": (p[53] as u16) | ((p[54] as u16 & 0x0F) << 8),
                "freqs_in_use": freq_bitmap(p[54], p[55], p[56]),
                "neighbor2": { "gs_id": p[57] & 0x7F, "freqs": freq_bitmap(p[58] << 4 | p[58] >> 4, p[59], p[60]) },
                "neighbor3_gs_id": (p[60] >> 4) as u16 | ((p[61] as u16 & 0x7) << 4),
            }),
            acars: None,
            fec_corrected: None,
            raw: p[..66].to_vec(),
        });
        let _ = bps;
    }

    fn parse_mpdu(&mut self, p: &[u8], bps: u32, out: &mut Vec<HfdlEvent>) {
        let downlink = p[0] & 2 != 0;
        // Collect (header_len, lpdu_sizes, src/dst ids).
        let (hdr_len, sizes, who): (usize, Vec<usize>, serde_json::Value) = if downlink {
            let n = ((p[0] >> 2) & 0x0F) as usize;
            let hdr = 6 + n;
            if p.len() < hdr + 2 {
                return;
            }
            let sizes = (0..n).map(|i| p[6 + i] as usize + 1).collect();
            (hdr, sizes, json!({ "dir": "downlink", "gs_id": p[1] & 0x7F, "gs_name": gs_name(p[1] & 0x7F), "aircraft_id": p[2] }))
        } else {
            let n_ac = ((p[0] >> 4) & 0x7) as usize + 1;
            let mut sizes = Vec::new();
            let mut idx = 2;
            let mut acs = Vec::new();
            for _ in 0..n_ac {
                if idx + 2 > p.len() {
                    return;
                }
                let ac_id = p[idx];
                let count = (p[idx + 1] >> 4) as usize;
                idx += 2;
                if idx + count > p.len() {
                    return;
                }
                for k in 0..count {
                    sizes.push(p[idx + k] as usize + 1);
                }
                idx += count;
                acs.push(json!({ "aircraft_id": ac_id, "lpdus": count }));
            }
            (idx, sizes, json!({ "dir": "uplink", "gs_id": p[1] & 0x7F, "gs_name": gs_name(p[1] & 0x7F), "aircraft": acs }))
        };
        if p.len() < hdr_len + 2 || !fcs_ok(&p[..hdr_len], &p[hdr_len..]) {
            return;
        }
        let mut off = hdr_len + 2;
        for size in sizes {
            if off + size > p.len() {
                break;
            }
            self.parse_lpdu(&p[off..off + size], &who, bps, out);
            off += size;
        }
    }

    fn parse_lpdu(&mut self, l: &[u8], who: &serde_json::Value, bps: u32, out: &mut Vec<HfdlEvent>) {
        if l.len() < 3 || !fcs_ok(&l[..l.len() - 2], &l[l.len() - 2..]) {
            return;
        }
        let body = &l[..l.len() - 2];
        let icao = |b: &[u8]| -> String {
            // ICAO bytes are MSB-first in raw air order: re-reverse.
            let rev = |x: u8| x.reverse_bits();
            format!("{:02X}{:02X}{:02X}", rev(b[0]), rev(b[1]), rev(b[2]))
        };
        // For downlink LPDUs, augment `who` with the ICAO resolved from
        // the channel-local aircraft-ID cache (HFDL-3), mirroring dumphfdl
        // which back-fills the ICAO from its ac_cache for non-logon
        // downlinks. Logon LPDUs in this same burst are processed first
        // and have already updated the cache by the time data follows.
        let resolved = |this: &Self, who: &serde_json::Value| -> serde_json::Value {
            if who.get("dir").and_then(|d| d.as_str()) == Some("downlink") {
                if let Some(ac_id) = who.get("aircraft_id").and_then(|v| v.as_u64()) {
                    if let Some(found) = this.ac_cache.lookup(ac_id as u8) {
                        let mut w = who.clone();
                        w["icao"] = serde_json::Value::String(found.to_string());
                        return w;
                    }
                }
            }
            who.clone()
        };
        match body[0] {
            0x0D | 0x1D => {
                let who = resolved(self, who);
                self.parse_hfnpdu(&body[1..], &who, bps, out);
            }
            0x8F | 0xBF if body.len() >= 4 => out.push(HfdlEvent {
                kind: "logon-request".into(),
                details: json!({ "icao": icao(&body[1..4]), "who": who }),
                acars: None,
                fec_corrected: None,
                raw: l.to_vec(),
            }),
            0x4F if body.len() >= 4 => out.push(HfdlEvent {
                kind: "logon-resume".into(),
                details: json!({ "icao": icao(&body[1..4]), "who": who }),
                acars: None,
                fec_corrected: None,
                raw: l.to_vec(),
            }),
            0x9F | 0x5F if body.len() >= 5 => {
                // Logon (resume) confirm: bind the assigned channel-local
                // aircraft ID to this ICAO in the cache (HFDL-3).
                let icao_str = icao(&body[1..4]);
                self.ac_cache.insert(body[4], &icao_str);
                out.push(HfdlEvent {
                    kind: "logon-confirm".into(),
                    details: json!({ "icao": icao_str, "assigned_id": body[4], "who": who }),
                    acars: None,
                    fec_corrected: None,
                    raw: l.to_vec(),
                });
            }
            0x3F if body.len() >= 5 => {
                // Logoff: the aircraft has left this channel — drop its
                // cached ID→ICAO mapping (HFDL-3).
                let icao_str = icao(&body[1..4]);
                self.ac_cache.remove_by_icao(&icao_str);
                out.push(HfdlEvent {
                    kind: "logoff-request".into(),
                    details: json!({
                        "icao": icao_str,
                        "reason": body[4],
                        "reason_text": logoff_reason(body[4]),
                        "who": who,
                    }),
                    acars: None,
                    fec_corrected: None,
                    raw: l.to_vec(),
                });
            }
            0x2F if body.len() >= 5 => {
                // Logon denied: ICAO + reason, same layout as logoff
                // (dumphfdl lpdu.c LOGON_DENIED → logoff_request_parse).
                // Drop any cached mapping for this ICAO (HFDL-3).
                let icao_str = icao(&body[1..4]);
                self.ac_cache.remove_by_icao(&icao_str);
                out.push(HfdlEvent {
                    kind: "logon-denied".into(),
                    details: json!({
                        "icao": icao_str,
                        "reason": body[4],
                        "reason_text": logon_denied_reason(body[4]),
                        "who": who,
                    }),
                    acars: None,
                    fec_corrected: None,
                    raw: l.to_vec(),
                });
            }
            t => out.push(HfdlEvent {
                kind: "lpdu".into(),
                details: json!({ "type": t, "type_name": lpdu_type_name(t), "who": who }),
                acars: None,
                fec_corrected: None,
                raw: l.to_vec(),
            }),
        }
    }

    fn parse_hfnpdu(&mut self, h: &[u8], who: &serde_json::Value, bps: u32, out: &mut Vec<HfdlEvent>) {
        // Never drop a CRC-valid data LPDU on the floor: when the HFNPDU
        // contents don't parse, emit the envelope with the payload hex
        // (dumphfdl-equivalent behaviour; silent drops cost 4+ frames on
        // the bench capture).
        let envelope = |out: &mut Vec<HfdlEvent>| {
            out.push(HfdlEvent {
                kind: "unnumbered-data".into(),
                details: json!({
                    "who": who,
                    "data_hex": h.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                }),
                acars: None,
                fec_corrected: None,
                raw: h.to_vec(),
            });
        };
        if h.len() < 2 || h[0] != 0xFF {
            envelope(out);
            return;
        }
        match h[1] {
            0xFF => {
                // Enveloped ACARS: SOH-prefixed parity-bearing block.
                if let Some(b) = xng_acars::block::parse(&h[2..]) {
                    out.push(HfdlEvent {
                        kind: "acars".into(),
                        details: json!({ "who": who, "bps": bps }),
                        acars: Some(b),
                        fec_corrected: None,
                        raw: h.to_vec(),
                    });
                } else {
                    envelope(out);
                }
            }
            0xD1 if h.len() >= 47 => {
                // Performance data — full 47-octet record (dumphfdl
                // hfnpdu.c performance_data_parse, facts only).
                let flight: String =
                    h[2..8].iter().map(|&c| (c & 0x7F) as char).collect();
                let lat = coordinate(
                    h[8] as u32 | (h[9] as u32) << 8 | ((h[10] as u32 & 0x0F) << 16),
                );
                let lon = coordinate(
                    (h[10] as u32 >> 4) | (h[11] as u32) << 4 | (h[12] as u32) << 12,
                );
                let (hour, min, sec) = utc_hms(u16::from_le_bytes([h[13], h[14]]));
                let freq_change_code = h[46] & 0x0F;
                out.push(HfdlEvent {
                    kind: "performance-data".into(),
                    details: json!({
                        "flight": flight.trim().to_string(),
                        "lat": lat, "lon": lon,
                        "utc_s": u16::from_le_bytes([h[13], h[14]]) as u32 * 2,
                        "utc": { "hour": hour, "min": min, "sec": sec },
                        "version": h[15],
                        "flight_leg": h[16],
                        "gs_id": h[17] & 0x7F,
                        "gs_name": gs_name(h[17] & 0x7F),
                        "freq_id": h[18],
                        "freq_search_cnt": {
                            "prev_leg": u16::from_le_bytes([h[19], h[20]]),
                            "cur_leg": u16::from_le_bytes([h[21], h[22]]),
                        },
                        "hfdl_disabled_duration": {
                            "prev_leg": u16::from_le_bytes([h[23], h[24]]),
                            "cur_leg": u16::from_le_bytes([h[25], h[26]]),
                        },
                        "mpdus_rx": mpdu_stats(&h[27..31]),
                        "mpdus_rx_errs": mpdu_stats(&h[31..35]),
                        "spdus_rx": u16::from_le_bytes([h[35], h[36]]),
                        "spdus_rx_errs": h[37],
                        "mpdus_tx": mpdu_stats(&h[38..42]),
                        "mpdus_delivered": mpdu_stats(&h[42..46]),
                        "freq_change_code": freq_change_code,
                        "freq_change_cause": freq_change_cause(freq_change_code),
                        "who": who,
                    }),
                    acars: None,
                    fec_corrected: None,
                    raw: h.to_vec(),
                });
            }
            0xD5 if h.len() >= 15 => {
                let flight: String =
                    h[2..8].iter().map(|&c| (c & 0x7F) as char).collect();
                let lat = coordinate(
                    h[8] as u32 | (h[9] as u32) << 8 | ((h[10] as u32 & 0x0F) << 16),
                );
                let lon = coordinate(
                    (h[10] as u32 >> 4) | (h[11] as u32) << 4 | (h[12] as u32) << 12,
                );
                let (hour, min, sec) = utc_hms(u16::from_le_bytes([h[13], h[14]]));
                // Up to 6 per-GS {gs_id, prop_freqs, tuned_freqs} records,
                // 6 octets each, starting at offset 15 (dumphfdl
                // frequency_data_parse, facts only).
                let mut freq_data = Vec::new();
                for f in 0..6usize {
                    let pos = 15 + f * 6;
                    if pos + 6 > h.len() {
                        break;
                    }
                    let prop_freqs = h[pos + 1] as u32
                        | (h[pos + 2] as u32) << 8
                        | ((h[pos + 3] as u32 & 0x0F) << 16);
                    let tuned_freqs = (h[pos + 3] as u32 >> 4)
                        | (h[pos + 4] as u32) << 4
                        | (h[pos + 5] as u32) << 12;
                    freq_data.push(json!({
                        "gs_id": h[pos] & 0x7F,
                        "gs_name": gs_name(h[pos] & 0x7F),
                        "prop_freqs": prop_freqs,
                        "tuned_freqs": tuned_freqs,
                    }));
                }
                out.push(HfdlEvent {
                    kind: "frequency-data".into(),
                    details: json!({
                        "flight": flight.trim().to_string(),
                        "lat": lat, "lon": lon,
                        "utc_s": u16::from_le_bytes([h[13], h[14]]) as u32 * 2,
                        "utc": { "hour": hour, "min": min, "sec": sec },
                        "freq_data": freq_data,
                        "who": who,
                    }),
                    acars: None,
                    fec_corrected: None,
                    raw: h.to_vec(),
                });
            }
            0xD0 if h.len() >= 5 => {
                let (seq, total) = (h[2] & 0x0F, (h[2] >> 4) + 1);
                let version = (h[3] as u16 >> 4) | ((h[4] as u16) << 4);
                out.push(HfdlEvent {
                    kind: "systable-partial".into(),
                    details: json!({ "seq": seq, "total": total, "version": version }),
                    acars: None,
                    fec_corrected: None,
                    raw: h.to_vec(),
                });
                if let Some(table) = self.systable.store(version, seq, total, &h[5..]) {
                    out.push(HfdlEvent {
                        kind: "systable-complete".into(),
                        details: serde_json::to_value(&table).unwrap_or_default(),
                        acars: None,
                        fec_corrected: None,
                        raw: Vec::new(),
                    });
                }
            }
            0xD2 if h.len() >= 4 => {
                // System table request: 16-bit request_data at offset 2
                // (dumphfdl systable_request_parse, facts only).
                out.push(HfdlEvent {
                    kind: "systable-request".into(),
                    details: json!({
                        "request_data": u16::from_le_bytes([h[2], h[3]]),
                        "who": who,
                    }),
                    acars: None,
                    fec_corrected: None,
                    raw: h.to_vec(),
                });
            }
            0xDE => {
                // Delayed echo: dumphfdl carries no body for this type.
                out.push(HfdlEvent {
                    kind: "delayed-echo".into(),
                    details: json!({ "who": who }),
                    acars: None,
                    fec_corrected: None,
                    raw: h.to_vec(),
                });
            }
            t => out.push(HfdlEvent {
                kind: "hfnpdu".into(),
                details: json!({
                    "type": t,
                    "type_name": hfnpdu_type_name(t),
                    "who": who,
                }),
                acars: None,
                fec_corrected: None,
                raw: h.to_vec(),
            }),
        }
    }
}

impl Default for PduParser {
    fn default() -> Self {
        Self::new()
    }
}

// ── builders (testing/modulation) ───────────────────────────────────────

fn with_fcs(mut v: Vec<u8>) -> Vec<u8> {
    let fcs = HDLC_FCS.checksum(&v);
    v.extend(fcs.to_le_bytes());
    v
}

/// Minimal squitter (66 octets).
pub fn build_spdu(gs_id: u8, frame_index: u16, systable_version: u16) -> Vec<u8> {
    let mut p = vec![0u8; 64];
    p[0] = 0b0000_0100; // SPDU, version 1
    p[1] = gs_id & 0x7F | 0x80;
    p[2] = (frame_index & 0xFF) as u8;
    p[3] = ((frame_index >> 8) & 0x0F) as u8;
    p[53] = (systable_version & 0xFF) as u8;
    p[54] = ((systable_version >> 8) & 0x0F) as u8 | 0x10; // freq bit 0
    with_fcs(p)
}

/// Unnumbered-data LPDU carrying an arbitrary HFNPDU.
pub fn build_lpdu_hfnpdu(hfnpdu: &[u8]) -> Vec<u8> {
    let mut l = vec![0x0D];
    l.extend_from_slice(hfnpdu);
    with_fcs(l)
}

/// LPDU carrying an enveloped ACARS HFNPDU.
pub fn build_lpdu_acars(acars_block: &[u8]) -> Vec<u8> {
    let mut l = vec![0x0D, 0xFF, 0xFF];
    l.extend_from_slice(acars_block);
    with_fcs(l)
}

/// Downlink MPDU wrapping LPDUs.
pub fn build_mpdu_downlink(gs_id: u8, aircraft_id: u8, lpdus: &[Vec<u8>]) -> Vec<u8> {
    let mut p = vec![
        0b0000_0011 | ((lpdus.len() as u8 & 0x0F) << 2),
        gs_id & 0x7F,
        aircraft_id,
        0,
        0,
        0,
    ];
    for l in lpdus {
        p.push((l.len() - 1) as u8);
    }
    let mut p = with_fcs(p);
    for l in lpdus {
        p.extend_from_slice(l);
    }
    p
}

/// Uplink MPDU from one GS to a single aircraft, wrapping LPDUs.
pub fn build_mpdu_uplink(gs_id: u8, aircraft_id: u8, lpdus: &[Vec<u8>]) -> Vec<u8> {
    // bit1=0 (uplink); n_ac field ((p[0]>>4)&0x7) = 0 -> one aircraft.
    let mut p = vec![0b0000_0001, gs_id & 0x7F, aircraft_id, (lpdus.len() as u8) << 4];
    for l in lpdus {
        p.push((l.len() - 1) as u8);
    }
    let mut p = with_fcs(p);
    for l in lpdus {
        p.extend_from_slice(l);
    }
    p
}

/// Logon-confirm LPDU (uplink GS→AC): binds `assigned_id` to `icao`.
pub fn build_lpdu_logon_confirm(icao: u32, assigned_id: u8) -> Vec<u8> {
    let rev = |x: u8| x.reverse_bits();
    let b = icao.to_be_bytes();
    let body = vec![0x9F, rev(b[1]), rev(b[2]), rev(b[3]), assigned_id];
    with_fcs(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spdu_roundtrip() {
        let s = build_spdu(7, 1234, 52);
        let mut parser = PduParser::new();
        let ev = parser.parse(&s, 300);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "squitter");
        assert_eq!(ev[0].details["gs_id"], 7);
        assert_eq!(ev[0].details["frame_index"], 1234);
        assert_eq!(ev[0].details["systable_version"], 52);
    }

    #[test]
    fn mpdu_with_acars_roundtrip() {
        let block = xng_acars::block::build(
            '2', "N471XG", None, "B6", '4', Some("M11A"), Some("UA0042"),
            "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
            false,
        );
        let mpdu = build_mpdu_downlink(3, 0xC7, &[build_lpdu_acars(&block)]);
        let mut parser = PduParser::new();
        let ev = parser.parse(&mpdu, 1800);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "acars");
        let b = ev[0].acars.as_ref().unwrap();
        assert!(b.crc_ok);
        assert_eq!(b.core.label, "B6");
        assert_eq!(b.core.app.as_ref().unwrap()["app"], "adsc");
    }

    #[test]
    fn systable_reassembles_across_mpdus() {
        let stations = vec![crate::systable::GroundStation {
            gs_id: 4,
            utc_sync: true,
            lat: 40.88,
            lon: -72.64,
            spdu_version: 2,
            frequencies: vec![
                crate::systable::GsFrequency { freq_hz: 21_931_000, master_frame_slot: 2 },
                crate::systable::GsFrequency { freq_hz: 11_387_000, master_frame_slot: 5 },
            ],
        }];
        let body = crate::systable::build_gs_record(&stations[0]);
        let pdus = crate::systable::build_systable_hfnpdus(77, &body, 2);
        let mut parser = PduParser::new();
        let ev1 = parser.parse(&build_mpdu_downlink(4, 0xC7, &[build_lpdu_hfnpdu(&pdus[0])]), 300);
        assert_eq!(ev1.len(), 1);
        assert_eq!(ev1[0].kind, "systable-partial");
        let ev2 = parser.parse(&build_mpdu_downlink(4, 0xC7, &[build_lpdu_hfnpdu(&pdus[1])]), 300);
        assert_eq!(ev2.len(), 2);
        assert_eq!(ev2[1].kind, "systable-complete");
        assert_eq!(ev2[1].details["version"], 77);
        assert_eq!(ev2[1].details["stations"][0]["gs_id"], 4);
        assert_eq!(ev2[1].details["stations"][0]["frequencies"][0]["freq_hz"], 21_931_000);
    }

    #[test]
    fn corrupted_mpdu_header_rejected() {
        let mut mpdu = build_mpdu_downlink(3, 0xC7, &[build_lpdu_acars(b"\x01dummy")]);
        mpdu[1] ^= 0x01;
        assert!(PduParser::new().parse(&mpdu, 300).is_empty());
    }

    /// Parse an HFNPDU body (starting with the 0xFF envelope byte) by
    /// wrapping it in an unnumbered-data LPDU and a downlink MPDU, then
    /// return the non-systable event(s).
    fn parse_hfnpdu_body(hfnpdu: &[u8]) -> Vec<HfdlEvent> {
        let mpdu = build_mpdu_downlink(4, 0xC7, &[build_lpdu_hfnpdu(hfnpdu)]);
        PduParser::new().parse(&mpdu, 300)
    }

    // ── HFDL-1.1 performance-data (0xD1) ────────────────────────────────
    //
    // The 47-octet field layout below is pinned to dumphfdl 1.7.0's
    // performance_data_parse() (src/hfnpdu.c) — the GPL oracle is read for
    // facts only; the test encodes its exact byte offsets so a regression
    // in our parser shows up as a mismatch against the reference layout.
    #[test]
    fn performance_data_full_record() {
        // dumphfdl offsets are relative to the 0xFF byte: buf[0]=0xFF,
        // buf[1]=0xD1, flight_id at 2..8, coord at 8..13, utc at 13..15,
        // version=15, flight_leg=16, gs_id=17&0x7F, freq_id=18, ...
        let mut h = vec![0u8; 47];
        h[0] = 0xFF;
        h[1] = 0xD1;
        h[2..8].copy_from_slice(b"UA0042");
        // lat ~= +40 deg, lon ~= -73 deg (20-bit signed, x180/2^19).
        let lat_raw: u32 = (40.0_f64 * (1u32 << 19) as f64 / 180.0).round() as u32 & 0xFFFFF;
        let lon_raw: u32 =
            ((-73.0_f64 * (1u32 << 19) as f64 / 180.0).round() as i32 as u32) & 0xFFFFF;
        h[8] = (lat_raw & 0xFF) as u8;
        h[9] = ((lat_raw >> 8) & 0xFF) as u8;
        h[10] = ((lat_raw >> 16) & 0x0F) as u8 | (((lon_raw & 0x0F) as u8) << 4);
        h[11] = ((lon_raw >> 4) & 0xFF) as u8;
        h[12] = ((lon_raw >> 12) & 0xFF) as u8;
        // utc raw counts half-seconds: 0x2A30 * 2 s = 21600 s = 06:00:00.
        let utc_raw: u16 = 10800;
        h[13] = (utc_raw & 0xFF) as u8;
        h[14] = (utc_raw >> 8) as u8;
        h[15] = 3; // version
        h[16] = 7; // flight_leg
        h[17] = 0x84; // gs_id 4 with high bit set -> masked to 4
        h[18] = 2; // freq_id
        h[19..21].copy_from_slice(&11u16.to_le_bytes()); // prev_leg freq_search_cnt
        h[21..23].copy_from_slice(&5u16.to_le_bytes()); // cur_leg freq_search_cnt
        h[23..25].copy_from_slice(&300u16.to_le_bytes()); // prev disabled dur
        h[25..27].copy_from_slice(&60u16.to_le_bytes()); // cur disabled dur
        // mpdus_rx: 1800=27, 1200=28, 600=29, 300=30.
        h[27] = 18;
        h[28] = 12;
        h[29] = 6;
        h[30] = 3;
        // mpdus_rx_errs 31..35.
        h[31] = 1;
        h[32] = 2;
        h[33] = 3;
        h[34] = 4;
        h[35..37].copy_from_slice(&1000u16.to_le_bytes()); // spdus_rx
        h[37] = 9; // spdus_rx_errs
        // mpdus_tx 38..42.
        h[38] = 80;
        h[39] = 70;
        h[40] = 60;
        h[41] = 50;
        // mpdus_delivered 42..46.
        h[42] = 8;
        h[43] = 7;
        h[44] = 6;
        h[45] = 5;
        h[46] = 0x04; // freq_change_code 4 -> "GS frequency change"

        let ev = parse_hfnpdu_body(&h);
        let e = ev.iter().find(|e| e.kind == "performance-data").expect("perf-data");
        let d = &e.details;
        assert_eq!(d["flight"], "UA0042");
        assert_eq!(d["version"], 3);
        assert_eq!(d["flight_leg"], 7);
        assert_eq!(d["gs_id"], 4);
        assert_eq!(d["gs_name"], "Riverhead, New York");
        assert_eq!(d["freq_id"], 2);
        assert_eq!(d["utc"]["hour"], 6);
        assert_eq!(d["utc"]["min"], 0);
        assert_eq!(d["utc"]["sec"], 0);
        assert_eq!(d["freq_search_cnt"]["prev_leg"], 11);
        assert_eq!(d["freq_search_cnt"]["cur_leg"], 5);
        assert_eq!(d["hfdl_disabled_duration"]["prev_leg"], 300);
        assert_eq!(d["hfdl_disabled_duration"]["cur_leg"], 60);
        assert_eq!(d["mpdus_rx"]["1800bps"], 18);
        assert_eq!(d["mpdus_rx"]["1200bps"], 12);
        assert_eq!(d["mpdus_rx"]["600bps"], 6);
        assert_eq!(d["mpdus_rx"]["300bps"], 3);
        assert_eq!(d["mpdus_rx_errs"]["1800bps"], 1);
        assert_eq!(d["mpdus_rx_errs"]["300bps"], 4);
        assert_eq!(d["spdus_rx"], 1000);
        assert_eq!(d["spdus_rx_errs"], 9);
        assert_eq!(d["mpdus_tx"]["1800bps"], 80);
        assert_eq!(d["mpdus_tx"]["300bps"], 50);
        assert_eq!(d["mpdus_delivered"]["1800bps"], 8);
        assert_eq!(d["mpdus_delivered"]["300bps"], 5);
        assert_eq!(d["freq_change_code"], 4);
        assert_eq!(d["freq_change_cause"], "GS frequency change");
        // Coordinates within the 20-bit quantization of the packed bytes.
        assert!((d["lat"].as_f64().unwrap() - 40.0).abs() < 0.001);
        assert!((d["lon"].as_f64().unwrap() - (-73.0)).abs() < 0.001);
    }

    #[test]
    fn performance_data_too_short_falls_through() {
        // 46-byte record (one short) must NOT be parsed as perf-data; it
        // falls through to the unnumbered-data envelope rather than panic.
        let mut h = vec![0u8; 46];
        h[0] = 0xFF;
        h[1] = 0xD1;
        let ev = parse_hfnpdu_body(&h);
        assert!(ev.iter().all(|e| e.kind != "performance-data"));
    }

    // ── HFDL-1.2 frequency-data (0xD5) ──────────────────────────────────
    //
    // Layout pinned to dumphfdl 1.7.0 frequency_data_parse(): 15-octet
    // header (flight/coord/utc) followed by up to 6 six-octet per-GS
    // records {gs_id, prop_freqs (20-bit), tuned_freqs (20-bit)}.
    #[test]
    fn frequency_data_per_gs_arrays() {
        let mut h = vec![0xFF, 0xD5];
        h.extend_from_slice(b"DLH456"); // flight_id at 2..8
        h.extend_from_slice(&[0u8; 7]); // coord(5) + utc(2) -> fills 8..15
        // Two GS records.
        let push_gs = |h: &mut Vec<u8>, gs_id: u8, prop: u32, tuned: u32| {
            h.push(gs_id);
            h.push((prop & 0xFF) as u8);
            h.push(((prop >> 8) & 0xFF) as u8);
            h.push(((prop >> 16) & 0x0F) as u8 | (((tuned & 0x0F) as u8) << 4));
            h.push(((tuned >> 4) & 0xFF) as u8);
            h.push(((tuned >> 12) & 0xFF) as u8);
        };
        push_gs(&mut h, 0x84, 0b101, 0b011); // gs_id 4 (high bit set)
        push_gs(&mut h, 0x0A, 0xABCDE, 0x12345); // gs_id 10

        let ev = parse_hfnpdu_body(&h);
        let e = ev.iter().find(|e| e.kind == "frequency-data").expect("freq-data");
        let fd = e.details["freq_data"].as_array().expect("freq_data array");
        assert_eq!(fd.len(), 2);
        assert_eq!(fd[0]["gs_id"], 4);
        assert_eq!(fd[0]["gs_name"], "Riverhead, New York");
        assert_eq!(fd[0]["prop_freqs"], 0b101);
        assert_eq!(fd[0]["tuned_freqs"], 0b011);
        assert_eq!(fd[1]["gs_id"], 10);
        assert_eq!(fd[1]["gs_name"], "Muan, South Korea");
        assert_eq!(fd[1]["prop_freqs"], 0xABCDE);
        assert_eq!(fd[1]["tuned_freqs"], 0x12345);
        assert_eq!(e.details["flight"], "DLH456");
    }

    #[test]
    fn frequency_data_no_gs_records() {
        // Bare 15-octet record (no per-GS data) yields an empty array.
        let mut h = vec![0xFF, 0xD5];
        h.extend_from_slice(b"AAL999");
        h.extend_from_slice(&[0u8; 7]);
        let ev = parse_hfnpdu_body(&h);
        let e = ev.iter().find(|e| e.kind == "frequency-data").expect("freq-data");
        assert_eq!(e.details["freq_data"].as_array().unwrap().len(), 0);
    }

    // ── HFDL-1.3 naming: 0xD2 / 0xDE / 0x2F ─────────────────────────────
    #[test]
    fn systable_request_named() {
        // dumphfdl systable_request_parse: request_data = uint16 LE at off 2.
        let h = vec![0xFF, 0xD2, 0x34, 0x12];
        let ev = parse_hfnpdu_body(&h);
        let e = ev.iter().find(|e| e.kind == "systable-request").expect("systable-request");
        assert_eq!(e.details["request_data"], 0x1234);
    }

    #[test]
    fn delayed_echo_named() {
        let h = vec![0xFF, 0xDE, 0x00];
        let ev = parse_hfnpdu_body(&h);
        assert!(ev.iter().any(|e| e.kind == "delayed-echo"));
    }

    #[test]
    fn logon_denied_named_with_reason() {
        // 0x2F LPDU: type, ICAO (3 bytes, bit-reversed), reason, FCS.
        // dumphfdl logon_denied_reason_codes: 0x01 = "Aircraft ID not available".
        let rev = |x: u8| x.reverse_bits();
        let body = vec![0x2F, rev(0x04), rev(0x00), rev(0x87), 0x01];
        let lpdu = with_fcs(body);
        let mpdu = build_mpdu_downlink(4, 0xC7, &[lpdu]);
        let ev = PduParser::new().parse(&mpdu, 300);
        let e = ev.iter().find(|e| e.kind == "logon-denied").expect("logon-denied");
        assert_eq!(e.details["icao"], "040087");
        assert_eq!(e.details["reason"], 1);
        assert_eq!(e.details["reason_text"], "Aircraft ID not available");
    }

    #[test]
    fn logoff_reason_text_named() {
        // dumphfdl logoff_request_reason_codes: 0x04 = "Invalid aircraft ID".
        let rev = |x: u8| x.reverse_bits();
        let body = vec![0x3F, rev(0x04), rev(0xC1), rev(0x1B), 0x04];
        let lpdu = with_fcs(body);
        let mpdu = build_mpdu_downlink(4, 0xC7, &[lpdu]);
        let ev = PduParser::new().parse(&mpdu, 300);
        let e = ev.iter().find(|e| e.kind == "logoff-request").expect("logoff-request");
        assert_eq!(e.details["icao"], "04C11B");
        assert_eq!(e.details["reason"], 4);
        assert_eq!(e.details["reason_text"], "Invalid aircraft ID");
    }

    // ── HFDL-3 aircraft-ID → ICAO cache ─────────────────────────────────
    //
    // dumphfdl (lpdu.c + ac_cache.c) records the ICAO from each
    // logon-confirm under its assigned channel-local aircraft ID, then
    // back-fills the ICAO on subsequent downlinks bearing that ID.
    #[test]
    fn cache_resolves_downlink_icao_after_logon_confirm() {
        let mut parser = PduParser::new();

        // Uplink logon-confirm: GS assigns aircraft ID 0x42 to ICAO 040087.
        let confirm = build_mpdu_uplink(4, 0xFF, &[build_lpdu_logon_confirm(0x040087, 0x42)]);
        let ev = parser.parse(&confirm, 300);
        let c = ev.iter().find(|e| e.kind == "logon-confirm").expect("logon-confirm");
        assert_eq!(c.details["icao"], "040087", "confirm carries the ICAO");
        assert_eq!(c.details["assigned_id"], 0x42);
        assert_eq!(parser.resolve_icao(0x42), Some("040087"));

        // Downlink performance-data from aircraft 0x42 must now carry the
        // resolved ICAO even though the wire frame only had the ac_id.
        let mut perf = vec![0u8; 47];
        perf[0] = 0xFF;
        perf[1] = 0xD1;
        perf[2..8].copy_from_slice(b"UAL042");
        let dl = build_mpdu_downlink(4, 0x42, &[build_lpdu_hfnpdu(&perf)]);
        let ev = parser.parse(&dl, 300);
        let p = ev.iter().find(|e| e.kind == "performance-data").expect("perf-data");
        assert_eq!(p.details["who"]["aircraft_id"], 0x42);
        assert_eq!(p.details["who"]["icao"], "040087", "ICAO back-filled from cache");
    }

    #[test]
    fn cache_evicts_on_logoff() {
        let mut parser = PduParser::new();
        let confirm = build_mpdu_uplink(4, 0xFF, &[build_lpdu_logon_confirm(0x04C11B, 0x55)]);
        parser.parse(&confirm, 300);
        assert_eq!(parser.resolve_icao(0x55), Some("04C11B"));

        // Logoff for the same ICAO clears the mapping.
        let rev = |x: u8| x.reverse_bits();
        let logoff = build_mpdu_downlink(
            4,
            0x55,
            &[with_fcs(vec![0x3F, rev(0x04), rev(0xC1), rev(0x1B), 0x06])],
        );
        parser.parse(&logoff, 300);
        assert_eq!(parser.resolve_icao(0x55), None, "logoff evicts the cache entry");

        // A later downlink from 0x55 no longer resolves an ICAO.
        let dl = build_mpdu_downlink(4, 0x55, &[build_lpdu_acars(b"\x01x")]);
        let ev = parser.parse(&dl, 300);
        assert!(ev.iter().all(|e| e.details["who"].get("icao").is_none()));
    }

    #[test]
    fn cache_ttl_expiry_drops_resolution() {
        let mut parser = PduParser::with_ac_cache_ttl(std::time::Duration::from_millis(0));
        let confirm = build_mpdu_uplink(4, 0xFF, &[build_lpdu_logon_confirm(0x040087, 0x42)]);
        parser.parse(&confirm, 300);
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(parser.resolve_icao(0x42), None, "entry expires past TTL");
    }
}
