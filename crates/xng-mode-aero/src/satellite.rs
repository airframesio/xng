//! Inmarsat-Aero satellite/beam resolution (AERO-2).
//!
//! The L-band analogue of the HFDL system table (`xng-mode-hfdl::systable`):
//! a *self-configuring* resolver that learns which satellite serves the
//! channel purely from the AES system-table broadcast Signal Units already
//! decoded in AERO-1.3, then tags every message with the resolved
//! satellite and beam.
//!
//! Two broadcast SUs drive it (both parsed in [`crate::su`]):
//!
//! - **0x0C `satellite_identification`** — the authoritative system-table
//!   broadcast. JAERO (`aerol.cpp`,
//!   `AES_system_table_broadcast_satellite_identification_COMPLETE`)
//!   decodes `satid`, the orbital `longitude` (`byte6 * 1.5°`, with
//!   `> 180 ⇒ West`), and the Psmc carriers. JAERO only *displays* this —
//!   it has no satellite-name table — so the resolved identity is the
//!   numeric satellite id plus its measured orbital longitude, taken
//!   verbatim from the broadcast.
//! - **0x07 `GES_beam_support`** — JAERO names this type and decodes no
//!   further fields; observing it confirms the serving GES advertises beam
//!   support on this channel (a presence flag in the resolved state).
//!
//! Beam (global vs spot) is read from the spot-beam flag JAERO carries in
//! the high bit of each carrier's high octet (the `*_spotbeam` fields the
//! 0x0C / 0x05 / assignment handlers already surface).
//!
//! Ocean-region naming: JAERO does not name regions. We add a *nominal*
//! best-effort ocean-region label by nearest classic Inmarsat region
//! centre longitude (the published Inmarsat-3 operational slots:
//! AOR-W ≈ 54°W, AOR-E ≈ 15.5°W, IOR ≈ 64°E, POR ≈ 178°E — Inmarsat-3 F5
//! is documented at 54°W and F3 at 178°E, see docs/REFERENCES.md /
//! PROVENANCE.md). It is clearly secondary to the measured longitude,
//! which is the ground truth from the broadcast.

use serde::Serialize;

/// Classic Inmarsat ocean region (nominal, by orbital longitude). These are
/// the four published Classic-Aero / Inmarsat-3 coverage regions; the slot
/// centres are operational facts (PROVENANCE / REFERENCES), not a JAERO
/// table. Used only as a human-readable hint alongside the measured
/// longitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OceanRegion {
    /// Atlantic Ocean Region West (nominal ≈ 54°W).
    AorW,
    /// Atlantic Ocean Region East (nominal ≈ 15.5°W).
    AorE,
    /// Indian Ocean Region (nominal ≈ 64°E).
    Ior,
    /// Pacific Ocean Region (nominal ≈ 178°E).
    Por,
}

impl OceanRegion {
    /// Short region code (AOR-W / AOR-E / IOR / POR).
    pub fn code(self) -> &'static str {
        match self {
            OceanRegion::AorW => "AOR-W",
            OceanRegion::AorE => "AOR-E",
            OceanRegion::Ior => "IOR",
            OceanRegion::Por => "POR",
        }
    }

    /// Nominal region-centre longitude in signed degrees (East positive,
    /// West negative). Published Inmarsat-3 operational slots.
    fn center_deg(self) -> f64 {
        match self {
            OceanRegion::AorW => -54.0,
            OceanRegion::AorE => -15.5,
            OceanRegion::Ior => 64.0,
            OceanRegion::Por => 178.0,
        }
    }

    /// Classify a satellite's orbital longitude (signed degrees, East
    /// positive) into the nearest classic region, within a generous
    /// tolerance. Returns `None` when no region centre is within range
    /// (e.g. a satellite repositioned far from any classic slot — we do not
    /// guess). Longitude wraps at ±180°.
    pub fn classify(longitude_deg_signed: f64) -> Option<OceanRegion> {
        const TOLERANCE_DEG: f64 = 35.0;
        let regions = [OceanRegion::AorW, OceanRegion::AorE, OceanRegion::Ior, OceanRegion::Por];
        let mut best: Option<(OceanRegion, f64)> = None;
        for r in regions {
            // Smallest absolute angular separation, accounting for wrap.
            let mut d = (longitude_deg_signed - r.center_deg()).abs();
            if d > 180.0 {
                d = 360.0 - d;
            }
            if d <= TOLERANCE_DEG && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((r, d));
            }
        }
        best.map(|(r, _)| r)
    }
}

