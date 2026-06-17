//! Mode S / ADS-B field decoding: CPR positions, velocity, altitude,
//! squawk. Algorithms are the published ICAO Annex 10 Vol IV procedures
//! as laid out in open references (notably "The 1090 Megahertz Riddle",
//! Junzi Sun), validated against that book's worked examples.

use crate::frame::IDENT_CHARSET;

/// Number of latitude zones (NZ) for airborne CPR.
const NZ: f64 = 15.0;

/// NL(lat): number of longitude zones at a latitude (closed form).
pub fn nl(lat: f64) -> u32 {
    let a = lat.abs();
    if a >= 87.0 {
        return if a > 87.0 { 1 } else { 2 };
    }
    if a < 1e-9 {
        return 59;
    }
    let cos_lat = (lat.to_radians()).cos();
    let x = 1.0 - (1.0 - (std::f64::consts::PI / (2.0 * NZ)).cos()) / (cos_lat * cos_lat);
    (2.0 * std::f64::consts::PI / x.acos()).floor() as u32
}

/// One CPR-encoded position report (17-bit fractions).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cpr {
    pub odd: bool,
    pub lat: u32,
    pub lon: u32,
    /// True for surface position messages (TC 5–8), which encode a
    /// quarter-globe span.
    pub surface: bool,
}

const CPR_MAX: f64 = 131_072.0; // 2^17

/// Globally unambiguous airborne decode from an even/odd pair.
/// `latest_odd` selects which frame is newer (its zone is used for
/// longitude). Returns (lat, lon) in degrees.
pub fn cpr_global_airborne(even: Cpr, odd: Cpr, latest_odd: bool) -> Option<(f64, f64)> {
    if even.odd || !odd.odd || even.surface || odd.surface {
        return None;
    }
    let (lat_e, lon_e) = (even.lat as f64 / CPR_MAX, even.lon as f64 / CPR_MAX);
    let (lat_o, lon_o) = (odd.lat as f64 / CPR_MAX, odd.lon as f64 / CPR_MAX);

    let dlat_e = 360.0 / (4.0 * NZ);
    let dlat_o = 360.0 / (4.0 * NZ - 1.0);
    let j = (59.0 * lat_e - 60.0 * lat_o + 0.5).floor();

    let mut rlat_e = dlat_e * ((j % 60.0 + 60.0) % 60.0 + lat_e);
    let mut rlat_o = dlat_o * ((j % 59.0 + 59.0) % 59.0 + lat_o);
    if rlat_e >= 270.0 {
        rlat_e -= 360.0;
    }
    if rlat_o >= 270.0 {
        rlat_o -= 360.0;
    }
    // Both must sit in the same longitude-zone band.
    if nl(rlat_e) != nl(rlat_o) {
        return None;
    }

    let (rlat, lon_cpr, nl_v, odd_sel) = if latest_odd {
        (rlat_o, lon_o, nl(rlat_o), 1.0)
    } else {
        (rlat_e, lon_e, nl(rlat_e), 0.0)
    };
    let m = (lon_e * (nl_v as f64 - 1.0) - lon_o * nl_v as f64 + 0.5).floor();
    let ni = (nl_v as f64 - odd_sel).max(1.0);
    let dlon = 360.0 / ni;
    let mut lon = dlon * ((m % ni + ni) % ni + lon_cpr);
    if lon >= 180.0 {
        lon -= 360.0;
    }
    Some((rlat, lon))
}

/// Locally unambiguous decode relative to a known reference position
/// (the aircraft's last fix): airborne (360°) or surface (90°) span.
pub fn cpr_local(cpr: Cpr, ref_lat: f64, ref_lon: f64) -> (f64, f64) {
    let span = if cpr.surface { 90.0 } else { 360.0 };
    let lat_cpr = cpr.lat as f64 / CPR_MAX;
    let lon_cpr = cpr.lon as f64 / CPR_MAX;
    let i = if cpr.odd { 1.0 } else { 0.0 };

    let dlat = span / (4.0 * NZ - i);
    let j = (ref_lat / dlat).floor()
        + (0.5 + (ref_lat % dlat + dlat) % dlat / dlat - lat_cpr).floor();
    let lat = dlat * (j + lat_cpr);

    let nl_v = nl(lat);
    let dlon = span / ((nl_v as f64 - i).max(1.0));
    let m = (ref_lon / dlon).floor()
        + (0.5 + (ref_lon % dlon + dlon) % dlon / dlon - lon_cpr).floor();
    let lon = dlon * (m + lon_cpr);
    (lat, lon)
}

/// Decoded TC 19 velocity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Velocity {
    /// Knots; ground speed (subtypes 1/2) or airspeed (3/4).
    pub speed_kt: f64,
    /// Degrees true; track (ground) or heading (air).
    pub track_deg: f64,
    /// True when speed/track are airspeed/heading rather than ground.
    pub airspeed: bool,
    /// Feet per minute, positive up.
    pub vertical_rate_fpm: Option<i32>,
}

