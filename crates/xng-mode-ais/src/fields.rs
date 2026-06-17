//! Field-level AIS message decoding (ITU-R M.1371-5): positions,
//! kinematics, static/voyage data, binary and safety messages.
//! Validated against pyais (MIT) as a decode oracle — see the vendored
//! vectors below.

use serde_json::{Value, json};

fn u(bits: &[u8], s: usize, n: usize) -> Option<u64> {
    if s + n > bits.len() {
        return None;
    }
    Some(bits[s..s + n].iter().fold(0u64, |v, &b| (v << 1) | b as u64))
}

fn i(bits: &[u8], s: usize, n: usize) -> Option<i64> {
    let v = u(bits, s, n)?;
    let sign = 1u64 << (n - 1);
    Some(if v & sign != 0 { v as i64 - (1i64 << n) } else { v as i64 })
}

/// 6-bit ASCII: 0..31 → '@'..'_', 32..63 → ' '..'?'. Trims '@' padding.
fn sixbit(bits: &[u8], s: usize, chars: usize) -> Option<String> {
    let mut out = String::new();
    for k in 0..chars {
        let v = u(bits, s + 6 * k, 6)? as u8;
        out.push(if v < 32 { (v + 64) as char } else { v as char });
    }
    Some(out.trim_end_matches(['@', ' ']).to_string())
}

fn position(bits: &[u8], lon_start: usize) -> Option<(f64, f64)> {
    let lon = i(bits, lon_start, 28)? as f64 / 600_000.0;
    let lat = i(bits, lon_start + 28, 27)? as f64 / 600_000.0;
    if lon.abs() > 180.0 || lat.abs() > 90.0 {
        return None; // 181/91 = not available
    }
    Some((lat, lon))
}

fn sog(bits: &[u8], s: usize) -> Option<f64> {
    match u(bits, s, 10)? {
        1023 => None,
        v => Some(v as f64 / 10.0),
    }
}

fn cog(bits: &[u8], s: usize) -> Option<f64> {
    match u(bits, s, 12)? {
        3600.. => None,
        v => Some(v as f64 / 10.0),
    }
}

fn heading(bits: &[u8], s: usize) -> Option<u64> {
    match u(bits, s, 9)? {
        511 => None,
        v => Some(v),
    }
}

/// Class-A Rate of Turn (ITU-R M.1371, signed 8-bit ROTais): the magnitude
/// is `(raw / 4.733)²` deg/min, carrying the sign of `raw`; `-128` (0x80) is
/// "not available". Reported to 0.1 deg/min. (±127 encode "turning faster
/// than 5°/30 s with no turn indicator"; the formula yields ~720 there.)
fn rot_deg_min(bits: &[u8], s: usize) -> Option<f64> {
    match i(bits, s, 8)? {
        -128 => None,
        raw => {
            let m = (raw as f64 / 4.733).powi(2);
            let signed = if raw < 0 { -m } else { m };
            Some((signed * 10.0).round() / 10.0)
        }
    }
}

fn data_hex(bits: &[u8], s: usize) -> String {
    bits[s..]
        .chunks(8)
        .map(|c| format!("{:02x}", c.iter().fold(0u8, |v, &b| (v << 1) | b)))
        .collect()
}

