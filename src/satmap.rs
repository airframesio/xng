//! IRA → satellite-name matching. Ring-alert frames carry the broadcasting
//! satellite's geocentric ECEF position (in units of 4 km). By propagating
//! the current Iridium-NEXT two-line elements with SGP4 and rotating the
//! TEME result into ECEF (by GMST), we can label each ring alert with the
//! actual NORAD satellite it came from (cf. iridium-toolkit `InfoIRAMAP`).
//!
//! TLEs come from Celestrak (auto-fetched by default) or a local file.

use std::sync::OnceLock;
use std::time::Duration;
use xng_types::{Message, MessageBody};

/// Process-global satellite map, loaded once at startup (immutable after).
static SATMAP: OnceLock<SatMap> = OnceLock::new();

/// Load the satellite map from `source` ("auto" = Celestrak, else a file
/// path) into the global, returning the satellite count. Call once at
/// startup before decode tasks spawn.
pub fn init(source: &str) -> anyhow::Result<usize> {
    let map = load(source)?;
    let n = map.len();
    let _ = SATMAP.set(map);
    Ok(n)
}

/// Enrich a ring-alert message with its matched satellite, if a satellite
/// map has been loaded (no-op otherwise).
pub fn enrich(msg: &mut Message) {
    if let Some(map) = SATMAP.get() {
        map.enrich(msg);
    }
}

/// Match gate: a decoded position must lie within this of a propagated
/// satellite to be attributed to it (IRA positions quantise to 4 km).
const MAX_DIST_KM: f64 = 100.0;

/// Celestrak Iridium-NEXT TLE group (current elements).
pub const CELESTRAK_URL: &str =
    "https://celestrak.org/NORAD/elements/gp.php?GROUP=iridium-NEXT&FORMAT=tle";

struct Sat {
    name: String,
    constants: sgp4::Constants,
    epoch_unix: f64,
}

pub struct SatMap {
    sats: Vec<Sat>,
}