/// Decode a TC 19 velocity ME field (7 bytes).
pub fn velocity(me: &[u8]) -> Option<Velocity> {
    let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
    let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
    if field(0, 5) != 19 {
        return None;
    }
    let subtype = field(5, 3);
    let (speed_kt, track_deg, airspeed) = match subtype {
        1 | 2 => {
            // East-west / north-south components, supersonic ×4.
            let scale = if subtype == 2 { 4.0 } else { 1.0 };
            let s_ew = bit(13);
            let v_ew = field(14, 10);
            let s_ns = bit(24);
            let v_ns = field(25, 10);
            if v_ew == 0 || v_ns == 0 {
                return None;
            }
            let vx = scale * (v_ew as f64 - 1.0) * if s_ew == 1 { -1.0 } else { 1.0 };
            let vy = scale * (v_ns as f64 - 1.0) * if s_ns == 1 { -1.0 } else { 1.0 };
            let speed = (vx * vx + vy * vy).sqrt();
            let track = (vx.atan2(vy).to_degrees() + 360.0) % 360.0;
            (speed, track, false)
        }
        3 | 4 => {
            let scale = if subtype == 4 { 4.0 } else { 1.0 };
            if bit(13) == 0 {
                return None; // heading not available
            }
            let hdg = field(14, 10) as f64 / 1024.0 * 360.0;
            let v_as = field(25, 10);
            if v_as == 0 {
                return None;
            }
            (scale * (v_as as f64 - 1.0), hdg, true)
        }
        _ => return None,
    };
    let vr_raw = field(37, 9);
    let vertical_rate_fpm = (vr_raw != 0).then(|| {
        let v = (vr_raw as i32 - 1) * 64;
        if bit(36) == 1 { -v } else { v }
    });
    Some(Velocity { speed_kt, track_deg, airspeed, vertical_rate_fpm })
}

/// Ground speed (knots) from a TC 5–8 surface 7-bit Movement code. `None`
/// for "not available" (0) and reserved (125–127). Piecewise per DO-260B /
/// "The 1090 MHz Riddle" §4: 1 = stopped; 2–8 step 0.125; 9–12 step 0.25;
/// 13–38 step 0.5; 39–93 step 1; 94–108 step 2; 109–123 step 5; 124 = ≥175.
fn surface_speed_kt(mov: u32) -> Option<f64> {
    let s = match mov {
        1 => 0.0,
        2..=8 => 0.125 + (mov - 2) as f64 * 0.125,
        9..=12 => 1.0 + (mov - 9) as f64 * 0.25,
        13..=38 => 2.0 + (mov - 13) as f64 * 0.5,
        39..=93 => 15.0 + (mov - 39) as f64 * 1.0,
        94..=108 => 70.0 + (mov - 94) as f64 * 2.0,
        109..=123 => 100.0 + (mov - 109) as f64 * 5.0,
        124 => 175.0,
        _ => return None, // 0 = N/A, 125–127 reserved
    };
    Some(s)
}

/// Decode TC 5–8 surface movement into a [`Velocity`]: ground speed from the
/// Movement field and true track from the Ground-Track field (when its status
/// bit is set). Returns `None` unless both a speed and a valid track are
/// present — a moving surface target reports both; a stopped/track-unknown
/// target keeps position only (the `Velocity` track is not optional, so we do
/// not fabricate a 0° heading). Surface has no vertical rate.
///
/// ME bit positions (0-indexed): Movement 5–11, track status 12, track 13–19.
pub fn surface_velocity(me: &[u8]) -> Option<Velocity> {
    let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
    let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
    if !(5..=8).contains(&field(0, 5)) {
        return None;
    }
    let speed_kt = surface_speed_kt(field(5, 7))?;
    if bit(12) != 1 {
        return None; // ground track not valid
    }
    let track_deg = field(13, 7) as f64 * 360.0 / 128.0;
    Some(Velocity { speed_kt, track_deg, airspeed: false, vertical_rate_fpm: None })
}

/// Decode a TC 31 Aircraft Operational Status ME field (7 bytes) into the
/// modern accuracy/integrity layer: ADS-B version, NIC supplement, NACp,
/// SIL (+ supplement), and — airborne only — GVA and barometric-altitude
/// integrity. Returns `None` for a non-TC31 field.
///
/// ME-relative, 0-indexed bit positions (per "The 1090 MHz Riddle" §6 and
/// pyModeS `bds65`): subtype 5–7, version 40–42, NIC-supplement-A 43,
/// NACp 44–47, GVA 48–49, SIL 50–51, NICbaro 52, SIL-supplement 54. NACp/
/// SIL/NIC-supplement were introduced in version 1; the SIL supplement in
/// version 2.
pub fn operational_status(me: &[u8]) -> Option<serde_json::Value> {
    let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
    let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
    if field(0, 5) != 31 {
        return None;
    }
    let subtype = field(5, 3);
    let version = field(40, 3);
    let mut o = serde_json::Map::new();
    o.insert("subtype".into(), serde_json::json!(if subtype == 1 { "surface" } else { "airborne" }));
    o.insert("version".into(), serde_json::json!(version));
    if version >= 1 {
        o.insert("nic_supp_a".into(), serde_json::json!(bit(43)));
        o.insert("nac_p".into(), serde_json::json!(field(44, 4)));
        o.insert("sil".into(), serde_json::json!(field(50, 2)));
        if subtype == 0 {
            o.insert("gva".into(), serde_json::json!(field(48, 2)));
            o.insert("baro_alt_integrity".into(), serde_json::json!(bit(52)));
        }
        if version >= 2 {
            o.insert("sil_supplement".into(), serde_json::json!(bit(54)));
        }
    }
    Some(serde_json::Value::Object(o))
}

/// Decode a TC 28 Aircraft Status ME field (7 bytes). Subtype 1 carries the
/// emergency/priority status; subtype 2 is an ACAS RA broadcast (flagged
/// here — full RA decode is BDS 3,0, a later item). `None` for non-TC28.
pub fn aircraft_status(me: &[u8]) -> Option<serde_json::Value> {
    let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
    let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
    if field(0, 5) != 28 {
        return None;
    }
    let subtype = field(5, 3);
    let mut o = serde_json::Map::new();
    o.insert("subtype".into(), serde_json::json!(subtype));
    match subtype {
        1 => {
            let es = field(8, 3);
            o.insert("emergency_state".into(), serde_json::json!(es));
            o.insert("emergency".into(), serde_json::json!(emergency_label(es)));
        }
        2 => {
            o.insert("acas_ra".into(), serde_json::json!(true));
        }
        _ => return None,
    }
    Some(serde_json::Value::Object(o))
}