/// Application-specific message (ASM) decode for the binary payload of a
/// type-8 (broadcast) or type-6 (addressed) message, dispatched by DAC/FID.
/// `p` is the bit offset where the application data begins (after the DAC/FID
/// header): bit 56 for type 8, bit 88 for type 6. Returns the decoded
/// application fields when the DAC/FID is recognised and the payload is long
/// enough; otherwise `None` so the caller falls back to `data_hex`.
///
/// DAC=200 (Inland AIS, UNECE ECE/TRANS/SC.3/176) subtypes are verified
/// against the pyais oracle. Field layouts and conventions (including the
/// re-use of the 1/600000-degree lat/lon scaling for the EMMA/signal-strength
/// coordinates) follow pyais so the emitted values match the oracle exactly.
fn asm_decode(dac: u64, fid: u64, bits: &[u8], p: usize) -> Option<Value> {
    let mut d = serde_json::Map::new();
    let mut put = |k: &str, v: Value| {
        d.insert(k.into(), v);
    };
    match (dac, fid) {
        // Inland ship static & voyage data (UNECE SC.3/176, FID 10).
        (200, 10) => {
            put("inland_vin", json!(sixbit(bits, p, 8)?));
            put("inland_length_m", json!(u(bits, p + 48, 13)? as f64 / 10.0));
            put("inland_beam_m", json!(u(bits, p + 61, 10)? as f64 / 10.0));
            put("inland_ship_type", json!(u(bits, p + 71, 14)?));
            put("inland_hazard", json!(u(bits, p + 85, 3)?));
            put("inland_draught_m", json!(u(bits, p + 88, 11)? as f64 / 100.0));
            put("inland_loaded", json!(u(bits, p + 99, 2)?));
        }
        // EMMA warning report (FID 23).
        (200, 23) => {
            // 1/600000-degree scaling, matching pyais.
            let ll = |s: usize, n: usize| -> Option<f64> {
                Some(i(bits, s, n)? as f64 / 600_000.0)
            };
            put("start_year", json!(u(bits, p, 8)?));
            put("start_month", json!(u(bits, p + 8, 4)?));
            put("start_day", json!(u(bits, p + 12, 5)?));
            put("end_year", json!(u(bits, p + 17, 8)?));
            put("end_month", json!(u(bits, p + 25, 4)?));
            put("end_day", json!(u(bits, p + 29, 5)?));
            put("start_hour", json!(u(bits, p + 34, 5)?));
            put("start_minute", json!(u(bits, p + 39, 6)?));
            put("end_hour", json!(u(bits, p + 45, 5)?));
            put("end_minute", json!(u(bits, p + 50, 6)?));
            put("start_lon", json!(ll(p + 56, 28)?));
            put("start_lat", json!(ll(p + 84, 27)?));
            put("end_lon", json!(ll(p + 111, 28)?));
            put("end_lat", json!(ll(p + 139, 27)?));
            put("emma_type", json!(u(bits, p + 166, 4)?));
            put("emma_min", json!(i(bits, p + 170, 9)?));
            put("emma_max", json!(i(bits, p + 179, 9)?));
            put("emma_intensity", json!(u(bits, p + 188, 2)?));
            put("emma_wind", json!(u(bits, p + 190, 4)?));
        }
        // Water-level report (FID 24): 4 × (gauge id, water level).
        (200, 24) => {
            // 12-bit country code (2 × 6-bit ASCII).
            let country = sixbit(bits, p, 2)?;
            if !country.is_empty() {
                put("inland_country", json!(country));
            }
            let mut gauges = Vec::new();
            for k in 0..4 {
                let s = p + 12 + k * 25;
                let id = u(bits, s, 11)?;
                let level = i(bits, s + 11, 14)?;
                gauges.push(json!({ "gauge_id": id, "water_level": level }));
            }
            put("water_gauges", json!(gauges));
        }
        // Signal-strength / bridge-status report (FID 40).
        (200, 40) => {
            let ll = |s: usize, n: usize| -> Option<f64> {
                Some(i(bits, s, n)? as f64 / 600_000.0)
            };
            put("lon", json!(ll(p, 28)?));
            put("lat", json!(ll(p + 28, 27)?));
            put("signal_form", json!(u(bits, p + 55, 4)?));
            put("signal_facing", json!(u(bits, p + 59, 9)?));
            put("signal_direction", json!(u(bits, p + 68, 3)?));
            put("signal_status_raw", json!(u(bits, p + 71, 30)?));
        }
        _ => return None,
    }
    if d.is_empty() { None } else { Some(Value::Object(d)) }
}

const NAV_STATUS: [&str; 16] = [
    "under way (engine)",
    "at anchor",
    "not under command",
    "restricted manoeuvrability",
    "constrained by draught",
    "moored",
    "aground",
    "engaged in fishing",
    "under way (sailing)",
    "reserved (HSC)",
    "reserved (WIG)",
    "reserved",
    "reserved",
    "reserved",
    "AIS-SART",
    "undefined",
];

/// EPFD (electronic position-fixing device) type names by code.
fn epfd_name(code: u64) -> &'static str {
    const EPFD: [&str; 9] = [
        "undefined", "GPS", "GLONASS", "GPS+GLONASS", "Loran-C", "Chayka",
        "integrated", "surveyed", "Galileo",
    ];
    match code {
        15 => "internal GNSS",
        c => EPFD.get(c as usize).copied().unwrap_or("undefined"),
    }
}

/// Classify a distress/safety transmitter by its MMSI prefix (ITU-R M.1371 /
/// the MID allocation for device MMSIs): 970 = AIS-SART (search & rescue
/// transmitter), 972 = AIS-MOB (man-overboard), 974 = EPIRB-AIS. These
/// devices send ordinary AIS messages; the prefix is what marks them as a
/// distress class. `None` for a normal MMSI.
pub fn distress_class(mmsi: u32) -> Option<&'static str> {
    match mmsi / 1_000_000 {
        970 => Some("AIS-SART"),
        972 => Some("AIS-MOB"),
        974 => Some("EPIRB-AIS"),
        _ => None,
    }
}

