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
/// Re-derive travel direction from two high fixes up to this far apart. A
/// visible pass is monotonic in latitude, and ring-alerts can be sparse, so
/// this is much wider than the footprint freshness gate — the sign of the
/// z change between fixes a minute or two apart is still unambiguous.
const DIR_RECOMPUTE_S: f64 = 120.0;
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
/// Iridium's 48 beams are a fixed **4-tier concentric pattern** — 3 Main
/// Mission Antennas, each painting a 16-beam sector (1+3+5+7), so the tiers
/// hold 3 / 9 / 15 / 21 beams from the inner ring outward (structure per the
/// MathWorks Satellite Communications Toolbox Iridium model, FCC filings).
const TIER_COUNT: [usize; 4] = [3, 9, 15, 21];
/// Each tier's ground radius from nadir (km), from the off-nadir boresight
/// angles (~11° / 24° / 42° / 57°) projected from 780 km onto the Earth
/// sphere. Calibrated so the outer tier matches the observed ~1480 km extent
/// (the MathWorks example's 45°/834 km undershoots real coverage, which runs
/// to ~8° elevation ≈ 57° off-nadir).
const TIER_RADIUS_KM: [f64; 4] = [152.0, 351.0, 744.0, 1475.0];
/// Radial band boundaries between tiers (km), midway between tier radii, with
/// an outer edge symmetric to the inner gap. Used to size each beam's radial
/// extent so adjacent tiers tile.
const TIER_BOUND_KM: [f64; 5] = [0.0, 251.0, 547.0, 1109.0, 1841.0];
/// Footprints are drawn a touch larger than touching so neighbours overlap,
/// as real Iridium beams do for handoff (contiguous, gap-free coverage).
const BEAM_OVERLAP_F: f64 = 1.06;
/// A canonical slot counts as decoded (drawn solid) when a reconstructed beam
/// sits within this distance of it; otherwise it renders faint as a
/// not-yet-decoded beam.
const DECODE_MATCH_KM: f64 = 280.0;
/// A beam counts as "active" (currently illuminating, drawn bright) if this
/// satellite was seen on it within this many seconds; older beams stay in
/// the pattern but render muted.
const ACTIVE_WINDOW_S: f64 = 30.0;

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

/// Which of the 4 concentric tiers a beam at ground distance `d` (km) from
/// nadir belongs to (nearest tier radius).
fn nearest_tier(d: f64) -> usize {
    (0..4)
        .min_by(|&a, &b| {
            (d - TIER_RADIUS_KM[a]).abs().total_cmp(&(d - TIER_RADIUS_KM[b]).abs())
        })
        .unwrap()
}

/// A tier's beam footprint as (radial semi-axis, azimuthal semi-axis) in km.
/// Radial = half the tier's radial band (so tiers tile inward/outward);
/// azimuthal = half the chord to the adjacent beam in the same tier. Outer
/// tiers come out radially elongated, matching the real oblique projection.
fn tier_axes(tier: usize) -> (f64, f64) {
    let r = TIER_RADIUS_KM[tier];
    let radial = (r - TIER_BOUND_KM[tier]).max(TIER_BOUND_KM[tier + 1] - r);
    let azim = r * (std::f64::consts::PI / TIER_COUNT[tier] as f64).sin();
    (radial * BEAM_OVERLAP_F, azim * BEAM_OVERLAP_F)
}