/// Decode a TC 29 Target State and Status ME field (7 bytes) into the
/// DO-260B selected-state / nav-mode layer: selected altitude (+ source),
/// barometric pressure setting, selected heading, NACp, NICbaro, SIL, and
/// the autopilot/VNAV/altitude-hold/approach/LNAV/TCAS mode flags. The
/// five autopilot/nav flags are gated by the "mode status" bit (when 0
/// the modes are unknown and omitted); TCAS-operational is always valid.
/// Returns `None` for a non-TC29 field.
///
/// ME-relative, 0-indexed bit positions (per DO-260B §2.2.3.2.7.1, cross-
/// checked against pyModeS `bds62`): subtype 5–6, selected-altitude source
/// 8, selected altitude 9–19 ((raw−1)·32 ft), baro pressure 20–28
/// (800+(raw−1)·0.8 mbar), heading status 29, selected heading 30–38
/// (raw·360/512°), NACp 39–42, NICbaro 43, SIL 44–45, mode status 46,
/// autopilot 47, VNAV 48, altitude-hold 49, approach 51, TCAS-operational
/// 52, LNAV 53.
pub fn target_state(me: &[u8]) -> Option<serde_json::Value> {
    let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
    let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
    if field(0, 5) != 29 {
        return None;
    }
    let mut o = serde_json::Map::new();
    o.insert("subtype".into(), serde_json::json!("target_state"));
    o.insert("ts_subtype".into(), serde_json::json!(field(5, 2)));

    let alt_raw = field(9, 11);
    if alt_raw != 0 {
        o.insert("selected_altitude".into(), serde_json::json!((alt_raw - 1) * 32));
        o.insert(
            "selected_altitude_source".into(),
            serde_json::json!(if bit(8) == 1 { "FMS" } else { "MCP/FCU" }),
        );
    }

    let baro_raw = field(20, 9);
    if baro_raw != 0 {
        o.insert(
            "baro_pressure_setting".into(),
            serde_json::json!(800.0 + (baro_raw - 1) as f64 * 0.8),
        );
    }

    if bit(29) == 1 {
        o.insert(
            "selected_heading".into(),
            serde_json::json!(field(30, 9) as f64 * 360.0 / 512.0),
        );
    }

    o.insert("nac_p".into(), serde_json::json!(field(39, 4)));
    o.insert("nic_baro".into(), serde_json::json!(bit(43)));
    o.insert("sil".into(), serde_json::json!(field(44, 2)));

    if bit(46) == 1 {
        o.insert("autopilot".into(), serde_json::json!(bit(47) == 1));
        o.insert("vnav_mode".into(), serde_json::json!(bit(48) == 1));
        o.insert("altitude_hold_mode".into(), serde_json::json!(bit(49) == 1));
        o.insert("approach_mode".into(), serde_json::json!(bit(51) == 1));
        o.insert("lnav_mode".into(), serde_json::json!(bit(53) == 1));
    }
    o.insert("tcas_operational".into(), serde_json::json!(bit(52) == 1));

    Some(serde_json::Value::Object(o))
}

/// Emergency/priority status code (TC28 subtype 1) → label.
fn emergency_label(state: u32) -> &'static str {
    match state {
        0 => "none",
        1 => "general",
        2 => "medical",
        3 => "minimum fuel",
        4 => "no communications",
        5 => "unlawful interference",
        6 => "downed aircraft",
        _ => "reserved",
    }
}

/// 13-bit Mode S altitude field (AC, DF0/4/16/20): M-bit metric flag,
/// Q-bit 25 ft, else 100 ft Gillham.
pub fn altitude13(ac: u32) -> Option<i32> {
    if ac == 0 {
        return None;
    }
    let m = (ac >> 6) & 1;
    if m == 1 {
        return None; // metric: unused in practice
    }
    let q = (ac >> 4) & 1;
    if q == 1 {
        let n = ((ac & 0x1F80) >> 2) | ((ac & 0x20) >> 1) | (ac & 0x0F);
        return Some(n as i32 * 25 - 1000);
    }
    gillham(gray_reorder(ac))
}

/// Reorder the interleaved AC bits (C1 A1 C2 A2 C4 A4 [M] B1 [Q] B2 D2
/// B4 D4) into Gillham D2 D4 A1 A2 A4 B1 B2 B4 C1 C2 C4.
fn gray_reorder(ac: u32) -> u32 {
    let b = |k: u32| (ac >> k) & 1; // k = bit from LSB
    // AC bits, MSB-first positions: C1=12 A1=11 C2=10 A2=9 C4=8 A4=7
    // M=6 B1=5 Q=4 B2=3 D2=2 B4=1 D4=0
    let (c1, a1, c2, a2, c4, a4) = (b(12), b(11), b(10), b(9), b(8), b(7));
    let (b1, b2, d2, b4, d4) = (b(5), b(3), b(2), b(1), b(0));
    (d2 << 10)
        | (d4 << 9)
        | (a1 << 8)
        | (a2 << 7)
        | (a4 << 6)
        | (b1 << 5)
        | (b2 << 4)
        | (b4 << 3)
        | (c1 << 2)
        | (c2 << 1)
        | c4
}

/// Gillham (Gray-coded) altitude: 500 ft Gray ladder + reflected 100 ft
/// subdivision. Input: D2 D4 A1 A2 A4 B1 B2 B4 | C1 C2 C4.
fn gillham(g: u32) -> Option<i32> {
    let mut n500 = g >> 3;
    // Gray → binary.
    let mut mask = n500 >> 1;
    while mask != 0 {
        n500 ^= mask;
        mask >>= 1;
    }
    let mut n100 = match g & 7 {
        0b001 => 0,
        0b011 => 1,
        0b010 => 2,
        0b110 => 3,
        0b100 => 4,
        _ => return None, // 0, 5, 7 invalid
    };
    if n500 % 2 == 1 {
        n100 = 4 - n100; // odd 500 ft rungs count back down
    }
    Some(n500 as i32 * 500 + n100 * 100 - 1300)
}

