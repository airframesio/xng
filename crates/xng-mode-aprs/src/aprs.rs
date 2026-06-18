//! APRS payload parsing (the application layer).
//!
//! Reference: **APRS Protocol Reference, Protocol Version 1.0.1** (Bob
//! Bruninga et al., 2000), referred to below as "APRS 1.0.1". Page/chapter
//! citations are inline at each parser and in the tests.
//!
//! The APRS info field is dispatched on its first byte, the *data-type
//! identifier* (APRS 1.0.1 Chapter 5, "APRS Data Types", and the
//! "APRS Data Type Identifiers" table on p.17):
//!
//! - `!` `=`         — position without / with messaging, no timestamp
//! - `/` `@`         — position with timestamp (no / with messaging)
//! - `_`             — weather report (positionless)
//! - `:`             — message, or bulletin/announcement (addressee `BLNn`)
//! - `>`             — status (free text, or Maidenhead grid locator)
//! - `;`             — object
//! - `)`             — item report
//! - `?`             — general query
//! - `T` (`T#...`)   — telemetry
//!
//! Mic-E (`` ` ``, `'`, 0x1c, 0x1d) is dispatched one level up, in
//! [`crate::decode_frame`], because it carries its latitude in the AX.25
//! destination address (see [`crate::mice`]).
//!
//! Position reports come in two forms (APRS 1.0.1 Chapter 6):
//! - **uncompressed**: `DDMM.mmN/DDDMM.mmW$` (lat 8 chars, sym-table id,
//!   lon 9 chars, symbol code), then an optional 7-byte Data Extension
//!   (course/speed, PHG, DFS or RNG — Chapter 7) and comment.
//! - **compressed** (Chapter 9): a Base-91 encoding — sym-table id, 4 bytes
//!   latitude, 4 bytes longitude, symbol code, 2 bytes course/speed (or radio
//!   range or altitude), compression-type byte.

use serde::Serialize;
use serde_json::json;

/// The decoded class of an APRS packet (becomes the `kind` string on the bus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AprsKind {
    Position,
    Weather,
    Message,
    Status,
    Object,
    Item,
    Telemetry,
    /// Telemetry PARM. definition: the names of the 5 analog + 8 digital
    /// channels (APRS 1.0.1 Chapter 13, p.69).
    TelemetryParm,
    /// Telemetry UNIT. definition: the units/labels of the 5 analog + 8 digital
    /// channels (APRS 1.0.1 Chapter 13, p.69).
    TelemetryUnit,
    /// Telemetry EQNS. definition: the 3 quadratic coefficients (a,b,c) per
    /// analog channel (APRS 1.0.1 Chapter 13, p.70).
    TelemetryEqns,
    /// Telemetry BITS. definition: the 8 digital bit-sense flags + project
    /// title (APRS 1.0.1 Chapter 13, p.70).
    TelemetryBits,
    MicE,
    Bulletin,
    Query,
    Raw,
}

impl AprsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AprsKind::Position => "position",
            AprsKind::Weather => "weather",
            AprsKind::Message => "message",
            AprsKind::Status => "status",
            AprsKind::Object => "object",
            AprsKind::Item => "item",
            AprsKind::Telemetry => "telemetry",
            AprsKind::TelemetryParm => "telemetry-parm",
            AprsKind::TelemetryUnit => "telemetry-unit",
            AprsKind::TelemetryEqns => "telemetry-eqns",
            AprsKind::TelemetryBits => "telemetry-bits",
            AprsKind::MicE => "mic-e",
            AprsKind::Bulletin => "bulletin",
            AprsKind::Query => "query",
            AprsKind::Raw => "raw",
        }
    }
}

/// A parsed APRS payload: a class plus a JSON object of decoded fields.
#[derive(Debug, Clone, Serialize)]
pub struct AprsPayload {
    #[serde(skip)]
    pub kind: AprsKind,
    /// Decoded fields (lat/lon/symbol/comment/...), as a JSON object.
    pub fields: serde_json::Value,
}

/// Parse an APRS information field into a class + decoded fields.
///
/// `info` is the AX.25 UI info field bytes. Always returns a payload; when
/// nothing more specific can be decoded the kind is [`AprsKind::Raw`] and the
/// raw text is preserved.
pub fn parse(info: &[u8]) -> AprsPayload {
    let text = String::from_utf8_lossy(info).to_string();
    if info.is_empty() {
        return raw(&text);
    }
    // APRS 1.0.1 p.17 — dispatch on the data-type identifier.
    match info[0] {
        b'!' | b'=' => parse_position(info, false),
        b'/' | b'@' => parse_position(info, true),
        b'_' => parse_weather_positionless(info),
        b':' => parse_message(info),
        b'>' => parse_status(info),
        b';' => parse_object(info),
        b')' => parse_item(info),
        b'?' => parse_query(info),
        b'T' => parse_telemetry(info),
        _ => raw(&text),
    }
}

fn raw(text: &str) -> AprsPayload {
    AprsPayload {
        kind: AprsKind::Raw,
        fields: json!({ "info": text }),
    }
}

/// Parse a position report (`!`, `=`, `/`, `@`). When `has_timestamp` is true
/// a 7-char timestamp directly follows the data-type id (APRS 1.0.1
/// Chapter 6, "Time Formats" p.22). Handles both uncompressed and Base-91
/// compressed lat/lon (Chapter 9).
fn parse_position(info: &[u8], has_timestamp: bool) -> AprsPayload {
    let mut idx = 1usize;
    let mut timestamp: Option<String> = None;
    if has_timestamp {
        if info.len() < 8 {
            return raw(&String::from_utf8_lossy(info));
        }
        timestamp = Some(String::from_utf8_lossy(&info[1..8]).to_string());
        idx = 8;
    }
    let rest = &info[idx..];
    if rest.is_empty() {
        return raw(&String::from_utf8_lossy(info));
    }

    // Compressed vs uncompressed: the uncompressed form starts with a digit
    // (latitude DD). The compressed form starts with the symbol-table id,
    // which is one of `/`, `\`, A-Z, a-j (never a digit). APRS 1.0.1 p.36.
    let first = rest[0];
    if first.is_ascii_digit() {
        parse_uncompressed_position(rest, timestamp)
    } else if rest.len() >= 13 {
        parse_compressed_position(rest, timestamp)
    } else {
        raw(&String::from_utf8_lossy(info))
    }
}

/// Uncompressed position: `DDMM.mmN<sym-table>DDDMM.mmW<sym-code>` + comment.
/// APRS 1.0.1 Chapter 6, "Position with no Timestamp" (p.32). Worked example
/// (p.32): `!4903.50N/07201.75W-` decodes to 49°03.50'N, 072°01.75'W with the
/// primary symbol table and symbol `-` (house).
fn parse_uncompressed_position(rest: &[u8], timestamp: Option<String>) -> AprsPayload {
    // Need at least 8 (lat) + 1 (sym table) + 9 (lon) + 1 (sym code) = 19.
    if rest.len() < 19 {
        return raw_pos(rest, timestamp);
    }
    let lat_s = &rest[0..8]; // DDMM.mmH
    let sym_table = rest[8] as char;
    let lon_s = &rest[9..18]; // DDDMM.mmH
    let sym_code = rest[18] as char;
    let after_sym = &rest[19..];

    let lat = parse_lat(lat_s);
    let lon = parse_lon(lon_s);
    let (Some(lat), Some(lon)) = (lat, lon) else {
        return raw_pos(rest, timestamp);
    };

    // A fixed-length 7-byte APRS Data Extension may immediately follow the
    // symbol code (APRS 1.0.1 Chapter 7, p.27). Decode it and strip it from
    // the comment when present.
    let (ext, comment_bytes) = parse_data_extension(after_sym);
    let comment = String::from_utf8_lossy(comment_bytes).trim().to_string();

    let mut fields = json!({
        "lat": lat,
        "lon": lon,
        "symbol_table": sym_table.to_string(),
        "symbol_code": sym_code.to_string(),
        "comment": comment,
        "compressed": false,
    });
    if let serde_json::Value::Object(m) = ext {
        if let serde_json::Value::Object(fm) = &mut fields {
            for (k, v) in m {
                fm.insert(k, v);
            }
        }
    }
    if let Some(ts) = timestamp {
        fields["timestamp"] = json!(ts);
    }
    AprsPayload {
        kind: AprsKind::Position,
        fields,
    }
}

