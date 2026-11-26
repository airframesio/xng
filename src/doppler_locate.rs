//! Receiver location from Iridium ring-alert Doppler, with no GPS.
//!
//! Every satellite broadcasts its ring alerts on one fixed frequency
//! ([`RING_ALERT_HZ`]), and each ring alert names its own position, which
//! `satmap` matches to a TLE satellite. As a satellite passes, the received
//! frequency moves by up to about ±37 kHz, by an amount set by where the
//! receiver is:
//!
//! ```text
//! f = F0 · (1 − ṙ / c) + b,   ṙ = (s − u)·v / |s − u|
//! ```
//!
//! where `s`, `v` are the satellite's Earth-fixed position and velocity (from
//! the TLE), `u` the receiver, and `b` the receiver's own frequency error. The
//! fit solves for latitude, longitude, `b` and `dt`, a constant lag between
//! when a burst arrived and when it was stamped (decode latency). Altitude is
//! held fixed (the configured receiver altitude, else 0 m): Doppler barely
//! constrains it.
//!
//! Method: a coarse global grid (`b` solved in closed form at each point, so
//! the search is 2-D), then Levenberg-Marquardt on the best few minima, then a
//! refit without measurements beyond 3σ. One satellite pass alone leaves a
//! mirror solution on the other side of its ground track, so a fit is called
//! *settled* only once it uses two or more satellites and two consecutive fits
//! agree within [`SETTLED_KM`]. Clean-room implementation of the standard
//! Doppler positioning method (cf. Tan et al., "New Method for Positioning
//! Using IRIDIUM Satellite Signals of Opportunity", IEEE Access 7, 2019).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use xng_types::{DecodeQuality, Message, MessageBody, Mode, SignalQuality};

use crate::satmap::{SatMap, SatState};

/// The simplex ring-alert channel (Hz).
pub const RING_ALERT_HZ: f64 = 1_626_270_833.0;
const C_KM_S: f64 = 299_792.458;
/// Measurements older than this are not fitted (the receiver's frequency
/// error drifts; 30 min usually holds two or three passes).
const WINDOW_S: f64 = 1800.0;
const MAX_MEAS: usize = 4000;
/// Fit no more often than this.
const SOLVE_EVERY_S: f64 = 60.0;
const MIN_MEAS: usize = 30;
/// Two consecutive fits this close (km) settle the estimate.
pub const SETTLED_KM: f64 = 20.0;
/// A ring alert whose offset from the nominal channel exceeds this is not a
/// ring alert on the simplex channel (max Doppler ±37 kHz plus receiver error).
const MAX_OFFSET_HZ: f64 = 60_000.0;

#[derive(Debug, Clone)]
struct Meas {
    sat: String,
    t: f64,
    f_hz: f64,
    st: SatState,
}

/// One fit.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Receiver frequency error at the ring-alert channel (Hz).
    pub offset_hz: f64,
    /// Stamp lag (s): how much later than arrival a burst was time-stamped.
    pub lag_s: f64,
    pub rms_hz: f64,
    pub used: usize,
    pub sats: usize,
    pub settled: bool,
    /// Distance (km) to the previous fit, when there was one.
    pub moved_km: Option<f64>,
    pub at: f64,
}

impl Fix {
    pub fn to_json(&self) -> Value {
        json!({
            "lat": (self.lat * 1e5).round() / 1e5,
            "lon": (self.lon * 1e5).round() / 1e5,
            "offset_hz": self.offset_hz.round(),
            "lag_s": (self.lag_s * 1000.0).round() / 1000.0,
            "rms_hz": (self.rms_hz * 10.0).round() / 10.0,
            "measurements": self.used,
            "satellites": self.sats,
            "settled": self.settled,
            "moved_km": self.moved_km.map(|d| (d * 10.0).round() / 10.0),
            "fitted_at": self.at,
        })
    }
}

/// Collects ring-alert Doppler measurements and fits the receiver location.
pub struct Locator {
    meas: VecDeque<Meas>,
    alt_km: f64,
    last_solve: f64,
    last: Option<Fix>,
}

impl Locator {
    pub fn new(alt_m: f64) -> Self {
        Self { meas: VecDeque::new(), alt_km: alt_m / 1000.0, last_solve: 0.0, last: None }
    }

