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
//! - `:`             — message
//! - `>`             — status
//! - `;`             — object
//! - `T` (`T#...`)   — telemetry
//!
//! Position reports come in two forms (APRS 1.0.1 Chapter 6):
//! - **uncompressed**: `DDMM.mmN/DDDMM.mmW$` (lat 8 chars, sym-table id,
//!   lon 9 chars, symbol code), then an optional comment.
//! - **compressed** (Chapter 9): a Base-91 encoding — sym-table id, 4 bytes
//!   latitude, 4 bytes longitude, symbol code, 2 bytes course/speed or
//!   altitude, compression-type byte.

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
    Telemetry,
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
            AprsKind::Telemetry => "telemetry",
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
    let comment = String::from_utf8_lossy(&rest[19..]).trim().to_string();

    let lat = parse_lat(lat_s);
    let lon = parse_lon(lon_s);
    let (Some(lat), Some(lon)) = (lat, lon) else {
        return raw_pos(rest, timestamp);
    };

    let mut fields = json!({
        "lat": lat,
        "lon": lon,
        "symbol_table": sym_table.to_string(),
        "symbol_code": sym_code.to_string(),
        "comment": comment,
        "compressed": false,
    });
    if let Some(ts) = timestamp {
        fields["timestamp"] = json!(ts);
    }
    AprsPayload {
        kind: AprsKind::Position,
        fields,
    }
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
    if let Some(ts) = timestamp {
        fields["timestamp"] = json!(ts);
    }
    AprsPayload {
        kind: AprsKind::Position,
        fields,
    }
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
    let addressee = String::from_utf8_lossy(&info[1..10]).trim_end().to_string();
    let body = String::from_utf8_lossy(&info[11..]).to_string();
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

/// Status (`>`). APRS 1.0.1 Chapter 16 (p.80): `>` then free-text status,
/// optionally prefixed with an 8-char `DDHHMMz` timestamp.
fn parse_status(info: &[u8]) -> AprsPayload {
    let text = String::from_utf8_lossy(&info[1..]).trim().to_string();
    AprsPayload {
        kind: AprsKind::Status,
        fields: json!({ "status": text }),
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
        let first = pos[0];
        let parsed = if first.is_ascii_digit() {
            parse_uncompressed_position(pos, None)
        } else if pos.len() >= 13 {
            parse_compressed_position(pos, None)
        } else {
            raw(&String::from_utf8_lossy(pos))
        };
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
    let body = s.strip_prefix("T#").unwrap_or_else(|| s.strip_prefix('T').unwrap_or(&s));
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
}