/// Parse the optional 7-byte APRS Data Extension that may follow position
/// data (APRS 1.0.1 Chapter 7, "APRS Data Extensions", p.27-30). Returns the
/// decoded extension fields plus the remaining comment bytes (with the
/// extension stripped off the front). Recognized forms:
///
/// - `CSE/SPD` — course/speed: `nnn/nnn` (p.27).
/// - `PHGphgd` — power/height/gain/directivity (p.28).
/// - `RNGrrrr` — pre-calculated radio range, miles (p.29).
/// - `DFSshgd` — DF signal strength / height / gain / directivity (p.30).
///
/// Anything else is left untouched as comment text.
fn parse_data_extension(s: &[u8]) -> (serde_json::Value, &[u8]) {
    let mut out = serde_json::Map::new();
    if s.len() < 7 {
        return (serde_json::Value::Object(out), s);
    }
    let head = &s[0..7];

    // PHGphgd (p.28): literal "PHG" then 4 digit codes.
    if &head[0..3] == b"PHG" {
        if let Some(phg) = decode_phg(&head[3..7]) {
            return (phg, &s[7..]);
        }
    }
    // DFSshgd (p.30): literal "DFS" then 4 digit codes.
    if &head[0..3] == b"DFS" {
        if let Some(dfs) = decode_dfs(&head[3..7]) {
            return (dfs, &s[7..]);
        }
    }
    // RNGrrrr (p.29): literal "RNG" then 4-digit range in miles.
    if &head[0..3] == b"RNG" {
        if let Ok(txt) = std::str::from_utf8(&head[3..7]) {
            if let Ok(rng) = txt.trim().parse::<u32>() {
                out.insert("radio_range_miles".into(), json!(rng));
                return (serde_json::Value::Object(out), &s[7..]);
            }
        }
    }
    // CSE/SPD (p.27): `nnn/nnn`, course then speed. The 4th byte is '/'.
    if head[3] == b'/' && head[0..3].iter().all(|c| c.is_ascii_digit()) {
        let course: Option<u32> = std::str::from_utf8(&head[0..3])
            .ok()
            .and_then(|t| t.parse().ok());
        let speed: Option<u32> = std::str::from_utf8(&head[4..7])
            .ok()
            .and_then(|t| t.parse().ok());
        if let (Some(course), Some(speed)) = (course, speed) {
            out.insert("course_deg".into(), json!(course));
            out.insert("speed_knots".into(), json!(speed));
            return (serde_json::Value::Object(out), &s[7..]);
        }
    }
    (serde_json::Value::Object(out), s)
}

/// PHG code table (APRS 1.0.1 Chapter 7, p.28 "PHG Codes"). The 4 chars are
/// power, height, gain, directivity codes. Worked example (p.28-29):
/// `PHG5132` => power 25 W, height 20 ft, gain 3 dB, directivity 90° (East).
fn decode_phg(codes: &[u8]) -> Option<serde_json::Value> {
    let p = codes[0];
    let h = codes[1];
    let g = codes[2];
    let d = codes[3];
    if !p.is_ascii_digit() || !d.is_ascii_digit() {
        return None;
    }
    // power = p^2 watts (p.29). height = 10 * 2^h feet. gain = g dB.
    let pv = (p - b'0') as i64;
    let power = pv * pv;
    // Height code may be any char 0-9 and above (p.28); use 2^(h-'0').
    let hv = (h as i64) - (b'0' as i64);
    let height = if hv >= 0 { 10i64 * (1i64 << hv) } else { 0 };
    let gv = (g as i64) - (b'0' as i64);
    let dv = (d - b'0') as i64;
    // Directivity code: 0=omni, 1-8 = 45..360 in 45-deg steps (1=45 NE ...
    // 8=360 N), per the p.28 table.
    let dir_deg = if dv == 0 { None } else { Some(dv * 45) };
    let mut m = serde_json::Map::new();
    m.insert("phg_power_w".into(), json!(power));
    m.insert("phg_height_ft".into(), json!(height));
    m.insert("phg_gain_db".into(), json!(gv));
    if let Some(deg) = dir_deg {
        m.insert("phg_directivity_deg".into(), json!(deg));
    } else {
        m.insert("phg_directivity_deg".into(), json!("omni"));
    }
    Some(serde_json::Value::Object(m))
}

/// DFS code table (APRS 1.0.1 Chapter 7, p.30 "DFS Codes"). The 4 chars are
/// strength (S-points), height, gain, directivity. Worked example (p.30):
/// `DFS2360` => strength S2, height 80 ft, gain 6 dB, directivity 270° (W).
fn decode_dfs(codes: &[u8]) -> Option<serde_json::Value> {
    let strength = codes[0];
    let h = codes[1];
    let g = codes[2];
    let d = codes[3];
    if !strength.is_ascii_digit()
        || !h.is_ascii_digit()
        || !g.is_ascii_digit()
        || !d.is_ascii_digit()
    {
        return None;
    }
    let sv = (strength - b'0') as i64;
    let hv = (h - b'0') as i64;
    let height = 10i64 * (1i64 << hv);
    let gv = (g - b'0') as i64;
    let dv = (d - b'0') as i64;
    let dir_deg = if dv == 0 { None } else { Some(dv * 45) };
    let mut m = serde_json::Map::new();
    m.insert("dfs_strength_s".into(), json!(sv));
    m.insert("dfs_height_ft".into(), json!(height));
    m.insert("dfs_gain_db".into(), json!(gv));
    if let Some(deg) = dir_deg {
        m.insert("dfs_directivity_deg".into(), json!(deg));
    } else {
        m.insert("dfs_directivity_deg".into(), json!("omni"));
    }
    Some(serde_json::Value::Object(m))
}

fn raw_pos(rest: &[u8], timestamp: Option<String>) -> AprsPayload {
    let mut p = raw(&String::from_utf8_lossy(rest));
    if let (Some(ts), serde_json::Value::Object(m)) = (timestamp, &mut p.fields) {
        m.insert("timestamp".into(), json!(ts));
    }
    p.kind = AprsKind::Position;
    p
}

/// Parse `DDMM.mmH` latitude (8 chars, H ∈ {N,S}). APRS 1.0.1 p.32. Returns
/// decimal degrees, negative for S. Returns `None` on malformed input.
fn parse_lat(s: &[u8]) -> Option<f64> {
    if s.len() != 8 {
        return None;
    }
    let dd: f64 = std::str::from_utf8(&s[0..2]).ok()?.trim().parse().ok()?;
    let mm: f64 = std::str::from_utf8(&s[2..7]).ok()?.trim().parse().ok()?; // MM.mm
    let hemi = s[7];
    if s[4] != b'.' {
        return None;
    }
    let deg = dd + mm / 60.0;
    match hemi {
        b'N' => Some(deg),
        b'S' => Some(-deg),
        _ => None,
    }
}

/// Parse `DDDMM.mmH` longitude (9 chars, H ∈ {E,W}). APRS 1.0.1 p.32.
fn parse_lon(s: &[u8]) -> Option<f64> {
    if s.len() != 9 {
        return None;
    }
    let ddd: f64 = std::str::from_utf8(&s[0..3]).ok()?.trim().parse().ok()?;
    let mm: f64 = std::str::from_utf8(&s[3..8]).ok()?.trim().parse().ok()?;
    let hemi = s[8];
    if s[5] != b'.' {
        return None;
    }
    let deg = ddd + mm / 60.0;
    match hemi {
        b'E' => Some(deg),
        b'W' => Some(-deg),
        _ => None,
    }
}

/// Base-91 compressed position. APRS 1.0.1 Chapter 9 (p.36):
/// `<sym-table> YYYY XXXX <sym-code> cs T`, where YYYY and XXXX are 4 Base-91
/// digits each. lat = 90 - N/380926, lon = -180 + N/190463 where N is the
/// 4-digit Base-91 value (each char minus 33, base 91).
fn parse_compressed_position(rest: &[u8], timestamp: Option<String>) -> AprsPayload {
    // sym-table(1) lat(4) lon(4) sym-code(1) cs(2) comp-type(1) = 13.
    let sym_table = rest[0] as char;
    let lat_n = base91_4(&rest[1..5]);
    let lon_n = base91_4(&rest[5..9]);
    let sym_code = rest[9] as char;
    let (Some(lat_n), Some(lon_n)) = (lat_n, lon_n) else {
        return raw_pos(rest, timestamp);
    };
    // APRS 1.0.1 p.38 conversion formulas.
    let lat = 90.0 - (lat_n as f64) / 380926.0;
    let lon = -180.0 + (lon_n as f64) / 190463.0;
    let comment = if rest.len() > 13 {
        String::from_utf8_lossy(&rest[13..]).trim().to_string()
    } else {
        String::new()
    };
    let mut fields = json!({
        "lat": lat,
        "lon": lon,
        "symbol_table": sym_table.to_string(),
        "symbol_code": sym_code.to_string(),
        "comment": comment,
        "compressed": true,
    });
    // Decode the compressed course/speed, radio-range or altitude sub-field
    // from the cs bytes (rest[10], rest[11]) per the compression-type byte
    // (rest[12]). APRS 1.0.1 Chapter 9, p.38-40.
    let cs = decode_compressed_cs(rest[10], rest[11], rest[12]);
    if let serde_json::Value::Object(m) = cs {
        if let serde_json::Value::Object(fm) = &mut fields {
            for (k, v) in m {
                fm.insert(k, v);
            }
        }
    }
    if let Some(ts) = timestamp {
        fields["timestamp"] = json!(ts);
    }
    AprsPayload {
        kind: AprsKind::Position,
        fields,
    }
}

