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
    /// NACv — Navigation Accuracy Category, velocity (ME bits 10–12).
    pub nac_v: u8,
    /// Vertical-rate source: `false` = GNSS (geometric), `true` =
    /// barometric (ME bit 35; airborne velocity only).
    pub vr_baro_source: bool,
    /// GNSS-height minus barometric-altitude difference, feet (ME bits
    /// 48–55; airborne velocity only). `None` when not available.
    pub geo_minus_baro_ft: Option<i32>,
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
    let nac_v = field(10, 3) as u8;
    let vr_baro_source = bit(35) == 1;
    let vr_raw = field(37, 9);
    let vertical_rate_fpm = (vr_raw != 0).then(|| {
        let v = (vr_raw as i32 - 1) * 64;
        if bit(36) == 1 { -v } else { v }
    });
    // GNSS-minus-baro: sign bit 48, magnitude bits 49–55; 0 and 127 mean
    // "not available" (pyModeS `bds09`).
    let diff_mag = field(49, 7);
    let geo_minus_baro_ft = (diff_mag != 0 && diff_mag != 127).then(|| {
        let v = (diff_mag as i32 - 1) * 25;
        if bit(48) == 1 { -v } else { v }
    });
    Some(Velocity {
        speed_kt,
        track_deg,
        airspeed,
        vertical_rate_fpm,
        nac_v,
        vr_baro_source,
        geo_minus_baro_ft,
    })
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
    // Surface velocity carries no NACv / VR-source / geo-baro fields.
    Some(Velocity {
        speed_kt,
        track_deg,
        airspeed: false,
        vertical_rate_fpm: None,
        nac_v: 0,
        vr_baro_source: false,
        geo_minus_baro_ft: None,
    })
}

/// Decode a TC 31 Aircraft Operational Status ME field (7 bytes) into the
/// modern accuracy/integrity layer: ADS-B version, NIC supplement, NACp,
/// SIL (+ supplement), and — airborne only — GVA and barometric-altitude
/// integrity. Returns `None` for a non-TC31 field.
///
/// ME-relative, 0-indexed bit positions (per "The 1090 MHz Riddle" §6 and
/// pyModeS `bds65`): subtype 5–7, operational-mode 24–39 (its low two bits
/// 38–39 are SDA, the rs1090 `bds65` layout), version 40–42,
/// NIC-supplement-A 43 (= NICa), NACp 44–47, GVA 48–49, SIL 50–51,
/// NICbaro 52, HRD 53, SIL-supplement 54. NIC-supplement-C is carried at
/// ME bit 19 (pyModeS `nic_a_c`, `msgbin[51]`). NACp/SIL/NIC-supplement
/// were introduced in version 1; SDA and the SIL supplement in version 2.
///
/// The emitted NIC-supplement bits (`nic_supp_a` / `nic_supp_c`) are the
/// per-aircraft state a position decoder pairs with a TC's own type code
/// to resolve the version-aware NIC (see [`nic_v1`] / [`nic_v2`]).
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
        // NIC-supplement-C (airborne) / -B disambiguation bit at ME 19.
        o.insert("nic_supp_c".into(), serde_json::json!(bit(19)));
        o.insert("nac_p".into(), serde_json::json!(field(44, 4)));
        o.insert("sil".into(), serde_json::json!(field(50, 2)));
        // HRD — heading reference (0 = true north, 1 = magnetic north).
        o.insert("hrd".into(), serde_json::json!(bit(53)));
        if subtype == 0 {
            o.insert("gva".into(), serde_json::json!(field(48, 2)));
            o.insert("baro_alt_integrity".into(), serde_json::json!(bit(52)));
        }
        if version >= 2 {
            o.insert("sil_supplement".into(), serde_json::json!(bit(54)));
            // SDA — system design assurance, the low 2 bits of the
            // operational-mode field (ME 38–39).
            o.insert("sda".into(), serde_json::json!(field(38, 2)));
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

/// Classify a DF18 CF (Control Field, frame bits 5–7) into the ADS-B
/// source / address-type it denotes, per DO-260B §2.2.3.2.1.2 as
/// implemented identically by readsb and dump1090-fa (`mode_s.c`, the
/// DF18 CF switch): CF=0 ADS-B non-transponder (ICAO addr), CF=1 ADS-B
/// anonymous/non-ICAO addr, CF=2 fine TIS-B, CF=3 coarse TIS-B, CF=5 fine
/// TIS-B with non-ICAO addr, CF=6 ADS-R rebroadcast; CF=4/7 unknown
/// format. Returns `(source, addr_type, description)`.
pub fn df18_cf_class(cf: u8) -> (&'static str, &'static str, &'static str) {
    match cf {
        0 => ("ADS-B", "icao_nt", "ADS-B non-transponder device (ICAO address)"),
        1 => ("ADS-B", "non_icao", "ADS-B with anonymous / non-ICAO address"),
        2 => ("TIS-B", "tisb_icao", "fine TIS-B (ICAO address)"),
        3 => ("TIS-B", "tisb_icao", "coarse TIS-B airborne position/velocity"),
        5 => ("TIS-B", "tisb_non_icao", "fine TIS-B with non-ICAO address"),
        6 => ("ADS-R", "adsr_icao", "ADS-R rebroadcast from an alternate data link"),
        _ => ("unknown", "unknown", "reserved / unknown CF format"),
    }
}

/// 13-bit Mode S altitude field (AC, DF0/4/16/20): M-bit metric flag,
/// Q-bit 25 ft, else 100 ft Gillham. The Gillham branch routes through the
/// dump1090-verified Mode A/C ladder ([`crate::mode_ac::gillham_ac13_ft`]),
/// which matches both dump1090 (`decodeAC13Field`) and pyModeS
/// (`_altcode.altcode_to_altitude`) exactly across all 4096 codes.
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
    crate::mode_ac::gillham_ac13_ft(ac)
}