    /// Record one ring alert received at `f_hz` from satellite `sat` (state
    /// `st` at `t`).
    pub fn push(&mut self, sat: &str, t: f64, f_hz: f64, st: SatState) {
        if (f_hz - RING_ALERT_HZ).abs() > MAX_OFFSET_HZ {
            return;
        }
        self.meas.push_back(Meas { sat: sat.to_string(), t, f_hz, st });
        while self.meas.len() > MAX_MEAS {
            self.meas.pop_front();
        }
    }

    pub fn last(&self) -> Option<&Fix> {
        self.last.as_ref()
    }

    /// Fit when due (`now` in Unix seconds); returns a new fix.
    pub fn maybe_solve(&mut self, now: f64) -> Option<Fix> {
        if now - self.last_solve < SOLVE_EVERY_S {
            return None;
        }
        self.last_solve = now;
        self.solve(now)
    }

    /// Fit the measurements within the window ending at `now`.
    pub fn solve(&mut self, now: f64) -> Option<Fix> {
        while self.meas.front().is_some_and(|m| m.t < now - WINDOW_S) {
            self.meas.pop_front();
        }
        let ms: Vec<&Meas> = self.meas.iter().collect();
        if ms.len() < MIN_MEAS {
            return None;
        }
        let (mut x, _) = global_fit(&ms, self.alt_km)?;
        // Drop measurements beyond 3σ (and at least 3 × 30 Hz), then refit.
        let rms = rms_of(&ms, &x, self.alt_km);
        let gate = (3.0 * rms).max(90.0);
        let kept: Vec<&Meas> =
            ms.iter().copied().filter(|m| residual(m, &x, self.alt_km).abs() <= gate).collect();
        if kept.len() >= MIN_MEAS && kept.len() < ms.len() {
            x = lm(&kept, x, self.alt_km);
        }
        let used = if kept.len() >= MIN_MEAS { kept } else { ms };
        let rms = rms_of(&used, &x, self.alt_km);
        let mut sats: Vec<&str> = used.iter().map(|m| m.sat.as_str()).collect();
        sats.sort_unstable();
        sats.dedup();
        let (lat, lon) = (x[0].to_degrees(), wrap_lon(x[1].to_degrees()));
        let moved_km = self.last.as_ref().map(|p| haversine_km(p.lat, p.lon, lat, lon));
        let fix = Fix {
            lat,
            lon,
            offset_hz: x[2],
            lag_s: x[3],
            rms_hz: rms,
            used: used.len(),
            sats: sats.len(),
            settled: sats.len() >= 2 && moved_km.is_some_and(|d| d <= SETTLED_KM),
            moved_km,
            at: now,
        };
        self.last = Some(fix.clone());
        Some(fix)
    }
}

fn wrap_lon(mut lon: f64) -> f64 {
    while lon > 180.0 {
        lon -= 360.0;
    }
    while lon < -180.0 {
        lon += 360.0;
    }
    lon
}

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * a.sqrt().asin()
}

/// WGS84 geodetic → ECEF (km).
pub fn geodetic_ecef(lat: f64, lon: f64, alt_km: f64) -> [f64; 3] {
    const A: f64 = 6378.137;
    const E2: f64 = 6.694_379_990_14e-3;
    let (sl, cl) = lat.sin_cos();
    let n = A / (1.0 - E2 * sl * sl).sqrt();
    [(n + alt_km) * cl * lon.cos(), (n + alt_km) * cl * lon.sin(), (n * (1.0 - E2) + alt_km) * sl]
}

/// Received frequency predicted for `m` at receiver `u` (ECEF km), stamp lag
/// `lag` (s), without the receiver offset.
fn doppler_hz(m: &Meas, u: &[f64; 3], lag: f64) -> f64 {
    // The burst arrived `lag` seconds before it was stamped.
    let dt = -lag;
    let mut s = [0.0; 3];
    let mut v = [0.0; 3];
    for i in 0..3 {
        s[i] = m.st.pos[i] + m.st.vel[i] * dt + 0.5 * m.st.acc[i] * dt * dt;
        v[i] = m.st.vel[i] + m.st.acc[i] * dt;
    }
    let d = [s[0] - u[0], s[1] - u[1], s[2] - u[2]];
    let r = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let rr = (d[0] * v[0] + d[1] * v[1] + d[2] * v[2]) / r;
    -RING_ALERT_HZ * rr / C_KM_S
}

/// Parameters: [lat rad, lon rad, offset Hz, lag s].
fn residual(m: &Meas, x: &[f64; 4], alt_km: f64) -> f64 {
    let u = geodetic_ecef(x[0], x[1], alt_km);
    m.f_hz - RING_ALERT_HZ - doppler_hz(m, &u, x[3]) - x[2]
}