/// Decode the compressed course/speed, pre-calculated radio range or altitude
/// from the two `cs` bytes plus the compression-type `T` byte. APRS 1.0.1
/// Chapter 9, "Course/Speed, Pre-Calculated Radio Range and Altitude" (p.38)
/// through "Altitude" (p.40).
///
/// All three bytes are base-91 (char - 33). The decode is selected by the
/// first `cs` byte `c`:
/// - `c == ' '` (space): no course/speed/range data; cs/T ignored (p.38).
/// - `c` in `!`..`z` (0..89 after -33): course = c*4 deg, speed = 1.08^s - 1
///   knots (p.39).
/// - `c == '{'` (90): radio range = 2 * 1.08^s miles (p.39).
/// - if the T byte's NMEA-source bits (4,3) = 10 (GGA): altitude =
///   1.002^cs feet, where cs = (c-33)*91 + (s-33) (p.40).
pub fn decode_compressed_cs(c: u8, s: u8, t: u8) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    // Space => no data (p.38). The cs/T bytes are ignored.
    if c == b' ' {
        return serde_json::Value::Object(out);
    }
    let cv = c as i32 - 33;
    let sv = s as i32 - 33;
    let tv = t as i32 - 33;
    if !(0..=90).contains(&cv) || !(0..=90).contains(&sv) {
        return serde_json::Value::Object(out);
    }

    // Compression-type T byte: bit 5 GPS fix, bits 4-3 NMEA source,
    // bits 2-0 origin (p.39). NMEA source 10b (GGA) => altitude.
    let nmea_source = (tv >> 3) & 0b11;
    let gps_fix = (tv >> 5) & 0b1;

    if c == b'{' {
        // Pre-calculated radio range (p.39): range = 2 * 1.08^s miles.
        let range = 2.0 * 1.08f64.powi(sv);
        out.insert(
            "radio_range_miles".into(),
            json!((range * 10.0).round() / 10.0),
        );
    } else if nmea_source == 0b10 {
        // GGA sentence => altitude (p.40): altitude = 1.002^(c*91+s) feet,
        // relative to the datum (cs is the base-91 pair value).
        let cs = cv * 91 + sv;
        let altitude = 1.002f64.powi(cs);
        out.insert("altitude_ft".into(), json!(altitude.round() as i64));
    } else {
        // Course/speed (p.39): course = c*4 deg, speed = 1.08^s - 1 knots.
        let course = cv * 4;
        let speed = 1.08f64.powi(sv) - 1.0;
        out.insert("course_deg".into(), json!(course));
        out.insert("speed_knots".into(), json!((speed * 10.0).round() / 10.0));
    }
    out.insert("gps_fix_current".into(), json!(gps_fix == 1));
    serde_json::Value::Object(out)
}

/// Decode a 4-character Base-91 group (each char is value-33, big-endian
/// base 91). APRS 1.0.1 p.37.
fn base91_4(b: &[u8]) -> Option<u32> {
    if b.len() != 4 {
        return None;
    }
    let mut n: u32 = 0;
    for &c in b {
        if !(33..=124).contains(&c) {
            return None;
        }
        n = n * 91 + (c as u32 - 33);
    }
    Some(n)
}

/// Positionless weather report (`_`). APRS 1.0.1 Chapter 12 (p.62): the
/// format is `_CSEsTTTT...` with an 8-char timestamp (MDHM) then
/// `c`/`s`/`g`/`t`/`r`/`p`/`P`/`h`/`b` weather fields. We extract the named
/// numeric fields we can verify against the documented field table.
fn parse_weather_positionless(info: &[u8]) -> AprsPayload {
    // _ MDHM (8 chars) then weather data.
    let body = &info[1..];
    let timestamp = if body.len() >= 8 {
        Some(String::from_utf8_lossy(&body[0..8]).to_string())
    } else {
        None
    };
    let wx_start = if timestamp.is_some() { 8 } else { 0 };
    let wx = &body[wx_start.min(body.len())..];
    let wx_str = String::from_utf8_lossy(wx).to_string();
    let mut fields = parse_weather_fields(&wx_str);
    if let (Some(ts), serde_json::Value::Object(m)) = (timestamp, &mut fields) {
        m.insert("timestamp".into(), json!(ts));
    }
    AprsPayload {
        kind: AprsKind::Weather,
        fields,
    }
}

/// Decode the APRS weather field set from a `cXXXsXXXgXXX...` string.
/// APRS 1.0.1 Chapter 12 weather-data table (p.63): `c` wind direction
/// (deg), `s` wind speed (mph), `g` gust (mph), `t` temperature (°F, signed),
/// `r` rain last hour (1/100 in), `p` rain 24h, `P` rain since midnight,
/// `h` humidity (%), `b` barometric pressure (1/10 hPa).
pub fn parse_weather_fields(s: &str) -> serde_json::Value {
    let bytes = s.as_bytes();
    let mut out = serde_json::Map::new();
    let mut i = 0;
    // (identifier, json key, field width).
    let specs: &[(u8, &str, usize)] = &[
        (b'c', "wind_dir_deg", 3),
        (b's', "wind_speed_mph", 3),
        (b'g', "wind_gust_mph", 3),
        (b't', "temp_f", 3),
        (b'r', "rain_1h_hundredths_in", 3),
        (b'p', "rain_24h_hundredths_in", 3),
        (b'P', "rain_since_midnight_hundredths_in", 3),
        (b'h', "humidity_pct", 2),
        (b'b', "baro_tenths_hpa", 5),
    ];
    while i < bytes.len() {
        let id = bytes[i];
        if let Some(&(_, key, width)) = specs.iter().find(|&&(c, _, _)| c == id) {
            let field = &bytes[i + 1..(i + 1 + width).min(bytes.len())];
            if field.len() == width {
                let txt = std::str::from_utf8(field).unwrap_or("");
                // 't' can be signed (e.g. t-05). humidity h00 means 100%.
                if let Ok(v) = txt.trim().parse::<i64>() {
                    let v = if id == b'h' && v == 0 { 100 } else { v };
                    out.insert(key.to_string(), json!(v));
                } else {
                    out.insert(key.to_string(), json!(txt));
                }
                i += 1 + width;
                continue;
            }
        }
        i += 1;
    }
    serde_json::Value::Object(out)
}

/// Message (`:`). APRS 1.0.1 Chapter 14 (p.71): `:ADDRESSEE:message{nnn`,
/// where ADDRESSEE is exactly 9 chars padded with spaces, followed by `:`
/// then the message text and an optional `{` message number.
fn parse_message(info: &[u8]) -> AprsPayload {
    // ":" + 9-char addressee + ":" => at least 11 bytes.
    if info.len() < 11 || info[10] != b':' {
        return raw_kind(info, AprsKind::Message);
    }
    let addressee_raw = &info[1..10];
    let addressee = String::from_utf8_lossy(addressee_raw)
        .trim_end()
        .to_string();
    let body = String::from_utf8_lossy(&info[11..]).to_string();

    // Telemetry definition messages (APRS 1.0.1 Chapter 13, p.69-70). These
    // ride on the message data-type id (`:`) and are addressed to the callsign
    // of the station transmitting the telemetry (p.68). The message body begins
    // with one of four 5-char keywords — `PARM.`, `UNIT.`, `EQNS.`, `BITS.` —
    // that name, scale and label the channels reported in the `T#` data values.
    // They are distinct from the raw telemetry data values (handled by
    // `parse_telemetry`). The keyword match is case-sensitive per the spec
    // tables. The addressee (the telemetry station) is preserved on the output.
    if let Some(def) = parse_telemetry_definition(&addressee, &body) {
        return def;
    }

    // Bulletins and announcements (APRS 1.0.1 Chapter 14, p.73): the addressee
    // is the literal "BLN" followed by a single identifier character then
    // (for general bulletins) 5 filler spaces, or (for group bulletins) a
    // group name. A digit identifier => general bulletin; a letter => an
    // announcement (p.73). Bulletins are NOT acknowledged, so they carry no
    // message number.
    if addressee_raw.len() >= 4 && &addressee_raw[0..3] == b"BLN" {
        let id_char = addressee_raw[3] as char;
        let group = String::from_utf8_lossy(&addressee_raw[4..])
            .trim_end()
            .to_string();
        let bulletin_kind = if id_char.is_ascii_alphabetic() {
            "announcement"
        } else {
            "bulletin"
        };
        let mut fields = json!({
            "addressee": addressee,
            "bulletin_id": id_char.to_string(),
            "bulletin_kind": bulletin_kind,
            "text": body,
        });
        if !group.is_empty() {
            fields["group"] = json!(group);
        }
        return AprsPayload {
            kind: AprsKind::Bulletin,
            fields,
        };
    }

    // Optional message number after a '{'. APRS 1.0.1 p.71.
    let (message, msg_no) = match body.rfind('{') {
        Some(p) => (body[..p].to_string(), Some(body[p + 1..].to_string())),
        None => (body, None),
    };
    let mut fields = json!({
        "addressee": addressee,
        "message": message,
    });
    if let Some(n) = msg_no {
        fields["message_number"] = json!(n);
    }
    AprsPayload {
        kind: AprsKind::Message,
        fields,
    }
}