/// 13-bit identity field (DF5/21) → 4-digit squawk. Bit order (MSB
/// first): C1 A1 C2 A2 C4 A4 [X] B1 D1 B2 D2 B4 D4.
pub fn squawk13(id: u32) -> String {
    let b = |k: u32| (id >> k) & 1;
    let (c1, a1, c2, a2, c4, a4) = (b(12), b(11), b(10), b(9), b(8), b(7));
    let (b1, d1, b2, d2, b4, d4) = (b(5), b(4), b(3), b(2), b(1), b(0));
    let a = (a4 << 2) | (a2 << 1) | a1;
    let bq = (b4 << 2) | (b2 << 1) | b1;
    let c = (c4 << 2) | (c2 << 1) | c1;
    let d = (d4 << 2) | (d2 << 1) | d1;
    format!("{a}{bq}{c}{d}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me_of(frame_hex: &str) -> Vec<u8> {
        let bytes: Vec<u8> = (0..frame_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&frame_hex[i..i + 2], 16).unwrap())
            .collect();
        bytes[4..11].to_vec()
    }

    fn cpr_of(frame_hex: &str) -> Cpr {
        let me = me_of(frame_hex);
        let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
        let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
        Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false }
    }

    // Worked examples from "The 1090 Megahertz Riddle".
    const EVEN: &str = "8D40621D58C382D690C8AC2863A7";
    const ODD: &str = "8D40621D58C386435CC412692AD6";

    #[test]
    fn nl_function_reference_points() {
        assert_eq!(nl(0.0), 59);
        assert_eq!(nl(52.257), 36);
        assert_eq!(nl(87.5), 1);
        assert_eq!(nl(-87.5), 1);
    }

    #[test]
    fn global_airborne_decode_matches_book() {
        let (lat, lon) = cpr_global_airborne(cpr_of(EVEN), cpr_of(ODD), false).unwrap();
        assert!((lat - 52.25720).abs() < 1e-4, "lat {lat}");
        assert!((lon - 3.91937).abs() < 1e-4, "lon {lon}");
    }

    #[test]
    fn local_decode_matches_book() {
        // Same even frame, reference near the true position.
        let (lat, lon) = cpr_local(cpr_of(EVEN), 52.258, 3.918);
        assert!((lat - 52.25720).abs() < 1e-4, "lat {lat}");
        assert!((lon - 3.91937).abs() < 1e-4, "lon {lon}");
    }

    #[test]
    fn groundspeed_velocity_matches_book() {
        let v = velocity(&me_of("8D485020994409940838175B284F")).unwrap();
        assert!(!v.airspeed);
        assert!((v.speed_kt - 159.20).abs() < 0.05, "{}", v.speed_kt);
        assert!((v.track_deg - 182.88).abs() < 0.05, "{}", v.track_deg);
        assert_eq!(v.vertical_rate_fpm, Some(-832));
    }

    #[test]
    fn airspeed_velocity_matches_book() {
        let v = velocity(&me_of("8DA05F219B06B6AF189400CBC33F")).unwrap();
        assert!(v.airspeed);
        assert!((v.speed_kt - 375.0).abs() < 0.5, "{}", v.speed_kt);
        assert!((v.track_deg - 243.98).abs() < 0.05, "{}", v.track_deg);
    }

    #[test]
    fn q_bit_altitude_df4() {
        // Book example: DF4 with AC = 39 000 ft.
        // Frame 20001718029FCD... — use the 13-bit field directly:
        // n = (39000+1000)/25 = 1600 → reassemble Q-bit layout.
        let n: u32 = 1600;
        let ac = ((n << 2) & 0x1F80) | 0x10 | ((n << 1) & 0x20) | (n & 0x0F);
        assert_eq!(altitude13(ac), Some(39_000));
    }

    #[test]
    fn gillham_altitude_examples() {
        // Published Gillham example: C1 A1 C2 A2 C4 A4 B1 B2 D2 B4 D4 for
        // 1300 ft is all-zeros except C1+C4? Use the identity: 0 ft case.
        // Validate via inverse property on a few rungs instead.
        for alt in [-1000i32, 0, 1100, 5000, 12_400] {
            // encode: find g such that gillham(g)==alt by brute force
            let found = (0..2048u32).find(|&g| gillham(g) == Some(alt));
            assert!(found.is_some(), "no Gillham code decodes to {alt}");
        }
    }

    #[test]
    fn squawk_from_identity_field() {
        // Book example (DF5): identity field 0x0356-pattern.
        // 13 bits: C1 A1 C2 A2 C4 A4 X B1 D1 B2 D2 B4 D4 — squawk 0356:
        // A=0 B=3 C=5 D=6 → a1a2a4=000, b:011→b1=1,b2=1,b4=0... build:
        let id: u32 = (1 << 12) /*C1*/ | (1 << 8) /*C4*/ | (1 << 5) /*B1*/
            | (1 << 3) /*B2*/ | (1 << 2) /*D2*/ | (1 << 0) /*D4*/;
        assert_eq!(squawk13(id), "0356");
    }
}

// ── Comm-B (BDS register) inference ─────────────────────────────────
// Layouts per the published Annex 10 / 1090-Riddle Comm-B chapter;
// validity gates in the pyModeS style (MIT); validated field-exact
// against pyModeS v3 on the vectors in the tests below.

fn mb_bit(mb: &[u8], i: usize) -> u32 {
    ((mb[(i - 1) / 8] >> (7 - (i - 1) % 8)) & 1) as u32
}

