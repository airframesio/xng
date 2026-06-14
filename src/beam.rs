//! Iridium 48-beam pattern reconstruction and geographic projection
//! (the live-map equivalent of iridium-toolkit's `beam-plotter.py`).
//!
//! Every Iridium satellite carries the same fixed 48-spot-beam pattern.
//! Ring-alert frames give us either the satellite's own geocentric ECEF
//! position (high altitude) or a single beam's ground footprint (low
//! altitude). By de-rotating an observed footprint into the satellite's
//! local frame — undoing the satellite's longitude, latitude, and orbital
//! inclination, the inclination signed by the direction of travel — we
//! recover that beam's fixed offset from nadir. Accumulated over many
//! observations this reconstructs the whole pattern, which we can then
//! re-project onto the ground beneath any tracked satellite to draw all
//! 48 cells (including beams not currently being observed).
//!
//! North- and south-bound passes mirror the pattern, so they are kept in
//! separate patterns and projected with the matching direction.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Ground footprints sit on this sphere (km); the toolkit uses 6371.
const R_EARTH_KM: f64 = 6371.0;
/// Iridium orbital inclination (deg) — the de-rotation angle.
const INC0_DEG: f64 = 84.0;
/// A satellite's high-altitude fix is "current" for a footprint within
/// this many seconds (matches beam-plotter's staleness gate).
const FRESH_S: f64 = 10.0;
/// A satellite's travel direction is stable across a whole pass, so once
/// established it is kept (sticky) across high-fix gaps up to this long
/// rather than reset to unknown on every gap — which otherwise leaves most
/// sparsely-seen satellites at direction 0, recording no footprints and
/// projecting nothing. Reset after this (a later sighting is a new pass,
/// possibly the opposite direction).
const DIR_STICKY_S: f64 = 600.0;
/// Altitude bands (km above the surface) that tell a satellite's own
/// position report from a ground beam footprint, and reject decodes whose
/// altitude is physically impossible. A BCH false-pass can yield a garbage
/// position; on the live map one bad satellite fix both plants a phantom
/// marker and corrupts every footprint de-rotated against it for the next
/// few seconds, so altitudes outside these bands are dropped. Operational
/// Iridium flies at ~780 km; footprints sit on the ground.
const SAT_ALT_MIN_KM: f64 = 600.0;
const SAT_ALT_MAX_KM: f64 = 1100.0;
const GROUND_ALT_MIN_KM: f64 = -150.0;
const GROUND_ALT_MAX_KM: f64 = 200.0;
/// Footprints a beam needs before it is confident enough to project: a
/// lone observation can sit far off through a stale satellite fix or a
/// mis-judged travel direction, so singletons are accumulated but not yet
/// drawn.
const MIN_OBS: u32 = 2;

/// How an IRA altitude reading is interpreted.
pub enum AltClass {
    /// The broadcasting satellite's own position (~780 km).
    Satellite,
    /// A ground beam footprint.
    Footprint,
    /// Physically impossible — a decode error to be ignored.
    Implausible,
}

/// Classify an IRA altitude (km above the surface) into satellite /
/// footprint / implausible. Shared by the reconstructor and the dashboard
/// so the map and the pattern agree on what is real.
pub fn classify_altitude(alt_km: f64) -> AltClass {
    if (SAT_ALT_MIN_KM..=SAT_ALT_MAX_KM).contains(&alt_km) {
        AltClass::Satellite
    } else if (GROUND_ALT_MIN_KM..=GROUND_ALT_MAX_KM).contains(&alt_km) {
        AltClass::Footprint
    } else {
        AltClass::Implausible
    }
}

fn inc_rad(north: i8) -> f64 {
    if north < 0 {
        -(180.0 - (90.0 - INC0_DEG)).to_radians()
    } else {
        -(90.0 - INC0_DEG).to_radians()
    }
}

// Elementary rotations matching beam-plotter's sign conventions.
fn rot_z(x: f64, y: f64, z: f64, a: f64) -> [f64; 3] {
    let (s, c) = a.sin_cos();
    [x * c - y * s, x * s + y * c, z]
}
fn rot_y(x: f64, y: f64, z: f64, a: f64) -> [f64; 3] {
    let (s, c) = a.sin_cos();
    [x * c - z * s, y, x * s + z * c]
}
fn rot_x(x: f64, y: f64, z: f64, a: f64) -> [f64; 3] {
    let (s, c) = a.sin_cos();
    [x, y * c - z * s, y * s + z * c]
}