/// Detect and decode a telemetry-definition message (APRS 1.0.1 Chapter 13,
/// p.69-70). These are ordinary APRS messages (data-type id `:`) whose text
/// begins with one of the four 5-byte keywords that define how to interpret a
/// station's `T#` telemetry data values:
///
/// - `PARM.` — Parameter Name message (p.69): channel names.
/// - `UNIT.` — Unit/Label message (p.69): channel units / digital labels.
/// - `EQNS.` — Equation Coefficients message (p.70): `a,b,c` per analog channel.
/// - `BITS.` — Bit Sense / Project Name message (p.70): 8 bit-sense flags + a
///   project title.
///
/// `addressee` is the callsign of the station the telemetry belongs to (p.68);
/// it is carried through onto the decoded payload. Returns `None` when `body`
/// is not a telemetry-definition message, so the caller falls through to the
/// ordinary message / bulletin handling.
fn parse_telemetry_definition(addressee: &str, body: &str) -> Option<AprsPayload> {
    // The keyword is the first 5 bytes ("XXXX."). Match case-sensitively per the
    // spec field tables (p.69-70), then hand the remainder to the dedicated
    // decoder.
    let (keyword, rest) = body.split_at(body.len().min(5));
    match keyword {
        "PARM." => Some(decode_telemetry_parm(addressee, rest)),
        "UNIT." => Some(decode_telemetry_unit(addressee, rest)),
        "EQNS." => Some(decode_telemetry_eqns(addressee, rest)),
        "BITS." => Some(decode_telemetry_bits(addressee, rest)),
        _ => None,
    }
}

/// PARM. — Telemetry Parameter Name message (APRS 1.0.1 Chapter 13, p.69).
/// The body is a comma-separated list naming up to 5 analog channels (A1-A5)
/// then up to 8 digital channels (B1-B8). The list may stop after any field
/// (p.69), so trailing channels are simply absent. Worked example (p.69):
/// `:N0QBF-11 :PARM.Battery,Btemp,ATemp,Pres,Alt,Camra,Chut,Sun,10m,ATV`.
fn decode_telemetry_parm(addressee: &str, rest: &str) -> AprsPayload {
    let (analog, digital) = split_telemetry_labels(rest);
    AprsPayload {
        kind: AprsKind::TelemetryParm,
        fields: json!({
            "addressee": addressee,
            "telemetry_kind": "parm",
            "analog_names": analog,
            "digital_names": digital,
        }),
    }
}

/// UNIT. — Telemetry Unit/Label message (APRS 1.0.1 Chapter 13, p.69). Same
/// 5-analog + 8-digital comma-separated layout as PARM., but the entries are
/// the units of the analog values and the labels of the digital channels.
/// Worked example (p.69):
/// `:N0QBF-11 :UNIT.v/100,deg.F,deg.F,Mbar,Kft,Click,OPEN,on,on,hi`.
fn decode_telemetry_unit(addressee: &str, rest: &str) -> AprsPayload {
    let (analog, digital) = split_telemetry_labels(rest);
    AprsPayload {
        kind: AprsKind::TelemetryUnit,
        fields: json!({
            "addressee": addressee,
            "telemetry_kind": "unit",
            "analog_units": analog,
            "digital_labels": digital,
        }),
    }
}

/// Split a PARM./UNIT. comma-separated body into the first 5 fields (analog
/// channels A1-A5) and the remaining fields (digital channels B1-B8). The spec
/// allows the list to terminate after any field (p.69), so each group may be
/// shorter than its maximum. Empty trailing entries are kept as empty strings
/// because a deliberately-blank channel name/unit is meaningful (it labels a
/// reported but unnamed channel).
fn split_telemetry_labels(rest: &str) -> (Vec<String>, Vec<String>) {
    if rest.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let parts: Vec<String> = rest.split(',').map(|s| s.to_string()).collect();
    let analog: Vec<String> = parts.iter().take(5).cloned().collect();
    let digital: Vec<String> = parts.iter().skip(5).take(8).cloned().collect();
    (analog, digital)
}

/// EQNS. — Telemetry Equation Coefficients message (APRS 1.0.1 Chapter 13,
/// p.70). The body is a comma-separated list of 3 coefficients (a, b, c) for
/// each of the 5 analog channels (up to 15 values; the list may stop early).
/// The decoded value of an analog channel is `a*v^2 + b*v + c`, where `v` is
/// the raw received value (p.70). Worked example (p.70):
/// `:N0QBF-11 :EQNS.0,5.2,0,0,.53,-32,3,4.39,49,-32,3,18,1,2,3` — for A1,
/// (a,b,c) = (0, 5.2, 0), so a raw value of 199 maps to 5.2*199 = 1034.8.
fn decode_telemetry_eqns(addressee: &str, rest: &str) -> AprsPayload {
    // Parse each comma-separated coefficient as a float (the spec allows forms
    // like ".53" and "-32"); group them into [a,b,c] triples per analog channel.
    let coeffs: Vec<f64> = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split(',')
            .map(|s| s.trim().parse::<f64>().unwrap_or(0.0))
            .collect()
    };
    let mut equations: Vec<serde_json::Value> = Vec::new();
    for chunk in coeffs.chunks(3) {
        // Only emit a complete (a,b,c) triple; a trailing partial group means
        // the list terminated mid-channel, which we drop rather than guess.
        if chunk.len() == 3 {
            equations.push(json!({ "a": chunk[0], "b": chunk[1], "c": chunk[2] }));
        }
    }
    AprsPayload {
        kind: AprsKind::TelemetryEqns,
        fields: json!({
            "addressee": addressee,
            "telemetry_kind": "eqns",
            "coefficients": coeffs,
            "equations": equations,
        }),
    }
}

/// BITS. — Telemetry Bit Sense / Project Name message (APRS 1.0.1 Chapter 13,
/// p.70). The body is an 8-character pattern of `1`/`0` giving the active sense
/// of each digital channel (the sense that matches the corresponding label),
/// optionally followed by a comma and a project title (0-23 chars). Worked
/// example (p.70): `:N0QBF-11 :BITS.10110000,N0QBF's Big Balloon`.
fn decode_telemetry_bits(addressee: &str, rest: &str) -> AprsPayload {
    // The 8 bit-sense flags come first; a project title may follow after a
    // comma. The spec shows exactly 8 bits, but tolerate a shorter run and stop
    // at the first non-bit character (the comma before the title, or the title
    // itself if no comma was sent).
    let bytes = rest.as_bytes();
    let mut bits: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < bytes.len() && bits.len() < 8 && (bytes[i] == b'0' || bytes[i] == b'1') {
        bits.push(bytes[i] == b'1');
        i += 1;
    }
    // The remainder is the project title, with a single leading comma separator
    // stripped if present (p.70 example uses a comma).
    let mut title = &rest[i..];
    title = title.strip_prefix(',').unwrap_or(title);
    AprsPayload {
        kind: AprsKind::TelemetryBits,
        fields: json!({
            "addressee": addressee,
            "telemetry_kind": "bits",
            "bit_sense": bits,
            "project_title": title,
        }),
    }
}

/// Status (`>`). APRS 1.0.1 Chapter 16 (p.80): `>` then free-text status,
/// optionally prefixed with an 8-char `DDHHMMz` timestamp. The status may also
/// carry a Maidenhead grid locator (p.81-82): a 4- or 6-character locator
/// immediately following the `>`, then the symbol-table id + symbol code, then
/// (optionally) a space and status text.
fn parse_status(info: &[u8]) -> AprsPayload {
    let body = &info[1..];
    // Maidenhead grid locator: 4 or 6 chars (AABB or AABBcc) immediately after
    // `>`, followed by a symbol table id + symbol code (p.82). A 6-char locator
    // is AA(letters) BB(digits) cc(letters); a 4-char is AA(letters) BB(digits).
    if let Some(fields) = parse_maidenhead_status(body) {
        return AprsPayload {
            kind: AprsKind::Status,
            fields,
        };
    }
    let text = String::from_utf8_lossy(body).trim().to_string();
    AprsPayload {
        kind: AprsKind::Status,
        fields: json!({ "status": text }),
    }
}