/// Build a beam's elliptical ground footprint (boundary lat/lon in degrees)
/// for a beam centred at satellite-frame offset (cross, along): an ellipse
/// with semi-major `a_rad` along the radial direction (nadir→beam) and
/// semi-minor `b_az` across it, re-projected to ECEF then lat/lon.
fn ellipse_poly(sat: [f64; 3], north: i8, cross: f64, along: f64, a_rad: f64, b_az: f64) -> Vec<(f64, f64)> {
    let rmag = (cross * cross + along * along).sqrt().max(1.0);
    let (urc, ura) = (cross / rmag, along / rmag); // radial unit (cross, along)
    let (utc, uta) = (-ura, urc); // azimuthal unit (perpendicular)
    (0..18)
        .map(|p| {
            let (s, c) = (std::f64::consts::TAU * p as f64 / 18.0).sin_cos();
            let dc = a_rad * c * urc + b_az * s * utc;
            let da = a_rad * c * ura + b_az * s * uta;
            let (lat, lon) = lat_lon(to_ecef(sat, cross + dc, along + da, north));
            (lat.to_degrees(), lon.to_degrees())
        })
        .collect()
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

/// One direction's accumulated 48-beam pattern: per beam, the running sums
/// needed for both the mean offset and its spread (so a cell can be drawn
/// at its actual reconstructed extent, as iridium-toolkit's beam-plotter
/// sizes each cell by the scatter of its observations).
#[derive(Default, Serialize, Deserialize)]
struct Pattern {
    /// beam id (1..=48) -> (sum_cross, sum_along, sum_sq_radial, count),
    /// where sum_sq_radial accumulates cross²+along² for the variance.
    cells: HashMap<u8, (f64, f64, f64, u32)>,
}

impl Pattern {
    fn add(&mut self, beam: u8, cross: f64, along: f64) {
        let e = self.cells.entry(beam).or_insert((0.0, 0.0, 0.0, 0));
        e.0 += cross;
        e.1 += along;
        e.2 += cross * cross + along * along;
        e.3 += 1;
    }
    /// Mean (cross, along) offset for a beam, once it has at least `min`
    /// observations (confidence gate). The drawn footprint radius is derived
    /// in `project` from the spacing to neighbouring beams (actual coverage),
    /// not from this scatter, so the cell tiles the footprint as a real beam
    /// does rather than collapsing to the reconstruction's tightness.
    fn mean(&self, beam: u8, min: u32) -> Option<(f64, f64)> {
        let &(sx, sy, _sq, n) = self.cells.get(&beam).filter(|c| c.3 >= min)?;
        let n = n as f64;
        let (mx, my) = (sx / n, sy / n);
        Some((mx, my))
    }
}

/// Tracks each satellite's latest high-altitude fix and travel direction,
/// plus when each of its beams was last seen illuminating the ground (so a
/// projected cell can be marked active vs merely reconstructed).
struct SatTrack {
    pos: [f64; 3],
    z_prev: f64,
    north: i8,
    time: f64,
    /// beam id -> last footprint time observed for THIS satellite.
    seen_beams: HashMap<u8, f64>,
}

/// One projected beam cell, with the reconstructed footprint radius (m) so
/// the map can draw each beam at its actual extent. `active` is true when
/// this satellite was seen illuminating this beam within ACTIVE_WINDOW_S
/// (so the map can brighten live beams and mute the rest of the pattern).
#[derive(Serialize)]
pub struct Cell {
    pub sat: u64,
    /// Decoded beam id (1..=48), or 0 for a not-yet-decoded canonical slot.
    pub beam: u8,
    pub lat: f64,
    pub lon: f64,
    /// Representative radius (m) — mean of the elliptical axes — kept for the
    /// coverage-footprint disc and the spot-beam markers that reuse it.
    pub radius_m: f64,
    /// Elliptical footprint boundary (lat, lon degrees), radially elongated
    /// per the oblique projection so beams tile contiguously.
    pub poly: Vec<(f64, f64)>,
    pub active: bool,
    /// True when this station has actually decoded a beam at this slot; false
    /// for a modelled (not-yet-decoded) slot, drawn faint.
    pub decoded: bool,
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
                    // Any prior fix within the recompute window gives a
                    // reliable heading: across a visible pass the satellite
                    // moves monotonically in latitude, so the sign of the
                    // geocentric-z change is unambiguous even for fixes a
                    // minute or two apart. Requiring two fixes <10 s apart
                    // (beam-plotter's gate) was too strict for the sparse
                    // ring-alert rate here, leaving most satellites with no
                    // direction (and therefore projecting no pattern).
                    Some(t) if time - t.time < DIR_RECOMPUTE_S => {
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
                // Preserve the per-beam observation history across fixes.
                let e = self.tracks.entry(sat).or_insert_with(|| SatTrack {
                    pos: ecef,
                    z_prev: ecef[2],
                    north,
                    time,
                    seen_beams: HashMap::new(),
                });
                e.pos = ecef;
                e.z_prev = ecef[2];
                e.north = north;
                e.time = time;
            }
            AltClass::Footprint => {
                // Read what we need from the track, then borrow the pattern.
                let (north, pos) = match self.tracks.get(&sat) {
                    Some(t) if t.north != 0 && time - t.time < FRESH_S => (t.north, t.pos),
                    _ => return,
                };
                let (cross, along) = to_sat_frame(pos, ecef, north);
                if let Some(t) = self.tracks.get_mut(&sat) {
                    t.seen_beams.insert(beam, time); // mark this beam active
                }
                let pat = if north < 0 { &mut self.south } else { &mut self.north };
                pat.add(beam, cross, along);
            }
            AltClass::Implausible => {}
        }
    }

    /// Project the full 48-beam pattern under every satellite whose track is
    /// current. Beams we have decoded render at their measured positions; the
    /// rest of the canonical 4-tier layout fills in as not-yet-decoded slots,
    /// so the map shows the whole intended pattern, gap-free, plus exactly
    /// what's been heard. Each beam is an elliptical footprint sized from its
    /// tier so neighbours overlap into contiguous coverage.
    pub fn project(&self, now: f64, max_age: f64) -> Vec<Cell> {
        let two_pi = std::f64::consts::TAU;
        // The 48 canonical slot centres (cross, along) for a given azimuth
        // phase, tier by tier (a small per-tier stagger breaks ring alignment).
        let slots_at = |phase: f64| -> Vec<(usize, f64, f64)> {
            let mut v = Vec::with_capacity(48);
            for (tier, &n) in TIER_COUNT.iter().enumerate() {
                let r = TIER_RADIUS_KM[tier];
                let off = phase + tier as f64 * 0.35;
                for k in 0..n {
                    let psi = off + k as f64 * two_pi / n as f64;
                    v.push((tier, r * psi.sin(), r * psi.cos()));
                }
            }
            v
        };

        let mut out = Vec::new();
        for (&sat, t) in &self.tracks {
            if t.north == 0 || now - t.time > max_age {
                continue;
            }
            let pat = if t.north < 0 { &self.south } else { &self.north };
            // Confident beam means for this direction (cross, along) in km.
            let means: Vec<(u8, f64, f64)> = (1..=48u8)
                .filter_map(|b| pat.mean(b, MIN_OBS).map(|(c, a)| (b, c, a)))
                .collect();

            // Fit the canonical pattern's azimuth phase to the decoded beams
            // (coarse search), so the modelled slots line up with what we've
            // actually heard and the not-yet-decoded slots land in the gaps.
            let mut phase = 0.0;
            if !means.is_empty() {
                let mut best = f64::INFINITY;
                for s in 0..120 {
                    let p = two_pi * s as f64 / 120.0;
                    let slots = slots_at(p);
                    let cost: f64 = means
                        .iter()
                        .map(|&(_, c, a)| {
                            slots
                                .iter()
                                .map(|&(_, sc, sa)| (c - sc).powi(2) + (a - sa).powi(2))
                                .fold(f64::INFINITY, f64::min)
                                .sqrt()
                        })
                        .sum();
                    if cost < best {
                        best = cost;
                        phase = p;
                    }
                }
            }

            // Match each decoded beam to its single nearest canonical slot
            // (within DECODE_MATCH_KM); those slots are then "decoded" and not
            // redrawn as faint, so one decoded beam frees exactly one slot.
            let slots = slots_at(phase);
            let mut matched = vec![false; slots.len()];
            for &(_, c, a) in &means {
                let mut best = (DECODE_MATCH_KM, None);
                for (i, &(_, sc, sa)) in slots.iter().enumerate() {
                    let d = ((c - sc).powi(2) + (a - sa).powi(2)).sqrt();
                    if d < best.0 {
                        best = (d, Some(i));
                    }
                }
                if let Some(i) = best.1 {
                    matched[i] = true;
                }
            }
            // Decoded beams: drawn solid at their measured positions, sized by
            // the tier their distance-from-nadir falls in.
            for &(beam, cross, along) in &means {
                let (a_rad, b_az) = tier_axes(nearest_tier((cross * cross + along * along).sqrt()));
                let active = t.seen_beams.get(&beam).is_some_and(|&ts| now - ts < ACTIVE_WINDOW_S);
                let (lat, lon) = lat_lon(to_ecef(t.pos, cross, along, t.north));
                out.push(Cell {
                    sat,
                    beam,
                    lat: lat.to_degrees(),
                    lon: lon.to_degrees(),
                    radius_m: (a_rad + b_az) / 2.0 * 1000.0,
                    poly: ellipse_poly(t.pos, t.north, cross, along, a_rad, b_az),
                    active,
                    decoded: true,
                });
            }
            // Not-yet-decoded slots: every unmatched canonical slot, drawn
            // faint so the full 48-beam pattern is visible.
            for (i, &(tier, sc, sa)) in slots.iter().enumerate() {
                if matched[i] {
                    continue;
                }
                let (a_rad, b_az) = tier_axes(tier);
                let (lat, lon) = lat_lon(to_ecef(t.pos, sc, sa, t.north));
                out.push(Cell {
                    sat,
                    beam: 0,
                    lat: lat.to_degrees(),
                    lon: lon.to_degrees(),
                    radius_m: (a_rad + b_az) / 2.0 * 1000.0,
                    poly: ellipse_poly(t.pos, t.north, sc, sa, a_rad, b_az),
                    active: false,
                    decoded: false,
                });
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
    fn direction_acquired_from_sparse_fixes() {
        // Two high fixes 60 s apart (never <10 s apart) must still establish
        // a heading, so a sparsely-seen satellite projects its pattern.
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(8, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(8, 780.0, ecef(46.0, -121.0, R_EARTH_KM + 780.0), 0, t0 + 60.0);
        // Footprints close to the latest fix get recorded → beam known.
        r.observe(8, 16.0, ecef(46.5, -120.0, R_EARTH_KM), 9, t0 + 62.0);
        r.observe(8, 16.0, ecef(46.5, -120.0, R_EARTH_KM), 9, t0 + 63.0);
        assert_eq!(r.beams_known(), 1, "sparse fixes still establish direction");
        assert!(
            r.project(t0 + 64.0, 600.0).iter().any(|c| c.beam == 9),
            "pattern projects for a sparsely-fixed satellite",
        );
    }

    #[test]
    fn full_pattern_fills_undecoded_slots() {
        // The canonical 4-tier model projects all 48 beams: the decoded ones
        // solid at their measured positions, the rest as faint not-yet-decoded
        // slots — so the map always shows the whole intended pattern.
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(5, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(5, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 1.0);
        r.observe(5, 16.0, ecef(41.2, -116.0, R_EARTH_KM), 20, t0 + 2.0);
        r.observe(5, 16.0, ecef(41.2, -116.0, R_EARTH_KM), 20, t0 + 3.0);
        let cells = r.project(t0 + 4.0, 60.0);
        // 48-beam pattern: decoded beam frees its one nearest slot (48), or
        // none if it's too far from any (49).
        assert!((47..=49).contains(&cells.len()), "full pattern, got {}", cells.len());
        assert!(cells.iter().any(|c| c.decoded && c.beam == 20), "decoded beam is solid");
        assert!(cells.iter().any(|c| !c.decoded && c.beam == 0), "undecoded slots fill the rest");
        assert!(cells.iter().all(|c| c.poly.len() >= 8), "every beam has a footprint polygon");
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
    fn active_flag_reflects_recent_footprint() {
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(2, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(2, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 1.0);
        r.observe(2, 16.0, ecef(41.5, -119.0, R_EARTH_KM), 7, t0 + 2.0);
        r.observe(2, 16.0, ecef(41.5, -119.0, R_EARTH_KM), 7, t0 + 3.0);
        // Seen ~1 s ago → active.
        let c = r.project(t0 + 4.0, 600.0).into_iter().find(|c| c.beam == 7).expect("beam 7");
        assert!(c.active, "recently-seen beam must be active");
        // Long past the active window but track still current → still in the
        // pattern, but muted (inactive).
        let c = r.project(t0 + 50.0, 600.0).into_iter().find(|c| c.beam == 7).expect("still present");
        assert!(!c.active, "beam not seen for >30 s must be muted");
    }

    #[test]
    fn beam_footprint_sized_by_tier() {
        // Footprints are sized by concentric tier: an outer beam (spaced and
        // projected wider) gets a larger footprint than an inner beam.
        let mut r = BeamReconstructor::new();
        let t0 = 1000.0;
        r.observe(1, 780.0, ecef(40.0, -120.0, R_EARTH_KM + 780.0), 0, t0);
        r.observe(1, 780.0, ecef(41.0, -120.0, R_EARTH_KM + 780.0), 0, t0 + 1.0);
        // Inner beam (~84 km from nadir) and an outer beam (~1170 km east).
        r.observe(1, 16.0, ecef(41.0, -119.0, R_EARTH_KM), 5, t0 + 2.0);
        r.observe(1, 16.0, ecef(41.0, -119.0, R_EARTH_KM), 5, t0 + 3.0);
        r.observe(1, 16.0, ecef(41.0, -106.0, R_EARTH_KM), 6, t0 + 4.0);
        r.observe(1, 16.0, ecef(41.0, -106.0, R_EARTH_KM), 6, t0 + 5.0);
        let cells = r.project(t0 + 6.0, 60.0);
        let inner = cells.iter().find(|c| c.beam == 5).expect("inner beam");
        let outer = cells.iter().find(|c| c.beam == 6).expect("outer beam");
        assert!(
            outer.radius_m > inner.radius_m,
            "outer-tier beam larger: {} vs {}",
            outer.radius_m,
            inner.radius_m
        );
        for c in [inner, outer] {
            assert!(
                (80_000.0..=450_000.0).contains(&c.radius_m),
                "plausible footprint, got {}",
                c.radius_m
            );
        }
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