/// Decode the fields of an AIS message; `None` when the type is not
/// (yet) field-decoded. Positions in degrees, speeds in knots.
pub fn decode(msg_type: u8, bits: &[u8]) -> Option<Value> {
    let mut d = serde_json::Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            d.insert(k.into(), v);
        }
    };
    match msg_type {
        // Class A position reports.
        1..=3 => {
            let status = u(bits, 38, 4)? as usize;
            put("nav_status", json!(NAV_STATUS[status]));
            if let Some(rot) = rot_deg_min(bits, 42) {
                put("rot_deg_min", json!(rot));
            }
            put("sog_kt", json!(sog(bits, 50)));
            put("position_accuracy", json!(u(bits, 60, 1)? == 1));
            if let Some((lat, lon)) = position(bits, 61) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            put("cog_deg", json!(cog(bits, 116)));
            put("heading_deg", json!(heading(bits, 128)));
            if let Some(ts) = u(bits, 137, 6) {
                if ts < 60 {
                    put("timestamp_sec", json!(ts));
                }
            }
            // Maneuver indicator: 0 = not available, 1 = no special, 2 = special.
            if let Some(m) = u(bits, 143, 2) {
                if m != 0 {
                    put("maneuver", json!(m));
                }
            }
            put("raim", json!(u(bits, 148, 1)? == 1));
        }
        // Base station report (4) / UTC-and-date response (11) — same shape.
        4 | 11 => {
            if let (Some(y), Some(mo), Some(da), Some(h), Some(mi), Some(s)) = (
                u(bits, 38, 14),
                u(bits, 52, 4),
                u(bits, 56, 5),
                u(bits, 61, 5),
                u(bits, 66, 6),
                u(bits, 72, 6),
            ) {
                if y > 0 {
                    put("utc", json!(format!("{y:04}-{mo:02}-{da:02}T{h:02}:{mi:02}:{s:02}Z")));
                }
            }
            put("position_accuracy", json!(u(bits, 78, 1)? == 1));
            if let Some((lat, lon)) = position(bits, 79) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            put("epfd", json!(epfd_name(u(bits, 134, 4)?)));
            put("raim", json!(u(bits, 148, 1)? == 1));
        }
        // Static and voyage data.
        5 => {
            put("ais_version", json!(u(bits, 38, 2)));
            put("imo", json!(u(bits, 40, 30)));
            put("callsign", json!(sixbit(bits, 70, 7)));
            put("name", json!(sixbit(bits, 112, 20)));
            put("ship_type", json!(u(bits, 232, 8)));
            put("to_bow", json!(u(bits, 240, 9)));
            put("to_stern", json!(u(bits, 249, 9)));
            put("to_port", json!(u(bits, 258, 6)));
            put("to_starboard", json!(u(bits, 264, 6)));
            put("epfd", json!(epfd_name(u(bits, 270, 4)?)));
            put("draught_m", json!(u(bits, 294, 8)? as f64 / 10.0));
            put("destination", json!(sixbit(bits, 302, 20)));
            // ETA (recurring, year-less): month 0 = not available.
            if let Some(mo) = u(bits, 274, 4) {
                if mo != 0 {
                    let (da, h, mi) = (u(bits, 278, 5)?, u(bits, 283, 5)?, u(bits, 288, 6)?);
                    put("eta", json!(format!("{mo:02}-{da:02}T{h:02}:{mi:02}")));
                }
            }
            // DTE: bit 0 = data terminal ready.
            put("dte_ready", json!(u(bits, 422, 1)? == 0));
        }
        // Addressed binary message. Application data starts at bit 88.
        6 => {
            put("seqno", json!(u(bits, 38, 2)));
            put("dest_mmsi", json!(u(bits, 40, 30)));
            put("retransmit", json!(u(bits, 70, 1)? == 1));
            let dac = u(bits, 72, 10)?;
            let fid = u(bits, 82, 6)?;
            put("dac", json!(dac));
            put("fid", json!(fid));
            match asm_decode(dac, fid, bits, 88) {
                Some(Value::Object(app)) => put("app", json!(app)),
                _ if bits.len() > 88 => put("data_hex", json!(data_hex(bits, 88))),
                _ => {}
            }
        }
        // Binary / safety acknowledgements.
        7 | 13 => {
            put("dest_mmsi", json!(u(bits, 40, 30)));
        }
        // Broadcast binary message. Application data starts at bit 56.
        8 => {
            let dac = u(bits, 40, 10)?;
            let fid = u(bits, 50, 6)?;
            put("dac", json!(dac));
            put("fid", json!(fid));
            match asm_decode(dac, fid, bits, 56) {
                Some(Value::Object(app)) => put("app", json!(app)),
                _ if bits.len() > 56 => put("data_hex", json!(data_hex(bits, 56))),
                _ => {}
            }
        }
        // SAR aircraft.
        9 => {
            match u(bits, 38, 12)? {
                4095 => {}
                alt => put("altitude_m", json!(alt)),
            }
            put("sog_kt", json!(sog(bits, 50)));
            if let Some((lat, lon)) = position(bits, 61) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
        }
        // Addressed safety-related text.
        12 => {
            put("seqno", json!(u(bits, 38, 2)));
            put("dest_mmsi", json!(u(bits, 40, 30)));
            put("retransmit", json!(u(bits, 70, 1)? == 1));
            put("text", json!(sixbit(bits, 72, (bits.len().saturating_sub(72)) / 6)));
        }
        // Broadcast safety-related text.
        14 => {
            put("text", json!(sixbit(bits, 40, (bits.len().saturating_sub(40)) / 6)));
        }
        // Class B position reports (19 adds name/type/dimensions).
        18 | 19 => {
            put("sog_kt", json!(sog(bits, 46)));
            put("position_accuracy", json!(u(bits, 56, 1)? == 1));
            if let Some((lat, lon)) = position(bits, 57) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            put("cog_deg", json!(cog(bits, 112)));
            put("heading_deg", json!(heading(bits, 124)));
            if let Some(ts) = u(bits, 133, 6) {
                if ts < 60 {
                    put("timestamp_sec", json!(ts));
                }
            }
            if msg_type == 18 {
                put("raim", json!(u(bits, 147, 1)? == 1));
            } else {
                // Type 19 extended: identity + dimensions + EPFD.
                put("name", json!(sixbit(bits, 143, 20)));
                put("ship_type", json!(u(bits, 263, 8)));
                put("to_bow", json!(u(bits, 271, 9)));
                put("to_stern", json!(u(bits, 280, 9)));
                put("to_port", json!(u(bits, 289, 6)));
                put("to_starboard", json!(u(bits, 295, 6)));
                put("epfd", json!(epfd_name(u(bits, 301, 4)?)));
                put("raim", json!(u(bits, 305, 1)? == 1));
                put("dte_ready", json!(u(bits, 306, 1)? == 0));
            }
        }
        // Aids to navigation.
        21 => {
            put("aton_type", json!(u(bits, 38, 5)));
            put("name", json!(sixbit(bits, 43, 20)));
            put("position_accuracy", json!(u(bits, 163, 1)? == 1));
            if let Some((lat, lon)) = position(bits, 164) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            put("to_bow", json!(u(bits, 219, 9)));
            put("to_stern", json!(u(bits, 228, 9)));
            put("to_port", json!(u(bits, 237, 6)));
            put("to_starboard", json!(u(bits, 243, 6)));
            put("epfd", json!(epfd_name(u(bits, 249, 4)?)));
            if let Some(ts) = u(bits, 253, 6) {
                if ts < 60 {
                    put("timestamp_sec", json!(ts));
                }
            }
            put("off_position", json!(u(bits, 259, 1)? == 1));
            put("raim", json!(u(bits, 268, 1)? == 1));
            put("virtual_aid", json!(u(bits, 269, 1)? == 1));
            // Optional name extension (6-bit ASCII beyond the 272-bit base).
            if bits.len() > 272 {
                if let Some(ext) = sixbit(bits, 272, (bits.len() - 272) / 6) {
                    if !ext.is_empty() {
                        put("name_ext", json!(ext));
                    }
                }
            }
        }
        // Static data report (part A: name; part B: type/vendor/dims).
        24 => match u(bits, 38, 2)? {
            0 => put("name", json!(sixbit(bits, 40, 20))),
            1 => {
                put("ship_type", json!(u(bits, 40, 8)));
                put("vendor_id", json!(sixbit(bits, 48, 3)));
                put("model", json!(u(bits, 66, 4)));
                put("serial", json!(u(bits, 70, 20)));
                put("callsign", json!(sixbit(bits, 90, 7)));
                // Auxiliary craft (MMSI 98x) carry a mothership MMSI in place
                // of dimensions.
                if (980..=989).contains(&(u(bits, 8, 30)? / 1_000_000)) {
                    put("mothership_mmsi", json!(u(bits, 132, 30)));
                } else {
                    put("to_bow", json!(u(bits, 132, 9)));
                    put("to_stern", json!(u(bits, 141, 9)));
                    put("to_port", json!(u(bits, 150, 6)));
                    put("to_starboard", json!(u(bits, 156, 6)));
                }
            }
            _ => return None,
        },
        // DGNSS broadcast binary message.
        17 => {
            // Raw field is in 1/10-minute units; reported /10 to match
            // the pyais oracle convention.
            let lon = i(bits, 40, 18)? as f64 / 10.0;
            let lat = i(bits, 58, 17)? as f64 / 10.0;
            {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            if bits.len() > 80 {
                put("data_hex", json!(data_hex(bits, 80)));
            }
        }
        // Data link management: up to four slot-reservation blocks.
        20 => {
            let mut blocks = Vec::new();
            for k in 0..4 {
                let s0 = 40 + k * 30;
                let (Some(offset), Some(number), Some(timeout), Some(increment)) = (
                    u(bits, s0, 12),
                    u(bits, s0 + 12, 4),
                    u(bits, s0 + 16, 3),
                    u(bits, s0 + 19, 11),
                ) else {
                    break;
                };
                if offset == 0 && number == 0 {
                    continue;
                }
                blocks.push(json!({
                    "offset": offset,
                    "slots": number,
                    "timeout_min": timeout,
                    "increment": increment,
                }));
            }
            if !blocks.is_empty() {
                put("reservations", json!(blocks));
            }
        }
        // Channel management (regional channel assignment).
        22 => {
            put("channel_a", json!(u(bits, 40, 12)));
            put("channel_b", json!(u(bits, 52, 12)));
            put("txrx", json!(u(bits, 64, 4)));
            put("high_power", json!(u(bits, 68, 1)? == 1));
            let addressed = u(bits, 139, 1)? == 1;
            put("addressed", json!(addressed));
            if !addressed {
                // Region corners in 1/10-minute units.
                put("ne_lon", json!(i(bits, 69, 18)? as f64 / 10.0));
                put("ne_lat", json!(i(bits, 87, 17)? as f64 / 10.0));
                put("sw_lon", json!(i(bits, 104, 18)? as f64 / 10.0));
                put("sw_lat", json!(i(bits, 122, 17)? as f64 / 10.0));
            }
            put("band_a", json!(u(bits, 140, 1)? == 1));
            put("band_b", json!(u(bits, 141, 1)? == 1));
            put("zone_size", json!(u(bits, 142, 3)));
        }
        // Group assignment command.
        23 => {
            put("ne_lon", json!(i(bits, 40, 18)? as f64 / 10.0));
            put("ne_lat", json!(i(bits, 58, 17)? as f64 / 10.0));
            put("sw_lon", json!(i(bits, 75, 18)? as f64 / 10.0));
            put("sw_lat", json!(i(bits, 93, 17)? as f64 / 10.0));
            put("station_type", json!(u(bits, 110, 4)));
            put("ship_type", json!(u(bits, 114, 8)));
            put("txrx", json!(u(bits, 144, 2)));
            put("interval", json!(u(bits, 146, 4)));
            put("quiet_min", json!(u(bits, 150, 4)));
        }
        // Long-range position report.
        27 => {
            let lon = i(bits, 44, 18)? as f64 / 600.0;
            let lat = i(bits, 62, 17)? as f64 / 600.0;
            if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            match u(bits, 79, 6)? {
                63 => {}
                v => put("sog_kt", json!(v)),
            }
        }
        _ => return None,
    }
    if d.is_empty() { None } else { Some(Value::Object(d)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AIVDM armored payload → bit vector.
    pub(super) fn bits_of(payload: &str, fill: usize) -> Vec<u8> {
        let mut bits = Vec::new();
        for c in payload.bytes() {
            let mut v = c as i32 - 48;
            if v > 40 {
                v -= 8;
            }
            for k in (0..6).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        }
        bits.truncate(bits.len() - fill);
        bits
    }

    fn typed(payload: &str, fill: usize) -> (u8, Vec<u8>) {
        let bits = bits_of(payload, fill);
        let t = bits[..6].iter().fold(0u8, |v, &b| (v << 1) | b);
        (t, bits)
    }

    // Oracle: pyais 2.x decode of the same sentences (2026-06-10/11).

    #[test]
    fn dgnss_t17_matches_pyais() {
        let (t, bits) = typed("A02R5Ph0E81:p7h5Ed1h=h", 4);
        assert_eq!(t, 17);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["lon"], 33.8);
        assert_eq!(d["lat"], 59.9);
        assert_eq!(d["data_hex"], "7c0556c07037");
    }

    #[test]
    fn link_mgmt_t20_matches_pyais() {
        let (t, bits) = typed("D028rqP2tN?b<`I6D0000000000", 2);
        assert_eq!(t, 20);
        let d = decode(t, &bits).unwrap();
        let r = &d["reservations"];
        assert_eq!(r[0]["offset"], 47);
        assert_eq!(r[0]["slots"], 1);
        assert_eq!(r[0]["timeout_min"], 7);
        assert_eq!(r[0]["increment"], 250);
        assert_eq!(r[1]["offset"], 2250);
        assert_eq!(r[1]["increment"], 1125);
        assert!(r.get(2).is_none());
    }

    #[test]
    fn channel_mgmt_t22_matches_pyais() {
        let (t, bits) = typed("F030p8B2N2PMaJR0r;6f3rj20000", 0);
        assert_eq!(t, 22);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["channel_a"], 2087);
        assert_eq!(d["channel_b"], 2088);
        assert_eq!(d["txrx"], 1);
        assert_eq!(d["high_power"], true);
        assert_eq!(d["ne_lon"], -7710.0);
        assert_eq!(d["ne_lat"], 3300.0);
        assert_eq!(d["sw_lon"], -8020.0);
        assert_eq!(d["sw_lat"], 3210.0);
        assert_eq!(d["addressed"], false);
        assert_eq!(d["zone_size"], 4);
    }

    #[test]
    fn group_assign_t23_matches_pyais() {
        let (t, bits) = typed("G02:Kn01R`sn@291nj600000900", 2);
        assert_eq!(t, 23);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["ne_lon"], 157.8);
        assert_eq!(d["ne_lat"], 3064.2);
        assert_eq!(d["sw_lon"], 109.6);
        assert_eq!(d["sw_lat"], 3040.8);
        assert_eq!(d["station_type"], 6);
        assert_eq!(d["ship_type"], 0);
        assert_eq!(d["txrx"], 0);
        assert_eq!(d["interval"], 9);
        assert_eq!(d["quiet_min"], 0);
    }

    #[test]
    fn class_a_position_matches_pyais() {
        let (t, bits) = typed("177KQJ5000G?tO`K>RA1wUbN0TKH", 0);
        assert_eq!(t, 1);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["nav_status"], "moored");
        assert!((d["lat"].as_f64().unwrap() - 47.582833).abs() < 1e-5);
        assert!((d["lon"].as_f64().unwrap() - -122.345833).abs() < 1e-5);
        assert_eq!(d["sog_kt"], 0.0);
        assert_eq!(d["cog_deg"], 51.0);
        assert_eq!(d["heading_deg"], 181);
        // AIS-3 added fields (hand-decoded from the same vector):
        assert_eq!(d["rot_deg_min"], 0.0); // not turning
        assert_eq!(d["position_accuracy"], false);
        assert_eq!(d["timestamp_sec"], 15);
        assert_eq!(d["raim"], false);
        assert!(d.get("maneuver").is_none()); // 0 = not available
    }

    #[test]
    fn rot_decode_helper() {
        // 8-bit signed ROTais → deg/min. raw 0 → 0; 18 → (18/4.733)² = 14.5;
        // -128 (0x80) → not available.
        let mk = |raw: i8| (0..8).map(|k| ((raw as u8 >> (7 - k)) & 1)).collect::<Vec<u8>>();
        assert_eq!(rot_deg_min(&mk(0), 0), Some(0.0));
        assert_eq!(rot_deg_min(&mk(18), 0), Some(14.5));
        assert_eq!(rot_deg_min(&mk(-18), 0), Some(-14.5));
        assert_eq!(rot_deg_min(&mk(-128), 0), None);
    }

    #[test]
    fn static_voyage_matches_pyais() {
        let payload = concat!(
            "55P5TL01VIaAL@7WKO@mBplU@<PDhh000000001S;AJ::4A80?4i@E53",
            "1@0000000000000"
        );
        let (t, bits) = typed(payload, 2);
        assert_eq!(t, 5);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["imo"], 6710932);
        assert_eq!(d["callsign"], "WDA9674");
        assert_eq!(d["name"], "MT.MITCHELL");
        assert_eq!(d["ship_type"], 99);
        assert_eq!(d["destination"], "SEATTLE");
        assert_eq!(d["draught_m"], 6.0);
        // AIS-3 type-5 fills (hand-decoded from the same vector):
        assert_eq!(d["ais_version"], 0);
        assert_eq!(d["to_bow"], 90);
        assert_eq!(d["to_stern"], 90);
        assert_eq!(d["to_port"], 10);
        assert_eq!(d["to_starboard"], 10);
        assert_eq!(d["epfd"], "GPS");
        assert_eq!(d["eta"], "01-02T08:00");
        assert_eq!(d["dte_ready"], true);
    }

    #[test]
    fn class_b_position_matches_pyais() {
        let (t, bits) = typed("B52K>;h00Fc>jpUlNV@ikwpUoP06", 0);
        assert_eq!(t, 18);
        let d = decode(t, &bits).unwrap();
        assert!((d["lat"].as_f64().unwrap() - 40.68454).abs() < 1e-5);
        assert!((d["lon"].as_f64().unwrap() - -74.072132).abs() < 1e-5);
        assert_eq!(d["sog_kt"], 0.1);
    }

    #[test]
    fn type8_binary_broadcast() {
        // DAC=1/FID=31 is not (yet) a verified ASM subtype → data_hex fallback.
        let bits = bits_of("83HOI:00Gh420h@", 2);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 1);
        assert_eq!(d["fid"], 31);
        assert_eq!(d["data_hex"], "01020304");
        assert!(d.get("app").is_none());
    }

    // AIS-1 ASM dispatch: DAC=200 (Inland AIS) subtypes. Vectors and asserted
    // field values are from the pyais test suite (test_msg_type_8_inland,
    // test_msg_type_8_inland_2, _dac_200_fid_23/24/40), decoded with pyais 3.1.

    #[test]
    fn type8_dac200_fid10_inland_static_matches_pyais() {
        // Norwegian public feed (pyais test_msg_type_8_inland): beam 7.5.
        let bits = bits_of("83m;Fa0j2d<<<<<<<0@pUg`50000", 0);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 10);
        let a = &d["app"];
        assert_eq!(a["inland_length_m"], 13.5);
        assert_eq!(a["inland_beam_m"], 7.5);
        assert_eq!(a["inland_ship_type"], 8000);
        assert_eq!(a["inland_hazard"], 5);
        assert_eq!(a["inland_draught_m"], 0.0);
        assert_eq!(a["inland_loaded"], 0);
    }

    #[test]
    fn type8_dac200_fid10_inland_static2_matches_pyais() {
        // pyais test_msg_type_8_inland_2: length 180.6, beam 42, loaded 0.
        let bits = bits_of("85M67F@j2U=7EW=RAkQkBDITMV=e", 0);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 10);
        let a = &d["app"];
        assert_eq!(a["inland_vin"], "T4]V\\6IG");
        assert_eq!(a["inland_length_m"], 180.6);
        assert_eq!(a["inland_beam_m"], 42.0);
        assert_eq!(a["inland_ship_type"], 10444);
        assert_eq!(a["inland_hazard"], 4);
        assert_eq!(a["inland_draught_m"], 9.47);
        assert_eq!(a["inland_loaded"], 0);
        assert!(d.get("data_hex").is_none());
    }

    #[test]
    fn type8_dac200_fid23_emma_matches_pyais() {
        let bits = bits_of("8007R@0j5iaG3BiLuO473qp2N=003=LL0k6wh?2Wf80", 2);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 23);
        let a = &d["app"];
        assert_eq!(a["start_year"], 26);
        assert_eq!(a["start_month"], 5);
        assert_eq!(a["start_day"], 14);
        assert_eq!(a["end_year"], 26);
        assert_eq!(a["end_month"], 5);
        assert_eq!(a["end_day"], 17);
        assert_eq!(a["start_hour"], 14);
        assert_eq!(a["start_minute"], 30);
        assert_eq!(a["end_hour"], 23);
        assert_eq!(a["end_minute"], 49);
        assert_eq!(a["start_lon"], 12.34);
        assert_eq!(a["start_lat"], 34.56);
        assert_eq!(a["end_lon"], 11.22);
        assert_eq!(a["end_lat"], 22.33);
        assert_eq!(a["emma_type"], 3);
        assert_eq!(a["emma_min"], -123);
        assert_eq!(a["emma_max"], 123);
        assert_eq!(a["emma_intensity"], 2);
        assert_eq!(a["emma_wind"], 2);
    }

    #[test]
    fn type8_dac200_fid24_water_level_matches_pyais() {
        let bits = bits_of("8007R@0j60006000Nh0bJGwewtW4", 0);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 24);
        let g = &d["app"]["water_gauges"];
        assert_eq!(g[0]["gauge_id"], 12);
        assert_eq!(g[0]["water_level"], 0);
        assert_eq!(g[1]["gauge_id"], 123);
        assert_eq!(g[1]["water_level"], 10);
        assert_eq!(g[2]["gauge_id"], 1234);
        assert_eq!(g[2]["water_level"], -10);
        assert_eq!(g[3]["gauge_id"], 2047);
        assert_eq!(g[3]["water_level"], 2500);
    }

    #[test]
    fn type8_dac200_fid40_signal_strength_matches_pyais() {
        let bits = bits_of("8007R@0j:1<RL0gfD21PD3cNIN00", 0);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 40);
        let a = &d["app"];
        assert_eq!(a["lon"], 33.44);
        assert_eq!(a["lat"], -56.89);
        assert_eq!(a["signal_form"], 3);
        assert_eq!(a["signal_facing"], 5);
        assert_eq!(a["signal_direction"], 0);
        assert_eq!(a["signal_status_raw"], 123456700);
    }

    #[test]
    fn type12_addressed_safety_text() {
        let bits = bits_of("<42Lati0W:Ot=C7P6B?=Pjoihhjhqq", 0);
        let d = decode(12, &bits).unwrap();
        assert_eq!(d["dest_mmsi"], 271002111);
        assert_eq!(d["retransmit"], false);
        assert_eq!(d["text"], "MSG FROM 271002099");
    }

    #[test]
    fn type14_broadcast_safety_text() {
        let bits = bits_of(">5?Per18=HB1U:1@E=B0m<L", 2);
        let d = decode(14, &bits).unwrap();
        assert_eq!(d["text"], "RCVD YR TEST MSG");
    }

    #[test]
    fn type6_addressed_binary() {
        let bits = bits_of("62?n;bQ:cbapaleEbP", 4);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dest_mmsi"], 313240222);
        assert_eq!(d["dac"], 669);
        assert_eq!(d["fid"], 11);
        assert_eq!(d["data_hex"], "55aa");
    }

    // AIS-3 tail: vectors + expected values from the pyais test suite.

    #[test]
    fn type4_base_station_matches_pyais() {
        let bits = bits_of("403OtVAv>lba;o?Ia`E`4G?02H6k", 0);
        let d = decode(4, &bits).unwrap();
        assert_eq!(d["position_accuracy"], true);
        assert_eq!(d["epfd"], "internal GNSS"); // EPFD code 15
        assert_eq!(d["utc"], "2019-11-09T10:41:11Z");
    }

    #[test]
    fn type18_class_b_flags_match_pyais() {
        let bits = bits_of("B5NJ;PP005l4ot5Isbl03wsUkP06", 0);
        let d = decode(18, &bits).unwrap();
        assert_eq!(d["position_accuracy"], false);
        assert_eq!(d["timestamp_sec"], 55);
        assert_eq!(d["raim"], false);
    }

    #[test]
    fn type19_extended_class_b_matches_pyais() {
        let bits = bits_of("C5N3SRgPEnJGEBT>NhWAwwo862PaLELTBJ:V00000000S0D:R220", 0);
        let d = decode(19, &bits).unwrap();
        assert_eq!(d["name"], "CAPT.J.RIMES");
        assert_eq!(d["ship_type"], 70);
        assert_eq!(d["to_bow"], 5);
        assert_eq!(d["to_stern"], 21);
        assert_eq!(d["to_port"], 4);
        assert_eq!(d["to_starboard"], 4);
        assert_eq!(d["epfd"], "GPS");
        assert_eq!(d["dte_ready"], true); // dte bit 0 = ready
        assert_eq!(d["position_accuracy"], false);
        assert_eq!(d["timestamp_sec"], 46);
    }

    #[test]
    fn type21_aton_matches_pyais() {
        // Two-fragment message: armored payloads concatenate, fill on the last.
        let bits = bits_of("E4eHJhPR37q0000000000000000KUOSc=rq4h00000a@20", 4);
        let d = decode(21, &bits).unwrap();
        assert_eq!(d["aton_type"], 1); // reference point
        assert_eq!(d["name"], "DFO2");
        assert_eq!(d["position_accuracy"], true);
        assert!((d["lat"].as_f64().unwrap() - 48.65457).abs() < 1e-5);
        assert!((d["lon"].as_f64().unwrap() - -123.429155).abs() < 1e-5);
        assert_eq!(d["to_bow"], 0);
        assert_eq!(d["off_position"], true);
        assert_eq!(d["raim"], true);
        assert_eq!(d["virtual_aid"], false);
        assert_eq!(d["epfd"], "GPS");
        assert!(d.get("name_ext").is_none());
    }

    #[test]
    fn type24b_regular_dimensions_match_pyais() {
        let bits = bits_of("H8=;nnT000000000000000Wg8Jb0", 0);
        let d = decode(24, &bits).unwrap();
        assert_eq!(d["to_bow"], 317);
        assert_eq!(d["to_stern"], 456);
        assert_eq!(d["to_port"], 26);
        assert_eq!(d["to_starboard"], 42);
        assert!(d.get("mothership_mmsi").is_none());
    }

    #[test]
    fn type24b_aux_craft_matches_pyais() {
        let bits = bits_of("H>W@vFTe6??406t2??21J0Wg8Jb0", 0);
        let d = decode(24, &bits).unwrap();
        assert_eq!(d["vendor_id"], "FOO");
        assert_eq!(d["model"], 1);
        assert_eq!(d["callsign"], "BOOBAZ");
        assert_eq!(d["mothership_mmsi"], 666666666);
        assert!(d.get("to_bow").is_none());
    }

    #[test]
    fn distress_class_by_mmsi_prefix() {
        assert_eq!(distress_class(970_12_3456), Some("AIS-SART"));
        assert_eq!(distress_class(972_00_0001), Some("AIS-MOB"));
        assert_eq!(distress_class(974_99_9999), Some("EPIRB-AIS"));
        assert_eq!(distress_class(366_123_456), None); // ordinary US ship
        assert_eq!(distress_class(0), None);
    }
}