/// Try to parse a Maidenhead-grid status (APRS 1.0.1 p.81-82). The locator is
/// 4 chars (`AAnn`) or 6 chars (`AAnngg`) followed by the symbol-table id and
/// symbol code. Returns the decoded fields, or `None` when the body is not a
/// grid-locator status.
fn parse_maidenhead_status(body: &[u8]) -> Option<serde_json::Value> {
    // Helper: is this byte a valid Maidenhead field/sub-square letter?
    let is_loc_letter = |b: u8| b.is_ascii_alphabetic();
    // Try 6-char locator + 2 symbol bytes = 8, else 4-char + 2 = 6.
    for loc_len in [6usize, 4usize] {
        if body.len() < loc_len + 2 {
            continue;
        }
        let loc = &body[0..loc_len];
        // Field pair (letters A-R), square pair (digits), and for 6-char a
        // sub-square pair (letters). p.82: letters may be upper or lower case.
        let ok = is_loc_letter(loc[0])
            && is_loc_letter(loc[1])
            && loc[2].is_ascii_digit()
            && loc[3].is_ascii_digit()
            && (loc_len == 4 || (is_loc_letter(loc[4]) && is_loc_letter(loc[5])));
        if !ok {
            continue;
        }
        let sym_table = body[loc_len] as char;
        let sym_code = body[loc_len + 1] as char;
        // The symbol-table id is `/`, `\`, or an overlay char; require it to be
        // a plausible table id to avoid false positives on plain text.
        if !(sym_table == '/' || sym_table == '\\' || sym_table.is_ascii_alphanumeric()) {
            continue;
        }
        let rest = &body[loc_len + 2..];
        // If status text follows, its first char must be a space (p.82).
        let text = String::from_utf8_lossy(rest).trim().to_string();
        let mut fields = serde_json::Map::new();
        fields.insert(
            "maidenhead".into(),
            json!(String::from_utf8_lossy(loc).to_uppercase()),
        );
        fields.insert("symbol_table".into(), json!(sym_table.to_string()));
        fields.insert("symbol_code".into(), json!(sym_code.to_string()));
        if !text.is_empty() {
            fields.insert("status".into(), json!(text));
        }
        return Some(serde_json::Value::Object(fields));
    }
    None
}

/// Item Report (`)`). APRS 1.0.1 Chapter 11, "Item Report Format" (p.59): the
/// `)` data-type id, then a variable-length item name (3-9 chars, any
/// printable ASCII except `!` and `_`), then `!` (live) or `_` (killed), then
/// a position (uncompressed or compressed). There is no timestamp. Worked
/// example (p.59): `)AID #2!4903.50N/07201.75WA` — item "AID #2", live, at
/// 49°03.50'N/072°01.75'W, symbol `/A` (Aid Station).
fn parse_item(info: &[u8]) -> AprsPayload {
    // Scan for the live/killed separator `!` or `_` after a 3-9 char name.
    let body = &info[1..];
    let mut sep_idx = None;
    for (i, &b) in body.iter().enumerate() {
        if (3..=9).contains(&i) && (b == b'!' || b == b'_') {
            sep_idx = Some(i);
            break;
        }
        // Names are 3-9 chars; stop scanning past the max.
        if i > 9 {
            break;
        }
    }
    let Some(sep) = sep_idx else {
        return raw_kind(info, AprsKind::Item);
    };
    let name = String::from_utf8_lossy(&body[0..sep]).to_string();
    let live = body[sep] == b'!';
    let pos = &body[sep + 1..];
    let mut fields = json!({
        "name": name,
        "live": live,
    });
    if !pos.is_empty() {
        let parsed = dispatch_position_body(pos);
        if let serde_json::Value::Object(pm) = parsed.fields {
            if let serde_json::Value::Object(m) = &mut fields {
                for (k, v) in pm {
                    if k != "info" {
                        m.insert(k, v);
                    }
                }
            }
        }
    }
    AprsPayload {
        kind: AprsKind::Item,
        fields,
    }
}

/// General Query (`?`). APRS 1.0.1 Chapter 15, "General Queries" (p.78):
/// `?QUERYTYPE?` optionally followed by a target footprint `lat,long,radius`.
/// Worked examples (p.78): `?APRS?`, `?WX?`, `?IGATE?`, and
/// `?APRS? 34.02,-117.15,0200` (footprint query).
fn parse_query(info: &[u8]) -> AprsPayload {
    let body = String::from_utf8_lossy(&info[1..]).to_string();
    // Query type runs up to the next '?' or end; a footprint may follow.
    let (qtype, footprint) = match body.find('?') {
        Some(p) => (body[..p].to_string(), body[p + 1..].trim().to_string()),
        None => (body.trim().to_string(), String::new()),
    };
    let mut fields = json!({ "query_type": qtype });
    if !footprint.is_empty() {
        // Optional target footprint: lat,long,radius in floating-point degrees
        // (p.78). North/east positive (leading space), south/west negative.
        let parts: Vec<&str> = footprint.split(',').collect();
        if parts.len() == 3 {
            if let (Ok(lat), Ok(lon), Ok(radius)) = (
                parts[0].trim().parse::<f64>(),
                parts[1].trim().parse::<f64>(),
                parts[2].trim().parse::<u32>(),
            ) {
                fields["lat"] = json!(lat);
                fields["lon"] = json!(lon);
                fields["radius_miles"] = json!(radius);
            }
        }
        if fields.get("lat").is_none() {
            fields["footprint"] = json!(footprint);
        }
    }
    AprsPayload {
        kind: AprsKind::Query,
        fields,
    }
}

/// Dispatch a position body (no data-type id, no timestamp) to the
/// uncompressed or compressed parser based on its first byte. Shared by object
/// and item reports. APRS 1.0.1 Chapter 6 / Chapter 9.
fn dispatch_position_body(pos: &[u8]) -> AprsPayload {
    let first = pos[0];
    if first.is_ascii_digit() {
        parse_uncompressed_position(pos, None)
    } else if pos.len() >= 13 {
        parse_compressed_position(pos, None)
    } else {
        raw(&String::from_utf8_lossy(pos))
    }
}

/// Object (`;`). APRS 1.0.1 Chapter 11 (p.58): `;NAME     *DDHHMMz<lat><sym>
/// <lon><sym>...`. NAME is 9 chars, then `*` (live) or `_` (killed), then a
/// 7-char timestamp, then a position in the same uncompressed/compressed form.
fn parse_object(info: &[u8]) -> AprsPayload {
    // ; + 9-char name + state(1) + 7-char timestamp = 18 minimum before pos.
    if info.len() < 18 {
        return raw_kind(info, AprsKind::Object);
    }
    let name = String::from_utf8_lossy(&info[1..10]).trim_end().to_string();
    let state = info[10] as char; // '*' live, '_' killed
    let timestamp = String::from_utf8_lossy(&info[11..18]).to_string();
    // Position data follows the timestamp; reuse the position parser on the
    // remainder (it handles both uncompressed and compressed forms).
    let pos = &info[18..];
    let mut fields = json!({
        "name": name,
        "live": state == '*',
        "timestamp": timestamp,
    });
    if !pos.is_empty() {
        let parsed = dispatch_position_body(pos);
        if let serde_json::Value::Object(pm) = parsed.fields {
            if let serde_json::Value::Object(m) = &mut fields {
                for (k, v) in pm {
                    if k != "info" {
                        m.insert(k, v);
                    }
                }
            }
        }
    }
    AprsPayload {
        kind: AprsKind::Object,
        fields,
    }
}

/// Telemetry (`T`). APRS 1.0.1 Chapter 13 (p.68): `T#sss,a1,a2,a3,a4,a5,bbbbbbbb`
/// — a sequence number then five analog values (0..255) then 8 digital bits.
fn parse_telemetry(info: &[u8]) -> AprsPayload {
    let s = String::from_utf8_lossy(info).to_string();
    // Expect "T#" prefix.
    let body = s
        .strip_prefix("T#")
        .unwrap_or_else(|| s.strip_prefix('T').unwrap_or(&s));
    let parts: Vec<&str> = body.split(',').collect();
    if parts.len() < 6 {
        return raw_kind(info, AprsKind::Telemetry);
    }
    let seq = parts[0].trim();
    let analog: Vec<serde_json::Value> = parts[1..6]
        .iter()
        .map(|p| match p.trim().parse::<f64>() {
            Ok(v) => json!(v),
            Err(_) => json!(p.trim()),
        })
        .collect();
    let digital = parts.get(6).map(|d| {
        d.trim()
            .chars()
            .take(8)
            .map(|c| c == '1')
            .collect::<Vec<bool>>()
    });
    let mut fields = json!({
        "sequence": seq,
        "analog": analog,
    });
    if let Some(d) = digital {
        fields["digital"] = json!(d);
    }
    AprsPayload {
        kind: AprsKind::Telemetry,
        fields,
    }
}