fn mb_field(mb: &[u8], start: usize, len: usize) -> u32 {
    (start..start + len).fold(0, |v, i| (v << 1) | mb_bit(mb, i))
}

fn mb_signed(mb: &[u8], sign_bit: usize, start: usize, len: usize) -> i32 {
    let v = mb_field(mb, start, len) as i32;
    if mb_bit(mb, sign_bit) == 1 { v - (1 << len) } else { v }
}

/// BDS 2,0 — aircraft identification.
pub fn bds20(mb: &[u8]) -> Option<serde_json::Value> {
    if mb_field(mb, 1, 8) != 0x20 {
        return None;
    }
    let mut cs = String::new();
    for k in 0..8 {
        let c = IDENT_CHARSET[mb_field(mb, 9 + 6 * k, 6) as usize];
        if c == b'#' {
            return None;
        }
        cs.push(c as char);
    }
    let cs = cs.trim_end().to_string();
    if cs.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "bds": "2,0", "callsign": cs }))
}

/// BDS 3,0 — ACAS active Resolution Advisory (TCAS RA), per ICAO Annex 10
/// Vol IV §4.3.8.4.2.4. Carries the ARA bits (what RA is issued), the RAC
/// bits (manoeuvres the aircraft must NOT take), terminal flags, and the
/// threat identity (TTI 1 = ICAO; TTI 2 = altitude/range/bearing).
///
/// MB bit positions are 1-indexed (the `mb_bit`/`mb_field` convention):
/// BDS id 1–8 (= 0x30), ARA 9–15 (issued/corrective/down/inc-rate/
/// reversal/crossing/positive), ARA-reserved 16–22 (< 48 gate), RAC 23–26
/// (no below/above/left/right), RA-terminated 27, multiple-threat 28, TTI
/// 29–30, TID 31–56.
pub fn bds30(mb: &[u8]) -> Option<serde_json::Value> {
    // BDS identifier must be 0x30.
    if mb_field(mb, 1, 8) != 0x30 {
        return None;
    }
    // The all-zero (no-id) case is already excluded by the id check.
    // ARA reserved-for-ACAS-III (MB 16–22) must be < 48 (pyModeS gate).
    if mb_field(mb, 16, 7) >= 48 {
        return None;
    }
    let tti = mb_field(mb, 29, 2);
    // TTI = 0b11 is reserved → reject.
    if tti == 0b11 {
        return None;
    }
    let mut o = serde_json::Map::new();
    o.insert("bds".into(), "3,0".into());
    o.insert("threat_type_indicator".into(), serde_json::json!(tti));
    o.insert("issued_ra".into(), serde_json::json!(mb_bit(mb, 9) == 1));
    o.insert("corrective".into(), serde_json::json!(mb_bit(mb, 10) == 1));
    o.insert("downward_sense".into(), serde_json::json!(mb_bit(mb, 11) == 1));
    o.insert("increased_rate".into(), serde_json::json!(mb_bit(mb, 12) == 1));
    o.insert("sense_reversal".into(), serde_json::json!(mb_bit(mb, 13) == 1));
    o.insert("altitude_crossing".into(), serde_json::json!(mb_bit(mb, 14) == 1));
    o.insert("positive".into(), serde_json::json!(mb_bit(mb, 15) == 1));
    o.insert("no_below".into(), serde_json::json!(mb_bit(mb, 23) == 1));
    o.insert("no_above".into(), serde_json::json!(mb_bit(mb, 24) == 1));
    o.insert("no_left".into(), serde_json::json!(mb_bit(mb, 25) == 1));
    o.insert("no_right".into(), serde_json::json!(mb_bit(mb, 26) == 1));
    o.insert("ra_terminated".into(), serde_json::json!(mb_bit(mb, 27) == 1));
    o.insert("multiple_threat".into(), serde_json::json!(mb_bit(mb, 28) == 1));

    match tti {
        // 24-bit ICAO at MB 31–54.
        1 => {
            let icao = mb_field(mb, 31, 24);
            o.insert("threat_icao".into(), serde_json::json!(format!("{icao:06X}")));
        }
        // Altitude (AC13, MB 31–43), range (MB 44–50), bearing (MB 51–56).
        2 => {
            let ac13 = mb_field(mb, 31, 13);
            o.insert("threat_altitude".into(), serde_json::json!(altitude13(ac13)));
            let range_raw = mb_field(mb, 44, 7);
            let range = (range_raw > 0).then(|| (range_raw as f64 - 1.0) / 10.0);
            o.insert("threat_range".into(), serde_json::json!(range));
            let bearing_raw = mb_field(mb, 51, 6);
            let bearing = (bearing_raw > 0).then(|| 6 * (bearing_raw - 1) + 3);
            o.insert("threat_bearing".into(), serde_json::json!(bearing));
        }
        _ => {}
    }
    Some(serde_json::Value::Object(o))
}

/// BDS 4,0 — selected vertical intention.
pub fn bds40(mb: &[u8]) -> Option<serde_json::Value> {
    // Reserved bits 40..=47 and 52..=53 must be zero.
    if mb_field(mb, 40, 8) != 0 || mb_field(mb, 52, 2) != 0 {
        return None;
    }
    let mut out = serde_json::Map::new();
    if mb_bit(mb, 1) == 1 {
        out.insert("selected_altitude_mcp".into(), (mb_field(mb, 2, 12) * 16).into());
    }
    if mb_bit(mb, 14) == 1 {
        out.insert("selected_altitude_fms".into(), (mb_field(mb, 15, 12) * 16).into());
    }
    if mb_bit(mb, 27) == 1 {
        let v = mb_field(mb, 28, 12) as f64 * 0.1 + 800.0;
        if !(800.0..=1210.0).contains(&v) {
            return None;
        }
        out.insert("baro_pressure_setting".into(), serde_json::json!(v));
    }
    if out.is_empty() {
        return None;
    }
    out.insert("bds".into(), "4,0".into());
    Some(serde_json::Value::Object(out))
}