/// 12-bit ADS-B airborne-position altitude field (TC 9–18): a 13-bit AC
/// field with the M bit removed (always 0). Q=1 → 25 ft linear; Q=0 →
/// 100 ft Gillham. Reinserts a zero M bit at position 6 and delegates to
/// [`altitude13`] — the dump1090 `decodeAC12Field` procedure, which both
/// dump1090 and pyModeS `bds05` follow. `None` for a zero field or an
/// invalid Gillham code.
pub fn altitude12(ac12: u32) -> Option<i32> {
    if ac12 == 0 {
        return None;
    }
    // Insert a zero M bit at position 6: top 6 bits shift up by one, low 6
    // bits stay (dump1090 `decodeAC12Field` Gillham branch).
    let ac13 = ((ac12 & 0x0FC0) << 1) | (ac12 & 0x003F);
    altitude13(ac13)
}

/// 12-bit GNSS-height field (TC 20–22 geometric altitude): an unsigned
/// integer count of metres, converted to feet. pyModeS `bds05`
/// (`int(ac * 3.28084)`). `None` for a zero field (not available).
pub fn gnss_height_ft(ac12: u32) -> Option<i32> {
    (ac12 != 0).then(|| (ac12 as f64 * 3.28084) as i32)
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

// ── Accuracy / integrity (NUCp / NIC / NACv / SDA) ──────────────────
// The version-dependent ADS-B quality layer. NUCp is the version-0
// (DO-260) position uncertainty derived directly from the type code;
// NIC is the version-1/2 (DO-260A/B) integrity category, which needs
// the type code *and* the NIC-supplement bits that arrive in a separate
// TC31 operational-status (and TC9-18 single-bit) message. NACv is the
// velocity accuracy carried in the TC19 message itself. SDA is the
// version-2 system-design-assurance from the TC31 operational mode.
//
// Lookup tables and the resolution procedure mirror pyModeS
// `uncertainty.py` (TC_NUCp_lookup / TC_NICv1_lookup / TC_NICv2_lookup)
// and its `nuc_p` / `nic_v1` / `nic_v2` / `nac_v` decoders — facts and
// table values only, no code ported; validated field-exact against
// pyModeS in the tests below (the published NIC golden-vector set).

/// NUCp (Navigation Uncertainty Category — Position), ADS-B version 0,
/// from the type code. `None` for type codes that carry no position
/// (i.e. not 5–8, 9–18 barometric, or 20–22 GNSS). pyModeS
/// `TC_NUCp_lookup`.
pub fn nuc_p(tc: u8) -> Option<u8> {
    Some(match tc {
        5 | 9 | 20 => 9,
        6 | 10 | 21 => 8,
        7 | 11 => 7,
        8 | 12 => 6,
        13 => 5,
        14 => 4,
        15 => 3,
        16 => 2,
        17 => 1,
        18 | 22 => 0,
        _ => return None,
    })
}

/// 95 % horizontal containment radius (metres) for a NUCp value, per
/// pyModeS `uncertainty.NUCp` (`RCu`). `None` where the category gives no
/// bound (NUCp 0).
pub fn nuc_p_rcu_m(nuc_p: u8) -> Option<u32> {
    Some(match nuc_p {
        9 => 3,
        8 => 10,
        7 => 93,
        6 => 185,
        5 => 463,
        4 => 926,
        3 => 1852,
        2 => 9260,
        1 => 18520,
        _ => return None,
    })
}

/// NIC (Navigation Integrity Category), ADS-B version 1 (DO-260A), from
/// the type code and the NIC supplement-A bit (the TC31
/// operational-status supplement). pyModeS `TC_NICv1_lookup` + `nic_v1`.
/// `None` for non-position type codes.
pub fn nic_v1(tc: u8, nic_supp_a: u8) -> Option<u8> {
    let s = nic_supp_a & 1;
    Some(match tc {
        5 | 9 | 20 => 11,
        6 | 10 | 21 => 10,
        7 => 9,
        8 | 18 | 22 => 0,
        11 => if s == 1 { 9 } else { 8 },
        12 => 7,
        13 => 6,
        14 => 5,
        15 => 4,
        16 => if s == 1 { 3 } else { 2 },
        17 => 1,
        _ => return None,
    })
}

/// NIC (Navigation Integrity Category), ADS-B version 2 (DO-260B), from
/// the type code and the supplement bits NICa (TC31) and NICb/NICc.
/// pyModeS `TC_NICv2_lookup` + `nic_v2`: airborne TC9-18 select on
/// `NICa*2 + NICb`, surface TC5-8 on `NICa*2 + NICc`, GNSS TC20-22 force
/// supplement 0. `None` for a non-position type code or an undefined
/// (TC, supplement) combination.
pub fn nic_v2(tc: u8, nic_a: u8, nic_bc: u8) -> Option<u8> {
    let ns = if (20..=22).contains(&tc) { 0 } else { (nic_a & 1) * 2 + (nic_bc & 1) };
    Some(match tc {
        5 | 9 | 20 => 11,
        6 | 10 | 21 => 10,
        7 => match ns {
            2 => 9,
            0 => 8,
            _ => return None,
        },
        8 => match ns {
            3 => 7,
            1 | 2 => 6,
            0 => 0,
            _ => return None,
        },
        11 => match ns {
            3 => 9,
            0 => 8,
            _ => return None,
        },
        12 => 7,
        13 => 6,
        14 => 5,
        15 => 4,
        16 => match ns {
            3 => 3,
            0 => 2,
            _ => return None,
        },
        17 => 1,
        18 | 22 => 0,
        _ => return None,
    })
}

/// NACv 95 % horizontal velocity figure-of-merit (m/s) for a NACv code,
/// per pyModeS `uncertainty.NACv` (`HFOMr`). `None` for NACv 0 (unknown
/// or > 10 m/s).
pub fn nac_v_hfom_mps(nac_v: u8) -> Option<f64> {
    Some(match nac_v {
        1 => 10.0,
        2 => 3.0,
        3 => 1.0,
        4 => 0.3,
        _ => return None,
    })
}

/// Per-fix position-quality object for an airborne / surface position
/// message (TC 5–8, 9–18, 20–22): the version-0 NUCp (and its containment
/// radius), the in-message NIC-supplement bit (NICb at ME bit 7 for
/// airborne barometric positions), and — when a version and the matching
/// operational-status supplement are known — the resolved version-aware
/// NIC. `nic_supp_a` / `nic_supp_c` come from the aircraft's last TC31
/// operational-status; pass `None` for `version` when no status has been
/// seen (NUCp still emits). Returns `None` for a non-position type code.
pub fn position_quality(
    tc: u8,
    nic_b: u8,
    version: Option<u8>,
    nic_supp_a: u8,
    nic_supp_c: u8,
) -> Option<serde_json::Value> {
    let nuc_p = nuc_p(tc)?;
    let mut o = serde_json::Map::new();
    o.insert("nuc_p".into(), serde_json::json!(nuc_p));
    if let Some(rc) = nuc_p_rcu_m(nuc_p) {
        o.insert("nuc_p_radius_m".into(), serde_json::json!(rc));
    }
    // NICb is meaningful only for airborne barometric positions (TC9-18),
    // where it disambiguates the version-2 NIC; surface uses NICc.
    if (9..=18).contains(&tc) {
        o.insert("nic_b".into(), serde_json::json!(nic_b & 1));
    }
    if let Some(v) = version {
        let nic = match v {
            1 => nic_v1(tc, nic_supp_a),
            2 => {
                // Airborne barometric uses NICb (from this message);
                // surface uses NICc (from operational status).
                let bc = if (9..=18).contains(&tc) { nic_b } else { nic_supp_c };
                nic_v2(tc, nic_supp_a, bc)
            }
            _ => None,
        };
        if let Some(nic) = nic {
            o.insert("nic".into(), serde_json::json!(nic));
            o.insert("nic_version".into(), serde_json::json!(v));
        }
    }
    Some(serde_json::Value::Object(o))
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
        // Oracle: pyModeS bds09.decode_bds09 on this frame → nac_v 0,
        // vr_source GNSS, geo_minus_baro 550 ft.
        assert_eq!(v.nac_v, 0);
        assert!(!v.vr_baro_source);
        assert_eq!(v.geo_minus_baro_ft, Some(550));
    }

    #[test]
    fn airspeed_velocity_matches_book() {
        let v = velocity(&me_of("8DA05F219B06B6AF189400CBC33F")).unwrap();
        assert!(v.airspeed);
        assert!((v.speed_kt - 375.0).abs() < 0.5, "{}", v.speed_kt);
        assert!((v.track_deg - 243.98).abs() < 0.05, "{}", v.track_deg);
        // Oracle: pyModeS bds09 → nac_v 0, vr_source BARO, geo diff N/A.
        assert_eq!(v.nac_v, 0);
        assert!(v.vr_baro_source);
        assert_eq!(v.geo_minus_baro_ft, None);
    }

    #[test]
    fn velocity_nacv_and_baro_source_match_pymodes() {
        // Oracle: pyModeS bds09.decode_bds09("8d3461cf9908388930080f948ea1")
        // → subtype 1, nac_v 1, vr_source BARO, vertical_rate +64,
        // geo_minus_baro 350.
        let v = velocity(&me_of("8d3461cf9908388930080f948ea1")).unwrap();
        assert_eq!(v.nac_v, 1);
        assert!(v.vr_baro_source);
        assert_eq!(v.vertical_rate_fpm, Some(64));
        assert_eq!(v.geo_minus_baro_ft, Some(350));
        // NACv code 1 → 10 m/s horizontal figure of merit.
        assert_eq!(nac_v_hfom_mps(v.nac_v), Some(10.0));
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
    fn altitude13_gillham_matches_dump1090_pymodes() {
        // Oracle: the dump1090 `decodeAC13Field` Gillham branch
        // (`modeAToModeC(decodeID13Field(ac))`), which matches pyModeS
        // `_altcode.altcode_to_altitude` byte-for-byte. AC13 fields are
        // built by re-inserting the M=0 bit into the verified AC12 Gillham
        // samples (ac12 << shifting per dump1090 decodeAC12Field):
        // ac12 0x248 → 5000 ft, 0x0C8 → 4800 ft, 0x0C2 → 5800 ft.
        for (ac12, exp) in [(0x248u32, 5000i32), (0x0C8, 4800), (0x0C2, 5800)] {
            let ac13 = ((ac12 & 0x0FC0) << 1) | (ac12 & 0x003F);
            assert_eq!(altitude13(ac13), Some(exp), "ac12 {ac12:#05x}");
        }
    }

    #[test]
    fn altitude12_q1_and_q0_match_pymodes() {
        // Q=1 (25-ft linear) path, from the pyModeS test_adsb altitude
        // vectors (the 12-bit ME altitude field of each frame):
        //   8D40621D58C382… → 38000 ft;  8d484fde5803b647… → -325 ft.
        let ac_of = |frame: &str| -> u32 {
            let me = me_of(frame);
            let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
            (8..20).fold(0u32, |v, i| (v << 1) | bit(i))
        };
        assert_eq!(altitude12(ac_of("8D40621D58C382D690C8AC2863A7")), Some(38000));
        assert_eq!(altitude12(ac_of("8d484fde5803b647ecec4fcdd74f")), Some(-325));
        assert_eq!(altitude12(ac_of("8d346355580b064116e70a269f97")), Some(1000));
        // Q=0 (Gillham) path, from CRC-valid pyModeS-verified frames built
        // around the verified AC12 Gillham samples.
        assert_eq!(altitude12(ac_of("8D40621D582482B504C5C9D9B414")), Some(5000));
        assert_eq!(altitude12(ac_of("8D40621D580C82B504C5C92B2279")), Some(4800));
        assert_eq!(altitude12(ac_of("8D40621D580C22B504C5C930E8B0")), Some(5800));
        // Zero field → not available.
        assert_eq!(altitude12(0), None);
    }

    #[test]
    fn gnss_height_matches_pymodes() {
        // pyModeS bds05 GNSS-height conversion: int(ac * 3.28084).
        // TC20 frame with ac12 = 3000 m → 9842 ft (pyModeS decode()).
        assert_eq!(gnss_height_ft(3000), Some(9842));
        assert_eq!(gnss_height_ft(1000), Some(3280));
        assert_eq!(gnss_height_ft(0), None);
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

    /// Type code of a 28-hex extended-squitter frame (ME bits 0–4).
    fn tc_of(frame_hex: &str) -> u8 {
        let me = me_of(frame_hex);
        me[0] >> 3
    }

    #[test]
    fn nuc_p_lookup_matches_pymodes() {
        // pyModeS uncertainty.TC_NUCp_lookup (version-0 NUCp by TC).
        assert_eq!(nuc_p(5), Some(9));
        assert_eq!(nuc_p(9), Some(9));
        assert_eq!(nuc_p(11), Some(7));
        assert_eq!(nuc_p(18), Some(0));
        assert_eq!(nuc_p(20), Some(9));
        assert_eq!(nuc_p(22), Some(0));
        // Non-position TCs (ident, velocity, status) have no NUCp.
        assert_eq!(nuc_p(1), None);
        assert_eq!(nuc_p(19), None);
        assert_eq!(nuc_p(31), None);
        // Containment radius (RCu): NUCp 7 → 93 m, NUCp 0 → none.
        assert_eq!(nuc_p_rcu_m(7), Some(93));
        assert_eq!(nuc_p_rcu_m(0), None);
    }

    #[test]
    fn nic_v1_matches_pymodes_golden_vectors() {
        // Oracle: the published pyModeS test_adsb NIC golden vectors
        // (frame → expected NIC). Each frame's own TC plus the NIC
        // supplement context the vector was captured under reproduce the
        // exact NIC pyModeS `nic_v1` returns. (TC16 and TC11 are the two
        // supplement-sensitive rows; both supplement values appear.)
        // (frame, nic_supp_a, expected_nic)
        let cases = [
            ("8D3C70A390AB11F55B8C57F65FE6", 0u8, 0u8), // TC18
            ("8DE1C9738A4A430B427D219C8225", 0, 1),     // TC17
            ("8D44058880B50006B1773DC2A7E9", 0, 2),     // TC16, supp 0
            ("8D44058881B50006B1773DC2A7E9", 1, 3),     // TC16, supp 1
            ("8D4AB42A78000640000000FA0D0A", 0, 4),     // TC15
            ("8D4405887099F5D9772F37F86CB6", 0, 5),     // TC14
            ("8D4841A86841528E72D9B472DAC2", 0, 6),     // TC13
            ("8D44057560B9760C0B840A51C89F", 0, 7),     // TC12
            ("8D40621D58C382D690C8AC2863A7", 0, 8),     // TC11, supp 0
            ("8F48511C598D04F12CCF82451642", 1, 9),     // TC11, supp 1
            ("8DA4D53A50DBF8C6330F3B35458F", 0, 10),    // TC10
            ("8D3C4ACF4859F1736F8E8ADF4D67", 0, 11),    // TC9
        ];
        for (frame, supp, exp) in cases {
            let tc = tc_of(frame);
            assert_eq!(
                nic_v1(tc, supp),
                Some(exp),
                "frame {frame} tc {tc} supp {supp}"
            );
        }
    }

    #[test]
    fn nic_v2_supplement_resolution_matches_pymodes() {
        // pyModeS nic_v2 / TC_NICv2_lookup: airborne TC selects on
        // NICa*2 + NICb. TC11 → 8 (supp 00) or 9 (supp 11); TC16 → 2
        // (00) or 3 (11); intermediate supplements undefined.
        assert_eq!(nic_v2(11, 0, 0), Some(8));
        assert_eq!(nic_v2(11, 1, 1), Some(9));
        assert_eq!(nic_v2(11, 0, 1), None); // ns=1 undefined for TC11
        assert_eq!(nic_v2(16, 0, 0), Some(2));
        assert_eq!(nic_v2(16, 1, 1), Some(3));
        // GNSS TCs ignore the supplement bits (forced to 0).
        assert_eq!(nic_v2(20, 1, 1), Some(11));
        assert_eq!(nic_v2(21, 1, 1), Some(10));
        assert_eq!(nic_v2(22, 1, 1), Some(0));
        // Non-position TC → None.
        assert_eq!(nic_v2(19, 0, 0), None);
    }

    #[test]
    fn operational_status_emits_nic_supplement_and_sda() {
        // Synthetic TC31 v2 airborne payload matching the pyModeS bds65
        // field layout (verified against bds65.decode_bds65): version 2,
        // nic_supp_a 1, nic_supp_c 1, nac_p 9, gva 2, sil 3, nic_baro 1,
        // hrd 1, sil_supplement 1, SDA 2 (ME 38–39).
        let mut me = [0u8; 7];
        let set = |me: &mut [u8; 7], start: usize, len: usize, val: u32| {
            for i in 0..len {
                if (val >> (len - 1 - i)) & 1 == 1 {
                    me[(start + i) / 8] |= 0x80 >> ((start + i) % 8);
                }
            }
        };
        set(&mut me, 0, 5, 31); // TC 31
        set(&mut me, 19, 1, 1); // nic_supp_c
        set(&mut me, 38, 2, 2); // SDA
        set(&mut me, 40, 3, 2); // version 2
        set(&mut me, 43, 1, 1); // nic_supp_a
        set(&mut me, 44, 4, 9); // nac_p
        set(&mut me, 48, 2, 2); // gva
        set(&mut me, 50, 2, 3); // sil
        set(&mut me, 52, 1, 1); // nic_baro
        set(&mut me, 53, 1, 1); // hrd
        set(&mut me, 54, 1, 1); // sil_supplement
        let v = operational_status(&me).unwrap();
        assert_eq!(v["version"], 2);
        assert_eq!(v["nic_supp_a"], 1);
        assert_eq!(v["nic_supp_c"], 1);
        assert_eq!(v["nac_p"], 9);
        assert_eq!(v["sil"], 3);
        assert_eq!(v["sil_supplement"], 1);
        assert_eq!(v["sda"], 2);
        assert_eq!(v["hrd"], 1);
        assert_eq!(v["gva"], 2);
        assert_eq!(v["baro_alt_integrity"], 1);
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

/// Sign-magnitude combine: a `len`-bit magnitude and a separate sign bit
/// (1 = negative). Mirrors pyModeS `_helpers.signed` — NOT two's
/// complement: sign=1, magnitude=0 represents −2^len.
fn sign_mag(magnitude: u32, len: usize, sign: u32) -> i32 {
    if sign == 1 { magnitude as i32 - (1 << len) } else { magnitude as i32 }
}

/// Status/value consistency gate (pyModeS `_helpers.wrong_status`): a
/// status-gated field must have an all-zero value field when its status
/// bit is clear. Returns true when status == 0 but the value is nonzero
/// (corrupt frame or a non-matching register that passed the format-ID).
/// All positions are 1-indexed (the `mb_bit`/`mb_field` convention).
fn wrong_status(mb: &[u8], status_bit: usize, value_start: usize, value_len: usize) -> bool {
    mb_bit(mb, status_bit) == 0 && mb_field(mb, value_start, value_len) != 0
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

/// BDS 1,0 — Data Link Capability Report (ICAO Doc 9871 Table A-2-16 /
/// Annex 10 Vol IV §3.1.2.6.10.2): which Comm-A/Comm-B/ELM services and
/// ACAS features the transponder supports.
///
/// MB positions (1-indexed): BDS id 1–8 (= 0x10), config-flag 9, reserved
/// 10–14, overlay-command 15, ACAS-operational 16, subnetwork version
/// 17–23, transponder-level-5 24, Mode-S-specific-services 25, uplink ELM
/// 26–28, downlink ELM 29–32, aircraft-ident-capability 33, squitter 34,
/// surveillance-identifier-code 35, common-usage-GICB 36, ACAS-hybrid 37,
/// ACAS-resolution-advisory 38, ACAS-RTCA version 39–40, DTE status 41–56.
pub fn bds10(mb: &[u8]) -> Option<serde_json::Value> {
    // BDS identifier must be 0x10.
    if mb_field(mb, 1, 8) != 0x10 {
        return None;
    }
    // Reserved bits 10–14 must be zero.
    if mb_field(mb, 10, 5) != 0 {
        return None;
    }
    // pyModeS OVC/subnet consistency heuristic.
    let ovc = mb_bit(mb, 15);
    let subnet = mb_field(mb, 17, 7);
    if (ovc == 1 && subnet < 5) || (ovc == 0 && subnet > 4) {
        return None;
    }
    Some(serde_json::json!({
        "bds": "1,0",
        "config": mb_bit(mb, 9) == 1,
        "overlay_command_capability": ovc == 1,
        "acas_operational": mb_bit(mb, 16) == 1,
        "mode_s_subnetwork_version": subnet,
        "transponder_level5": mb_bit(mb, 24) == 1,
        "mode_s_specific_services": mb_bit(mb, 25) == 1,
        "uplink_elm_throughput": mb_field(mb, 26, 3),
        "downlink_elm_throughput": mb_field(mb, 29, 4),
        "aircraft_identification_capability": mb_bit(mb, 33) == 1,
        "squitter_capability": mb_bit(mb, 34) == 1,
        "surveillance_identifier_code": mb_bit(mb, 35) == 1,
        "common_usage_gicb_capability": mb_bit(mb, 36) == 1,
        "acas_hybrid_surveillance": mb_bit(mb, 37) == 1,
        "acas_resolution_advisory": mb_bit(mb, 38) == 1,
        "acas_rtca_version": mb_field(mb, 39, 2),
        "dte_status": mb_field(mb, 41, 16),
    }))
}

/// Capability-map index (MB bit 1..24) → BDS register (ICAO Doc 9871
/// Table A-2-25), as used by BDS 1,7.
const GICB_CAPABILITY_BDS: [&str; 24] = [
    "0,5", "0,6", "0,7", "0,8", "0,9", "0,A", "2,0", "2,1", "4,0", "4,1", "4,2", "4,3", "4,4",
    "4,5", "4,8", "5,0", "5,1", "5,2", "5,3", "5,4", "5,5", "5,6", "5,F", "6,0",
];

/// BDS 1,7 — Common Usage GICB Capability Report: a 24-bit map (MB 1–24)
/// of which common-usage registers the transponder will report via ground-
/// initiated Comm-B. MB 25–56 must be zero and the BDS 2,0 capability
/// (MB bit 7) must be set (pyModeS `bds17`).
pub fn bds17(mb: &[u8]) -> Option<serde_json::Value> {
    if !is_bds17_pattern(mb) {
        return None;
    }
    let supported: Vec<&str> = GICB_CAPABILITY_BDS
        .iter()
        .enumerate()
        .filter(|&(i, _)| mb_bit(mb, i + 1) == 1)
        .map(|(_, &c)| c)
        .collect();
    Some(serde_json::json!({ "bds": "1,7", "supported_bds": supported }))
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

/// Strict BDS 1,7 (GICB capability) pattern test: the BDS 2,0 capability
/// (MB bit 7) is mandatory and MB bits 25–56 (32 bits) must be zero
/// (pyModeS `bds17.is_bds17`). Used both by `bds17` and to disambiguate
/// the meteorological registers from a capability report.
fn is_bds17_pattern(mb: &[u8]) -> bool {
    if mb.iter().all(|&b| b == 0) {
        return false;
    }
    if mb_bit(mb, 7) == 0 {
        return false;
    }
    mb_field(mb, 25, 32) == 0
}

/// BDS 4,4 — Meteorological Routine Air Report (MRAR): wind, static air
/// temperature, pressure, turbulence, humidity at the aircraft's position
/// (ICAO Doc 9871 Table A-2-33). A pilot-optional, heuristic register —
/// like pyModeS it is kept out of the default `bds_infer` set (it collides
/// with the EHS registers) and offered via the meteo-aware path.
///
/// MB positions (1-indexed): FOM 1–4, wind status 5, wind speed 6–14 (kt),
/// wind direction 15–23 (raw·180/256°), temp sign 24, temp 25–34
/// (signed·0.25 °C), pressure status 35, pressure 36–46 (hPa), turbulence
/// status 47, turbulence 48–49, humidity status 50, humidity 51–56
/// (raw·100/64 %).
pub fn bds44(mb: &[u8]) -> Option<serde_json::Value> {
    if mb.iter().all(|&b| b == 0) {
        return None;
    }
    let fom = mb_field(mb, 1, 4);
    if fom > 4 {
        return None;
    }
    // Wind must be present (pyModeS heuristic).
    if mb_bit(mb, 5) == 0 {
        return None;
    }
    if wrong_status(mb, 35, 36, 11) || wrong_status(mb, 47, 48, 2) || wrong_status(mb, 50, 51, 6) {
        return None;
    }
    let wind_speed = mb_field(mb, 6, 9);
    if wind_speed > 250 {
        return None;
    }
    let wind_dir_raw = mb_field(mb, 15, 9);
    let temp_raw = mb_field(mb, 25, 10);
    let temp_c = sign_mag(temp_raw, 10, mb_bit(mb, 24)) as f64 * 0.25;
    if !(-80.0..=60.0).contains(&temp_c) {
        return None;
    }
    // Reject all-zero meteorological data.
    if wind_speed == 0 && wind_dir_raw == 0 && temp_raw == 0 {
        return None;
    }
    let mut o = serde_json::Map::new();
    o.insert("bds".into(), "4,4".into());
    o.insert("figure_of_merit".into(), serde_json::json!(fom));
    o.insert("wind_speed".into(), serde_json::json!(wind_speed));
    o.insert("wind_direction".into(), serde_json::json!(wind_dir_raw as f64 * 180.0 / 256.0));
    o.insert("static_air_temperature".into(), serde_json::json!(temp_c));
    if mb_bit(mb, 35) == 1 {
        o.insert("static_pressure".into(), serde_json::json!(mb_field(mb, 36, 11)));
    }
    if mb_bit(mb, 47) == 1 {
        o.insert("turbulence".into(), serde_json::json!(mb_field(mb, 48, 2)));
    }
    if mb_bit(mb, 50) == 1 {
        o.insert("humidity".into(), serde_json::json!(mb_field(mb, 51, 6) as f64 * 100.0 / 64.0));
    }
    Some(serde_json::Value::Object(o))
}

/// BDS 4,5 — Meteorological Hazard Report (MHR): turbulence, wind shear,
/// microburst, icing, wake-vortex hazard levels plus static air
/// temperature, pressure, and radio height (ICAO Doc 9871 Table A-2-32).
/// Heuristic register; offered via the meteo-aware path only.
///
/// MB positions (1-indexed): turbulence status 1 / level 2–3; wind shear
/// status 4 / level 5–6; microburst status 7 / level 8–9; icing status 10
/// / level 11–12; wake vortex status 13 / level 14–15; temp status 16 /
/// sign 17 / magnitude 18–26 (signed·0.25 °C); pressure status 27 /
/// pressure 28–38 (hPa); radio-height status 39 / height 40–51 (raw·16 ft);
/// reserved 52–56 (must be zero).
pub fn bds45(mb: &[u8]) -> Option<serde_json::Value> {
    if mb.iter().all(|&b| b == 0) {
        return None;
    }
    // Disambiguate from a BDS 1,7 capability report (pyModeS gate).
    if is_bds17_pattern(mb) {
        return None;
    }
    // Reserved bits 52–56 must be zero.
    if mb_field(mb, 52, 5) != 0 {
        return None;
    }
    let gates = [
        (1usize, 2usize, 2usize),  // turbulence
        (4, 5, 2),                 // wind shear
        (7, 8, 2),                 // microburst
        (10, 11, 2),               // icing
        (13, 14, 2),               // wake vortex
        (16, 17, 10),              // temperature (sign + 9-bit magnitude)
        (27, 28, 11),              // static pressure
        (39, 40, 12),              // radio height
    ];
    for (s, vs, vl) in gates {
        if wrong_status(mb, s, vs, vl) {
            return None;
        }
    }
    // Temperature range check (only when its status bit is set).
    if mb_bit(mb, 16) == 1 {
        let temp_c = sign_mag(mb_field(mb, 18, 9), 9, mb_bit(mb, 17)) as f64 * 0.25;
        if !(-80.0..=60.0).contains(&temp_c) {
            return None;
        }
    }
    let mut o = serde_json::Map::new();
    o.insert("bds".into(), "4,5".into());
    if mb_bit(mb, 1) == 1 {
        o.insert("turbulence".into(), serde_json::json!(mb_field(mb, 2, 2)));
    }
    if mb_bit(mb, 4) == 1 {
        o.insert("wind_shear".into(), serde_json::json!(mb_field(mb, 5, 2)));
    }
    if mb_bit(mb, 7) == 1 {
        o.insert("microburst".into(), serde_json::json!(mb_field(mb, 8, 2)));
    }
    if mb_bit(mb, 10) == 1 {
        o.insert("icing".into(), serde_json::json!(mb_field(mb, 11, 2)));
    }
    if mb_bit(mb, 13) == 1 {
        o.insert("wake_vortex".into(), serde_json::json!(mb_field(mb, 14, 2)));
    }
    if mb_bit(mb, 16) == 1 {
        let temp_c = sign_mag(mb_field(mb, 18, 9), 9, mb_bit(mb, 17)) as f64 * 0.25;
        o.insert("static_air_temperature".into(), serde_json::json!(temp_c));
    }
    if mb_bit(mb, 27) == 1 {
        o.insert("static_pressure".into(), serde_json::json!(mb_field(mb, 28, 11)));
    }
    if mb_bit(mb, 39) == 1 {
        o.insert("radio_height".into(), serde_json::json!(mb_field(mb, 40, 12) * 16));
    }
    Some(serde_json::Value::Object(o))
}

/// Infer the BDS register of a DF20/21 MB field, mirroring the phased
/// precedence of pyModeS `_infer.py`:
///
/// 1. Format-ID fast path (BDS 1,0 / 1,7 / 2,0 / 3,0): these carry an
///    explicit identifier byte (or, for 1,7, a strict capability-map
///    pattern) and are mutually exclusive, so the first that validates
///    wins outright — the heuristic registers are not even consulted.
/// 2. Heuristic set (EHS: BDS 4,0 / 5,0 / 6,0): only when no format-ID
///    register matched, accept only if exactly one validates (xng's
///    original exactly-one rule, preserved unchanged).
/// 3. Meteorological fallback (BDS 4,4 MRAR / 4,5 MHR): only when the
///    heuristic set is empty — these collide with EHS, so pyModeS hides
///    them behind `include_meteo`; here they are a last resort that never
///    perturbs ELS/EHS decoding.
pub fn bds_infer(mb: &[u8]) -> Option<serde_json::Value> {
    // Phase 1 — format-ID fast path, first match wins.
    if let Some(v) = [bds10(mb), bds17(mb), bds20(mb), bds30(mb)].into_iter().flatten().next() {
        return Some(v);
    }
    // Phase 2 — heuristic EHS set, exactly one must validate.
    let ehs: Vec<serde_json::Value> =
        [bds40(mb), bds50(mb), bds60(mb)].into_iter().flatten().collect();
    if ehs.len() == 1 {
        return Some(ehs.into_iter().next().unwrap());
    }
    if !ehs.is_empty() {
        return None; // ambiguous within the EHS set
    }
    // Phase 3 — meteorological fallback, exactly one must validate.
    let met: Vec<serde_json::Value> = [bds44(mb), bds45(mb)].into_iter().flatten().collect();
    (met.len() == 1).then(|| met.into_iter().next().unwrap())
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

    /// Inverse of `mb_payload`: a 7-byte MB → the 56-bit payload integer.
    fn payload56(mb: &[u8]) -> u64 {
        mb.iter().fold(0u64, |v, &b| (v << 8) | b as u64)
    }

    // Oracle: pyModeS v3 decode() on these frames (2026-06-11).

    #[test]
    fn bds20_callsign_matches_pymodes() {
        let v = bds_infer(&mb_of("A000083E202CC371C31DE0AA1CCF")).unwrap();
        assert_eq!(v["bds"], "2,0");
        assert_eq!(v["callsign"], "KLM1017");
    }

    // Oracle: pyModeS bds10 / test_bds_commb TestBds10* golden frame
    // (A800178D10010080F50000D5893C, full expected field dict).

    #[test]
    fn bds10_full_field_decode_matches_pymodes() {
        let v = bds_infer(&mb_of("A800178D10010080F50000D5893C")).unwrap();
        assert_eq!(v["bds"], "1,0");
        assert_eq!(v["config"], false);
        assert_eq!(v["overlay_command_capability"], false);
        assert_eq!(v["acas_operational"], true);
        assert_eq!(v["mode_s_subnetwork_version"], 0);
        assert_eq!(v["transponder_level5"], false);
        assert_eq!(v["mode_s_specific_services"], true);
        assert_eq!(v["uplink_elm_throughput"], 0);
        assert_eq!(v["downlink_elm_throughput"], 0);
        assert_eq!(v["aircraft_identification_capability"], true);
        assert_eq!(v["squitter_capability"], true);
        assert_eq!(v["surveillance_identifier_code"], true);
        assert_eq!(v["common_usage_gicb_capability"], true);
        assert_eq!(v["acas_hybrid_surveillance"], false);
        assert_eq!(v["acas_resolution_advisory"], true);
        assert_eq!(v["acas_rtca_version"], 1);
        assert_eq!(v["dte_status"], 0);
    }

    #[test]
    fn bds10_validity_gates_match_pymodes() {
        assert!(bds10(&mb_payload(0)).is_none());
        // Wrong BDS id (0x20).
        assert!(bds10(&mb_of("A0001838201584F23468207CDFA5")).is_none());
        // Reserved bits 10–14 nonzero (set bit 9, 0-indexed → MB 10).
        let golden = payload56(&mb_of("A800178D10010080F50000D5893C"));
        assert!(bds10(&mb_payload(golden | (1 << (55 - 9)))).is_none());
    }

    // Oracle: pyModeS bds17 / test_bds_commb TestBds17* golden frame
    // (A0000638FA81C10000000081A92F, full capability list).

    #[test]
    fn bds17_capability_list_matches_pymodes() {
        let v = bds_infer(&mb_of("A0000638FA81C10000000081A92F")).unwrap();
        assert_eq!(v["bds"], "1,7");
        let expected = ["0,5", "0,6", "0,7", "0,8", "0,9", "2,0", "4,0", "5,0", "5,1", "5,2", "6,0"];
        let got: Vec<&str> =
            v["supported_bds"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn bds17_validity_gates_match_pymodes() {
        let golden = payload56(&mb_of("A0000638FA81C10000000081A92F"));
        assert!(bds17(&mb_payload(golden)).is_some());
        assert!(bds17(&mb_payload(0)).is_none());
        // Clear the mandatory BDS 2,0 capability (MB bit 7).
        assert!(bds17(&mb_payload(golden & !(1 << (55 - 6)))).is_none());
        // Trailing bits 25–56 nonzero (set bit 24, 0-indexed → MB 25).
        assert!(bds17(&mb_payload(golden | (1 << (55 - 24)))).is_none());
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

    // Oracle: pyModeS bds44 / test_bds_commb TestBds44* (golden frame
    // A0001692185BD5CF400000DFC696 + synthetic multi-field payloads).

    #[test]
    fn bds44_golden_vector_matches_pymodes() {
        let v = bds44(&mb_of("A0001692185BD5CF400000DFC696")).unwrap();
        assert_eq!(v["bds"], "4,4");
        assert_eq!(v["figure_of_merit"], 1);
        assert_eq!(v["wind_speed"], 22);
        assert!((v["wind_direction"].as_f64().unwrap() - 344.5).abs() < 0.5);
        assert!((v["static_air_temperature"].as_f64().unwrap() - (-48.75)).abs() < 0.1);
        assert!(v.get("static_pressure").is_none());
        assert!(v.get("humidity").is_none());
    }

    #[test]
    fn bds44_multi_field_matches_pymodes() {
        // test_multi_field_decode: pressure/turbulence/humidity branches.
        let payload = (1u64 << (55 - 3))    // FOM = 1
            | (1 << (55 - 4))               // wind status
            | (50 << (55 - 13))             // wind speed 50 kt
            | (256 << (55 - 22))            // wind dir raw 256 → 180.0°
            | (1 << (55 - 34))              // pressure status
            | (1013 << (55 - 45))           // pressure 1013 hPa
            | (1 << (55 - 46))              // turbulence status
            | (0b10 << (55 - 48))           // turbulence level 2
            | (1 << (55 - 49))              // humidity status
            | (32 << (55 - 55)); // humidity raw 32 → 50.0%
        let v = bds44(&mb_payload(payload)).unwrap();
        assert_eq!(v["wind_speed"], 50);
        assert!((v["wind_direction"].as_f64().unwrap() - 180.0).abs() < 0.01);
        assert_eq!(v["static_pressure"], 1013);
        assert_eq!(v["turbulence"], 2);
        assert!((v["humidity"].as_f64().unwrap() - 50.0).abs() < 0.01);
    }

    #[test]
    fn bds44_validity_gates_match_pymodes() {
        // FOM > 4, wind speed > 250, |temp| out of range, all-zero meteo.
        assert!(bds44(&mb_payload(0)).is_none());
        let base = (1u64 << (55 - 3)) | (1 << (55 - 4)) | (50 << (55 - 13)) | (100 << (55 - 33));
        assert!(bds44(&mb_payload(base)).is_some());
        // FOM = 5.
        assert!(bds44(&mb_payload((base & !(0xF << (55 - 3))) | (5 << (55 - 3)))).is_none());
        // Wind speed 251.
        assert!(bds44(&mb_payload((1 << (55 - 3)) | (1 << (55 - 4)) | (251 << (55 - 13)))).is_none());
        // Temperature +60.25 °C (raw 241).
        assert!(bds44(&mb_payload((1 << (55 - 3)) | (1 << (55 - 4)) | (50 << (55 - 13)) | (241 << (55 - 33)))).is_none());
        // All-zero meteo (wind/dir/temp all 0) with FOM+wind-status set.
        assert!(bds44(&mb_payload((1 << (55 - 3)) | (1 << (55 - 4)))).is_none());
        // Pressure status clear but raw nonzero.
        assert!(bds44(&mb_payload((1 << (55 - 3)) | (1 << (55 - 4)) | (50 << (55 - 13)) | (100 << (55 - 33)) | (1 << (55 - 45)))).is_none());
    }

    // Oracle: pyModeS bds45 / test_bds_commb TestBds45* (golden frame
    // A00004190001FB80000000000000 + synthetic multi-hazard payloads).

    #[test]
    fn bds45_golden_temperature_only_matches_pymodes() {
        let v = bds45(&mb_of("A00004190001FB80000000000000")).unwrap();
        assert_eq!(v["bds"], "4,5");
        assert!((v["static_air_temperature"].as_f64().unwrap() - (-4.5)).abs() < 0.1);
        for k in ["turbulence", "wind_shear", "microburst", "icing", "wake_vortex", "static_pressure", "radio_height"] {
            assert!(v.get(k).is_none(), "unexpected {k}");
        }
    }

    #[test]
    fn bds45_multi_hazard_matches_pymodes() {
        // test_multi_hazard_decode: the 5 hazard branches + pressure +
        // radio height (raw 500 → 8000 ft).
        let payload = (1u64 << (55 - 0))    // turbulence status
            | (0b10 << (55 - 2))            // turbulence 2
            | (1 << (55 - 3))               // wind shear status
            | (0b01 << (55 - 5))            // wind shear 1
            | (1 << (55 - 6))               // microburst status
            | (0b11 << (55 - 8))            // microburst 3
            | (1 << (55 - 9))               // icing status
            | (0b10 << (55 - 11))           // icing 2
            | (1 << (55 - 12))              // wake vortex status
            | (0b01 << (55 - 14))           // wake vortex 1
            | (1 << (55 - 26))              // pressure status
            | (1013 << (55 - 37))           // pressure 1013 hPa
            | (1 << (55 - 38))              // radio height status
            | (500 << (55 - 50)); // radio height raw 500 → 8000 ft
        let v = bds45(&mb_payload(payload)).unwrap();
        assert_eq!(v["turbulence"], 2);
        assert_eq!(v["wind_shear"], 1);
        assert_eq!(v["microburst"], 3);
        assert_eq!(v["icing"], 2);
        assert_eq!(v["wake_vortex"], 1);
        assert_eq!(v["static_pressure"], 1013);
        assert_eq!(v["radio_height"], 8000);
        assert!(v.get("static_air_temperature").is_none());
    }

    #[test]
    fn bds45_validity_gates_match_pymodes() {
        let golden = mb_of("A00004190001FB80000000000000");
        assert!(bds45(&golden).is_some());
        assert!(bds45(&mb_payload(0)).is_none());
        // Reserved tail nonzero → reject.
        let mut bad = golden.clone();
        *bad.last_mut().unwrap() |= 0x01;
        assert!(bds45(&bad).is_none());
        // Temperature +60.25 °C (status set, raw 241).
        assert!(bds45(&mb_payload((1 << (55 - 15)) | (241 << (55 - 25)))).is_none());
        // Turbulence status clear but level nonzero (wrong_status).
        assert!(bds45(&mb_payload(0b01 << (55 - 2))).is_none());
        // BDS 1,7-shaped payload → rejected by the disambiguation gate.
        assert!(bds45(&mb_payload(0xFF81C300000000)).is_none());
    }

    #[test]
    fn bds_infer_routes_meteo_when_strict_set_empty() {
        // The strict ELS/EHS set rejects these; the meteo fallback then
        // surfaces them. Confirms met reports reach comm_b unambiguously.
        let v = bds_infer(&mb_of("A0001692185BD5CF400000DFC696")).unwrap();
        assert_eq!(v["bds"], "4,4");
        let v = bds_infer(&mb_of("A00004190001FB80000000000000")).unwrap();
        assert_eq!(v["bds"], "4,5");
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
    fn df18_cf_classification_matches_readsb_dump1090() {
        // Reference: the DF18 CF switch in readsb / dump1090-fa mode_s.c
        // (identical mapping). source / addr_type / format-known.
        assert_eq!(df18_cf_class(0).0, "ADS-B");
        assert_eq!(df18_cf_class(0).1, "icao_nt");
        assert_eq!(df18_cf_class(1).0, "ADS-B");
        assert_eq!(df18_cf_class(1).1, "non_icao");
        assert_eq!(df18_cf_class(2).0, "TIS-B"); // fine TIS-B
        assert_eq!(df18_cf_class(3).0, "TIS-B"); // coarse TIS-B
        assert_eq!(df18_cf_class(5).0, "TIS-B"); // fine TIS-B non-ICAO
        assert_eq!(df18_cf_class(5).1, "tisb_non_icao");
        assert_eq!(df18_cf_class(6).0, "ADS-R"); // rebroadcast
        assert_eq!(df18_cf_class(6).1, "adsr_icao");
        // CF=4 and CF=7 are not assigned a format by either decoder.
        assert_eq!(df18_cf_class(4).0, "unknown");
        assert_eq!(df18_cf_class(7).0, "unknown");
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