fn raw_kind(info: &[u8], kind: AprsKind) -> AprsPayload {
    let mut p = raw(&String::from_utf8_lossy(info));
    p.kind = kind;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 6, worked example p.32:
    /// `!4903.50N/07201.75W-` => 49°03.50'N = 49.0583°, 072°01.75'W =
    /// -72.0291°, primary symbol table `/`, symbol code `-` (house).
    #[test]
    fn uncompressed_position_spec_example() {
        let p = parse(b"!4903.50N/07201.75W-");
        assert_eq!(p.kind, AprsKind::Position);
        let lat = p.fields["lat"].as_f64().unwrap();
        let lon = p.fields["lon"].as_f64().unwrap();
        assert!((lat - 49.058333).abs() < 1e-5, "lat={lat}");
        assert!((lon - (-72.029166)).abs() < 1e-5, "lon={lon}");
        assert_eq!(p.fields["symbol_table"], "/");
        assert_eq!(p.fields["symbol_code"], "-");
        assert_eq!(p.fields["compressed"], false);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 p.32, the same example with a trailing
    /// comment: `=4903.50N/07201.75W-Test 001234` keeps the comment text.
    #[test]
    fn uncompressed_position_with_comment() {
        let p = parse(b"=4903.50N/07201.75W-Test 001234");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["comment"], "Test 001234");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 9 compressed example (p.38-39):
    /// the compressed data `/5L!!<*e7>` — sym-table `/`, lat group "5L!!",
    /// lon group "<*e7", sym-code `>`, then 2 cs bytes + 1 comp-type byte
    /// (`{?!`) — decodes to 49.5°N, -72.75°W via the documented formulas
    /// lat = 90 - N/380926, lon = -180 + N/190463. We feed the example after
    /// the data-type id `!` and verify the Base-91 lat/lon conversion lands
    /// on those documented coordinates.
    #[test]
    fn compressed_position_spec_example() {
        // 13-byte compressed body, prefixed with the '!' DTI.
        let p = parse(b"!/5L!!<*e7>{?!");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["compressed"], true);
        let lat = p.fields["lat"].as_f64().unwrap();
        let lon = p.fields["lon"].as_f64().unwrap();
        // APRS 1.0.1 p.39: these groups decode to 49.5 N, 72.75 W.
        assert!((lat - 49.5).abs() < 1e-3, "lat={lat}");
        assert!((lon - (-72.75)).abs() < 1e-3, "lon={lon}");
        assert_eq!(p.fields["symbol_table"], "/");
        assert_eq!(p.fields["symbol_code"], ">");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 14 message example (p.71):
    /// `:WU2Z     :Testing{003` — addressee "WU2Z", text "Testing",
    /// message number "003".
    #[test]
    fn message_spec_example() {
        let p = parse(b":WU2Z     :Testing{003");
        assert_eq!(p.kind, AprsKind::Message);
        assert_eq!(p.fields["addressee"], "WU2Z");
        assert_eq!(p.fields["message"], "Testing");
        assert_eq!(p.fields["message_number"], "003");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 16 status example (p.80):
    /// `>Net Control Center` is a free-text status.
    #[test]
    fn status_spec_example() {
        let p = parse(b">Net Control Center");
        assert_eq!(p.kind, AprsKind::Status);
        assert_eq!(p.fields["status"], "Net Control Center");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 11 object example (p.58):
    /// `;LEADER   *092345z4903.50N/07201.75W>` — object "LEADER", live,
    /// timestamp 092345z, position 49°03.50'N / 072°01.75'W, symbol `>`.
    #[test]
    fn object_spec_example() {
        let p = parse(b";LEADER   *092345z4903.50N/07201.75W>");
        assert_eq!(p.kind, AprsKind::Object);
        assert_eq!(p.fields["name"], "LEADER");
        assert_eq!(p.fields["live"], true);
        assert_eq!(p.fields["timestamp"], "092345z");
        let lat = p.fields["lat"].as_f64().unwrap();
        assert!((lat - 49.058333).abs() < 1e-5, "lat={lat}");
        assert_eq!(p.fields["symbol_code"], ">");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 13 telemetry example (p.68):
    /// `T#005,199,000,255,073,123,01101001` — sequence 005, five analog
    /// channels, 8 digital bits.
    #[test]
    fn telemetry_spec_example() {
        let p = parse(b"T#005,199,000,255,073,123,01101001");
        assert_eq!(p.kind, AprsKind::Telemetry);
        assert_eq!(p.fields["sequence"], "005");
        let analog = p.fields["analog"].as_array().unwrap();
        assert_eq!(analog.len(), 5);
        assert_eq!(analog[0], 199.0);
        assert_eq!(analog[2], 255.0);
        let digital = p.fields["digital"].as_array().unwrap();
        assert_eq!(digital.len(), 8);
        assert_eq!(digital[0], false);
        assert_eq!(digital[1], true);
        assert_eq!(digital[2], true);
        assert_eq!(digital[7], true);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 12 weather field table (p.63).
    /// A positionless report `_10090556c220s004g005t077r000p000P000h50b09900`
    /// (the format shown in the Chapter 12 examples) decodes the named
    /// weather fields.
    #[test]
    fn weather_spec_example() {
        let p = parse(b"_10090556c220s004g005t077r000p000P000h50b09900");
        assert_eq!(p.kind, AprsKind::Weather);
        assert_eq!(p.fields["timestamp"], "10090556");
        assert_eq!(p.fields["wind_dir_deg"], 220);
        assert_eq!(p.fields["wind_speed_mph"], 4);
        assert_eq!(p.fields["wind_gust_mph"], 5);
        assert_eq!(p.fields["temp_f"], 77);
        assert_eq!(p.fields["humidity_pct"], 50);
        assert_eq!(p.fields["baro_tenths_hpa"], 9900);
    }

    /// Southern/eastern hemisphere sign handling (APRS 1.0.1 p.32: S => lat
    /// negative, E => lon positive).
    #[test]
    fn position_hemispheres() {
        let p = parse(b"!3358.00S/15112.00E-");
        let lat = p.fields["lat"].as_f64().unwrap();
        let lon = p.fields["lon"].as_f64().unwrap();
        assert!(lat < 0.0, "S hemisphere => negative lat, got {lat}");
        assert!(lon > 0.0, "E hemisphere => positive lon, got {lon}");
        assert!((lat - (-33.9666)).abs() < 1e-3);
        assert!((lon - 151.2).abs() < 1e-3);
    }

    /// An unrecognized data-type identifier falls through to raw.
    #[test]
    fn unknown_dti_is_raw() {
        let p = parse(b"$GPGGA,nonsense");
        assert_eq!(p.kind, AprsKind::Raw);
        assert_eq!(p.fields["info"], "$GPGGA,nonsense");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 9 compressed course/speed
    /// sub-field (p.39). The cs characters `7P` decode to course = 7*4 = 88
    /// degrees and speed = 1.08^47 - 1 = 36.2 knots. We feed the full p.40
    /// compressed field `/5L!!<*e7>7P[` (sym-table `/`, lat "5L!!", lon "<*e7",
    /// sym-code `>`, cs "7P", T `[`) after the `!` DTI.
    #[test]
    fn compressed_course_speed_p39() {
        let p = parse(b"!/5L!!<*e7>7P[");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["compressed"], true);
        assert_eq!(p.fields["course_deg"], 88);
        let spd = p.fields["speed_knots"].as_f64().unwrap();
        assert!((spd - 36.2).abs() < 0.1, "speed={spd}");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 9 pre-calculated radio range
    /// sub-field (p.39). cs `{?` => c=`{` (range marker), s=`?` (63-33=30),
    /// range = 2 * 1.08^30 ≈ 20 miles. Full field `/5L!!<*e7>{?!`.
    #[test]
    fn compressed_radio_range_p39() {
        let p = parse(b"!/5L!!<*e7>{?!");
        assert_eq!(p.kind, AprsKind::Position);
        let rng = p.fields["radio_range_miles"].as_f64().unwrap();
        assert!((rng - 20.0).abs() < 0.5, "range={rng}");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 9 altitude sub-field (p.40). When
    /// the T byte indicates a GGA sentence (NMEA-source bits 4,3 = 10), cs `S]`
    /// decodes to altitude = 1.002^((83-33)*91 + (93-33)) = 1.002^4610 ≈ 10004
    /// feet. Full field `/5L!!<*e7OS]S` (T byte `S` => GGA).
    #[test]
    fn compressed_altitude_p40() {
        let p = parse(b"!/5L!!<*e7OS]S");
        assert_eq!(p.kind, AprsKind::Position);
        let alt = p.fields["altitude_ft"].as_i64().unwrap();
        assert!((alt - 10004).abs() <= 2, "altitude={alt}");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 9, the special-case `c == space`:
    /// no course/speed/range data, cs/T ignored (p.38). Full field
    /// `/5L!!<*e7> sT` (cs first byte is a space) carries no extension fields.
    #[test]
    fn compressed_space_no_extension_p38() {
        let p = parse(b"!/5L!!<*e7> sT");
        assert_eq!(p.kind, AprsKind::Position);
        assert!(p.fields.get("course_deg").is_none());
        assert!(p.fields.get("speed_knots").is_none());
        assert!(p.fields.get("radio_range_miles").is_none());
        assert!(p.fields.get("altitude_ft").is_none());
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 7 course/speed data extension
    /// (p.27): the 7-byte `nnn/nnn` field. `088/036` => course 88°, speed 36
    /// knots, appended to a position comment.
    #[test]
    fn uncompressed_course_speed_extension_p27() {
        let p = parse(b"!4903.50N/07201.75W>088/036Heading out");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["course_deg"], 88);
        assert_eq!(p.fields["speed_knots"], 36);
        // The extension is stripped from the comment.
        assert_eq!(p.fields["comment"], "Heading out");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 7 PHG data extension (p.28-29).
    /// `PHG5132` => power 5^2 = 25 watts, height 10*2^1 = 20 ft, gain 3 dB,
    /// directivity code 2 = 90° (East). Worked example p.28-29.
    #[test]
    fn phg_extension_p28() {
        let p = parse(b"=4903.50N/07201.75W#PHG5132");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["phg_power_w"], 25);
        assert_eq!(p.fields["phg_height_ft"], 20);
        assert_eq!(p.fields["phg_gain_db"], 3);
        assert_eq!(p.fields["phg_directivity_deg"], 90);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 7 DFS data extension (p.30).
    /// `DFS2360` => strength S2, height 10*2^3 = 80 ft, gain 6 dB, directivity
    /// code 0 = omni. Worked example p.30: "weak signal (around strength S2)
    /// heard on an omni antenna with 6 dB gain at 80 feet".
    #[test]
    fn dfs_extension_p30() {
        let p = parse(b"@234517h4903.50N/07201.75W\\DFS2360");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["dfs_strength_s"], 2);
        assert_eq!(p.fields["dfs_height_ft"], 80);
        assert_eq!(p.fields["dfs_gain_db"], 6);
        assert_eq!(p.fields["dfs_directivity_deg"], "omni");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 7 pre-calculated radio range
    /// extension (p.29): `RNG0050` indicates a radio range of 50 miles.
    #[test]
    fn rng_extension_p29() {
        let p = parse(b"=4903.50N/07201.75W-RNG0050");
        assert_eq!(p.kind, AprsKind::Position);
        assert_eq!(p.fields["radio_range_miles"], 50);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 11 Item Report (p.59), worked
    /// example: `)AID #2!4903.50N/07201.75WA` — item "AID #2", live, at
    /// 49°03.50'N/072°01.75'W, symbol `/A` (Aid Station).
    #[test]
    fn item_spec_example_p59() {
        let p = parse(b")AID #2!4903.50N/07201.75WA");
        assert_eq!(p.kind, AprsKind::Item);
        assert_eq!(p.fields["name"], "AID #2");
        assert_eq!(p.fields["live"], true);
        let lat = p.fields["lat"].as_f64().unwrap();
        assert!((lat - 49.058333).abs() < 1e-5, "lat={lat}");
        assert_eq!(p.fields["symbol_table"], "/");
        assert_eq!(p.fields["symbol_code"], "A");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 11 killed Item (p.59): the same
    /// item with `_` (kill character) instead of `!`.
    #[test]
    fn item_killed_p59() {
        let p = parse(b")AID #2_4903.50N/07201.75WA");
        assert_eq!(p.kind, AprsKind::Item);
        assert_eq!(p.fields["name"], "AID #2");
        assert_eq!(p.fields["live"], false);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 11 Item with compressed position
    /// (p.59): `)MOBIL!\5L!!<*e79_sT` — Mobil Gas Station, compressed lat/lon,
    /// symbol `\9` (Gas Station). cs first byte `9` is space-equivalent? No:
    /// the spec field is `\5L!!<*e79_sT` — sym-table `\`, lat "5L!!", lon
    /// "<*e7", sym-code `9`, cs `_s`, T `T`.
    #[test]
    fn item_compressed_p59() {
        let p = parse(b")MOBIL!\\5L!!<*e79_sT");
        assert_eq!(p.kind, AprsKind::Item);
        assert_eq!(p.fields["name"], "MOBIL");
        assert_eq!(p.fields["live"], true);
        assert_eq!(p.fields["compressed"], true);
        assert_eq!(p.fields["symbol_table"], "\\");
        assert_eq!(p.fields["symbol_code"], "9");
        let lat = p.fields["lat"].as_f64().unwrap();
        assert!((lat - 49.5).abs() < 1e-3, "lat={lat}");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 14 General Bulletin (p.73),
    /// worked example: `:BLN3     :Snow expected in Tampa RSN` — bulletin id
    /// "3", text "Snow expected in Tampa RSN".
    #[test]
    fn bulletin_spec_example_p73() {
        let p = parse(b":BLN3     :Snow expected in Tampa RSN");
        assert_eq!(p.kind, AprsKind::Bulletin);
        assert_eq!(p.fields["bulletin_id"], "3");
        assert_eq!(p.fields["bulletin_kind"], "bulletin");
        assert_eq!(p.fields["text"], "Snow expected in Tampa RSN");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 14 Announcement (p.73), worked
    /// example: `:BLNQ     :Mt St Helen digi will be QRT this weekend` — a
    /// letter identifier "Q" makes this an announcement (p.73).
    #[test]
    fn announcement_spec_example_p73() {
        let p = parse(b":BLNQ     :Mt St Helen digi will be QRT this weekend");
        assert_eq!(p.kind, AprsKind::Bulletin);
        assert_eq!(p.fields["bulletin_id"], "Q");
        assert_eq!(p.fields["bulletin_kind"], "announcement");
        assert_eq!(
            p.fields["text"],
            "Mt St Helen digi will be QRT this weekend"
        );
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 14 Group Bulletin (p.74), worked
    /// example: `:BLN4WX   :Stand by your snowplows` — group bulletin id "4",
    /// group name "WX".
    #[test]
    fn group_bulletin_spec_example_p74() {
        let p = parse(b":BLN4WX   :Stand by your snowplows");
        assert_eq!(p.kind, AprsKind::Bulletin);
        assert_eq!(p.fields["bulletin_id"], "4");
        assert_eq!(p.fields["group"], "WX");
        assert_eq!(p.fields["text"], "Stand by your snowplows");
    }

    /// A normal message (non-bulletin) addressee still decodes as a message,
    /// not a bulletin (regression guard for the BLN detection).
    #[test]
    fn normal_message_not_bulletin() {
        let p = parse(b":WU2Z     :Testing{003");
        assert_eq!(p.kind, AprsKind::Message);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 15 General Query (p.78), worked
    /// examples: `?APRS?` (all-stations query) and `?WX?` (weather query).
    #[test]
    fn general_query_spec_examples_p78() {
        let p = parse(b"?APRS?");
        assert_eq!(p.kind, AprsKind::Query);
        assert_eq!(p.fields["query_type"], "APRS");

        let p = parse(b"?WX?");
        assert_eq!(p.kind, AprsKind::Query);
        assert_eq!(p.fields["query_type"], "WX");

        let p = parse(b"?IGATE?");
        assert_eq!(p.kind, AprsKind::Query);
        assert_eq!(p.fields["query_type"], "IGATE");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 15 General Query with target
    /// footprint (p.78): `?APRS? 34.02,-117.15,0200` — query within 200 miles
    /// of 34.02°N, 117.15°W (floating-point degrees, p.78).
    #[test]
    fn query_with_footprint_p78() {
        let p = parse(b"?APRS? 34.02,-117.15,0200");
        assert_eq!(p.kind, AprsKind::Query);
        assert_eq!(p.fields["query_type"], "APRS");
        let lat = p.fields["lat"].as_f64().unwrap();
        let lon = p.fields["lon"].as_f64().unwrap();
        assert!((lat - 34.02).abs() < 1e-6);
        assert!((lon - (-117.15)).abs() < 1e-6);
        assert_eq!(p.fields["radius_miles"], 200);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 16 Status with Maidenhead grid
    /// locator (p.82), worked examples: `>IO91SX/-` (6-char locator IO91SX,
    /// symbol `/-`) and `>IO91SX/- My house` (with status text starting with a
    /// space, p.82).
    #[test]
    fn maidenhead_status_p82() {
        let p = parse(b">IO91SX/-");
        assert_eq!(p.kind, AprsKind::Status);
        assert_eq!(p.fields["maidenhead"], "IO91SX");
        assert_eq!(p.fields["symbol_table"], "/");
        assert_eq!(p.fields["symbol_code"], "-");

        let p = parse(b">IO91SX/- My house");
        assert_eq!(p.kind, AprsKind::Status);
        assert_eq!(p.fields["maidenhead"], "IO91SX");
        assert_eq!(p.fields["status"], "My house");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 16 4-char Maidenhead status
    /// (p.82): `>IO91/G` (4-char locator IO91, symbol `/G` grid).
    #[test]
    fn maidenhead_status_4char_p82() {
        let p = parse(b">IO91/G");
        assert_eq!(p.kind, AprsKind::Status);
        assert_eq!(p.fields["maidenhead"], "IO91");
        assert_eq!(p.fields["symbol_table"], "/");
        assert_eq!(p.fields["symbol_code"], "G");
    }

    /// A plain free-text status that merely starts with letters must NOT be
    /// misdetected as a Maidenhead locator (regression guard).
    #[test]
    fn plain_status_not_maidenhead() {
        let p = parse(b">Net Control Center");
        assert_eq!(p.kind, AprsKind::Status);
        assert_eq!(p.fields["status"], "Net Control Center");
        assert!(p.fields.get("maidenhead").is_none());
    }

    // ----------------------------------------------------------------------
    // Telemetry DEFINITION messages — APRS 1.0.1 Chapter 13 (p.69-70).
    //
    // These define how a station's `T#` telemetry data values are named,
    // labelled, scaled and bit-sensed. They ride on the message data-type id
    // (`:`) addressed to the telemetry station's callsign (p.68). Every worked
    // example below is the spec's own N0QBF-11 beacon example, used as the
    // independent oracle. The addressee in those examples is the 8-char
    // "N0QBF-11" padded to the fixed 9-char message addressee field with one
    // trailing space (Chapter 14 message format, p.71).
    // ----------------------------------------------------------------------

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 13 Parameter Name message (p.69),
    /// worked example:
    /// `:N0QBF-11 :PARM.Battery,Btemp,ATemp,Pres,Alt,Camra,Chut,Sun,10m,ATV`
    /// — the 5 analog channel names then the digital channel names.
    #[test]
    fn telemetry_parm_spec_example_p69() {
        let p = parse(b":N0QBF-11 :PARM.Battery,Btemp,ATemp,Pres,Alt,Camra,Chut,Sun,10m,ATV");
        assert_eq!(p.kind, AprsKind::TelemetryParm);
        assert_eq!(p.fields["addressee"], "N0QBF-11");
        let analog = p.fields["analog_names"].as_array().unwrap();
        assert_eq!(analog.len(), 5);
        assert_eq!(analog[0], "Battery");
        assert_eq!(analog[1], "Btemp");
        assert_eq!(analog[2], "ATemp");
        assert_eq!(analog[3], "Pres");
        assert_eq!(analog[4], "Alt");
        let digital = p.fields["digital_names"].as_array().unwrap();
        // After the 5 analog names, the remaining 5 are digital labels (the
        // spec example stops after B5, the list "may stop at any field", p.69).
        assert_eq!(digital.len(), 5);
        assert_eq!(digital[0], "Camra");
        assert_eq!(digital[1], "Chut");
        assert_eq!(digital[2], "Sun");
        assert_eq!(digital[3], "10m");
        assert_eq!(digital[4], "ATV");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 13 Unit/Label message (p.69),
    /// worked example:
    /// `:N0QBF-11 :UNIT.v/100,deg.F,deg.F,Mbar,Kft,Click,OPEN,on,on,hi`
    /// — the 5 analog units then the digital channel labels.
    #[test]
    fn telemetry_unit_spec_example_p69() {
        let p = parse(b":N0QBF-11 :UNIT.v/100,deg.F,deg.F,Mbar,Kft,Click,OPEN,on,on,hi");
        assert_eq!(p.kind, AprsKind::TelemetryUnit);
        assert_eq!(p.fields["addressee"], "N0QBF-11");
        let units = p.fields["analog_units"].as_array().unwrap();
        assert_eq!(units.len(), 5);
        assert_eq!(units[0], "v/100");
        assert_eq!(units[1], "deg.F");
        assert_eq!(units[2], "deg.F");
        assert_eq!(units[3], "Mbar");
        assert_eq!(units[4], "Kft");
        let labels = p.fields["digital_labels"].as_array().unwrap();
        assert_eq!(labels.len(), 5);
        assert_eq!(labels[0], "Click");
        assert_eq!(labels[1], "OPEN");
        assert_eq!(labels[2], "on");
        assert_eq!(labels[3], "on");
        assert_eq!(labels[4], "hi");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 13 Equation Coefficients message
    /// (p.70), worked example:
    /// `:N0QBF-11 :EQNS.0,5.2,0,0,.53,-32,3,4.39,49,-32,3,18,1,2,3`
    /// — three coefficients (a,b,c) for each of the 5 analog channels. The spec
    /// gives the conversion `value = a*v^2 + b*v + c` and the worked A1 result
    /// (p.70): with (a,b,c)=(0,5.2,0) a raw value v=199 yields 1034.8.
    #[test]
    fn telemetry_eqns_spec_example_p70() {
        let p = parse(b":N0QBF-11 :EQNS.0,5.2,0,0,.53,-32,3,4.39,49,-32,3,18,1,2,3");
        assert_eq!(p.kind, AprsKind::TelemetryEqns);
        assert_eq!(p.fields["addressee"], "N0QBF-11");
        let eqns = p.fields["equations"].as_array().unwrap();
        assert_eq!(eqns.len(), 5);
        // A1 = (0, 5.2, 0) per p.70.
        assert_eq!(eqns[0]["a"].as_f64().unwrap(), 0.0);
        assert_eq!(eqns[0]["b"].as_f64().unwrap(), 5.2);
        assert_eq!(eqns[0]["c"].as_f64().unwrap(), 0.0);
        // A2 = (0, .53, -32); A3 = (3, 4.39, 49); A5 = (1, 2, 3).
        assert_eq!(eqns[1]["b"].as_f64().unwrap(), 0.53);
        assert_eq!(eqns[1]["c"].as_f64().unwrap(), -32.0);
        assert_eq!(eqns[2]["a"].as_f64().unwrap(), 3.0);
        assert_eq!(eqns[2]["b"].as_f64().unwrap(), 4.39);
        assert_eq!(eqns[2]["c"].as_f64().unwrap(), 49.0);
        assert_eq!(eqns[4]["a"].as_f64().unwrap(), 1.0);
        assert_eq!(eqns[4]["b"].as_f64().unwrap(), 2.0);
        assert_eq!(eqns[4]["c"].as_f64().unwrap(), 3.0);

        // Reproduce the spec's own worked conversion (p.70): A1 with raw v=199.
        let a = eqns[0]["a"].as_f64().unwrap();
        let b = eqns[0]["b"].as_f64().unwrap();
        let c = eqns[0]["c"].as_f64().unwrap();
        let v = 199.0_f64;
        let value = a * v * v + b * v + c;
        assert!((value - 1034.8).abs() < 1e-9, "value={value}");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 13 Bit Sense / Project Name
    /// message (p.70), worked example:
    /// `:N0QBF-11 :BITS.10110000,N0QBF's Big Balloon`
    /// — the 8 digital bit-sense flags then the project title.
    #[test]
    fn telemetry_bits_spec_example_p70() {
        let p = parse(b":N0QBF-11 :BITS.10110000,N0QBF's Big Balloon");
        assert_eq!(p.kind, AprsKind::TelemetryBits);
        assert_eq!(p.fields["addressee"], "N0QBF-11");
        let bits = p.fields["bit_sense"].as_array().unwrap();
        assert_eq!(bits.len(), 8);
        // 10110000 per p.70.
        assert_eq!(bits[0], true);
        assert_eq!(bits[1], false);
        assert_eq!(bits[2], true);
        assert_eq!(bits[3], true);
        assert_eq!(bits[4], false);
        assert_eq!(bits[5], false);
        assert_eq!(bits[6], false);
        assert_eq!(bits[7], false);
        assert_eq!(p.fields["project_title"], "N0QBF's Big Balloon");
    }

    /// The four telemetry-definition keywords are case-sensitive (the spec field
    /// tables show them in capitals, p.69-70) and a normal message that merely
    /// mentions one must still decode as a plain message — regression guard for
    /// the new dispatch.
    #[test]
    fn telemetry_definition_does_not_swallow_normal_message() {
        // Lower-case keyword: not a definition message.
        let p = parse(b":N0QBF-11 :parm.is a normal word");
        assert_eq!(p.kind, AprsKind::Message);
        // A message whose text only mentions PARM in prose, not as the keyword.
        let p = parse(b":WU2Z     :see PARM. list{003");
        assert_eq!(p.kind, AprsKind::Message);
        assert_eq!(p.fields["addressee"], "WU2Z");
    }

    /// The spec allows the PARM./UNIT. list to terminate after any field (p.69):
    /// a definition with only the 5 analog names and no digital labels still
    /// decodes, with an empty digital list.
    #[test]
    fn telemetry_parm_analog_only_p69() {
        let p = parse(b":N0QBF-11 :PARM.Battery,Btemp,ATemp,Pres,Alt");
        assert_eq!(p.kind, AprsKind::TelemetryParm);
        assert_eq!(p.fields["analog_names"].as_array().unwrap().len(), 5);
        assert_eq!(p.fields["digital_names"].as_array().unwrap().len(), 0);
    }

    /// A BITS. message with no project title (only the 8-bit pattern) decodes
    /// the bit-sense flags and leaves the title empty (APRS 1.0.1 p.70: the
    /// project title is 0-23 chars, so it may be absent).
    #[test]
    fn telemetry_bits_no_title_p70() {
        let p = parse(b":N0QBF-11 :BITS.10110000");
        assert_eq!(p.kind, AprsKind::TelemetryBits);
        assert_eq!(p.fields["bit_sense"].as_array().unwrap().len(), 8);
        assert_eq!(p.fields["project_title"], "");
    }
}