/// A resolved satellite identity learned from the system-table broadcasts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedSatellite {
    /// Numeric satellite id from the 0x0C broadcast (JAERO `satid`).
    pub satellite_id: u8,
    /// Measured orbital longitude magnitude in degrees (JAERO
    /// `byte6 * 1.5`, folded to ≤ 180 with a direction).
    pub longitude_deg: f64,
    /// `"E"` or `"W"` (JAERO `> 180 ⇒ W`).
    pub longitude_dir: String,
    /// Nominal ocean region by nearest classic slot (best-effort hint).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<OceanRegion>,
}

impl ResolvedSatellite {
    /// Signed orbital longitude (East positive, West negative).
    fn longitude_signed(&self) -> f64 {
        if self.longitude_dir == "W" {
            -self.longitude_deg
        } else {
            self.longitude_deg
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "satellite_id": self.satellite_id,
            "longitude_deg": self.longitude_deg,
            "longitude_dir": self.longitude_dir,
        });
        if let Some(r) = self.region {
            v["region"] = serde_json::json!(r.code());
        }
        v
    }
}

/// Self-configuring satellite/beam resolver. Feed it every structured
/// P-channel SU value ([`crate::su::parse_p_su`] output); it latches the
/// most recent satellite identity and beam state seen on the channel and
/// annotates outgoing message `details` from that latched state.
#[derive(Debug, Default)]
pub struct SatelliteResolver {
    satellite: Option<ResolvedSatellite>,
    /// Latest beam observed (true = spot beam, false = global). Learned
    /// from the Psmc spot-beam flag in the 0x0C broadcast.
    spot_beam: Option<bool>,
    /// Whether a 0x07 GES_beam_support broadcast has been seen.
    beam_support_seen: bool,
    /// GES id from the most recent 0x05 / control broadcast (context only).
    ges_id: Option<u8>,
}