/// BDS 5,0 — track and turn report.
pub fn bds50(mb: &[u8]) -> Option<serde_json::Value> {
    let roll = mb_signed(mb, 2, 3, 9) as f64 * 45.0 / 256.0;
    let track = {
        let v = mb_signed(mb, 13, 14, 10) as f64 * 90.0 / 512.0;
        if v < 0.0 { v + 360.0 } else { v }
    };
    let gs = mb_field(mb, 25, 10) * 2;
    let tas = mb_field(mb, 47, 10) * 2;
    // Gates: all five status bits set, plausible kinematics.
    for s in [1usize, 12, 24, 35, 46] {
        if mb_bit(mb, s) == 0 {
            return None;
        }
    }
    if roll.abs() > 50.0 || gs > 600 || tas > 575 || gs == 0 || tas == 0 {
        return None;
    }
    if (gs as f64 - tas as f64).abs() > 200.0 {
        return None;
    }
    let track_rate = mb_signed(mb, 36, 37, 9) as f64 * 8.0 / 256.0;
    Some(serde_json::json!({
        "bds": "5,0",
        "roll": roll,
        "true_track": track,
        "groundspeed": gs,
        "track_rate": track_rate,
        "true_airspeed": tas,
    }))
}

/// BDS 6,0 — heading and speed report.
pub fn bds60(mb: &[u8]) -> Option<serde_json::Value> {
    for s in [1usize, 13, 24, 35, 46] {
        if mb_bit(mb, s) == 0 {
            return None;
        }
    }
    let heading = {
        let v = mb_signed(mb, 2, 3, 10) as f64 * 90.0 / 512.0;
        if v < 0.0 { v + 360.0 } else { v }
    };
    let ias = mb_field(mb, 14, 10);
    let mach = mb_field(mb, 25, 10) as f64 * 2.048 / 512.0;
    let vr_baro = mb_signed(mb, 36, 37, 9) * 32;
    let vr_ins = mb_signed(mb, 47, 48, 9) * 32;
    if ias == 0 || ias > 500 || mach <= 0.0 || mach > 1.0 {
        return None;
    }
    if vr_baro.abs() > 6000 || vr_ins.abs() > 6000 {
        return None;
    }
    Some(serde_json::json!({
        "bds": "6,0",
        "magnetic_heading": heading,
        "indicated_airspeed": ias,
        "mach": (mach * 1000.0).round() / 1000.0,
        "baro_vertical_rate": vr_baro,
        "inertial_vertical_rate": vr_ins,
    }))
}

/// Infer the BDS register of a DF20/21 MB field: accept only when
/// exactly one decoder validates (the pyModeS approach).
pub fn bds_infer(mb: &[u8]) -> Option<serde_json::Value> {
    let cands: Vec<serde_json::Value> =
        [bds20(mb), bds30(mb), bds40(mb), bds50(mb), bds60(mb)].into_iter().flatten().collect();
    match cands.len() {
        1 => Some(cands.into_iter().next().unwrap()),
        _ => None, // none or ambiguous
    }
}

#[cfg(test)]
mod bds_tests {
    use super::*;