fn lat_lon(p: [f64; 3]) -> (f64, f64) {
    (p[2].atan2((p[0] * p[0] + p[1] * p[1]).sqrt()), p[1].atan2(p[0]))
}

/// De-rotate an ECEF footprint (km) into the satellite frame; returns the
/// beam's (cross-track, along-track) offset in km.
fn to_sat_frame(sat: [f64; 3], fp: [f64; 3], north: i8) -> (f64, f64) {
    let (lat, lon) = lat_lon(sat);
    let inc = inc_rad(north);
    let p = rot_z(fp[0], fp[1], fp[2], -lon);
    let p = rot_y(p[0], p[1], p[2], -lat);
    let p = rot_x(p[0], p[1], p[2], -inc);
    (p[1], p[2])
}

/// Re-rotate a satellite-frame beam offset (km) back to ECEF (km) for a
/// satellite at `sat` travelling `north`. The footprint is on the ground
/// sphere, so the radial component is recovered from the offset.
fn to_ecef(sat: [f64; 3], y: f64, z: f64, north: i8) -> [f64; 3] {
    let (lat, lon) = lat_lon(sat);
    let inc = inc_rad(north);
    let x = (R_EARTH_KM * R_EARTH_KM - y * y - z * z).max(0.0).sqrt();
    let p = rot_x(x, y, z, inc);
    let p = rot_y(p[0], p[1], p[2], lat);
    rot_z(p[0], p[1], p[2], lon)
}

/// One direction's accumulated 48-beam pattern: per beam, the running mean
/// of the (cross, along) offset.
#[derive(Default, Serialize, Deserialize)]
struct Pattern {
    /// beam id (1..=48) -> (sum_cross, sum_along, count)
    cells: HashMap<u8, (f64, f64, u32)>,
}

impl Pattern {
    fn add(&mut self, beam: u8, cross: f64, along: f64) {
        let e = self.cells.entry(beam).or_insert((0.0, 0.0, 0));
        e.0 += cross;
        e.1 += along;
        e.2 += 1;
    }
    /// Mean (cross, along) offset for a beam, but only once it has at least
    /// `min` observations (confidence gate).
    fn mean(&self, beam: u8, min: u32) -> Option<(f64, f64)> {
        self.cells.get(&beam).filter(|c| c.2 >= min).map(|c| (c.0 / c.2 as f64, c.1 / c.2 as f64))
    }
}

/// Tracks each satellite's latest high-altitude fix and travel direction.
struct SatTrack {
    pos: [f64; 3],
    z_prev: f64,
    north: i8,
    time: f64,
}

/// One projected beam cell.
#[derive(Serialize)]
pub struct Cell {
    pub sat: u64,
    pub beam: u8,
    pub lat: f64,
    pub lon: f64,
}

/// Live reconstructor: fed every ring-alert frame, accumulates the pattern,
/// and projects it under the currently tracked satellites.
#[derive(Default)]
pub struct BeamReconstructor {
    north: Pattern,
    south: Pattern,
    #[allow(clippy::type_complexity)]
    tracks: HashMap<u64, SatTrack>,
}

