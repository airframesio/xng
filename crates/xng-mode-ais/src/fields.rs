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

/// IMO ASM longitude/latitude pair (IMO SN.1/Circ.289 convention): longitude
/// in a 25-bit signed field, latitude in a 24-bit signed field, both scaled at
/// 1/1000 minute = raw / 60000 degrees. The "not available" sentinels are
/// longitude 181° (raw 181*60000 = 0x6791AC0) and latitude 91° (0x3412140);
/// either sentinel — or an out-of-range value — yields `None` for that pair.
/// Returns `(lon, lat)` in degrees. `s` is the bit offset of the longitude
/// field; the latitude immediately follows.
fn imo_lonlat(bits: &[u8], s: usize) -> Option<(f64, f64)> {
    let lon = i(bits, s, 25)? as f64 / 60_000.0;
    let lat = i(bits, s + 25, 24)? as f64 / 60_000.0;
    if lon.abs() > 180.0 || lat.abs() > 90.0 {
        return None;
    }
    Some((lon, lat))
}

/// DAC=1 (IMO international) Application-Specific Message decode, dispatched by
/// FID per IMO SN.1/Circ.289 ("Guidance on the use of AIS application-specific
/// messages", 2 June 2010) and the legacy layouts in IMO SN/Circ.236 retained
/// by ITU-R M.1371-5 Annex 5. pyais does not decode DAC=1, so every layout here
/// is grounded in the cited circular section, not an OSS oracle. `p` is the bit
/// offset of the application data (after the 16-bit DAC+FID header). Unhandled
/// FIDs return `None` so the caller falls back to `data_hex`.
fn dac1_decode(fid: u64, bits: &[u8], p: usize) -> Option<Value> {
    let mut d = serde_json::Map::new();
    let mut put = |k: &str, v: Value| {
        d.insert(k.into(), v);
    };
    // Optional integer field with a documented "not available" sentinel: emit
    // the value only when it is in range, otherwise omit the key entirely.
    macro_rules! opt_u {
        ($key:expr, $off:expr, $len:expr, $na:expr) => {{
            let v = u(bits, p + $off, $len)?;
            if v != $na {
                put($key, json!(v));
            }
        }};
    }
    match fid {
        // FID 11 — Meteorological and hydrological data (legacy, IMO
        // SN/Circ.236 Annex 4 / ITU-R M.1371-5 Annex 5 §3.2). 352-bit
        // application block. NOTE: latitude precedes longitude here, the
        // reverse of FID 31. lat 24 / lon 25 are 1/1000-min signed.
        11 => {
            // lat first (24), then lon (25) — both 1/1000 min, raw/60000 deg.
            let lat = i(bits, p, 24)? as f64 / 60_000.0;
            let lon = i(bits, p + 24, 25)? as f64 / 60_000.0;
            if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            opt_u!("day", 49, 5, 0);
            opt_u!("hour", 54, 5, 24);
            opt_u!("minute", 59, 6, 60);
            // Wind speed avg/gust: knots, 127 = N/A.
            opt_u!("wind_speed_kt", 65, 7, 127);
            opt_u!("wind_gust_kt", 72, 7, 127);
            // Wind direction / gust direction: degrees, 511 = N/A.
            opt_u!("wind_dir_deg", 79, 9, 511);
            opt_u!("wind_gust_dir_deg", 88, 9, 511);
            // Air temperature: 0.1 °C signed, raw range -600..+600, 0x7FF=-1024 N/A.
            let at = i(bits, p + 97, 11)?;
            if at != -1024 {
                put("air_temp_c", json!(at as f64 / 10.0));
            }
            // Relative humidity %: 0..100, 127 = N/A.
            opt_u!("humidity_pct", 108, 7, 127);
            // Dew point: 0.1 °C signed, 501 (raw 0x1F5) = N/A in the legacy field.
            let dp = i(bits, p + 115, 10)?;
            if dp != 501 {
                put("dew_point_c", json!(dp as f64 / 10.0));
            }
            // Air pressure: hPa, value 0 = N/A, offset +800 (range 800..1200);
            // 402 reserved. Field 9 bits, 511 = N/A per Circ.236.
            let pr = u(bits, p + 125, 9)?;
            if pr != 511 {
                put("pressure_hpa", json!(pr + 800));
            }
            opt_u!("pressure_tendency", 134, 2, 3);
            // Horizontal visibility: 0.1 NM, 127 = N/A (7-bit).
            let vis = u(bits, p + 136, 7)?;
            if vis != 127 {
                put("visibility_nm", json!(vis as f64 / 10.0));
            }
            // Water level: 0.1 m, offset -10 m, range -10..+30; raw 0..401,
            // 511 (0x1FF) = N/A (9-bit unsigned).
            let wl = u(bits, p + 143, 9)?;
            if wl != 511 {
                put("water_level_m", json!(wl as f64 / 10.0 - 10.0));
            }
            opt_u!("water_level_trend", 152, 2, 3);
            // Surface current speed 0.1 kt (255 N/A) and direction deg (511 N/A).
            let sc = u(bits, p + 154, 8)?;
            if sc != 255 {
                put("surface_current_kt", json!(sc as f64 / 10.0));
            }
            opt_u!("surface_current_dir_deg", 162, 9, 511);
        }
        // FID 31 — Meteorological and hydrological data (IMO SN.1/Circ.289
        // Annex, §"Meteorological and Hydrological Data"; ITU-R M.1371-5
        // Annex 8 Table). 360-bit application block. Supersedes FID 11 with a
        // higher-resolution position (lon 25 / lat 24, 1/1000 min) placed
        // FIRST, position-accuracy flag, and tenth-of-unit scalings.
        31 => {
            if let Some((lon, lat)) = imo_lonlat(bits, p) {
                put("lon", json!(lon));
                put("lat", json!(lat));
            }
            put("position_accuracy", json!(u(bits, p + 49, 1)? == 1));
            opt_u!("day", 50, 5, 0);
            opt_u!("hour", 55, 5, 24);
            opt_u!("minute", 60, 6, 60);
            // Average wind speed / gust: knots, 0..126, 127 = N/A.
            opt_u!("wind_speed_kt", 66, 7, 127);
            opt_u!("wind_gust_kt", 73, 7, 127);
            opt_u!("wind_dir_deg", 80, 9, 360);
            opt_u!("wind_gust_dir_deg", 89, 9, 360);
            // Air temperature: 0.1 °C signed, -60.0..+60.0, raw -1024 = N/A.
            let at = i(bits, p + 98, 11)?;
            if at != -1024 {
                put("air_temp_c", json!(at as f64 / 10.0));
            }
            opt_u!("humidity_pct", 109, 7, 101);
            // Dew point: 0.1 °C, range -20.0..+50.0, raw 501 = N/A (10-bit
            // signed-offset field per Circ.289).
            let dp = i(bits, p + 116, 10)?;
            if dp != 501 {
                put("dew_point_c", json!(dp as f64 / 10.0));
            }
            // Air pressure: hPa absolute, 0..401 → 799..1200 (offset +799),
            // 402..510 reserved, 511 = N/A.
            let pr = u(bits, p + 126, 9)?;
            if pr <= 401 {
                put("pressure_hpa", json!(pr + 799));
            }
            opt_u!("pressure_tendency", 135, 2, 3);
            // Visibility: bit 137 is the ">" greater-than flag; 0.1 NM, 7-bit
            // value, 127 = N/A.
            let vis_gt = u(bits, p + 137, 1)? == 1;
            let vis = u(bits, p + 138, 7)?;
            if vis != 127 {
                put("visibility_nm", json!(vis as f64 / 10.0));
                if vis_gt {
                    put("visibility_greater", json!(true));
                }
            }
            // Water level (incl. tide): 0.01 m, offset -10 m, range -10..+30;
            // raw 0..4000, 4001 = N/A (12-bit unsigned).
            let wl = u(bits, p + 145, 12)?;
            if wl != 4001 {
                put("water_level_m", json!(wl as f64 / 100.0 - 10.0));
            }
            opt_u!("water_level_trend", 157, 2, 3);
            // Surface current speed 0.1 kt (255 N/A) + direction deg (360 N/A).
            let sc = u(bits, p + 159, 8)?;
            if sc != 255 {
                put("surface_current_kt", json!(sc as f64 / 10.0));
            }
            opt_u!("surface_current_dir_deg", 167, 9, 360);
        }
        // FID 16 — Number of persons on board (IMO SN.1/Circ.289 Annex,
        // §"Number of persons on board"; ITU-R M.1371-5 Annex 5 §3.10).
        // 13-bit unsigned count, 0 = not available.
        16 => {
            opt_u!("persons_on_board", 0, 13, 0);
        }
        // FID 17 — VTS-generated/synthetic targets (IMO SN.1/Circ.289 Annex,
        // §"VTS-generated/synthetic targets"). A repeating 122-bit target
        // record: id-type 2, target id 42 (interpretation depends on id-type),
        // spare 4, lat 24, lon 25 (1/1000 min, note lat-then-lon), COG 9
        // (deg), timestamp 6 (UTC second), SOG 10 (0.1 kt). Up to 4 fit a slot.
        17 => {
            const REC: usize = 122;
            let mut targets = Vec::new();
            let mut o = 0usize;
            while p + o + REC <= bits.len() {
                let id_type = u(bits, p + o, 2)?;
                let target_raw = u(bits, p + o + 2, 42)?;
                // 4-bit spare at offset 44, latitude begins at 48.
                let lat = i(bits, p + o + 48, 24)? as f64 / 60_000.0;
                let lon = i(bits, p + o + 72, 25)? as f64 / 60_000.0;
                let cog = u(bits, p + o + 97, 9)?;
                let ts = u(bits, p + o + 106, 6)?;
                let sog = u(bits, p + o + 112, 10)?;
                let mut t = serde_json::Map::new();
                t.insert("id_type".into(), json!(id_type));
                // id-type 0 = MMSI (30-bit value in the high bits of the field).
                if id_type == 0 {
                    t.insert("mmsi".into(), json!(target_raw >> 12));
                } else {
                    t.insert("target_id".into(), json!(target_raw));
                }
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    t.insert("lat".into(), json!(lat));
                    t.insert("lon".into(), json!(lon));
                }
                if cog != 360 {
                    t.insert("cog_deg".into(), json!(cog));
                }
                if ts < 60 {
                    t.insert("timestamp_sec".into(), json!(ts));
                }
                if sog != 1023 {
                    t.insert("sog_kt".into(), json!(sog as f64 / 10.0));
                }
                targets.push(Value::Object(t));
                o += REC;
            }
            if targets.is_empty() {
                return None;
            }
            put("targets", json!(targets));
        }
        // FID 21 — Weather observation report from ship (IMO SN.1/Circ.289
        // Annex, §"Weather observation report from ship"; the non-WMO variant).
        // Leading 1-bit variant flag (0 = as-developed layout) then location
        // name (6-bit ASCII × 20), then lon/lat (1/1000 min). We decode the
        // grounded leading fields (variant flag, location, position, UTC) and
        // defer the WMO-coded weather block to data_hex via 'remaining'.
        21 => {
            put("variant", json!(u(bits, p, 1)?));
            if let Some(loc) = sixbit(bits, p + 1, 20) {
                if !loc.is_empty() {
                    put("location", json!(loc));
                }
            }
            // lon 25 / lat 24, 1/1000 min, immediately after the 120-bit name.
            if let Some((lon, lat)) = imo_lonlat(bits, p + 121) {
                put("lon", json!(lon));
                put("lat", json!(lat));
            }
            opt_u!("day", 170, 5, 0);
            opt_u!("hour", 175, 5, 24);
            opt_u!("minute", 180, 6, 60);
        }
        // FID 22 — Area notice, broadcast (IMO SN.1/Circ.289 Annex, §"Area
        // notice"). Header: message linkage 10, notice description 7,
        // start month 4 / day 5 / hour 5 / minute 6, duration-minutes 18, then
        // 1..n 90-bit sub-area shape records. We decode the header (the
        // grounded part) and count the sub-area records; the per-shape geometry
        // is deferred (see 'remaining').
        22 | 23 => {
            put("message_linkage", json!(u(bits, p, 10)?));
            put("notice_description", json!(u(bits, p + 10, 7)?));
            opt_u!("start_month", 17, 4, 0);
            opt_u!("start_day", 21, 5, 0);
            opt_u!("start_hour", 26, 5, 24);
            opt_u!("start_minute", 31, 6, 60);
            // Duration in minutes; 262143 = "cancel"/not available.
            opt_u!("duration_min", 37, 18, 262143);
            let header = 55usize;
            let n = (bits.len().saturating_sub(p + header)) / 90;
            put("sub_area_count", json!(n));
        }
        // FID 24 — Extended ship static and voyage-related data (IMO
        // SN.1/Circ.289 Annex, §"Extended ship static and voyage related
        // data"). message linkage 10, air draught 13 (0.1 m, 0 = N/A), then
        // last-port + next-two-ports as UN/LOCODE 6-bit-ASCII (5 chars each),
        // ETAs, and solid/liquid/packed cargo amounts. We decode the grounded
        // linkage + air-draught + ports; the cargo table is deferred.
        24 => {
            put("message_linkage", json!(u(bits, p, 10)?));
            let ad = u(bits, p + 10, 13)?;
            if ad != 0 {
                put("air_draught_m", json!(ad as f64 / 10.0));
            }
            if let Some(lp) = sixbit(bits, p + 23, 5) {
                if !lp.is_empty() {
                    put("last_port", json!(lp));
                }
            }
            if let Some(np) = sixbit(bits, p + 53, 5) {
                if !np.is_empty() {
                    put("next_port", json!(np));
                }
            }
            if let Some(np2) = sixbit(bits, p + 83, 5) {
                if !np2.is_empty() {
                    put("second_next_port", json!(np2));
                }
            }
        }
        // FID 25 — Dangerous cargo indication (IMO SN.1/Circ.289 Annex,
        // §"Dangerous cargo indication"). message linkage 10, then amount-unit
        // 2, amount of cargo 10, then 1..17 cargo codes of 17 bits each
        // (IMDG/IGC/etc.). We decode linkage + amount; per-item codes are
        // deferred.
        25 => {
            put("message_linkage", json!(u(bits, p, 10)?));
            put("amount_unit", json!(u(bits, p + 10, 2)?));
            put("amount", json!(u(bits, p + 12, 10)?));
            let items = (bits.len().saturating_sub(p + 22)) / 17;
            put("cargo_item_count", json!(items));
        }
        // FID 26 — Environmental / tidal / sensor report (IMO SN.1/Circ.289
        // Annex, §"Environmental"). A header (lon 25 / lat 24 site position,
        // day 5 / hour 5 / minute 6) followed by repeating 85-bit sensor
        // report blocks each tagged by a 4-bit sensor-report type. The position
        // + timestamp header is grounded; the per-sensor blocks (type-specific)
        // are deferred to data_hex (see 'remaining').
        26 => {
            // Circ.289 environmental: each report begins with a 4-bit type and
            // the sensor data; the leading common block carries day/hour/minute
            // and the measurement site position. Decode that common block only.
            if let Some((lon, lat)) = imo_lonlat(bits, p) {
                put("lon", json!(lon));
                put("lat", json!(lat));
            }
            opt_u!("day", 49, 5, 0);
            opt_u!("hour", 54, 5, 24);
            opt_u!("minute", 59, 6, 60);
            let n = (bits.len().saturating_sub(p + 65)) / 85;
            put("sensor_report_count", json!(n));
        }
        // FID 27 — Route information, broadcast (IMO SN.1/Circ.289 Annex,
        // §"Route information"). message linkage 10, sender class 3, route type
        // 5, start month 4 / day 5 / hour 5 / minute 6, duration 18, waypoint
        // count 5, then count × (lon 28 / lat 27) waypoints at the core
        // 1/10000-min resolution (raw / 600000 degrees, like the position
        // messages — NOT the 1/1000-min ASM scaling).
        27 | 28 => {
            put("message_linkage", json!(u(bits, p, 10)?));
            put("sender_class", json!(u(bits, p + 10, 3)?));
            put("route_type", json!(u(bits, p + 13, 5)?));
            opt_u!("start_month", 18, 4, 0);
            opt_u!("start_day", 22, 5, 0);
            opt_u!("start_hour", 27, 5, 24);
            opt_u!("start_minute", 32, 6, 60);
            opt_u!("duration_min", 38, 18, 262143);
            let count = u(bits, p + 56, 5)? as usize;
            put("waypoint_count", json!(count));
            let mut wps = Vec::new();
            for k in 0..count {
                let s = p + 61 + k * 55;
                let lon = i(bits, s, 28)? as f64 / 600_000.0;
                let lat = i(bits, s + 28, 27)? as f64 / 600_000.0;
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    wps.push(json!({ "lon": lon, "lat": lat }));
                }
            }
            if !wps.is_empty() {
                put("waypoints", json!(wps));
            }
        }
        // FID 29 — Text description, broadcast (IMO SN.1/Circ.289 Annex,
        // §"Text description"). message linkage 10, then up to 906 bits of
        // 6-bit ASCII text.
        29 | 30 => {
            put("message_linkage", json!(u(bits, p, 10)?));
            let chars = (bits.len().saturating_sub(p + 10)) / 6;
            if let Some(text) = sixbit(bits, p + 10, chars) {
                if !text.is_empty() {
                    put("text", json!(text));
                }
            }
        }
        // FID 32 — Tidal window (IMO SN.1/Circ.289 Annex, §"Tidal window").
        // message linkage 10, month 4, day 5, then 1..3 tidal-window records
        // of 88 bits each: lon 25 / lat 24, from-UTC hour 5 / minute 6,
        // to-UTC hour 5 / minute 6, current direction 9 (deg), current speed
        // 8 (0.1 kt). We decode the header + each window's position/time/
        // current (all grounded).
        32 => {
            put("message_linkage", json!(u(bits, p, 10)?));
            opt_u!("month", 10, 4, 0);
            opt_u!("day", 14, 5, 0);
            let mut windows = Vec::new();
            let mut o = 19usize;
            while p + o + 88 <= bits.len() {
                let lon = i(bits, p + o, 25)? as f64 / 60_000.0;
                let lat = i(bits, p + o + 25, 24)? as f64 / 60_000.0;
                let from_h = u(bits, p + o + 49, 5)?;
                let from_m = u(bits, p + o + 54, 6)?;
                let to_h = u(bits, p + o + 60, 5)?;
                let to_m = u(bits, p + o + 65, 6)?;
                let cur_dir = u(bits, p + o + 71, 9)?;
                // Current speed: 8-bit, 0.1 kt, 255 = not available.
                let cur_spd = u(bits, p + o + 80, 8)?;
                let mut w = serde_json::Map::new();
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    w.insert("lon".into(), json!(lon));
                    w.insert("lat".into(), json!(lat));
                }
                if from_h < 24 {
                    w.insert("from".into(), json!(format!("{from_h:02}:{from_m:02}")));
                }
                if to_h < 24 {
                    w.insert("to".into(), json!(format!("{to_h:02}:{to_m:02}")));
                }
                if cur_dir != 360 {
                    w.insert("current_dir_deg".into(), json!(cur_dir));
                }
                if cur_spd != 255 {
                    w.insert("current_speed_kt".into(), json!(cur_spd as f64 / 10.0));
                }
                if !w.is_empty() {
                    windows.push(Value::Object(w));
                }
                o += 88;
            }
            if !windows.is_empty() {
                put("tidal_windows", json!(windows));
            }
        }
        _ => return None,
    }
    if d.is_empty() { None } else { Some(Value::Object(d)) }
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
///
/// DAC=1 (IMO international application identifiers) subtypes follow IMO
/// SN.1/Circ.289 (and the legacy SN/Circ.236 layouts retained by ITU-R
/// M.1371-5 Annex 5 / Annex 8). pyais has NO DAC=1 decoder, so these are
/// spec-derived: each FID arm cites the governing circular annex in a comment
/// and in PROVENANCE.md. The IMO ASM lat/lon convention is 1/1000-minute
/// (raw / 60000 degrees), distinct from the 1/600000-degree of the core
/// position messages; sentinel longitude 181° / latitude 91° mean
/// "not available". See [`imo_lonlat`].
fn asm_decode(dac: u64, fid: u64, bits: &[u8], p: usize) -> Option<Value> {
    let mut d = serde_json::Map::new();
    let mut put = |k: &str, v: Value| {
        d.insert(k.into(), v);
    };
    // Optional unsigned field with a documented "not available"/"unknown"
    // sentinel: emit the key only when the value is in range. `$off` is
    // relative to the application data start `p`.
    macro_rules! opt_u {
        ($key:expr, $off:expr, $len:expr, $na:expr) => {{
            let v = u(bits, p + $off, $len)?;
            if v != $na {
                put($key, json!(v));
            }
        }};
    }
    // Optional 6-bit-ASCII string field: emit only when non-empty.
    macro_rules! opt_str {
        ($key:expr, $off:expr, $chars:expr) => {{
            let s = sixbit(bits, p + $off, $chars)?;
            if !s.is_empty() {
                put($key, json!(s));
            }
        }};
    }
    match (dac, fid) {
        (1, _) => return dac1_decode(fid, bits, p),
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
        // ETA at lock/bridge/terminal (FID 21) and RTA reply (FID 22). Inland
        // AIS, UNECE ECE/TRANS/SC.3/176 Ed.1 Annex (Test Standard for Inland
        // AIS, §"ETA report"/"RTA report"); cross-checked against the IALA ASM
        // registry and e-Navigation.nl. Both ride in message 6 (addressed) and
        // share an identical leading block: five 6-bit-ASCII location strings
        // — UN country code (12 b / 2 chars), UN/LOCODE (18 b / 3 chars),
        // fairway section number (30 b / 5 chars), terminal code (30 b / 5
        // chars), fairway hectometre (30 b / 5 chars) — then month 4 (0=N/A),
        // day 5 (0=N/A), hour 5 (24=N/A), minute 6 (60=N/A). pyais has NO
        // decoder for these, so the layout is spec-derived, not OSS-oracle.
        (200, 21) | (200, 22) => {
            opt_str!("inland_country", 0, 2);
            opt_str!("un_locode", 12, 3);
            opt_str!("fairway_section", 30, 5);
            opt_str!("terminal_code", 60, 5);
            opt_str!("fairway_hectometre", 90, 5);
            // Time block starts at bit 120 (12+18+30+30+30): month 4, day 5,
            // hour 5, minute 6.
            opt_u!("month", 120, 4, 0);
            opt_u!("day", 124, 5, 0);
            opt_u!("hour", 129, 5, 24);
            opt_u!("minute", 134, 6, 60);
            if fid == 21 {
                // ETA: assisting tugs 3 (7=unknown) at bit 140, air draught 12
                // (0.01 m, 0=not used) at bit 143, spare 5.
                opt_u!("assisting_tugs", 140, 3, 7);
                let ad = u(bits, p + 143, 12)?;
                if ad != 0 {
                    put("air_draught_m", json!(ad as f64 / 100.0));
                }
            } else {
                // RTA: lock/bridge/terminal status 2 (3=N/A) at bit 140,
                // spare 2. 0=operational, 1=limited operation, 2=out of order.
                opt_u!("status", 140, 2, 3);
            }
        }
        // Number of persons on board (FID 55). Inland AIS, UNECE
        // ECE/TRANS/SC.3/176 Ed.1 Annex (Test Standard for Inland AIS,
        // §"Number of persons on board"); cross-checked against the IALA ASM
        // registry and e-Navigation.nl. Message 6, fixed 168 bits. Body:
        // crew 8 (255=unknown), passengers 13 (8191=unknown), shipboard
        // personnel 8 (255=unknown), spare 51. pyais has no decoder for this.
        (200, 55) => {
            opt_u!("crew", 0, 8, 255);
            opt_u!("passengers", 8, 13, 8191);
            opt_u!("personnel", 21, 8, 255);
        }
        // AtoN monitoring data (DAC 235 UK / DAC 250 Ireland, FID 10). Message
        // 6. Layout per the AIVDM/AIVDO reference (gpsd, §"IALA/regional AtoN
        // monitoring"): analogue internal 10 (0.05 V/step, 0.05–36 V),
        // analogue external #1 10, analogue external #2 10, RACON status 2,
        // light status 2, health 1, status external 8, off-position 1, spare 4.
        // pyais has no decoder for this DAC; the layout is spec-derived.
        (235, 10) | (250, 10) => {
            let volts = |s: usize| -> Option<f64> { Some(u(bits, s, 10)? as f64 * 0.05) };
            put("voltage_internal", json!(volts(p)?));
            put("voltage_external_1", json!(volts(p + 10)?));
            put("voltage_external_2", json!(volts(p + 20)?));
            put("racon_status", json!(u(bits, p + 30, 2)?));
            put("light_status", json!(u(bits, p + 32, 2)?));
            put("health_alarm", json!(u(bits, p + 34, 1)? == 1));
            put("status_external", json!(u(bits, p + 35, 8)?));
            put("off_position", json!(u(bits, p + 43, 1)? == 1));
        }
        // Regional DACs with no clean-room body layout available. Per the
        // mandate, emit a header-only identification (DAC/FID + a human-
        // readable name) so downstream consumers can route the message; the
        // body falls through to data_hex at the caller. These cover:
        //   DAC 366/316 — US/Canada St. Lawrence Seaway & PAWSS (gpsd lists the
        //     DAC/FID pairs but documents no bit layout);
        //   DAC 367     — US environmental / area-notice (NOT in the gpsd
        //     tables nor in pyais; IALA ASM registry has the layouts but they
        //     are not reproduced clean-room here);
        //   DAC 265     — Sweden / STM (Sea Traffic Management) route exchange
        //     (no clean-room layout available).
        // See the IALA ASM registry (iala.int/asm) for the per-FID body fields.
        // The undecoded body is preserved as `body_hex` so nothing is lost.
        (366, _) | (316, _) | (367, _) | (265, _) => {
            let region = match dac {
                366 | 316 => "US/Canada Seaway (PAWSS)",
                367 => "US environmental/area-notice",
                _ => "Sweden STM route",
            };
            put("region", json!(region));
            put("fid", json!(fid));
            if bits.len() > p {
                put("body_hex", json!(data_hex(bits, p)));
            }
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

/// SOTDMA / ITDMA sync-state names (ITU-R M.1371-5 §3.3.7.2.1, the "sync
/// state" column): 0 = synchronised directly to UTC, 1 = synchronised
/// indirectly to UTC, 2 = synchronised to a base station, 3 = synchronised to
/// another station (the one reporting the highest number of received
/// stations). Same enumeration for both access schemes.
fn sync_state_name(code: u64) -> &'static str {
    match code {
        0 => "UTC direct",
        1 => "UTC indirect",
        2 => "base station",
        _ => "other station",
    }
}

/// Decode the radio communication state carried by SOTDMA/ITDMA position
/// reports (the "AIS-3 leftover"). `raw` is the value of the comm-state field
/// (19 bits for the message types that always use SOTDMA — 1/2/4/11; for
/// types 3/9/18 the field is 20 bits, where the most-significant bit selects
/// ITDMA(=1)/SOTDMA(=0) and the low 19 bits are passed here). `itdma`
/// selects the access scheme.
///
/// SOTDMA (ITU-R M.1371-5 §3.3.7.2.1, Table 21): sync state (2) | slot
/// time-out (3) | sub-message (14). The slot time-out (frames remaining
/// before a new slot is selected, 0 = last transmission in this slot)
/// determines the sub-message — time-out 0 → slot offset (offset to the slot
/// used in the next frame); 1 → UTC hour (sub bits 13..9) and minute (sub
/// bits 8..2); 3/5/7 → number of other stations received (0..16383); 2/4/6 →
/// slot number used for this transmission (0..2249).
///
/// ITDMA (ITU-R M.1371-5 §3.3.7.3.2, Table 23): sync state (2) | slot
/// increment (13) | number of slots (3) | keep flag (1). Slot increment is
/// the offset to the next slot to be used (0 = no further transmission);
/// number-of-slots N encodes N+1 consecutive slots; the keep flag, when set,
/// retains the slot for one additional frame.
///
/// Verified field-for-field against the pyais 3.1.0 oracle
/// (`get_sotdma_comm_state` / `get_itdma_comm_state`, util.py) on real AIVDM
/// vectors — see the unit tests below.
fn comm_state(raw: u64, itdma: bool) -> Value {
    let mut d = serde_json::Map::new();
    let sync = (raw >> 17) & 0x3;
    d.insert("sync_state".into(), json!(sync));
    d.insert("sync_state_text".into(), json!(sync_state_name(sync)));
    if itdma {
        // §3.3.7.3.2: increment (13) | num slots (3) | keep flag (1).
        let slot_increment = (raw >> 4) & 0x1fff;
        let num_slots = (raw >> 1) & 0x7;
        let keep_flag = raw & 0x1;
        d.insert("scheme".into(), json!("ITDMA"));
        d.insert("slot_increment".into(), json!(slot_increment));
        // N encodes N+1 consecutive slots; expose both the raw field and the
        // human count so consumers needn't redo the +1.
        d.insert("num_slots".into(), json!(num_slots));
        d.insert("slots_allocated".into(), json!(num_slots + 1));
        d.insert("keep_flag".into(), json!(keep_flag == 1));
    } else {
        // §3.3.7.2.1: slot time-out (3) then the time-out-dependent sub-msg.
        let slot_timeout = (raw >> 14) & 0x7;
        let sub = raw & 0x3fff;
        d.insert("scheme".into(), json!("SOTDMA"));
        d.insert("slot_timeout".into(), json!(slot_timeout));
        match slot_timeout {
            0 => {
                d.insert("slot_offset".into(), json!(sub));
            }
            1 => {
                d.insert("utc_hour".into(), json!((sub >> 9) & 0x1f));
                d.insert("utc_minute".into(), json!((sub >> 2) & 0x3f));
            }
            2 | 4 | 6 => {
                d.insert("slot_number".into(), json!(sub));
            }
            // 3, 5, 7
            _ => {
                d.insert("received_stations".into(), json!(sub));
            }
        }
    }
    Value::Object(d)
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
            // Radio communication state (19 bits at bit 149). Types 1 & 2 use
            // SOTDMA; type 3 uses ITDMA (ITU-R M.1371-5 §3.3.7.2/§3.3.7.3).
            if let Some(raw) = u(bits, 149, 19) {
                put("comm_state", comm_state(raw, msg_type == 3));
            }
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
            // Radio communication state (19 bits at bit 149). Base stations
            // (4) and the UTC/date response (11) always use SOTDMA
            // (ITU-R M.1371-5 §3.3.7.2.1).
            if let Some(raw) = u(bits, 149, 19) {
                put("comm_state", comm_state(raw, false));
            }
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
            // Radio communication state (20 bits at bit 148): the MSB selects
            // the access scheme — 1 = ITDMA, 0 = SOTDMA — and the low 19 bits
            // carry the state (ITU-R M.1371-5 §3.3.7.4, communication-state
            // selector flag).
            if let Some(raw) = u(bits, 148, 20) {
                put("comm_state", comm_state(raw & 0x7ffff, raw >> 19 == 1));
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
                // Class-B CS radio communication state (20 bits at bit 148):
                // MSB selects ITDMA(1)/SOTDMA(0), low 19 bits carry the state
                // (ITU-R M.1371-5 §3.3.7.4). Type 19 has no comm-state field.
                if let Some(raw) = u(bits, 148, 20) {
                    put("comm_state", comm_state(raw & 0x7ffff, raw >> 19 == 1));
                }
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
        // DAC=1/FID=31 with only a 32-bit payload is too short for the
        // Circ.289 met/hydro layout → dac1_decode returns None → data_hex
        // fallback (the dispatch is wired, the truncated body just can't fill
        // the fields). A full-length FID=31 body is exercised below.
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

    // ----------------------------------------------------------------------
    // SOTDMA / ITDMA radio communication state (the AIS-3 leftover),
    // ITU-R M.1371-5 §3.3.7.2 (SOTDMA) / §3.3.7.3 (ITDMA) / §3.3.7.4 (the
    // SOTDMA/ITDMA selector flag in messages 9/18/26).
    //
    // ORACLE: pyais 3.1.0 (MIT) DOES expose the comm-state — `radio` field +
    // `get_communication_state()` (util.py `get_sotdma_comm_state` /
    // `get_itdma_comm_state`). Every assertion below is the value pyais 3.1.0
    // produced for the SAME AIVDM sentence (captured 2026-06-16). No pyais
    // code was copied; only the decoded field values are the reference. The
    // bit offsets (149 for the 19-bit SOTDMA field of types 1/2/4/11; 148 for
    // the 20-bit selector+state field of types 9/18) were cross-checked to
    // reproduce pyais's `radio` raw value exactly across all of these vectors.
    // ----------------------------------------------------------------------

    /// pyais-oracle helper: decode the SOTDMA/ITDMA comm-state out of an AIVDM
    /// payload (single-fragment, `fill` fill bits) and return the `comm_state`
    /// object. Asserts the message type so a wrong vector fails loudly.
    fn comm_of(payload: &str, fill: usize, want_type: u8) -> Value {
        let (t, bits) = typed(payload, fill);
        assert_eq!(t, want_type);
        decode(t, &bits).unwrap()["comm_state"].clone()
    }

    #[test]
    fn comm_state_sotdma_utc_hour_minute_matches_pyais() {
        // Type 1, MMSI 477553000 (the crate's canonical type-1 vector).
        // pyais: radio=149208, slot_timeout=1, sync_state=1 (UTC indirect),
        // utc_hour=3, utc_minute=54.
        let c = comm_of("177KQJ5000G?tO`K>RA1wUbN0TKH", 0, 1);
        assert_eq!(c["scheme"], "SOTDMA");
        assert_eq!(c["sync_state"], 1);
        assert_eq!(c["sync_state_text"], "UTC indirect");
        assert_eq!(c["slot_timeout"], 1);
        assert_eq!(c["utc_hour"], 3);
        assert_eq!(c["utc_minute"], 54);
        // The other sub-fields are absent for this slot_timeout.
        assert!(c.get("slot_offset").is_none());
        assert!(c.get("slot_number").is_none());
        assert!(c.get("received_stations").is_none());
    }

    #[test]
    fn comm_state_sotdma_slot_offset_matches_pyais() {
        // Type 1, MMSI 366053209. pyais: radio=161, slot_timeout=0,
        // sync_state=0 (UTC direct), slot_offset=161.
        let c = comm_of("15M67FC000G?ufbE`FepT8u8002Q", 0, 1);
        assert_eq!(c["scheme"], "SOTDMA");
        assert_eq!(c["sync_state"], 0);
        assert_eq!(c["sync_state_text"], "UTC direct");
        assert_eq!(c["slot_timeout"], 0);
        assert_eq!(c["slot_offset"], 161);
        assert!(c.get("utc_hour").is_none());
    }

    #[test]
    fn comm_state_sotdma_slot_number_matches_pyais() {
        // Type 1, MMSI 244670316. pyais: radio=33359, slot_timeout=2,
        // sync_state=0, slot_number=591.
        let c = comm_of("13aEOK?P00PD2wVMdLDRhgvL289?", 0, 1);
        assert_eq!(c["slot_timeout"], 2);
        assert_eq!(c["sync_state"], 0);
        assert_eq!(c["slot_number"], 591);
        assert!(c.get("received_stations").is_none());
    }

    #[test]
    fn comm_state_sotdma_received_stations_matches_pyais() {
        // Type 1, MMSI 545921920. pyais: radio=49198, slot_timeout=3,
        // sync_state=0, received_stations=46.
        let c = comm_of("1H8`KP0P00PD@l8MD6QQ9wvJ2<0f", 0, 1);
        assert_eq!(c["slot_timeout"], 3);
        assert_eq!(c["sync_state"], 0);
        assert_eq!(c["received_stations"], 46);
        assert!(c.get("slot_number").is_none());
    }

    #[test]
    fn comm_state_type4_base_station_matches_pyais() {
        // Type 4, MMSI 3669145 (the crate's canonical type-4 vector).
        // pyais: radio=98739, slot_timeout=6, sync_state=0, slot_number=435.
        let c = comm_of("403OtVAv>lba;o?Ia`E`4G?02H6k", 0, 4);
        assert_eq!(c["scheme"], "SOTDMA");
        assert_eq!(c["slot_timeout"], 6);
        assert_eq!(c["sync_state"], 0);
        assert_eq!(c["slot_number"], 435);
    }

    #[test]
    fn comm_state_type4_sync_base_station_matches_pyais() {
        // Type 4, MMSI 2288218. pyais: radio=166109, slot_timeout=2,
        // sync_state=1 (UTC indirect), slot_number=2269.
        let c = comm_of("402;bFQv@kkLc00Dl4LE52100`SM", 0, 4);
        assert_eq!(c["slot_timeout"], 2);
        assert_eq!(c["sync_state"], 1);
        assert_eq!(c["slot_number"], 2269);
    }

    #[test]
    fn comm_state_type18_itdma_matches_pyais() {
        // Type 18, MMSI 367430530 (the crate's canonical type-18 vector).
        // pyais: radio=917510 (> 0x7ffff → ITDMA selector set), sync_state=3
        // (other station), keep_flag=0, slot_increment=0, num_slots=3.
        let c = comm_of("B5NJ;PP005l4ot5Isbl03wsUkP06", 0, 18);
        assert_eq!(c["scheme"], "ITDMA");
        assert_eq!(c["sync_state"], 3);
        assert_eq!(c["sync_state_text"], "other station");
        assert_eq!(c["slot_increment"], 0);
        assert_eq!(c["num_slots"], 3);
        assert_eq!(c["slots_allocated"], 4); // N+1 consecutive slots
        assert_eq!(c["keep_flag"], false);
        // ITDMA carries no slot_timeout / SOTDMA sub-fields.
        assert!(c.get("slot_timeout").is_none());
        assert!(c.get("slot_offset").is_none());
    }

    #[test]
    fn comm_state_type18_sotdma_matches_pyais() {
        // Type 18, MMSI 423302100. pyais: radio=0 (≤ 0x7ffff → SOTDMA),
        // slot_offset=0, slot_timeout=0, sync_state=0.
        let c = comm_of("B6CdCm0t3`tba35RbDM21Oh00000", 0, 18);
        assert_eq!(c["scheme"], "SOTDMA");
        assert_eq!(c["slot_timeout"], 0);
        assert_eq!(c["sync_state"], 0);
        assert_eq!(c["slot_offset"], 0);
        assert!(c.get("keep_flag").is_none());
    }

    #[test]
    fn comm_state_type9_itdma_matches_pyais() {
        // Type 9 SAR aircraft, MMSI 366000005. pyais: radio=703773 (ITDMA),
        // sync_state=1 (UTC indirect), keep_flag=1, slot_increment=3025,
        // num_slots=6.
        let c = comm_of("95M2oQ@41Tr4L4BD5`8L3Sup6clMwd?cT5i", 0, 9);
        assert_eq!(c["scheme"], "ITDMA");
        assert_eq!(c["sync_state"], 1);
        assert_eq!(c["sync_state_text"], "UTC indirect");
        assert_eq!(c["slot_increment"], 3025);
        assert_eq!(c["num_slots"], 6);
        assert_eq!(c["slots_allocated"], 7);
        assert_eq!(c["keep_flag"], true);
    }

    #[test]
    fn comm_state_type3_is_itdma() {
        // Type 3 always uses ITDMA (ITU-R M.1371-5 §3.3.7.3): same wire layout
        // as type 1, so re-using the type-1 vector but forcing the type nibble
        // to 3 must select the ITDMA interpretation. Spec-derived from the
        // raw word: radio=149208 read as ITDMA → sync=(149208>>17)&3=1,
        // slot_increment=(149208>>4)&0x1fff=1133, num_slots=(149208>>1)&7=4,
        // keep_flag=149208&1=0.
        let mut bits = bits_of("177KQJ5000G?tO`K>RA1wUbN0TKH", 0);
        // Overwrite the 6-bit type field with 3 (000011).
        for (k, b) in [0u8, 0, 0, 0, 1, 1].iter().enumerate() {
            bits[k] = *b;
        }
        let c = decode(3, &bits).unwrap()["comm_state"].clone();
        assert_eq!(c["scheme"], "ITDMA");
        assert_eq!(c["sync_state"], 1);
        assert_eq!(c["slot_increment"], 1133);
        assert_eq!(c["num_slots"], 4);
        assert_eq!(c["keep_flag"], false);
    }

    #[test]
    fn comm_state_helper_spec_branches() {
        // Spec-derived unit checks of comm_state() against ITU-R M.1371-5
        // §3.3.7.2.1 hand-built raw words (independent of the bit-offset
        // extraction), covering each slot-timeout branch and the ITDMA case.
        //
        // SOTDMA, slot_timeout=5 (in {3,5,7}) → received_stations.
        // raw = sync(2)=2 | timeout(3)=5 | sub(14)=1000.
        let raw = (2u64 << 17) | (5 << 14) | 1000;
        let c = comm_state(raw, false);
        assert_eq!(c["sync_state"], 2);
        assert_eq!(c["sync_state_text"], "base station");
        assert_eq!(c["slot_timeout"], 5);
        assert_eq!(c["received_stations"], 1000);
        // SOTDMA, slot_timeout=4 (in {2,4,6}) → slot_number.
        let raw = (3u64 << 17) | (4 << 14) | 2249;
        let c = comm_state(raw, false);
        assert_eq!(c["sync_state"], 3);
        assert_eq!(c["slot_number"], 2249);
        // SOTDMA, slot_timeout=1 → UTC hour/minute. sub = hour<<9 | minute<<2.
        let raw = (0u64 << 17) | (1 << 14) | ((23u64 << 9) | (59u64 << 2));
        let c = comm_state(raw, false);
        assert_eq!(c["utc_hour"], 23);
        assert_eq!(c["utc_minute"], 59);
        // ITDMA: increment(13) | num_slots(3) | keep(1).
        // raw = sync=1 | incr=8191 | num_slots=4 | keep=1.
        let raw = (1u64 << 17) | (8191u64 << 4) | (4u64 << 1) | 1;
        let c = comm_state(raw, true);
        assert_eq!(c["scheme"], "ITDMA");
        assert_eq!(c["sync_state"], 1);
        assert_eq!(c["slot_increment"], 8191);
        assert_eq!(c["num_slots"], 4);
        assert_eq!(c["slots_allocated"], 5);
        assert_eq!(c["keep_flag"], true);
    }

    // ----------------------------------------------------------------------
    // DAC=1 IMO SN.1/Circ.289 application-specific messages.
    //
    // ORACLE NOTE: pyais has NO DAC=1 decoder, so there is no OSS decode
    // oracle for these. Each test is SPEC-DERIVED per the mandate: the
    // expected outputs are the documented physical quantities from the IMO
    // circular (cited per FID in the dac1_decode source). The fixtures are
    // built by an INDEPENDENT bit packer (`build_t8_dac1`, a plain MSB-first
    // bit writer that takes (value, width) pairs in document order) which
    // shares no code with the decoder — the decoder reads by (offset, width).
    // A wrong offset or width in the decoder would mismatch the packer, so
    // this is not a self-encode/self-decode loopback of the decode logic.
    // The FID=11 (legacy) vector is additionally cross-checked against the
    // known field ORDER difference vs FID=31 (lat-before-lon), so a copy of
    // FID=31's layout would fail FID=11.
    // ----------------------------------------------------------------------

    /// Independent MSB-first bit packer: push `value` as `width` bits. Bit
    /// positions at or above 64 are zero (so wide spare/fill blocks can be
    /// packed in one call without shifting past the u64 width).
    fn pack(bits: &mut Vec<u8>, value: u64, width: usize) {
        for k in (0..width).rev() {
            let bit = if k < 64 { ((value >> k) & 1) as u8 } else { 0 };
            bits.push(bit);
        }
    }

    /// Pack a signed `value` into `width` two's-complement bits.
    fn pack_i(bits: &mut Vec<u8>, value: i64, width: usize) {
        let masked = (value as u64) & ((1u64 << width) - 1);
        pack(bits, masked, width);
    }

    /// 6-bit ASCII pack of `s` padded with '@' (value 0) to `chars`.
    fn pack_str(bits: &mut Vec<u8>, s: &str, chars: usize) {
        let bytes: Vec<u8> = s.bytes().collect();
        for k in 0..chars {
            let c = bytes.get(k).copied().unwrap_or(b'@');
            // Inverse of fields::sixbit: 'A'..'_' (65..95) → 1..31, ' '..'?' → 32..63, '@' → 0.
            let v = match c {
                b'@' => 0u64,
                65..=95 => (c - 64) as u64,
                32..=63 => c as u64,
                _ => 0,
            };
            pack(bits, v, 6);
        }
    }

    /// Build a type-8 frame: 6-bit type(8) + repeat(2) + mmsi(30) + spare(2)
    /// + DAC(10)=1 + FID(6) header, then the caller-supplied application bits.
    fn build_t8_dac1(fid: u64, app: &[u8]) -> Vec<u8> {
        let mut bits = Vec::new();
        pack(&mut bits, 8, 6); // message type 8
        pack(&mut bits, 0, 2); // repeat indicator
        pack(&mut bits, 123_456_789, 30); // source MMSI
        pack(&mut bits, 0, 2); // spare
        pack(&mut bits, 1, 10); // DAC = 1 (IMO international)
        pack(&mut bits, fid, 6); // FID
        bits.extend_from_slice(app);
        bits
    }

    #[test]
    fn dac1_fid31_met_hydro_spec_example() {
        // IMO SN.1/Circ.289 met/hydro (FID 31). Worked field values:
        // lon = 12.345°, lat = 48.678°, pos-accuracy = 1, day 14 / 13:45 UTC,
        // wind 15 kt avg / 22 kt gust from 270° (gust 280°), air 23.5 °C,
        // humidity 65%, dew point 17.0 °C, pressure 1013 hPa (raw 214),
        // tendency 1 (increasing), visibility 8.0 NM, water level +1.50 m
        // (raw 1150), trend 0, surface current 1.2 kt toward 090°.
        let mut a = Vec::new();
        pack_i(&mut a, (12.345 * 60_000.0_f64).round() as i64, 25); // lon (1/1000 min)
        pack_i(&mut a, (48.678 * 60_000.0_f64).round() as i64, 24); // lat
        pack(&mut a, 1, 1); // position accuracy
        pack(&mut a, 14, 5); // day
        pack(&mut a, 13, 5); // hour
        pack(&mut a, 45, 6); // minute
        pack(&mut a, 15, 7); // wind speed kt
        pack(&mut a, 22, 7); // wind gust kt
        pack(&mut a, 270, 9); // wind dir deg
        pack(&mut a, 280, 9); // wind gust dir deg
        pack_i(&mut a, 235, 11); // air temp 0.1 °C → 23.5
        pack(&mut a, 65, 7); // humidity %
        pack_i(&mut a, 170, 10); // dew point 0.1 °C → 17.0
        pack(&mut a, 214, 9); // pressure raw → 214 + 799 = 1013 hPa
        pack(&mut a, 1, 2); // pressure tendency
        pack(&mut a, 0, 1); // visibility ">" flag
        pack(&mut a, 80, 7); // visibility 0.1 NM → 8.0
        pack(&mut a, 1150, 12); // water level raw → 1150/100 - 10 = +1.50 m
        pack(&mut a, 0, 2); // water level trend
        pack(&mut a, 12, 8); // surface current 0.1 kt → 1.2
        pack(&mut a, 90, 9); // surface current dir deg
        let bits = build_t8_dac1(31, &a);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 1);
        assert_eq!(d["fid"], 31);
        let m = &d["app"];
        assert!((m["lon"].as_f64().unwrap() - 12.345).abs() < 1e-4);
        assert!((m["lat"].as_f64().unwrap() - 48.678).abs() < 1e-4);
        assert_eq!(m["position_accuracy"], true);
        assert_eq!(m["day"], 14);
        assert_eq!(m["hour"], 13);
        assert_eq!(m["minute"], 45);
        assert_eq!(m["wind_speed_kt"], 15);
        assert_eq!(m["wind_gust_kt"], 22);
        assert_eq!(m["wind_dir_deg"], 270);
        assert_eq!(m["wind_gust_dir_deg"], 280);
        assert_eq!(m["air_temp_c"], 23.5);
        assert_eq!(m["humidity_pct"], 65);
        assert_eq!(m["dew_point_c"], 17.0);
        assert_eq!(m["pressure_hpa"], 1013);
        assert_eq!(m["pressure_tendency"], 1);
        assert_eq!(m["visibility_nm"], 8.0);
        assert_eq!(m["water_level_m"], 1.5);
        assert_eq!(m["water_level_trend"], 0);
        assert_eq!(m["surface_current_kt"], 1.2);
        assert_eq!(m["surface_current_dir_deg"], 90);
        assert!(d.get("data_hex").is_none());
    }

    #[test]
    fn dac1_fid11_legacy_met_hydro_lat_before_lon() {
        // IMO SN/Circ.236 legacy met/hydro (FID 11): latitude FIRST (24 bits),
        // then longitude (25). Same physical position as the FID 31 test, but
        // a decoder that copied FID 31's lon-first layout would read these
        // swapped. day 14 / 06:30, wind 10 kt, air 5.0 °C, pressure 1000 hPa
        // (raw 200 → +800), pressure tendency 0.
        let mut a = Vec::new();
        pack_i(&mut a, (48.678 * 60_000.0_f64).round() as i64, 24); // lat first
        pack_i(&mut a, (12.345 * 60_000.0_f64).round() as i64, 25); // lon second
        pack(&mut a, 14, 5); // day
        pack(&mut a, 6, 5); // hour
        pack(&mut a, 30, 6); // minute
        pack(&mut a, 10, 7); // wind speed kt
        pack(&mut a, 127, 7); // wind gust = N/A
        pack(&mut a, 511, 9); // wind dir = N/A
        pack(&mut a, 511, 9); // wind gust dir = N/A
        pack_i(&mut a, 50, 11); // air temp 0.1 °C → 5.0
        pack(&mut a, 127, 7); // humidity N/A
        pack_i(&mut a, 501, 10); // dew point N/A
        pack(&mut a, 200, 9); // pressure raw → 200 + 800 = 1000 hPa
        pack(&mut a, 0, 2); // pressure tendency
        pack(&mut a, 127, 7); // visibility N/A
        pack(&mut a, 511, 9); // water level N/A
        pack(&mut a, 0, 2); // water level trend
        pack(&mut a, 255, 8); // surface current N/A
        pack(&mut a, 511, 9); // surface current dir N/A
        let bits = build_t8_dac1(11, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert!((m["lat"].as_f64().unwrap() - 48.678).abs() < 1e-4);
        assert!((m["lon"].as_f64().unwrap() - 12.345).abs() < 1e-4);
        assert_eq!(m["day"], 14);
        assert_eq!(m["hour"], 6);
        assert_eq!(m["minute"], 30);
        assert_eq!(m["wind_speed_kt"], 10);
        assert_eq!(m["air_temp_c"], 5.0);
        assert_eq!(m["pressure_hpa"], 1000);
        // N/A sentinels are omitted, not emitted as junk.
        assert!(m.get("wind_gust_kt").is_none());
        assert!(m.get("humidity_pct").is_none());
        assert!(m.get("visibility_nm").is_none());
    }

    #[test]
    fn dac1_fid16_persons_on_board() {
        // IMO SN.1/Circ.289 persons-on-board: single 13-bit count.
        let mut a = Vec::new();
        pack(&mut a, 1542, 13);
        pack(&mut a, 0, 30); // trailing spare to fill the slot
        let bits = build_t8_dac1(16, &a);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["app"]["persons_on_board"], 1542);
    }

    #[test]
    fn dac1_fid16_persons_zero_is_not_available() {
        let mut a = Vec::new();
        pack(&mut a, 0, 13); // 0 = not available
        pack(&mut a, 0, 30);
        let bits = build_t8_dac1(16, &a);
        // Whole app object is empty → dac1_decode returns None → data_hex.
        let d = decode(8, &bits).unwrap();
        assert!(d.get("app").is_none());
        assert!(d.get("data_hex").is_some());
    }

    #[test]
    fn dac1_fid17_vts_targets() {
        // IMO SN.1/Circ.289 VTS-generated/synthetic targets: 120-bit records.
        // One target: id-type 0 (MMSI), MMSI 244660000, lat 52.000 / lon
        // 4.000, COG 123°, timestamp 30 s, SOG 12.3 kt.
        let mut a = Vec::new();
        pack(&mut a, 0, 2); // id type = MMSI
        pack(&mut a, 244_660_000u64 << 12, 42); // MMSI in high 30 bits of 42-bit id
        pack(&mut a, 0, 4); // spare
        pack_i(&mut a, (52.000 * 60_000.0_f64).round() as i64, 24); // lat
        pack_i(&mut a, (4.000 * 60_000.0_f64).round() as i64, 25); // lon
        pack(&mut a, 123, 9); // COG
        pack(&mut a, 30, 6); // timestamp
        pack(&mut a, 123, 10); // SOG 0.1 kt → 12.3
        let bits = build_t8_dac1(17, &a);
        let d = decode(8, &bits).unwrap();
        let t = &d["app"]["targets"][0];
        assert_eq!(t["id_type"], 0);
        assert_eq!(t["mmsi"], 244_660_000u64);
        assert!((t["lat"].as_f64().unwrap() - 52.0).abs() < 1e-3);
        assert!((t["lon"].as_f64().unwrap() - 4.0).abs() < 1e-3);
        assert_eq!(t["cog_deg"], 123);
        assert_eq!(t["timestamp_sec"], 30);
        assert_eq!(t["sog_kt"], 12.3);
    }

    #[test]
    fn dac1_fid22_area_notice_header() {
        // IMO SN.1/Circ.289 area notice (broadcast): header + sub-area shapes.
        // linkage 5, notice description 9 (caution: marine mammals), valid from
        // month 6 / day 15 / 08:00, duration 180 min, one 90-bit sub-area.
        let mut a = Vec::new();
        pack(&mut a, 5, 10); // message linkage
        pack(&mut a, 9, 7); // notice description
        pack(&mut a, 6, 4); // start month
        pack(&mut a, 15, 5); // start day
        pack(&mut a, 8, 5); // start hour
        pack(&mut a, 0, 6); // start minute
        pack(&mut a, 180, 18); // duration minutes
        pack(&mut a, 0, 90); // one sub-area shape record (geometry deferred)
        let bits = build_t8_dac1(22, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert_eq!(m["message_linkage"], 5);
        assert_eq!(m["notice_description"], 9);
        assert_eq!(m["start_month"], 6);
        assert_eq!(m["start_day"], 15);
        assert_eq!(m["start_hour"], 8);
        assert_eq!(m["start_minute"], 0);
        assert_eq!(m["duration_min"], 180);
        assert_eq!(m["sub_area_count"], 1);
    }

    #[test]
    fn dac1_fid24_extended_static() {
        // IMO SN.1/Circ.289 extended ship static/voyage: linkage 3, air draught
        // 25.5 m (raw 255), last port "NLRTM", next port "DEHAM".
        let mut a = Vec::new();
        pack(&mut a, 3, 10); // message linkage
        pack(&mut a, 255, 13); // air draught 0.1 m → 25.5
        pack_str(&mut a, "NLRTM", 5); // last port (UN/LOCODE)
        pack_str(&mut a, "DEHAM", 5); // next port
        pack_str(&mut a, "@@@@@", 5); // second next port = unused
        pack(&mut a, 0, 100); // remaining cargo table (deferred)
        let bits = build_t8_dac1(24, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert_eq!(m["message_linkage"], 3);
        assert_eq!(m["air_draught_m"], 25.5);
        assert_eq!(m["last_port"], "NLRTM");
        assert_eq!(m["next_port"], "DEHAM");
        assert!(m.get("second_next_port").is_none());
    }

    #[test]
    fn dac1_fid25_dangerous_cargo() {
        // IMO SN.1/Circ.289 dangerous cargo indication: linkage 7, amount unit
        // 1 (tons), amount 500, two 17-bit cargo codes present.
        let mut a = Vec::new();
        pack(&mut a, 7, 10); // message linkage
        pack(&mut a, 1, 2); // amount unit
        pack(&mut a, 500, 10); // amount
        pack(&mut a, 0, 17); // cargo code 1 (codes deferred)
        pack(&mut a, 0, 17); // cargo code 2
        let bits = build_t8_dac1(25, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert_eq!(m["message_linkage"], 7);
        assert_eq!(m["amount_unit"], 1);
        assert_eq!(m["amount"], 500);
        assert_eq!(m["cargo_item_count"], 2);
    }

    #[test]
    fn dac1_fid26_environmental_header() {
        // IMO SN.1/Circ.289 environmental: site position + day/time header,
        // then N sensor report blocks. Position 50.5 / 1.25, day 20 / 09:15,
        // one 85-bit sensor report.
        let mut a = Vec::new();
        pack_i(&mut a, (1.25 * 60_000.0_f64).round() as i64, 25); // lon
        pack_i(&mut a, (50.5 * 60_000.0_f64).round() as i64, 24); // lat
        pack(&mut a, 20, 5); // day
        pack(&mut a, 9, 5); // hour
        pack(&mut a, 15, 6); // minute
        pack(&mut a, 0, 85); // one sensor report block (deferred)
        let bits = build_t8_dac1(26, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert!((m["lon"].as_f64().unwrap() - 1.25).abs() < 1e-3);
        assert!((m["lat"].as_f64().unwrap() - 50.5).abs() < 1e-3);
        assert_eq!(m["day"], 20);
        assert_eq!(m["hour"], 9);
        assert_eq!(m["minute"], 15);
        assert_eq!(m["sensor_report_count"], 1);
    }

    #[test]
    fn dac1_fid27_route_information() {
        // IMO SN.1/Circ.289 route information (broadcast): linkage 2, sender
        // class 0, route type 1 (mandatory), valid from month 7 / day 4 /
        // 12:00, duration 360 min, 2 waypoints at 1/10000-min (raw/600000)
        // resolution: (4.0, 52.0) and (4.5, 52.5).
        let mut a = Vec::new();
        pack(&mut a, 2, 10); // message linkage
        pack(&mut a, 0, 3); // sender class
        pack(&mut a, 1, 5); // route type
        pack(&mut a, 7, 4); // start month
        pack(&mut a, 4, 5); // start day
        pack(&mut a, 12, 5); // start hour
        pack(&mut a, 0, 6); // start minute
        pack(&mut a, 360, 18); // duration minutes
        pack(&mut a, 2, 5); // waypoint count
        pack_i(&mut a, (4.0 * 600_000.0_f64).round() as i64, 28); // wp1 lon (1/10000 min)
        pack_i(&mut a, (52.0 * 600_000.0_f64).round() as i64, 27); // wp1 lat
        pack_i(&mut a, (4.5 * 600_000.0_f64).round() as i64, 28); // wp2 lon
        pack_i(&mut a, (52.5 * 600_000.0_f64).round() as i64, 27); // wp2 lat
        let bits = build_t8_dac1(27, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert_eq!(m["message_linkage"], 2);
        assert_eq!(m["sender_class"], 0);
        assert_eq!(m["route_type"], 1);
        assert_eq!(m["start_month"], 7);
        assert_eq!(m["duration_min"], 360);
        assert_eq!(m["waypoint_count"], 2);
        let wps = m["waypoints"].as_array().unwrap();
        assert_eq!(wps.len(), 2);
        assert!((wps[0]["lon"].as_f64().unwrap() - 4.0).abs() < 1e-4);
        assert!((wps[0]["lat"].as_f64().unwrap() - 52.0).abs() < 1e-4);
        assert!((wps[1]["lon"].as_f64().unwrap() - 4.5).abs() < 1e-4);
        assert!((wps[1]["lat"].as_f64().unwrap() - 52.5).abs() < 1e-4);
    }

    #[test]
    fn dac1_fid29_text_description() {
        // IMO SN.1/Circ.289 text description (broadcast): linkage + 6-bit text.
        let mut a = Vec::new();
        pack(&mut a, 4, 10); // message linkage
        pack_str(&mut a, "PILOT ON BOARD", 14);
        let bits = build_t8_dac1(29, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert_eq!(m["message_linkage"], 4);
        assert_eq!(m["text"], "PILOT ON BOARD");
    }

    #[test]
    fn dac1_fid32_tidal_window() {
        // IMO SN.1/Circ.289 tidal window: header (linkage, month, day) + 88-bit
        // window records. linkage 6, month 7, day 4; one window at 51.0 / 3.0,
        // 06:00–09:30, current 045° at 1.5 kt.
        let mut a = Vec::new();
        pack(&mut a, 6, 10); // message linkage
        pack(&mut a, 7, 4); // month
        pack(&mut a, 4, 5); // day
        // window record (88 bits)
        pack_i(&mut a, (3.0 * 60_000.0_f64).round() as i64, 25); // lon
        pack_i(&mut a, (51.0 * 60_000.0_f64).round() as i64, 24); // lat
        pack(&mut a, 6, 5); // from hour
        pack(&mut a, 0, 6); // from minute
        pack(&mut a, 9, 5); // to hour
        pack(&mut a, 30, 6); // to minute
        pack(&mut a, 45, 9); // current direction
        pack(&mut a, 15, 8); // current speed 0.1 kt → 1.5
        let bits = build_t8_dac1(32, &a);
        let d = decode(8, &bits).unwrap();
        let m = &d["app"];
        assert_eq!(m["message_linkage"], 6);
        assert_eq!(m["month"], 7);
        assert_eq!(m["day"], 4);
        let w = &m["tidal_windows"][0];
        assert!((w["lon"].as_f64().unwrap() - 3.0).abs() < 1e-3);
        assert!((w["lat"].as_f64().unwrap() - 51.0).abs() < 1e-3);
        assert_eq!(w["from"], "06:00");
        assert_eq!(w["to"], "09:30");
        assert_eq!(w["current_dir_deg"], 45);
        assert_eq!(w["current_speed_kt"], 1.5);
    }

    // ----------------------------------------------------------------------
    // DAC=200 Inland AIS message-6 application messages: FID 21 (ETA), FID 22
    // (RTA), FID 55 (number of persons on board); plus regional AtoN
    // monitoring (DAC 235/250 FID 10) and the header-only regional DACs.
    //
    // ORACLE NOTE: pyais has NO decoder for any of these (it only ships
    // DAC=200 FID 10/23/24/40). The layouts are SPEC-DERIVED from UNECE
    // ECE/TRANS/SC.3/176 (Inland AIS) and the AIVDM/AIVDO reference, cross-
    // checked between two independent sources (the IALA ASM registry /
    // e-Navigation.nl and gpsd's AIVDM.html) which agree field-for-field —
    // see the per-FID citation in `asm_decode`. Fixtures are built by the
    // INDEPENDENT MSB-first packer (`pack`/`pack_i`/`pack_str`), which shares
    // no code with the by-offset decoder, so this is not a self-loopback.
    // ----------------------------------------------------------------------

    /// Build a message-6 (addressed) frame for DAC=200: type(6) + repeat(2) +
    /// mmsi(30) + seqno(2) + dest_mmsi(30) + retransmit(1) + spare(1) +
    /// DAC(10)=200 + FID(6) header (88 bits), then the application bits.
    fn build_t6_dac200(fid: u64, app: &[u8]) -> Vec<u8> {
        let mut bits = Vec::new();
        pack(&mut bits, 6, 6); // message type 6
        pack(&mut bits, 0, 2); // repeat
        pack(&mut bits, 211_000_001, 30); // source MMSI (German inland prefix 211)
        pack(&mut bits, 0, 2); // sequence number
        pack(&mut bits, 211_000_002, 30); // destination MMSI
        pack(&mut bits, 0, 1); // retransmit
        pack(&mut bits, 0, 1); // spare
        pack(&mut bits, 200, 10); // DAC = 200 (Inland AIS)
        pack(&mut bits, fid, 6); // FID
        bits.extend_from_slice(app);
        bits
    }

    /// Build a generic message-6 frame for an arbitrary DAC/FID.
    fn build_t6(dac: u64, fid: u64, app: &[u8]) -> Vec<u8> {
        let mut bits = Vec::new();
        pack(&mut bits, 6, 6);
        pack(&mut bits, 0, 2);
        pack(&mut bits, 235_000_001, 30);
        pack(&mut bits, 0, 2);
        pack(&mut bits, 235_000_002, 30);
        pack(&mut bits, 0, 1);
        pack(&mut bits, 0, 1);
        pack(&mut bits, dac, 10);
        pack(&mut bits, fid, 6);
        bits.extend_from_slice(app);
        bits
    }

    #[test]
    fn dac200_fid21_eta_lock_bridge_terminal() {
        // Inland AIS ETA report: country "DE", LOCODE "DUI" (Duisburg),
        // fairway section "10010", terminal "T01AB", hectometre "12345",
        // ETA 6-15 14:30, 2 assisting tugs, air draught 7.25 m (raw 725).
        let mut a = Vec::new();
        pack_str(&mut a, "DE", 2); // UN country code
        pack_str(&mut a, "DUI", 3); // UN/LOCODE
        pack_str(&mut a, "10010", 5); // fairway section number
        pack_str(&mut a, "T01AB", 5); // terminal code
        pack_str(&mut a, "12345", 5); // fairway hectometre
        pack(&mut a, 6, 4); // ETA month
        pack(&mut a, 15, 5); // ETA day
        pack(&mut a, 14, 5); // ETA hour
        pack(&mut a, 30, 6); // ETA minute
        pack(&mut a, 2, 3); // assisting tugs
        pack(&mut a, 725, 12); // air draught 0.01 m → 7.25
        pack(&mut a, 0, 5); // spare
        let bits = build_t6_dac200(21, &a);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 21);
        let m = &d["app"];
        assert_eq!(m["inland_country"], "DE");
        assert_eq!(m["un_locode"], "DUI");
        assert_eq!(m["fairway_section"], "10010");
        assert_eq!(m["terminal_code"], "T01AB");
        assert_eq!(m["fairway_hectometre"], "12345");
        assert_eq!(m["month"], 6);
        assert_eq!(m["day"], 15);
        assert_eq!(m["hour"], 14);
        assert_eq!(m["minute"], 30);
        assert_eq!(m["assisting_tugs"], 2);
        assert_eq!(m["air_draught_m"], 7.25);
        assert!(d.get("data_hex").is_none());
    }

    #[test]
    fn dac200_fid21_eta_na_sentinels_omitted() {
        // All time/tug/air-draught sentinels → omitted keys (not junk values).
        let mut a = Vec::new();
        pack_str(&mut a, "NL", 2);
        pack_str(&mut a, "RTM", 3);
        pack_str(&mut a, "@@@@@", 5); // empty fairway section
        pack_str(&mut a, "@@@@@", 5); // empty terminal
        pack_str(&mut a, "@@@@@", 5); // empty hectometre
        pack(&mut a, 0, 4); // month N/A
        pack(&mut a, 0, 5); // day N/A
        pack(&mut a, 24, 5); // hour N/A
        pack(&mut a, 60, 6); // minute N/A
        pack(&mut a, 7, 3); // tugs unknown
        pack(&mut a, 0, 12); // air draught not used
        pack(&mut a, 0, 5);
        let bits = build_t6_dac200(21, &a);
        let m = &decode(6, &bits).unwrap()["app"];
        assert_eq!(m["inland_country"], "NL");
        assert_eq!(m["un_locode"], "RTM");
        assert!(m.get("fairway_section").is_none());
        assert!(m.get("month").is_none());
        assert!(m.get("day").is_none());
        assert!(m.get("hour").is_none());
        assert!(m.get("minute").is_none());
        assert!(m.get("assisting_tugs").is_none());
        assert!(m.get("air_draught_m").is_none());
    }

    #[test]
    fn dac200_fid22_rta_lock_bridge_terminal() {
        // Inland AIS RTA reply: country "BE", LOCODE "ANR" (Antwerp), fairway
        // section "20020", terminal "L05CD", hectometre "06789", RTA 7-04
        // 09:15, status 1 (limited operation).
        let mut a = Vec::new();
        pack_str(&mut a, "BE", 2);
        pack_str(&mut a, "ANR", 3);
        pack_str(&mut a, "20020", 5);
        pack_str(&mut a, "L05CD", 5);
        pack_str(&mut a, "06789", 5);
        pack(&mut a, 7, 4); // RTA month
        pack(&mut a, 4, 5); // RTA day
        pack(&mut a, 9, 5); // RTA hour
        pack(&mut a, 15, 6); // RTA minute
        pack(&mut a, 1, 2); // status: limited operation
        pack(&mut a, 0, 2); // spare
        let bits = build_t6_dac200(22, &a);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 22);
        let m = &d["app"];
        assert_eq!(m["inland_country"], "BE");
        assert_eq!(m["un_locode"], "ANR");
        assert_eq!(m["fairway_section"], "20020");
        assert_eq!(m["terminal_code"], "L05CD");
        assert_eq!(m["fairway_hectometre"], "06789");
        assert_eq!(m["month"], 7);
        assert_eq!(m["day"], 4);
        assert_eq!(m["hour"], 9);
        assert_eq!(m["minute"], 15);
        assert_eq!(m["status"], 1);
        // RTA has no air-draught / tugs fields.
        assert!(m.get("assisting_tugs").is_none());
        assert!(m.get("air_draught_m").is_none());
    }

    #[test]
    fn dac200_fid55_number_of_persons() {
        // Inland AIS number-of-persons: 4 crew, 250 passengers, 3 personnel.
        let mut a = Vec::new();
        pack(&mut a, 4, 8); // crew
        pack(&mut a, 250, 13); // passengers
        pack(&mut a, 3, 8); // shipboard personnel
        pack(&mut a, 0, 51); // spare
        let bits = build_t6_dac200(55, &a);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dac"], 200);
        assert_eq!(d["fid"], 55);
        let m = &d["app"];
        assert_eq!(m["crew"], 4);
        assert_eq!(m["passengers"], 250);
        assert_eq!(m["personnel"], 3);
        assert!(d.get("data_hex").is_none());
    }

    #[test]
    fn dac200_fid55_unknown_sentinels_omitted() {
        // crew 255 / passengers 8191 / personnel 255 are all "unknown".
        let mut a = Vec::new();
        pack(&mut a, 255, 8);
        pack(&mut a, 8191, 13);
        pack(&mut a, 255, 8);
        pack(&mut a, 0, 51);
        let bits = build_t6_dac200(55, &a);
        // Every field unknown → app object empty → None → data_hex fallback.
        let d = decode(6, &bits).unwrap();
        assert!(d.get("app").is_none());
        assert!(d.get("data_hex").is_some());
    }

    #[test]
    fn dac200_fid55_partial_known() {
        // Only crew known (12); passengers & personnel unknown.
        let mut a = Vec::new();
        pack(&mut a, 12, 8);
        pack(&mut a, 8191, 13);
        pack(&mut a, 255, 8);
        pack(&mut a, 0, 51);
        let bits = build_t6_dac200(55, &a);
        let m = &decode(6, &bits).unwrap()["app"];
        assert_eq!(m["crew"], 12);
        assert!(m.get("passengers").is_none());
        assert!(m.get("personnel").is_none());
    }

    #[test]
    fn dac235_fid10_aton_monitoring() {
        // UK/Ireland AtoN monitoring (DAC 235, FID 10). internal 12.00 V
        // (raw 240), ext#1 6.05 V (raw 121), ext#2 0.05 V (raw 1), RACON
        // status 2, light status 1, health alarm set, status external 0xA5,
        // off-position true.
        let mut a = Vec::new();
        pack(&mut a, 240, 10); // analogue internal: 240 * 0.05 = 12.0 V
        pack(&mut a, 121, 10); // analogue external #1: 121 * 0.05 = 6.05 V
        pack(&mut a, 1, 10); // analogue external #2: 0.05 V
        pack(&mut a, 2, 2); // RACON status
        pack(&mut a, 1, 2); // light status
        pack(&mut a, 1, 1); // health alarm
        pack(&mut a, 0xA5, 8); // status external
        pack(&mut a, 1, 1); // off-position
        pack(&mut a, 0, 4); // spare
        let bits = build_t6(235, 10, &a);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dac"], 235);
        assert_eq!(d["fid"], 10);
        let m = &d["app"];
        assert!((m["voltage_internal"].as_f64().unwrap() - 12.0).abs() < 1e-9);
        assert!((m["voltage_external_1"].as_f64().unwrap() - 6.05).abs() < 1e-9);
        assert!((m["voltage_external_2"].as_f64().unwrap() - 0.05).abs() < 1e-9);
        assert_eq!(m["racon_status"], 2);
        assert_eq!(m["light_status"], 1);
        assert_eq!(m["health_alarm"], true);
        assert_eq!(m["status_external"], 0xA5);
        assert_eq!(m["off_position"], true);
        assert!(d.get("data_hex").is_none());
    }

    #[test]
    fn dac250_fid10_aton_shares_layout() {
        // Ireland (DAC 250) uses the same FID-10 AtoN layout as the UK.
        let mut a = Vec::new();
        pack(&mut a, 200, 10); // 10.0 V
        pack(&mut a, 0, 10);
        pack(&mut a, 0, 10);
        pack(&mut a, 0, 2);
        pack(&mut a, 0, 2);
        pack(&mut a, 0, 1);
        pack(&mut a, 0, 8);
        pack(&mut a, 0, 1);
        pack(&mut a, 0, 4);
        let bits = build_t6(250, 10, &a);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dac"], 250);
        assert!((d["app"]["voltage_internal"].as_f64().unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn regional_dacs_emit_header_only() {
        // DAC 366/316 (Seaway), 367 (US env), 265 (Sweden STM): no clean-room
        // body layout, so a header-only identification is emitted and the raw
        // body is preserved as `body_hex` for downstream re-parse.
        for (dac, fid, region) in [
            (366u64, 1u64, "US/Canada Seaway (PAWSS)"),
            (316, 2, "US/Canada Seaway (PAWSS)"),
            (367, 33, "US environmental/area-notice"),
            (265, 1, "Sweden STM route"),
        ] {
            // Type-8 broadcast carrier (these DACs are broadcast in practice).
            let mut bits = Vec::new();
            pack(&mut bits, 8, 6);
            pack(&mut bits, 0, 2);
            pack(&mut bits, 366_000_001, 30);
            pack(&mut bits, 0, 2);
            pack(&mut bits, dac, 10);
            pack(&mut bits, fid, 6);
            pack(&mut bits, 0xDEAD_BEEF, 64); // body bits → 8 octets of hex
            let d = decode(8, &bits).unwrap();
            assert_eq!(d["dac"], dac);
            assert_eq!(d["fid"], fid);
            let app = &d["app"];
            assert_eq!(app["region"], region);
            assert_eq!(app["fid"], fid);
            // Body preserved (64 bits → 16 hex chars), structured app emitted
            // instead of a top-level data_hex.
            assert_eq!(app["body_hex"].as_str().unwrap().len(), 16);
            assert!(d.get("data_hex").is_none());
        }
    }
}
