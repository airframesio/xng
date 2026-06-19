//! HFDL system-table reassembly: 0xD0 HFNPDU partials carry slices of the
//! ground-station table (id, position, frequencies, master frame slots).
//! Facts from ICAO Annex 10 Vol III Ch. 11 and dumphfdl's systable handling
//! (GPL — read for the wire layout only); see docs/notes/HFDL.md.
//!
//! Each partial repeats a 5-octet header (type, 0xD0, total/seq, 12-bit
//! version split across octets 3-4); the remainder is one slice of the
//! table. Slices keyed by (version, total) accumulate until every sequence
//! number is present, then concatenate in order and parse as consecutive
//! ground-station records.

use serde::{Deserialize, Serialize};

/// Frequencies per station are bounded by the 20-bit frequency bitmaps
/// used in SPDUs and frequency-data HFNPDUs.
const MAX_FREQS: usize = 20;
const GS_RECORD_MIN: usize = 8; // 7-octet fixed part + at least one frequency/slot octet

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GsFrequency {
    pub freq_hz: u32,
    pub master_frame_slot: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroundStation {
    pub gs_id: u8,
    /// Human-readable station name from the built-in public ARINC HFDL GS
    /// list (mirrors dumphfdl's per-station `name` JSON field, which it
    /// fills from its config systable). Populated on decode; `None` for the
    /// 12 unassigned IDs in 1..=17 and any ID outside that range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gs_name: Option<String>,
    pub utc_sync: bool,
    pub lat: f64,
    pub lon: f64,
    pub spdu_version: u8,
    pub frequencies: Vec<GsFrequency>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemTable {
    pub version: u16,
    pub stations: Vec<GroundStation>,
}

impl SystemTable {
    /// Serialize the learned system table to `path` as pretty JSON.
    ///
    /// Cold-start enrichment counterpart to [`SystemTable::load`]: a
    /// long-running receiver writes the most recent reassembled table so a
    /// later run starts with known GS positions/frequencies instead of
    /// waiting for the next over-the-air 0xD0 set. This is the serde
    /// equivalent of dumphfdl's libconfig `systable_save_config()`
    /// (src/systable.c) — same persisted facts (id 0..=127, optional name,
    /// lat/lon, frequencies), JSON rather than libconfig so it round-trips
    /// through the crate's existing serde_json channel with no new
    /// dependency.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Load a previously [`saved`](SystemTable::save) system table from
    /// `path`. Counterpart to dumphfdl's `systable_read_from_file()`.
    pub fn load(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Free-function form of [`SystemTable::load`] matching the
/// `load_system_table(path)` API named in the task / CLI follow-up.
pub fn load_system_table(path: impl AsRef<std::path::Path>) -> std::io::Result<SystemTable> {
    SystemTable::load(path)
}

/// Free-function form of [`SystemTable::save`] matching the
/// `save_system_table(path)` API named in the task / CLI follow-up.
pub fn save_system_table(
    table: &SystemTable,
    path: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    table.save(path)
}

/// 20-bit two's-complement coordinate, degrees ×180/2^19.
fn coordinate(v: u32) -> f64 {
    let r = ((v << 12) as i32) >> 12;
    r as f64 * 180.0 / (1 << 19) as f64
}

/// 3 octets of BCD, nibbles low→high, in units of 100 Hz.
fn bcd_frequency(b: &[u8]) -> u32 {
    let nib = |x: u8| (x & 0x0F) as u32;
    100 * nib(b[0])
        + 1_000 * nib(b[0] >> 4)
        + 10_000 * nib(b[1])
        + 100_000 * nib(b[1] >> 4)
        + 1_000_000 * nib(b[2])
        + 10_000_000 * nib(b[2] >> 4)
}

/// 12-bit wrapping version comparison: newer if it leads by < half the space.
pub fn version_is_newer(new: u16, old: u16) -> bool {
    let d = (new.wrapping_sub(old)) & 0x0FFF;
    d != 0 && d < 2048
}

/// Parse a reassembled table body (concatenated partial payloads) into
/// ground-station records. Returns None if any record is malformed.
pub fn parse_stations(mut buf: &[u8]) -> Option<Vec<GroundStation>> {
    let mut stations = Vec::new();
    while buf.len() >= GS_RECORD_MIN {
        let freq_cnt = ((buf[6] >> 3) & 0x1F) as usize;
        if freq_cnt > MAX_FREQS {
            return None;
        }
        let rec_len = 7 + freq_cnt * 4;
        if buf.len() < rec_len {
            return None;
        }
        let lat = coordinate(buf[1] as u32 | (buf[2] as u32) << 8 | ((buf[3] as u32 & 0x0F) << 16));
        let lon = coordinate((buf[3] as u32) >> 4 | (buf[4] as u32) << 4 | (buf[5] as u32) << 12);
        let frequencies = (0..freq_cnt)
            .map(|f| {
                let p = 7 + f * 4;
                GsFrequency {
                    freq_hz: bcd_frequency(&buf[p..p + 3]),
                    master_frame_slot: buf[p + 3] & 0x0F,
                }
            })
            .collect();
        let gs_id = buf[0] & 0x7F;
        stations.push(GroundStation {
            gs_id,
            gs_name: crate::pdu::gs_name(gs_id).map(str::to_string),
            utc_sync: buf[0] & 0x80 != 0,
            lat,
            lon,
            spdu_version: buf[6] & 0x07,
            frequencies,
        });
        buf = &buf[rec_len..];
    }
    // Trailing octets shorter than a record are padding; a table with no
    // stations at all is a parse failure, not an empty network.
    if stations.is_empty() {
        None
    } else {
        Some(stations)
    }
}

/// Accumulates 0xD0 partial payloads until a complete set reassembles.
#[derive(Debug, Default)]
pub struct SystableAssembler {
    version: u16,
    parts: Vec<Option<Vec<u8>>>,
}

impl SystableAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store one partial's payload (the octets after the 5-byte header).
    /// Returns the decoded table when this part completes the set.
    pub fn store(
        &mut self,
        version: u16,
        seq: u8,
        total: u8,
        payload: &[u8],
    ) -> Option<SystemTable> {
        let (seq, total) = (seq as usize, total as usize);
        if total == 0 || seq >= total || payload.is_empty() {
            return None;
        }
        // A different version or set size obsoletes anything collected.
        if self.version != version || self.parts.len() != total {
            self.parts = vec![None; total];
            self.version = version;
        }
        // Same slot with different content: trust the newest copy.
        if self.parts[seq].as_deref() != Some(payload) {
            self.parts[seq] = Some(payload.to_vec());
        }
        if self.parts.iter().any(|p| p.is_none()) {
            return None;
        }
        let body: Vec<u8> = self.parts.drain(..).flatten().flatten().collect();
        parse_stations(&body).map(|stations| SystemTable { version, stations })
    }
}

// ── builders (testing/modulation) ───────────────────────────────────────

/// Encode one ground-station record.
pub fn build_gs_record(gs: &GroundStation) -> Vec<u8> {
    let coord = |deg: f64| -> u32 {
        ((deg * (1 << 19) as f64 / 180.0).round() as i32 as u32) & 0xFFFFF
    };
    let (lat, lon) = (coord(gs.lat), coord(gs.lon));
    let mut v = vec![
        gs.gs_id & 0x7F | if gs.utc_sync { 0x80 } else { 0 },
        (lat & 0xFF) as u8,
        ((lat >> 8) & 0xFF) as u8,
        ((lat >> 16) & 0x0F) as u8 | (((lon & 0x0F) as u8) << 4),
        ((lon >> 4) & 0xFF) as u8,
        ((lon >> 12) & 0xFF) as u8,
        (gs.spdu_version & 0x07) | ((gs.frequencies.len() as u8 & 0x1F) << 3),
    ];
    for f in &gs.frequencies {
        let units = f.freq_hz / 100;
        let mut b = [0u8; 3];
        for (i, d) in (0..6).map(|i| (units / 10u32.pow(i)) % 10).enumerate() {
            b[i / 2] |= (d as u8) << ((i % 2) * 4);
        }
        v.extend_from_slice(&b);
        v.push(f.master_frame_slot & 0x0F);
    }
    v
}

/// Split a table body into `total` 0xD0 HFNPDUs (header + slice each).
pub fn build_systable_hfnpdus(version: u16, body: &[u8], total: u8) -> Vec<Vec<u8>> {
    let total = total.max(1) as usize;
    let chunk = body.len().div_ceil(total);
    (0..total)
        .map(|i| {
            let mut h = vec![
                0xFF,
                0xD0,
                (((total - 1) as u8) << 4) | i as u8,
                ((version & 0x0F) as u8) << 4,
                (version >> 4) as u8,
            ];
            h.extend_from_slice(&body[i * chunk..body.len().min((i + 1) * chunk)]);
            h
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stations() -> Vec<GroundStation> {
        vec![
            GroundStation {
                gs_id: 1,
                gs_name: None,
                utc_sync: true,
                lat: 37.0179,
                lon: -122.9059,
                spdu_version: 2,
                frequencies: vec![
                    GsFrequency { freq_hz: 21_934_000, master_frame_slot: 0 },
                    GsFrequency { freq_hz: 17_919_000, master_frame_slot: 3 },
                    GsFrequency { freq_hz: 13_276_000, master_frame_slot: 7 },
                ],
            },
            GroundStation {
                gs_id: 13,
                gs_name: None,
                utc_sync: true,
                lat: -37.6691,
                lon: 144.8410,
                spdu_version: 2,
                frequencies: vec![
                    GsFrequency { freq_hz: 21_949_000, master_frame_slot: 1 },
                ],
            },
        ]
    }

    #[test]
    fn gs_record_roundtrip() {
        let stations = sample_stations();
        let body: Vec<u8> = stations.iter().flat_map(|g| build_gs_record(g)).collect();
        let parsed = parse_stations(&body).expect("parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].gs_id, 1);
        assert_eq!(parsed[0].frequencies[0].freq_hz, 21_934_000);
        assert_eq!(parsed[0].frequencies[2].master_frame_slot, 7);
        assert!((parsed[0].lat - 37.0179).abs() < 0.001);
        assert!((parsed[0].lon + 122.9059).abs() < 0.001);
        assert_eq!(parsed[1].gs_id, 13);
        assert!((parsed[1].lat + 37.6691).abs() < 0.001);
        // Decode-side enrichment from the built-in ARINC GS list: id 1 =
        // San Francisco, id 13 = Santa Cruz, Bolivia (same public list
        // asserted by pdu::gs_name and mirroring dumphfdl's `name`).
        assert_eq!(parsed[0].gs_name.as_deref(), Some("San Francisco, USA"));
        assert_eq!(parsed[1].gs_name.as_deref(), Some("Santa Cruz, Bolivia"));
    }

    #[test]
    fn gs_name_none_for_unassigned_id() {
        // ID 12 is one of the 12 holes in the public 1..=17 GS list; the
        // decoded record must leave gs_name unset rather than invent a name.
        let mut gs = sample_stations()[0].clone();
        gs.gs_id = 12;
        let body = build_gs_record(&gs);
        let parsed = parse_stations(&body).expect("parses");
        assert_eq!(parsed[0].gs_id, 12);
        assert_eq!(parsed[0].gs_name, None);
    }

    #[test]
    fn bcd_frequency_decodes() {
        // 13,276,700 Hz = 132767 ×100Hz units; digits 7,6,7,2,3,1 low→high.
        assert_eq!(bcd_frequency(&[0x67, 0x27, 0x13]), 13_276_700);
        assert_eq!(bcd_frequency(&[0x00, 0x00, 0x00]), 0);
    }

    #[test]
    fn reassembly_out_of_order() {
        let body: Vec<u8> =
            sample_stations().iter().flat_map(|g| build_gs_record(g)).collect();
        let pdus = build_systable_hfnpdus(52, &body, 3);
        let mut asm = SystableAssembler::new();
        assert!(asm.store(52, 2, 3, &pdus[2][5..]).is_none());
        assert!(asm.store(52, 0, 3, &pdus[0][5..]).is_none());
        let table = asm.store(52, 1, 3, &pdus[1][5..]).expect("complete");
        assert_eq!(table.version, 52);
        assert_eq!(table.stations.len(), 2);
        assert_eq!(table.stations[1].frequencies[0].freq_hz, 21_949_000);
    }

    #[test]
    fn version_change_discards_partial_set() {
        let body: Vec<u8> =
            sample_stations().iter().flat_map(|g| build_gs_record(g)).collect();
        let old = build_systable_hfnpdus(51, &body, 2);
        let new = build_systable_hfnpdus(52, &body, 2);
        let mut asm = SystableAssembler::new();
        assert!(asm.store(51, 0, 2, &old[0][5..]).is_none());
        // Version bump: the stored part 0 must not combine with new part 1.
        assert!(asm.store(52, 1, 2, &new[1][5..]).is_none());
        assert!(asm.store(52, 0, 2, &new[0][5..]).is_some());
    }

    #[test]
    fn malformed_body_rejected() {
        // freq_cnt = 31 exceeds the 20-frequency bound.
        let mut body = build_gs_record(&sample_stations()[0]);
        body[6] |= 0x1F << 3;
        assert!(parse_stations(&body).is_none());
        assert!(parse_stations(&[]).is_none());
    }

    /// Unique temp path so concurrent test runs don't collide (no
    /// tempfile dependency in this crate).
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("xng_hfdl_systable_{tag}_{}_{nanos}.json", std::process::id()))
    }

    #[test]
    fn systable_persistence_round_trip() {
        // Build a table the way the decoder would: real GS records run
        // through parse_stations so gs_name enrichment (id 1 = San
        // Francisco) and the None hole (id 12) are both present.
        let mut stations = sample_stations();
        stations[1].gs_id = 12; // force an unassigned-id (no name) record
        let body: Vec<u8> = stations.iter().flat_map(build_gs_record).collect();
        let parsed = parse_stations(&body).expect("parses");
        let table = SystemTable { version: 52, stations: parsed };
        assert_eq!(table.stations[0].gs_name.as_deref(), Some("San Francisco, USA"));
        assert_eq!(table.stations[1].gs_name, None);

        let path = temp_path("rt");
        // Both the method form and the free-function form must round-trip.
        save_system_table(&table, &path).expect("save");
        let loaded = load_system_table(&path).expect("load");
        // All integer/string/bool fields round-trip exactly; coordinates
        // are f64 and compared to ~sub-metre tolerance (text serialization
        // of f64 is round-trip-exact in practice but not guaranteed by the
        // serde contract, so coordinates are not asserted bit-identical).
        assert_eq!(loaded.version, 52);
        assert_eq!(loaded.stations.len(), table.stations.len());
        for (a, b) in loaded.stations.iter().zip(&table.stations) {
            assert_eq!(a.gs_id, b.gs_id);
            assert_eq!(a.gs_name, b.gs_name);
            assert_eq!(a.utc_sync, b.utc_sync);
            assert_eq!(a.spdu_version, b.spdu_version);
            assert_eq!(a.frequencies, b.frequencies);
            assert!((a.lat - b.lat).abs() < 1e-6, "lat {} vs {}", a.lat, b.lat);
            assert!((a.lon - b.lon).abs() < 1e-6, "lon {} vs {}", a.lon, b.lon);
        }
        // Enrichment and the None hole survive serialization.
        assert_eq!(loaded.stations[0].gs_name.as_deref(), Some("San Francisco, USA"));
        assert_eq!(loaded.stations[1].gs_name, None);
        assert_eq!(loaded.stations[0].frequencies[0].freq_hz, 21_934_000);
        assert_eq!(loaded.stations[0].frequencies[2].master_frame_slot, 7);

        // Once a table has been through one save→load cycle it is a fixed
        // point: saving the loaded table and loading again reproduces it
        // bit-for-bit (PartialEq, floats included). This pins exact
        // persistence without depending on f64 text round-trip being a
        // fixed point for freshly computed coordinates.
        let path2 = temp_path("rt2");
        loaded.save(&path2).expect("save method");
        let reloaded = SystemTable::load(&path2).expect("load method");
        assert_eq!(reloaded, loaded, "save→load must be a fixed point");

        // A station with no name serializes without a gs_name key, and
        // loading a doc that omits the key restores None (serde default).
        let json = serde_json::to_string(&table).unwrap();
        assert!(!json.contains("\"gs_name\":null"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn systable_load_missing_file_errors() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        assert!(load_system_table(&path).is_err());
    }

    #[test]
    fn version_wraparound() {
        assert!(version_is_newer(1, 4095));
        assert!(version_is_newer(53, 52));
        assert!(!version_is_newer(52, 52));
        assert!(!version_is_newer(52, 53));
        assert!(!version_is_newer(4095, 1));
    }
}