impl BeamReconstructor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one ring-alert: the satellite id, its decoded altitude (km),
    /// raw ECEF position (km), beam id, and time (s). High-altitude frames
    /// update the satellite track; low-altitude frames (a beam footprint)
    /// are de-rotated into the matching-direction pattern.
    pub fn observe(&mut self, sat: u64, alt_km: f64, ecef: [f64; 3], beam: u8, time: f64) {
        match classify_altitude(alt_km) {
            AltClass::Satellite => {
                let north = match self.tracks.get(&sat) {
                    // Two fixes close together: recompute direction from the
                    // climb/descent of the geocentric z component.
                    Some(t) if time - t.time < FRESH_S => {
                        let dz = ecef[2] - t.z_prev;
                        if dz > 0.0 {
                            1
                        } else if dz < 0.0 {
                            -1
                        } else {
                            t.north
                        }
                    }
                    // Short gap: keep the last known direction (sticky).
                    Some(t) if time - t.time < DIR_STICKY_S => t.north,
                    // First fix or a long gap (likely a new pass): unknown.
                    _ => 0,
                };
                self.tracks.insert(sat, SatTrack { pos: ecef, z_prev: ecef[2], north, time });
            }
            AltClass::Footprint => {
                if let Some(t) = self.tracks.get(&sat) {
                    if t.north != 0 && time - t.time < FRESH_S {
                        let (cross, along) = to_sat_frame(t.pos, ecef, t.north);
                        let pat = if t.north < 0 { &mut self.south } else { &mut self.north };
                        pat.add(beam, cross, along);
                    }
                }
            }
            AltClass::Implausible => {}
        }
    }

    /// Project the reconstructed pattern under every satellite whose track
    /// is current (within `max_age` s of `now`), returning all 48 cells per
    /// satellite that have been reconstructed for that travel direction.
    pub fn project(&self, now: f64, max_age: f64) -> Vec<Cell> {
        let mut out = Vec::new();
        for (&sat, t) in &self.tracks {
            if t.north == 0 || now - t.time > max_age {
                continue;
            }
            let pat = if t.north < 0 { &self.south } else { &self.north };
            for beam in 1..=48u8 {
                if let Some((cross, along)) = pat.mean(beam, MIN_OBS) {
                    let e = to_ecef(t.pos, cross, along, t.north);
                    let (lat, lon) = lat_lon(e);
                    out.push(Cell {
                        sat,
                        beam,
                        lat: lat.to_degrees(),
                        lon: lon.to_degrees(),
                    });
                }
            }
        }
        out
    }

    /// Default on-disk location for the accumulated pattern.
    pub fn default_path() -> std::path::PathBuf {
        std::env::temp_dir().join("xng_beampattern.json")
    }

    /// Load the accumulated patterns from disk (tracks start empty); an
    /// absent or unreadable file yields an empty reconstructor.
    pub fn load(path: &std::path::Path) -> Self {
        let mut s = Self::default();
        if let Ok(txt) = std::fs::read_to_string(path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                if let Some(n) = v.get("north").cloned().and_then(|x| serde_json::from_value(x).ok()) {
                    s.north = n;
                }
                if let Some(so) = v.get("south").cloned().and_then(|x| serde_json::from_value(x).ok()) {
                    s.south = so;
                }
            }
        }
        s
    }

    /// Persist the accumulated patterns (best-effort). Two safeguards keep a
    /// hard-won pattern from being lost across restarts: never overwrite the
    /// file with an empty pattern (a cold start or a failed load would
    /// otherwise clobber good data on the next checkpoint), and write
    /// atomically via a temp file + rename so a process killed mid-write
    /// cannot truncate the live file.
    pub fn save(&self, path: &std::path::Path) {
        if self.beams_known() == 0 {
            return;
        }
        let v = serde_json::json!({ "north": &self.north, "south": &self.south });
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Number of distinct beams reconstructed (either direction).
    pub fn beams_known(&self) -> usize {
        let mut s: std::collections::HashSet<u8> = self.north.cells.keys().copied().collect();
        s.extend(self.south.cells.keys());
        s.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ecef(lat_deg: f64, lon_deg: f64, r: f64) -> [f64; 3] {
        let (la, lo) = (lat_deg.to_radians(), lon_deg.to_radians());
        [r * la.cos() * lo.cos(), r * la.cos() * lo.sin(), r * la.sin()]
    }

    #[test]
    fn sat_frame_round_trips() {
        // A footprint on the ground, de-rotated into the sat frame and back
        // (same satellite + direction) must return to its lat/lon.
        let sat = ecef(40.0, -120.0, R_EARTH_KM + 780.0);
        let fp = ecef(41.5, -118.5, R_EARTH_KM);
        for north in [1i8, -1] {
            let (cross, along) = to_sat_frame(sat, fp, north);
            let back = to_ecef(sat, cross, along, north);
            let (lat, lon) = lat_lon(back);
            assert!((lat.to_degrees() - 41.5).abs() < 0.05, "lat {} (n={north})", lat.to_degrees());
            assert!((lon.to_degrees() - (-118.5)).abs() < 0.05, "lon {} (n={north})", lon.to_degrees());
        }
    }

    #[test]
    fn nadir_beam_projects_under_satellite() {
        // A footprint directly below the satellite (same lat/lon, on the
        // ground) has ~zero offset and projects back under the satellite.
        let sat = ecef(35.0, 10.0, R_EARTH_KM + 780.0);
        let fp = ecef(35.0, 10.0, R_EARTH_KM);
        let (cross, along) = to_sat_frame(sat, fp, 1);
        assert!(cross.abs() < 1.0 && along.abs() < 1.0, "nadir offset ({cross},{along})");
        // Project under a *different* satellite: cell lands under it.
        let sat2 = ecef(-20.0, 150.0, R_EARTH_KM + 780.0);
        let e = to_ecef(sat2, cross, along, 1);
        let (lat, lon) = lat_lon(e);
        assert!((lat.to_degrees() - (-20.0)).abs() < 0.5, "lat {}", lat.to_degrees());
        assert!((lon.to_degrees() - 150.0).abs() < 0.5, "lon {}", lon.to_degrees());
    }

    #[test]
    fn reconstruct_and_project() {
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        // Track satellite 5 northbound (two rising high fixes), then two
        // footprints for beam 12 (one is below the confidence gate).
        r.observe(5, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(5, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 1.0);
        r.observe(5, 16.0, ecef(41.5, -119.0, R_EARTH_KM), 12, t0 + 2.0);
        // A single observation is held back (MIN_OBS gate).
        assert!(r.project(t0 + 3.0, 60.0).iter().all(|c| c.beam != 12));
        r.observe(5, 16.0, ecef(41.5, -119.0, R_EARTH_KM), 12, t0 + 3.0);
        assert_eq!(r.beams_known(), 1);
        let cells = r.project(t0 + 4.0, 60.0);
        let c = cells.iter().find(|c| c.beam == 12).expect("beam 12 projected");
        // Re-projected under the same (still-current) satellite → original.
        assert!((c.lat - 41.5).abs() < 0.1, "lat {}", c.lat);
        assert!((c.lon - (-119.0)).abs() < 0.1, "lon {}", c.lon);
    }

    #[test]
    fn pattern_survives_save_load_round_trip() {
        // Accumulate a beam, persist, reload — the means must survive so a
        // restart does not discard the hard-won pattern.
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(7, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(7, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 1.0);
        r.observe(7, 16.0, ecef(41.5, -119.0, R_EARTH_KM), 12, t0 + 2.0);
        r.observe(7, 16.0, ecef(41.5, -119.0, R_EARTH_KM), 12, t0 + 3.0);
        assert_eq!(r.beams_known(), 1);
        let path = std::env::temp_dir().join("xng_beampattern_test.json");
        r.save(&path);
        let r2 = BeamReconstructor::load(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(r2.beams_known(), 1, "reloaded pattern lost its beam");
        // And it projects under a fresh directional track.
        let mut r2 = r2;
        r2.observe(7, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 100.0);
        r2.observe(7, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 101.0);
        let cells = r2.project(t0 + 102.0, 60.0);
        assert!(cells.iter().any(|c| c.beam == 12), "reloaded beam did not project");
    }

    #[test]
    fn direction_is_sticky_across_short_gaps() {
        // Establish northbound direction, then a high fix after a gap longer
        // than FRESH_S but shorter than DIR_STICKY_S: the direction must
        // persist (not reset to 0), so a following footprint still records.
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(3, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(3, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 1.0);
        // Gap of 60 s (> FRESH_S 10, < DIR_STICKY_S 600): direction sticky.
        let t1 = t0 + 61.0;
        r.observe(3, 780.0, ecef(50.0, -120.0, R_EARTH_KM + 780.0), 0, t1);
        r.observe(3, 16.0, ecef(50.5, -119.0, R_EARTH_KM), 9, t1 + 1.0);
        r.observe(3, 16.0, ecef(50.5, -119.0, R_EARTH_KM), 9, t1 + 2.0);
        assert_eq!(r.beams_known(), 1, "footprint recorded via sticky direction");
        // A long gap (> DIR_STICKY_S) resets direction to unknown: a lone
        // high fix after it cannot record a footprint.
        let t2 = t1 + 700.0;
        r.observe(3, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t2);
        r.observe(3, 16.0, ecef(40.5, -119.0, R_EARTH_KM), 14, t2 + 1.0);
        assert_eq!(r.beams_known(), 1, "no new beam after a direction reset");
    }

    #[test]
    fn rejects_implausible_altitude() {
        // A BCH false-pass yields a garbage position at an impossible
        // altitude. It must neither create a satellite track nor be taken
        // as a footprint, so it cannot corrupt the pattern.
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(9, 3836.0, ecef(47.0, -52.0, R_EARTH_KM + 3836.0), 21, t0);
        assert!(r.project(t0 + 1.0, 60.0).is_empty(), "no track from a bad fix");
        // A real northbound satellite, then a garbage "footprint" at 1900 km
        // (outside the ground band) must be ignored.
        r.observe(9, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 2.0);
        r.observe(9, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 3.0);
        r.observe(9, 1900.0, ecef(41.5, -119.0, R_EARTH_KM + 1900.0), 7, t0 + 4.0);
        assert_eq!(r.beams_known(), 0, "implausible footprint not recorded");
    }
}