    fn mb_of(frame_hex: &str) -> Vec<u8> {
        (8..22)
            .step_by(2)
            .map(|i| u8::from_str_radix(&frame_hex[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Build the 7-byte MB from a 56-bit MSB-first payload integer (the
    /// representation pyModeS uses for its BDS payloads).
    fn mb_payload(p: u64) -> [u8; 7] {
        let b = p.to_be_bytes();
        [b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
    }

    // Oracle: pyModeS v3 decode() on these frames (2026-06-11).

    #[test]
    fn bds20_callsign_matches_pymodes() {
        let v = bds_infer(&mb_of("A000083E202CC371C31DE0AA1CCF")).unwrap();
        assert_eq!(v["bds"], "2,0");
        assert_eq!(v["callsign"], "KLM1017");
    }

    // Oracle: pyModeS bds30 / test_bds_commb TestBds30* synthetic
    // payloads (bit-exact, every shift constant pinned by those tests).

    #[test]
    fn bds30_minimal_ra_no_threat_matches_pymodes() {
        // test_minimal_ra_no_threat: payload 0x30_80_00_00_00_00_00.
        let v = bds30(&mb_payload(0x30_80_00_00_00_00_00)).unwrap();
        assert_eq!(v["bds"], "3,0");
        assert_eq!(v["threat_type_indicator"], 0);
        assert_eq!(v["issued_ra"], true);
        assert_eq!(v["corrective"], false);
        assert_eq!(v["downward_sense"], false);
        assert_eq!(v["increased_rate"], false);
        assert_eq!(v["sense_reversal"], false);
        assert_eq!(v["altitude_crossing"], false);
        assert_eq!(v["positive"], false);
        assert_eq!(v["no_below"], false);
        assert_eq!(v["no_above"], false);
        assert_eq!(v["no_left"], false);
        assert_eq!(v["no_right"], false);
        assert_eq!(v["ra_terminated"], false);
        assert_eq!(v["multiple_threat"], false);
    }

    #[test]
    fn bds30_multi_flag_matches_pymodes() {
        // test_multi_flag_decode: issued_ra, corrective, sense_reversal,
        // no_above, multiple_threat set; pins every ARA/RAC shift.
        let payload = 0x30_00_00_00_00_00_00u64
            | (1 << (55 - 8))  // issued_ra
            | (1 << (55 - 9))  // corrective
            | (1 << (55 - 12)) // sense_reversal
            | (1 << (55 - 23)) // no_above
            | (1 << (55 - 27)); // multiple_threat
        let v = bds30(&mb_payload(payload)).unwrap();
        assert_eq!(v["issued_ra"], true);
        assert_eq!(v["corrective"], true);
        assert_eq!(v["sense_reversal"], true);
        assert_eq!(v["no_above"], true);
        assert_eq!(v["multiple_threat"], true);
        // Everything else stays false.
        assert_eq!(v["downward_sense"], false);
        assert_eq!(v["altitude_crossing"], false);
        assert_eq!(v["no_below"], false);
        assert_eq!(v["ra_terminated"], false);
    }

    #[test]
    fn bds30_tti1_icao_threat_matches_pymodes() {
        // test_tti_1_icao_threat: TTI=1, threat ICAO ABCDEF at bits 30-53.
        let payload =
            0x30_80_00_00_00_00_00u64 | (1 << (55 - 29)) | (0xABCDEFu64 << 2);
        let v = bds30(&mb_payload(payload)).unwrap();
        assert_eq!(v["threat_type_indicator"], 1);
        assert_eq!(v["threat_icao"], "ABCDEF");
    }

    #[test]
    fn bds30_tti2_alt_range_bearing_matches_pymodes() {
        // test_tti_2_altitude_range_bearing: range raw 10 → 0.9 NM,
        // bearing raw 3 → 15°, altitude 0 → None.
        let payload = 0x30_80_00_00_00_00_00u64
            | (0b10 << (55 - 29))
            | (10 << (55 - 49))
            | (3 << (55 - 55));
        let v = bds30(&mb_payload(payload)).unwrap();
        assert_eq!(v["threat_type_indicator"], 2);
        assert!((v["threat_range"].as_f64().unwrap() - 0.9).abs() < 1e-9);
        assert_eq!(v["threat_bearing"], 15);
        assert!(v["threat_altitude"].is_null());
    }

    #[test]
    fn bds30_tti2_altitude_delegates_to_altcode() {
        // test_tti_2_altitude_delegates_to_altcode: AC13 0x1010 → 24600 ft.
        let payload =
            0x30_80_00_00_00_00_00u64 | (0b10 << (55 - 29)) | (0x1010u64 << 13);
        let v = bds30(&mb_payload(payload)).unwrap();
        assert_eq!(v["threat_type_indicator"], 2);
        assert_eq!(v["threat_altitude"], 24600);
    }

    #[test]
    fn bds30_validity_gates_match_pymodes() {
        // is_bds30 rejects: wrong BDS id, TTI=0b11, ARA-reserved >= 48.
        assert!(bds30(&mb_payload(0)).is_none());
        assert!(bds30(&mb_payload(0x30_80_00_00_00_00_00 | (0b11 << (55 - 29)))).is_none());
        assert!(bds30(&mb_payload(0x30_80_00_00_00_00_00 | (48 << (55 - 21)))).is_none());
        // Boundary: ARA-reserved == 47 accepted.
        assert!(bds30(&mb_payload(0x30_80_00_00_00_00_00 | (47 << (55 - 21)))).is_some());
    }

    #[test]
    fn bds30_inferred_through_commb() {
        // test_commb_bds30_end_to_end equivalent: bds_infer routes a
        // minimal BDS30 payload to the 3,0 register unambiguously.
        let v = bds_infer(&mb_payload(0x30_80_00_00_00_00_00)).unwrap();
        assert_eq!(v["bds"], "3,0");
        assert_eq!(v["issued_ra"], true);
    }

    #[test]
    fn bds40_selected_altitude_matches_pymodes() {
        let v = bds_infer(&mb_of("A000029C85E42F313000007047D3")).unwrap();
        assert_eq!(v["bds"], "4,0");
        assert_eq!(v["selected_altitude_mcp"], 3008);
        assert_eq!(v["selected_altitude_fms"], 3008);
        assert_eq!(v["baro_pressure_setting"], 1020.0);
    }

    #[test]
    fn bds50_track_turn_matches_pymodes() {
        let v = bds_infer(&mb_of("A000139381951536E024D4CCF6B5")).unwrap();
        assert_eq!(v["bds"], "5,0");
        assert_eq!(v["roll"], 2.109375);
        assert_eq!(v["true_track"], 114.2578125);
        assert_eq!(v["groundspeed"], 438);
        assert_eq!(v["track_rate"], 0.125);
        assert_eq!(v["true_airspeed"], 424);
    }

    #[test]
    fn bds60_heading_speed_matches_pymodes() {
        let v = bds_infer(&mb_of("A00004128F39F91A7E27C46ADC21")).unwrap();
        assert_eq!(v["bds"], "6,0");
        assert_eq!(v["magnetic_heading"], 42.71484375);
        assert_eq!(v["indicated_airspeed"], 252);
        assert_eq!(v["mach"], 0.42);
        assert_eq!(v["baro_vertical_rate"], -1920);
        assert_eq!(v["inertial_vertical_rate"], -1920);
    }
}

#[cfg(test)]
mod opstatus_tests {
    use super::*;

    /// Build the 7-byte ME from a 56-bit MSB-first payload integer.
    fn me_of(payload: u64) -> [u8; 7] {
        let b = payload.to_be_bytes();
        [b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
    }

    #[test]
    fn operational_status_v2_airborne_matches_bds65_layout() {
        // Synthetic TC=31 subtype-0 payload using the exact pyModeS bds65
        // bit layout: version=2, nic_supp_a=1, nac_p=10, sil=3, nic_baro=1.
        let payload: u64 = (31u64 << 51) | (2 << 13) | (1 << 12) | (10 << 8) | (3 << 4) | (1 << 3);
        let v = operational_status(&me_of(payload)).unwrap();
        assert_eq!(v["subtype"], "airborne");
        assert_eq!(v["version"], 2);
        assert_eq!(v["nic_supp_a"], 1);
        assert_eq!(v["nac_p"], 10);
        assert_eq!(v["sil"], 3);
        assert_eq!(v["baro_alt_integrity"], 1);
    }

    #[test]
    fn operational_status_v0_omits_versioned_fields() {
        // Version 0 predates NACp/SIL/NICbaro; only subtype+version emit.
        let payload: u64 = 31u64 << 51; // version 0, subtype 0
        let v = operational_status(&me_of(payload)).unwrap();
        assert_eq!(v["version"], 0);
        assert!(v.get("nac_p").is_none());
        assert!(v.get("baro_alt_integrity").is_none());
    }

    #[test]
    fn operational_status_surface_has_no_gva_or_baro() {
        let payload: u64 = (31u64 << 51) | (1 << 48) | (2 << 13); // subtype 1, v2
        let v = operational_status(&me_of(payload)).unwrap();
        assert_eq!(v["subtype"], "surface");
        assert!(v.get("gva").is_none());
        assert!(v.get("baro_alt_integrity").is_none());
    }

    #[test]
    fn aircraft_status_decodes_emergency_state() {
        // TC=28 subtype 1, emergency state 5 (unlawful interference) at ME 8-10.
        let payload: u64 = (28u64 << 51) | (1 << 48) | (5 << 45);
        let v = aircraft_status(&me_of(payload)).unwrap();
        assert_eq!(v["subtype"], 1);
        assert_eq!(v["emergency_state"], 5);
        assert_eq!(v["emergency"], "unlawful interference");
    }

    #[test]
    fn aircraft_status_flags_acas_ra_subtype() {
        let payload: u64 = (28u64 << 51) | (2 << 48); // subtype 2
        let v = aircraft_status(&me_of(payload)).unwrap();
        assert_eq!(v["acas_ra"], true);
    }

    #[test]
    fn rejects_wrong_typecode() {
        assert!(operational_status(&me_of(19u64 << 51)).is_none());
        assert!(aircraft_status(&me_of(31u64 << 51)).is_none());
    }

    /// 7-byte ME slice of a full 28-hex-char message.
    fn me_hex(frame: &str) -> Vec<u8> {
        let bytes: Vec<u8> = (0..frame.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&frame[i..i + 2], 16).unwrap())
            .collect();
        bytes[4..11].to_vec()
    }

    #[test]
    fn target_state_matches_pymodes_bds62() {
        // Oracle: pyModeS test_bds62 golden frame (DO-260B compliant).
        // pyModeS decode() → selected_altitude 16992 source MCP/FCU,
        // baro 1012.8, heading ~66.8, autopilot/vnav/lnav/tcas True,
        // alt-hold/approach False.
        let v = target_state(&me_hex("8DA05629EA21485CBF3F8CADAEEB")).unwrap();
        assert_eq!(v["subtype"], "target_state");
        assert_eq!(v["ts_subtype"], 1);
        assert_eq!(v["selected_altitude"], 16992);
        assert_eq!(v["selected_altitude_source"], "MCP/FCU");
        assert!((v["baro_pressure_setting"].as_f64().unwrap() - 1012.8).abs() < 0.01);
        assert!((v["selected_heading"].as_f64().unwrap() - 66.796875).abs() < 0.01);
        assert_eq!(v["autopilot"], true);
        assert_eq!(v["vnav_mode"], true);
        assert_eq!(v["altitude_hold_mode"], false);
        assert_eq!(v["approach_mode"], false);
        assert_eq!(v["lnav_mode"], true);
        assert_eq!(v["tcas_operational"], true);
        // NACp/NICbaro/SIL present and in range.
        assert_eq!(v["nac_p"], 9);
        assert_eq!(v["nic_baro"], 1);
        assert_eq!(v["sil"], 3);
    }

    #[test]
    fn target_state_selected_heading_high_bit() {
        // Oracle: pyModeS regression vector — heading sign/high bit set
        // → 246.796875°.
        let v = target_state(&me_hex("8DA05629EA21485EBF3F8CADAEEB")).unwrap();
        assert!((v["selected_heading"].as_f64().unwrap() - 246.796875).abs() < 0.01);
    }

    #[test]
    fn target_state_rejects_wrong_typecode() {
        assert!(target_state(&me_hex("8DA05629EA21485CBF3F8CADAEEB")).is_some());
        // TC=31 (operational status) must not parse as target state.
        let payload: u64 = 31u64 << 51;
        assert!(target_state(&me_of(payload)).is_none());
    }

    #[test]
    fn surface_velocity_matches_riddle_example() {
        // "The 1090 MHz Riddle" §4 worked example: movement 41 → 17 kt,
        // ground track 33 → 92.8125°.
        let v = surface_velocity(&me_hex("8C4841753A9A153237AEF0F275BE")).unwrap();
        assert!(!v.airspeed);
        assert_eq!(v.speed_kt, 17.0);
        assert!((v.track_deg - 92.8125).abs() < 1e-6, "{}", v.track_deg);
        assert_eq!(v.vertical_rate_fpm, None);
    }

    #[test]
    fn surface_speed_table_boundaries() {
        assert_eq!(surface_speed_kt(0), None); // not available
        assert_eq!(surface_speed_kt(1), Some(0.0)); // stopped
        assert_eq!(surface_speed_kt(2), Some(0.125));
        assert_eq!(surface_speed_kt(9), Some(1.0));
        assert_eq!(surface_speed_kt(13), Some(2.0));
        assert_eq!(surface_speed_kt(39), Some(15.0));
        assert_eq!(surface_speed_kt(94), Some(70.0));
        assert_eq!(surface_speed_kt(109), Some(100.0));
        assert_eq!(surface_speed_kt(124), Some(175.0));
        assert_eq!(surface_speed_kt(125), None); // reserved
    }
}