impl SatelliteResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Have we resolved a satellite yet?
    pub fn is_resolved(&self) -> bool {
        self.satellite.is_some()
    }

    /// The currently resolved satellite, if any.
    pub fn satellite(&self) -> Option<&ResolvedSatellite> {
        self.satellite.as_ref()
    }

    /// Observe one structured P-channel SU value. Updates the resolved
    /// satellite/beam state from system-table broadcasts (0x0C / 0x07 /
    /// 0x05); ignores everything else. Idempotent for non-system SUs.
    pub fn observe(&mut self, su: &serde_json::Value) {
        match su["su_type"].as_str() {
            Some("satellite-id") => {
                let satellite_id = su["satellite_id"].as_u64().unwrap_or(0) as u8;
                let longitude_deg = su["longitude_deg"].as_f64().unwrap_or(0.0);
                let longitude_dir =
                    su["longitude_dir"].as_str().unwrap_or("E").to_owned();
                let mut sat = ResolvedSatellite {
                    satellite_id,
                    longitude_deg,
                    longitude_dir,
                    region: None,
                };
                sat.region = OceanRegion::classify(sat.longitude_signed());
                self.satellite = Some(sat);
                // The Psmc1 spot-beam flag is the beam this carrier serves.
                if let Some(spot) = su["psmc1_spotbeam"].as_bool() {
                    self.spot_beam = Some(spot);
                }
            }
            Some("ges-beam-support") => {
                self.beam_support_seen = true;
            }
            Some("smc-channels") => {
                if let Some(g) = su["ges_id"].as_u64() {
                    self.ges_id = Some(g as u8);
                }
            }
            _ => {}
        }
    }

    /// Resolved-satellite JSON for the message `details` channel, or `None`
    /// when nothing has been learned yet. Beam state is folded in.
    pub fn details(&self) -> Option<serde_json::Value> {
        let sat = self.satellite.as_ref()?;
        let mut v = serde_json::json!({ "resolved_satellite": sat.to_json() });
        let beam = match self.spot_beam {
            Some(true) => "spot",
            Some(false) => "global",
            None => "unknown",
        };
        v["beam"] = serde_json::json!(beam);
        if self.beam_support_seen {
            v["ges_beam_support"] = serde_json::json!(true);
        }
        if let Some(g) = self.ges_id {
            v["resolved_ges_id"] = serde_json::json!(g);
        }
        Some(v)
    }

    /// Merge the resolved satellite/beam annotation into an existing
    /// `details` object in place. No-op (leaving `details` untouched) until
    /// a satellite has been resolved. Existing keys are never overwritten.
    pub fn annotate(&self, details: &mut serde_json::Value) {
        let Some(extra) = self.details() else { return };
        if let (serde_json::Value::Object(dst), serde_json::Value::Object(src)) =
            (details, extra)
        {
            for (k, v) in src {
                dst.entry(k).or_insert(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::su::{self, parse_p_su};

    /// AERO-2: nominal ocean-region classification by the published
    /// Inmarsat-3 slot centres. Oracle: operational slot longitudes
    /// (Inmarsat-3 F5 = 54°W documented, F3 = 178°E documented; AOR-E /
    /// IOR are the classic ≈15.5°W / ≈64°E centres). See module docs /
    /// PROVENANCE.
    #[test]
    fn ocean_region_classifies_classic_slots() {
        assert_eq!(OceanRegion::classify(-54.0), Some(OceanRegion::AorW));
        assert_eq!(OceanRegion::classify(-15.5), Some(OceanRegion::AorE));
        assert_eq!(OceanRegion::classify(64.0), Some(OceanRegion::Ior));
        assert_eq!(OceanRegion::classify(178.0), Some(OceanRegion::Por));
        // Near-slot positions still classify (within tolerance).
        assert_eq!(OceanRegion::classify(-40.0), Some(OceanRegion::AorW));
        assert_eq!(OceanRegion::classify(98.0), Some(OceanRegion::Ior));
        // Wrap: -179°E is adjacent to POR (178°E), 3° away.
        assert_eq!(OceanRegion::classify(-179.0), Some(OceanRegion::Por));
        // A longitude far from every slot is not guessed.
        assert_eq!(OceanRegion::classify(115.0), None);
        // Codes.
        assert_eq!(OceanRegion::AorW.code(), "AOR-W");
        assert_eq!(OceanRegion::Por.code(), "POR");
    }

    /// AERO-2: the resolver learns the satellite from a real 0x0C
    /// satellite_identification SU (the same bytes the AERO-1.3 oracle
    /// test pins) and tags downstream messages. Oracle = JAERO 0x0C field
    /// layout (`aerol.cpp`).
    #[test]
    fn resolver_learns_satellite_from_0x0c_broadcast() {
        // Build the JAERO-layout 0x0C SU used in su.rs's verified test:
        // satid 20, seqno 10, longitude index 200 → 300° → 60.0°W,
        // Psmc1 channel 0x0123 (global beam), Psmc2 spot beam.
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x0C;
        su10[2] = 0x29; // seqno 10, satid_hi 1
        su10[3] = 0x40; // satid_lo 4 → satid 20
        su10[5] = 200; // 300.0° → 60.0°W
        su10[6] = 0x01;
        su10[7] = 0x23; // Psmc1 channel 0x0123, no spot beam
        su10[8] = 0x80 | 0x04;
        su10[9] = 0x56; // Psmc2 spot beam (not the serving carrier)
        let su = su::su_with_crc(su10);
        let v = parse_p_su(&su).expect("0x0C parses");

        let mut r = SatelliteResolver::new();
        assert!(!r.is_resolved());
        // A non-system SU does not resolve anything.
        r.observe(&serde_json::json!({ "su_type": "log-control" }));
        assert!(!r.is_resolved());

        r.observe(&v);
        assert!(r.is_resolved());
        let sat = r.satellite().unwrap();
        assert_eq!(sat.satellite_id, 20);
        assert_eq!(sat.longitude_deg, 60.0);
        assert_eq!(sat.longitude_dir, "W");
        // 60°W is the classic AOR-W slot region (≈54°W, within tolerance).
        assert_eq!(sat.region, Some(OceanRegion::AorW));

        // details() carries the resolved satellite + beam.
        let d = r.details().unwrap();
        assert_eq!(d["resolved_satellite"]["satellite_id"], 20);
        assert_eq!(d["resolved_satellite"]["longitude_deg"], 60.0);
        assert_eq!(d["resolved_satellite"]["longitude_dir"], "W");
        assert_eq!(d["resolved_satellite"]["region"], "AOR-W");
        // Psmc1 (the serving carrier) was global, so beam = global.
        assert_eq!(d["beam"], "global");

        // annotate() merges into a message details object without clobbering.
        let mut details = serde_json::json!({ "su_type": "log-control", "beam": "preset" });
        r.annotate(&mut details);
        assert_eq!(details["su_type"], "log-control"); // untouched
        assert_eq!(details["beam"], "preset"); // existing key preserved
        assert_eq!(details["resolved_satellite"]["satellite_id"], 20); // added
    }

    /// AERO-2: a later 0x0C broadcast re-resolves (self-configuring,
    /// like HFDL re-learning the table); 0x07 sets the beam-support flag.
    #[test]
    fn resolver_reconfigures_and_tracks_beam_support() {
        let make_0x0c = |satid_hi: u8, satid_lo: u8, lon: u8, spot: bool| {
            let mut su10 = vec![0u8; 10];
            su10[0] = 0x0C;
            su10[2] = (10u8 << 2) | satid_hi; // seqno 10
            su10[3] = satid_lo << 4;
            su10[5] = lon;
            su10[6] = if spot { 0x80 } else { 0x00 } | 0x02;
            su10[7] = 0x00; // Psmc1 channel 0x0200
            su::su_with_crc(su10.clone())
        };

        let mut r = SatelliteResolver::new();
        // First: satid 5, 100×1.5 = 150°E (POR region, global beam).
        let v1 = parse_p_su(&make_0x0c(0, 5, 100, false)).unwrap();
        r.observe(&v1);
        let d = r.details().unwrap();
        assert_eq!(d["resolved_satellite"]["satellite_id"], 5);
        assert_eq!(d["resolved_satellite"]["longitude_dir"], "E");
        assert_eq!(d["resolved_satellite"]["region"], "POR"); // 150°E ≈ 178°E slot
        assert_eq!(d["beam"], "global");

        // GES_beam_support seen → flag set.
        let bs = parse_p_su(&su::su_with_crc({
            let mut s = vec![0u8; 10];
            s[0] = 0x07;
            s
        }))
        .unwrap();
        r.observe(&bs);

        // Re-resolve to a different satellite on a spot beam.
        // satid 6, 40×1.5 = 60°E (IOR region, spot beam).
        let v2 = parse_p_su(&make_0x0c(0, 6, 40, true)).unwrap();
        r.observe(&v2);
        let d = r.details().unwrap();
        assert_eq!(d["resolved_satellite"]["satellite_id"], 6);
        assert_eq!(d["resolved_satellite"]["region"], "IOR"); // 60°E ≈ 64°E slot
        assert_eq!(d["beam"], "spot");
        assert_eq!(d["ges_beam_support"], true);
    }
}