impl SatMap {
    /// Parse a 3-line-per-satellite TLE text block (name / line1 / line2).
    pub fn from_tle(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let mut sats = Vec::new();
        let mut i = 0;
        while i + 2 < lines.len() + 1 {
            if i + 2 >= lines.len() {
                break;
            }
            let name = lines[i].trim();
            let l1 = lines[i + 1].trim();
            let l2 = lines[i + 2].trim();
            if l1.starts_with("1 ") && l2.starts_with("2 ") {
                if let Ok(elements) =
                    sgp4::Elements::from_tle(Some(name.to_string()), l1.as_bytes(), l2.as_bytes())
                {
                    if let Ok(constants) = sgp4::Constants::from_elements(&elements) {
                        let epoch_unix = elements.datetime.and_utc().timestamp() as f64
                            + elements.datetime.and_utc().timestamp_subsec_nanos() as f64 / 1e9;
                        sats.push(Sat { name: name.to_string(), constants, epoch_unix });
                    }
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        SatMap { sats }
    }

    pub fn len(&self) -> usize {
        self.sats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sats.is_empty()
    }

    /// Propagate a satellite to `unix` and return its geocentric ECEF
    /// position in km (TEME → ECEF via GMST rotation about Z).
    fn propagate_ecef(s: &Sat, unix: f64) -> Option<[f64; 3]> {
        let min = (unix - s.epoch_unix) / 60.0;
        let pred = s.constants.propagate(sgp4::MinutesSinceEpoch(min)).ok()?;
        let [tx, ty, tz] = pred.position; // TEME, km
        let (sin_g, cos_g) = gmst_rad(unix).sin_cos();
        Some([tx * cos_g + ty * sin_g, -tx * sin_g + ty * cos_g, tz])
    }

    /// Nearest satellite (name, distance km) to a geocentric ECEF position
    /// (km) at `unix` seconds, within `MAX_DIST_KM`.
    pub fn match_sat(&self, ex: f64, ey: f64, ez: f64, unix: f64) -> Option<(&str, f64)> {
        let mut best: Option<(&str, f64)> = None;
        for s in &self.sats {
            let Some([cx, cy, cz]) = Self::propagate_ecef(s, unix) else {
                continue;
            };
            let d = ((cx - ex).powi(2) + (cy - ey).powi(2) + (cz - ez).powi(2)).sqrt();
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((s.name.as_str(), d));
            }
        }
        best.filter(|&(_, d)| d <= MAX_DIST_KM)
    }

    /// Enrich a ring-alert message with the matched satellite name.
    pub fn enrich(&self, msg: &mut Message) {
        let MessageBody::Iridium { kind, details } = &mut msg.body else {
            return;
        };
        if *kind != "ring-alert" {
            return;
        }
        let (Some(x), Some(y), Some(z)) = (
            details.get("x").and_then(|v| v.as_i64()),
            details.get("y").and_then(|v| v.as_i64()),
            details.get("z").and_then(|v| v.as_i64()),
        ) else {
            return;
        };
        let unix = msg.timestamp.timestamp() as f64
            + msg.timestamp.timestamp_subsec_nanos() as f64 / 1e9;
        // IRA ECEF components are in units of 4 km.
        if let Some((name, dist)) =
            self.match_sat(x as f64 * 4.0, y as f64 * 4.0, z as f64 * 4.0, unix)
        {
            let name = name.to_string();
            if let Some(obj) = details.as_object_mut() {
                obj.insert("satellite".into(), serde_json::json!(name));
                obj.insert("satellite_dist_km".into(), serde_json::json!(dist.round()));
            }
        }
    }
}

/// Greenwich Mean Sidereal Time (radians) at a Unix timestamp (IAU 1982).
fn gmst_rad(unix: f64) -> f64 {
    let jd = unix / 86400.0 + 2440587.5;
    let t = (jd - 2451545.0) / 36525.0;
    let gmst_sec = 67310.54841
        + (876600.0 * 3600.0 + 8640184.812866) * t
        + 0.093104 * t * t
        - 6.2e-6 * t * t * t;
    gmst_sec.rem_euclid(86400.0) * std::f64::consts::TAU / 86400.0
}

/// Fetch a TLE text block over HTTP(S).
pub fn fetch_tle(url: &str) -> anyhow::Result<String> {
    let body = ureq::get(url)
        .timeout(Duration::from_secs(20))
        .call()?
        .into_string()?;
    Ok(body)
}

/// Load a SatMap from the configured source: a file path, or (default)
/// the Celestrak Iridium-NEXT group auto-fetched over HTTPS.
pub fn load(source: &str) -> anyhow::Result<SatMap> {
    let text = if source == "auto" {
        fetch_tle(CELESTRAK_URL)?
    } else {
        std::fs::read_to_string(source)?
    };
    let map = SatMap::from_tle(&text);
    if map.is_empty() {
        anyhow::bail!("no TLEs parsed from {source}");
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmst_at_j2000() {
        // J2000 (2000-01-01 12:00:00 UTC, unix 946728000): GMST ≈ 280.46°.
        let g = gmst_rad(946_728_000.0).to_degrees();
        assert!((g - 280.46).abs() < 0.05, "gmst={g}");
    }

    #[test]
    fn propagates_tle_to_leo_radius() {
        // Canonical valid TLE (ISS); at its own epoch the propagated radius
        // must be a plausible LEO value (~6780 km). Validates that TLE
        // parsing + SGP4 propagation are wired correctly.
        let tle = "ISS (ZARYA)\n\
            1 25544U 98067A   08264.51782528 -.00002182  00000-0 -11606-4 0  2927\n\
            2 25544  51.6416 247.4627 0006703 130.5360 325.0288 15.72125391563537\n";
        let map = SatMap::from_tle(tle);
        assert_eq!(map.len(), 1);
        let s = &map.sats[0];
        let pred = s.constants.propagate(sgp4::MinutesSinceEpoch(0.0)).unwrap();
        let [x, y, z] = pred.position;
        let r = (x * x + y * y + z * z).sqrt();
        assert!((6600.0..7000.0).contains(&r), "radius {r} km out of LEO range");
    }

    #[test]
    fn matches_satellite_to_its_own_position() {
        // Two real Iridium-NEXT TLEs (Celestrak snapshot). Propagating one
        // to a time near its epoch and feeding its ECEF back to match_sat
        // must identify that satellite (≈0 km) and not its neighbour —
        // exercising parse, propagate, TEME→ECEF, nearest-search and gate.
        let tle = "IRIDIUM 106\n\
            1 41917U 17003A   26164.81636817  .00000002  00000+0 -64509-5 0  9993\n\
            2 41917  86.3963  89.9654 0001784  83.5618 276.5781 14.34217470492724\n\
            IRIDIUM 103\n\
            1 41918U 17003B   26164.87345235  .00000072  00000+0  18697-4 0  9993\n\
            2 41918  86.3961  89.8413 0002359  78.7002 281.4459 14.34217734492750\n";
        let map = SatMap::from_tle(tle);
        assert_eq!(map.len(), 2);
        // A time ~30 min after the first TLE's epoch.
        let unix = map.sats[0].epoch_unix + 1800.0;
        let [x, y, z] = SatMap::propagate_ecef(&map.sats[0], unix).unwrap();
        let (name, dist) = map.match_sat(x, y, z, unix).expect("a match");
        assert_eq!(name, "IRIDIUM 106");
        assert!(dist < 1.0, "self-match distance {dist} km");
        // A position 9000 km from any satellite must not match (gate).
        assert!(map.match_sat(0.0, 0.0, 9000.0, unix).is_none());
    }
}
