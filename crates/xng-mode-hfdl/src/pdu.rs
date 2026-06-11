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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HfdlEvent {
    pub kind: String,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acars: Option<AcarsBlock>,
    #[serde(skip_serializing)]
    pub raw: Vec<u8>,
}

pub struct PduParser {
    systable: crate::systable::SystableAssembler,
}

impl PduParser {
    pub fn new() -> Self {
        Self { systable: crate::systable::SystableAssembler::new() }
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
        match body[0] {
            0x0D | 0x1D => self.parse_hfnpdu(&body[1..], who, bps, out),
            0x8F | 0xBF | 0x4F if body.len() >= 4 => out.push(HfdlEvent {
                kind: "logon-request".into(),
                details: json!({ "icao": icao(&body[1..4]), "who": who }),
                acars: None,
                raw: l.to_vec(),
            }),
            0x9F | 0x5F if body.len() >= 5 => out.push(HfdlEvent {
                kind: "logon-confirm".into(),
                details: json!({ "icao": icao(&body[1..4]), "assigned_id": body[4], "who": who }),
                acars: None,
                raw: l.to_vec(),
            }),
            0x3F if body.len() >= 5 => out.push(HfdlEvent {
                kind: "logoff-request".into(),
                details: json!({ "icao": icao(&body[1..4]), "reason": body[4], "who": who }),
                acars: None,
                raw: l.to_vec(),
            }),
            t => out.push(HfdlEvent {
                kind: "lpdu".into(),
                details: json!({ "type": t, "who": who }),
                acars: None,
                raw: l.to_vec(),
            }),
        }
    }

    fn parse_hfnpdu(&mut self, h: &[u8], who: &serde_json::Value, bps: u32, out: &mut Vec<HfdlEvent>) {
        if h.len() < 2 || h[0] != 0xFF {
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
                        raw: h.to_vec(),
                    });
                }
            }
            0xD1 | 0xD5 if h.len() >= 15 => {
                let flight: String =
                    h[2..8].iter().map(|&c| (c & 0x7F) as char).collect();
                let lat = coordinate(
                    h[8] as u32 | (h[9] as u32) << 8 | ((h[10] as u32 & 0x0F) << 16),
                );
                let lon = coordinate(
                    (h[10] as u32 >> 4) | (h[11] as u32) << 4 | (h[12] as u32) << 12,
                );
                out.push(HfdlEvent {
                    kind: if h[1] == 0xD1 { "performance-data" } else { "frequency-data" }.into(),
                    details: json!({
                        "flight": flight.trim().to_string(),
                        "lat": lat, "lon": lon,
                        "utc_s": u16::from_le_bytes([h[13], h[14]]) as u32 * 2,
                        "who": who,
                    }),
                    acars: None,
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
                    raw: h.to_vec(),
                });
                if let Some(table) = self.systable.store(version, seq, total, &h[5..]) {
                    out.push(HfdlEvent {
                        kind: "systable-complete".into(),
                        details: serde_json::to_value(&table).unwrap_or_default(),
                        acars: None,
                        raw: Vec::new(),
                    });
                }
            }
            t => out.push(HfdlEvent {
                kind: "hfnpdu".into(),
                details: json!({ "type": t, "who": who }),
                acars: None,
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
}