fn rms_of(ms: &[&Meas], x: &[f64; 4], alt_km: f64) -> f64 {
    let ss: f64 = ms.iter().map(|m| residual(m, x, alt_km).powi(2)).sum();
    (ss / ms.len().max(1) as f64).sqrt()
}

/// Satellite above the receiver's horizon (elevation > 0).
fn visible(m: &Meas, u: &[f64; 3]) -> bool {
    let d = [m.st.pos[0] - u[0], m.st.pos[1] - u[1], m.st.pos[2] - u[2]];
    d[0] * u[0] + d[1] * u[1] + d[2] * u[2] > 0.0
}

/// Coarse grid then LM on the best distinct minima. Returns the best
/// parameters and their cost.
fn global_fit(ms: &[&Meas], alt_km: f64) -> Option<([f64; 4], f64)> {
    // Subsample for the grid so its cost stays bounded.
    let step = (ms.len() / 400).max(1);
    let sub: Vec<&Meas> = ms.iter().step_by(step).copied().collect();
    let mut cands: Vec<(f64, f64, f64, f64)> = Vec::new(); // (cost, lat, lon, b)
    let mut lat = -88.0f64;
    while lat <= 88.0 {
        let dlon = (2.0 / lat.to_radians().cos().max(0.05)).min(30.0);
        let mut lon = -180.0f64;
        while lon < 180.0 {
            let (la, lo) = (lat.to_radians(), lon.to_radians());
            let u = geodetic_ecef(la, lo, alt_km);
            // The heard satellites must be above this point's horizon (allow
            // a few mismatched bursts).
            let vis = sub.iter().filter(|m| visible(m, &u)).count();
            if vis * 20 >= sub.len() * 19 {
                let d: Vec<f64> =
                    sub.iter().map(|m| m.f_hz - RING_ALERT_HZ - doppler_hz(m, &u, 0.0)).collect();
                let b = d.iter().sum::<f64>() / d.len() as f64;
                let cost = d.iter().map(|x| (x - b).powi(2)).sum::<f64>() / d.len() as f64;
                cands.push((cost, la, lo, b));
            }
            lon += dlon;
        }
        lat += 2.0;
    }
    cands.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Best few minima at least 500 km apart.
    let mut seeds: Vec<(f64, f64, f64)> = Vec::new();
    for &(_, la, lo, b) in &cands {
        if seeds.iter().all(|&(sla, slo, _)| {
            haversine_km(sla.to_degrees(), slo.to_degrees(), la.to_degrees(), lo.to_degrees()) > 500.0
        }) {
            seeds.push((la, lo, b));
            if seeds.len() >= 6 {
                break;
            }
        }
    }
    seeds
        .into_iter()
        .map(|(la, lo, b)| {
            let x = lm(ms, [la, lo, b, 0.0], alt_km);
            (x, rms_of(ms, &x, alt_km))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Levenberg-Marquardt on [lat, lon, offset, lag] with a numeric Jacobian.
fn lm(ms: &[&Meas], mut x: [f64; 4], alt_km: f64) -> [f64; 4] {
    const H: [f64; 4] = [1e-6, 1e-6, 1.0, 1e-3];
    let cost = |x: &[f64; 4]| ms.iter().map(|m| residual(m, x, alt_km).powi(2)).sum::<f64>();
    let mut c = cost(&x);
    let mut lambda = 1e-3;
    for _ in 0..60 {
        let mut jtj = [[0.0f64; 4]; 4];
        let mut jtr = [0.0f64; 4];
        for m in ms {
            let r0 = residual(m, &x, alt_km);
            let mut j = [0.0f64; 4];
            for k in 0..4 {
                let mut xp = x;
                xp[k] += H[k];
                j[k] = (residual(m, &xp, alt_km) - r0) / H[k];
            }
            for a in 0..4 {
                jtr[a] += j[a] * r0;
                for b in 0..4 {
                    jtj[a][b] += j[a] * j[b];
                }
            }
        }
        let mut improved = false;
        for _ in 0..10 {
            let mut a = jtj;
            for (k, row) in a.iter_mut().enumerate() {
                row[k] *= 1.0 + lambda;
            }
            let Some(dx) = solve4(a, jtr) else { break };
            let mut xn = x;
            for k in 0..4 {
                xn[k] -= dx[k];
            }
            xn[0] = xn[0].clamp(-1.56, 1.56);
            xn[3] = xn[3].clamp(-30.0, 30.0);
            let cn = cost(&xn);
            if cn < c {
                let rel = (c - cn) / c.max(1e-12);
                x = xn;
                c = cn;
                lambda = (lambda / 3.0).max(1e-9);
                improved = true;
                if rel < 1e-10 {
                    return x;
                }
                break;
            }
            lambda *= 4.0;
        }
        if !improved {
            break;
        }
    }
    x
}

/// Solve a 4×4 linear system by Gaussian elimination with partial pivoting.
fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> Option<[f64; 4]> {
    for col in 0..4 {
        let p = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[p][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, p);
        b.swap(col, p);
        for r in col + 1..4 {
            let f = a[r][col] / a[col][col];
            for k in col..4 {
                a[r][k] -= f * a[col][k];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0; 4];
    for r in (0..4).rev() {
        let s: f64 = (r + 1..4).map(|k| a[r][k] * x[k]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

// --- process-global hook ---------------------------------------------------

static LOCATOR: OnceLock<Mutex<Locator>> = OnceLock::new();

/// Turn the locator on (after the satellite map loaded). `alt_m` is the
/// receiver altitude above the ellipsoid when known, else 0.
pub fn enable(alt_m: f64) {
    if std::env::var("XNG_IRIDIUM_LOCATE").is_ok_and(|v| v == "0" || v == "off") {
        return;
    }
    let _ = LOCATOR.set(Mutex::new(Locator::new(alt_m)));
}

/// Feed a decoded message; returns a `receiver-position` Iridium message when
/// a new fit is made. Uses satellite-matched ring alerts only (`satmap`
/// enrichment must have run).
pub fn observe(msg: &Message) -> Option<Message> {
    let lock = LOCATOR.get()?;
    let MessageBody::Iridium { kind, details } = &msg.body else {
        return None;
    };
    if kind != "ring-alert" {
        return None;
    }
    let sat = details.get("satellite").and_then(Value::as_str)?;
    let map: &SatMap = crate::satmap::get()?;
    let t = msg.timestamp.timestamp_millis() as f64 / 1e3;
    let st = map.state_ecef(sat, t)?;
    let mut loc = lock.lock().ok()?;
    loc.push(sat, t, msg.frequency_hz as f64, st);
    let fix = loc.maybe_solve(t)?;
    tracing::info!(
        "Iridium Doppler receiver estimate {:.4}, {:.4} ({} ring alerts of {} satellites, {:.0} Hz rms, {})",
        fix.lat,
        fix.lon,
        fix.used,
        fix.sats,
        fix.rms_hz,
        if fix.settled { "settled" } else { "provisional" }
    );
    Some(Message {
        mode: Mode::Iridium,
        timestamp: msg.timestamp,
        frequency_hz: RING_ALERT_HZ as u64,
        signal: SignalQuality::default(),
        decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
        body: MessageBody::Iridium { kind: "receiver-position".into(), details: fix.to_json() },
        raw: None,
        source: msg.source.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TLE checksum (mod 10 of digits, '-' counts 1).
    fn cks(l: &str) -> String {
        let s: u32 = l
            .chars()
            .map(|c| match c {
                '0'..='9' => c.to_digit(10).unwrap(),
                '-' => 1,
                _ => 0,
            })
            .sum();
        format!("{l}{}", s % 10)
    }

    /// An Iridium-like constellation: 6 planes × 11 satellites (86.4°,
    /// 14.34 rev/day), epoch 2026-11-26 00:00 UTC.
    pub(super) fn tles() -> String {
        let mut out = String::new();
        for p in 0..6 {
            for k in 0..11 {
                let i = p * 11 + k;
                let raan = p as f64 * 31.6;
                let ma = (k as f64 * 360.0 / 11.0 + p as f64 * 16.0) % 360.0;
                let n = 90000 + i;
                let l1 = format!("1 {n:05}U 26001A   26330.00000000  .00000000  00000-0  00000-0 0  999");
                let l2 = format!("2 {n:05}  86.4000 {raan:8.4} 0002000  90.0000 {ma:8.4} 14.34217000    1");
                assert_eq!(l1.len(), 68);
                assert_eq!(l2.len(), 68, "{l2}");
                out.push_str(&format!("SIM {i}\n{}\n{}\n", cks(&l1), cks(&l2)));
            }
        }
        out
    }

    const EPOCH: f64 = 1_795_651_200.0; // 2026-11-26T00:00:00Z

    /// Synthesize the ring alerts a receiver at (lat, lon) would measure over
    /// `dur` seconds: one per satellite every 4 s while above 10°, with
    /// receiver offset `b`, stamp lag `lag` and noise `sigma` Hz.
    fn simulate(map: &SatMap, lat: f64, lon: f64, b: f64, lag: f64, sigma: f64, dur: f64) -> Locator {
        let mut loc = Locator::new(0.0);
        let u = geodetic_ecef(lat.to_radians(), lon.to_radians(), 0.0);
        let mut seed = 12345u64;
        let mut gauss = || {
            // Box-Muller on a small LCG: deterministic.
            let mut uni = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 11) as f64 + 0.5) / (1u64 << 53) as f64
            };
            let (a, b) = (uni(), uni());
            (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
        };
        let mut t = EPOCH;
        while t < EPOCH + dur {
            for i in 0..66 {
                let name = format!("SIM {i}");
                // True arrival time t; stamped at t + lag.
                let st = map.state_ecef(&name, t).unwrap();
                let d = [st.pos[0] - u[0], st.pos[1] - u[1], st.pos[2] - u[2]];
                let r = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                let rn = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
                let sin_el = (d[0] * u[0] + d[1] * u[1] + d[2] * u[2]) / (r * rn);
                if sin_el < 10f64.to_radians().sin() {
                    continue;
                }
                let rr = (d[0] * st.vel[0] + d[1] * st.vel[1] + d[2] * st.vel[2]) / r;
                let f = RING_ALERT_HZ * (1.0 - rr / C_KM_S) + b + sigma * gauss();
                let ts = t + lag;
                loc.push(&name, ts, f, map.state_ecef(&name, ts).unwrap());
            }
            t += 4.0;
        }
        loc
    }

    #[test]
    fn locates_receiver_from_synthetic_ring_alerts() {
        let map = SatMap::from_tle(&tles());
        assert_eq!(map.len(), 66);
        for &(lat, lon) in &[(38.5125, -121.4925), (-33.87, 151.21), (51.48, -0.0015), (64.1, -21.9)] {
            let mut loc = simulate(&map, lat, lon, 1830.0, 0.8, 25.0, 1800.0);
            let n = loc.meas.len();
            let fix = loc.solve(EPOCH + 1800.0 + 1.0).unwrap_or_else(|| panic!("no fix, {n} measurements"));
            let err = haversine_km(lat, lon, fix.lat, fix.lon);
            assert!(err < 5.0, "({lat},{lon}) -> {fix:?}, {err:.1} km off, {n} meas");
            assert!((fix.offset_hz - 1830.0).abs() < 30.0, "{fix:?}");
            assert!((fix.lag_s - 0.8).abs() < 0.6, "{fix:?}");
            assert!(fix.rms_hz < 40.0, "{fix:?}");
            assert!(fix.sats >= 2, "{fix:?}");
        }
    }

    #[test]
    fn rejects_outliers_and_settles() {
        let map = SatMap::from_tle(&tles());
        let (lat, lon) = (38.5125, -121.4925);
        let mut loc = simulate(&map, lat, lon, -950.0, 0.0, 20.0, 1800.0);
        // Corrupt every 15th measurement by several kHz (a wrong-channel or
        // mis-matched burst).
        for (i, m) in loc.meas.iter_mut().enumerate() {
            if i % 15 == 0 {
                m.f_hz += 4000.0;
            }
        }
        let first = loc.solve(EPOCH + 1801.0).unwrap();
        assert!(!first.settled, "first fit has nothing to agree with");
        let second = loc.solve(EPOCH + 1802.0).unwrap();
        assert!(haversine_km(lat, lon, second.lat, second.lon) < 5.0, "{second:?}");
        assert!(second.settled, "{second:?}");
        assert!(second.rms_hz < 40.0, "{second:?}");
    }

    #[test]
    fn too_few_measurements_give_no_fix() {
        let map = SatMap::from_tle(&tles());
        let mut loc = simulate(&map, 38.5, -121.5, 0.0, 0.0, 20.0, 40.0);
        assert!(loc.meas.len() < MIN_MEAS);
        assert!(loc.solve(EPOCH + 41.0).is_none());
    }

    #[test]
    fn off_channel_bursts_are_ignored() {
        let map = SatMap::from_tle(&tles());
        let mut loc = Locator::new(0.0);
        let st = map.state_ecef("SIM 0", EPOCH).unwrap();
        loc.push("SIM 0", EPOCH, RING_ALERT_HZ + 250_000.0, st);
        assert!(loc.meas.is_empty());
    }
}
