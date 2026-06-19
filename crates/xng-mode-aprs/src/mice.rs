//! Mic-E data-format decode (APRS 1.0.1 Chapter 10, "Mic-E Data Format").
//!
//! Reference: **APRS Protocol Reference, Protocol Version 1.0.1** (29 August
//! 2000), Chapter 10 (pp. 42-56). Mic-E is the single most common APRS
//! position encoding on the air (Kenwood TH-D7 / TM-D700, the original Mic
//! Encoder, the PIC-E and most modern trackers all use it), so a decoder that
//! skips it misses roughly half of all real traffic.
//!
//! Mic-E splits a compressed position report across **two** AX.25 fields:
//!
//! - The 6-character AX.25 **destination address** ("tocall") carries the six
//!   latitude digits, the 3-bit message code (bits A/B/C), the N/S latitude
//!   indicator, the longitude offset (+0 / +100 degrees), and the W/E
//!   longitude indicator (Chapter 10, "Mic-E Destination Address Field",
//!   p.43, with the per-character encoding table on p.44).
//! - The AX.25 **information field** carries the longitude (3 bytes, d+28 /
//!   m+28 / h+28), the speed and course (3 bytes SP+28 / DC+28 / SE+28), and
//!   the symbol code + symbol-table id (p.46-52). An optional trailing field
//!   carries Mic-E telemetry or status text (p.54).
//!
//! Because the latitude lives in the destination address, this decoder takes
//! BOTH the raw 6-character destination callsign and the info field; the
//! crate wires it in at the AX.25 level (see `crate::decode_frame`).

use serde::Serialize;
use serde_json::json;

/// A decoded Mic-E payload. Field names mirror the rest of the APRS payloads
/// (`lat`/`lon`/`symbol_table`/`symbol_code`/`comment`) so the bus message is
/// uniform across position kinds.
#[derive(Debug, Clone, Serialize)]
pub struct MicE {
    pub fields: serde_json::Value,
}

/// Mic-E message type, decoded from the 3 message bits A/B/C carried (one bit
/// each) in destination characters 1, 2 and 3. APRS 1.0.1 p.45, "Mic-E
/// Message Types" table.
///
/// Each destination character is either a "Standard" 1, a "Custom" 1, or a 0.
/// All-three-Standard / mixed rules: the message-type table is indexed by the
/// raw A/B/C bits (1 = either Standard or Custom, 0 = zero), and whether the
/// set 1-bits are Standard or Custom selects the Standard vs Custom column.
/// A mix of Standard and Custom 1-bits is "unknown" (p.45 final note).
fn message_type(a: BitKind, b: BitKind, c: BitKind) -> (&'static str, &'static str) {
    use BitKind::*;
    // Bit value (0/1) for table indexing.
    let av = a.bit();
    let bv = b.bit();
    let cv = c.bit();
    let abc = (av << 2) | (bv << 1) | cv;

    // Emergency: all three bits 0 (p.45).
    if abc == 0 {
        return ("emergency", "Emergency");
    }

    // Determine Standard vs Custom from the 1-bits present. A mixture is
    // "unknown" (p.45).
    let has_std = matches!(a, Std) || matches!(b, Std) || matches!(c, Std);
    let has_cust = matches!(a, Custom) || matches!(b, Custom) || matches!(c, Custom);
    if has_std && has_cust {
        return ("unknown", "Unknown");
    }
    let custom = has_cust;

    // p.45 table, indexed by A/B/C bit pattern (111..001).
    let (std_name, cust_name) = match abc {
        0b111 => ("M0: Off Duty", "C0: Custom-0"),
        0b110 => ("M1: En Route", "C1: Custom-1"),
        0b101 => ("M2: In Service", "C2: Custom-2"),
        0b100 => ("M3: Returning", "C3: Custom-3"),
        0b011 => ("M4: Committed", "C4: Custom-4"),
        0b010 => ("M5: Special", "C5: Custom-5"),
        0b001 => ("M6: Priority", "C6: Custom-6"),
        _ => ("Unknown", "Unknown"),
    };
    if custom {
        ("custom", cust_name)
    } else {
        ("standard", std_name)
    }
}

/// One message bit as decoded from a destination character: zero, a Standard
/// 1, or a Custom 1 (APRS 1.0.1 p.44 encoding table, p.45 Std/Custom rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitKind {
    Zero,
    Std,
    Custom,
}

impl BitKind {
    fn bit(self) -> u8 {
        match self {
            BitKind::Zero => 0,
            _ => 1,
        }
    }
}

/// One decoded destination character (APRS 1.0.1 p.44 table, bytes 1-6).
struct DestChar {
    /// Latitude digit 0-9, or `None` for a space (position ambiguity).
    digit: Option<u8>,
    /// Message bit kind (only meaningful for chars 1-3).
    msg: BitKind,
    /// North (true) for the N/S indicator (only meaningful for char 4).
    north: Option<bool>,
    /// Longitude offset +100 (true) vs +0 (only meaningful for char 5).
    long_offset_100: Option<bool>,
    /// West (true) for the W/E indicator (only meaningful for char 6).
    west: Option<bool>,
}

/// Decode one Mic-E destination-address character per the APRS 1.0.1 p.44
/// table. The destination characters arrive already un-shifted to plain ASCII
/// (the AX.25 layer has reversed the 1-bit left shift), so we match the ASCII
/// values directly.
fn decode_dest_char(ch: u8) -> Option<DestChar> {
    // Digits 0-9: lat digit 0-9, msg bit 0, South, +0, East (p.44 left table).
    if ch.is_ascii_digit() {
        let d = ch - b'0';
        return Some(DestChar {
            digit: Some(d),
            msg: BitKind::Zero,
            north: Some(false),
            long_offset_100: Some(false),
            west: Some(false),
        });
    }
    // A-K: Custom 1 message bit; A-J carry lat digits 0-9, K = space. These
    // characters are NOT valid in bytes 4-6 (p.44 note).
    if (b'A'..=b'K').contains(&ch) {
        let digit = if ch == b'K' { None } else { Some(ch - b'A') };
        return Some(DestChar {
            digit,
            msg: BitKind::Custom,
            north: None,
            long_offset_100: None,
            west: None,
        });
    }
    // L: lat digit space, msg 0, South, +0, East (p.44).
    if ch == b'L' {
        return Some(DestChar {
            digit: None,
            msg: BitKind::Zero,
            north: Some(false),
            long_offset_100: Some(false),
            west: Some(false),
        });
    }
    // P-Z: Standard 1 message bit; North, +100, West. P-Y carry lat 0-9,
    // Z = space (p.44 right table).
    if (b'P'..=b'Z').contains(&ch) {
        let digit = if ch == b'Z' { None } else { Some(ch - b'P') };
        return Some(DestChar {
            digit,
            msg: BitKind::Std,
            north: Some(true),
            long_offset_100: Some(true),
            west: Some(true),
        });
    }
    None
}

/// Decode the Mic-E destination address (6 plain-ASCII characters) into
/// latitude, message type, and the N/S, longitude-offset and W/E indicators.
/// APRS 1.0.1 Chapter 10, p.43-45.
///
/// Returns `(lat_degrees, north, long_offset_100, west, msg_kind, msg_name,
/// ambiguity)`. `ambiguity` is the count of trailing latitude digits that were
/// transmitted as spaces (position ambiguity, p.53-54).
#[allow(clippy::type_complexity)]
fn decode_dest(dest: &str) -> Option<(f64, bool, bool, bool, &'static str, &'static str, u8)> {
    let bytes = dest.as_bytes();
    if bytes.len() < 6 {
        return None;
    }
    let mut chars = Vec::with_capacity(6);
    for &b in &bytes[0..6] {
        chars.push(decode_dest_char(b)?);
    }
    // Latitude digits: chars 0-5 are deg(0,1) min(2,3) hundredths(4,5).
    // Spaces (None) are position ambiguity; the spec masks the trailing
    // digits. We count them and treat masked digits as 0 for the numeric
    // value (p.53-54).
    let mut digits = [0u8; 6];
    let mut ambiguity = 0u8;
    for (i, c) in chars.iter().enumerate() {
        match c.digit {
            Some(d) => digits[i] = d,
            None => {
                digits[i] = 0;
                ambiguity += 1;
            }
        }
    }
    let deg = digits[0] as f64 * 10.0 + digits[1] as f64;
    let min = digits[2] as f64 * 10.0
        + digits[3] as f64
        + (digits[4] as f64 * 10.0 + digits[5] as f64) / 100.0;
    let lat_mag = deg + min / 60.0;

    // N/S from char 4 (index 3), offset from char 5 (index 4), W/E from char 6
    // (index 5). p.43-44.
    let north = chars[3].north?;
    let long_offset_100 = chars[4].long_offset_100?;
    let west = chars[5].west?;
    let lat = if north { lat_mag } else { -lat_mag };

    // Message bits A/B/C from chars 1-3 (indices 0-2). p.44-45.
    let (kind, name) = message_type(chars[0].msg, chars[1].msg, chars[2].msg);

    Some((lat, north, long_offset_100, west, kind, name, ambiguity))
}

/// Decode the Mic-E longitude from the 3 info-field bytes d+28 / m+28 / h+28,
/// using the longitude offset from the destination address. APRS 1.0.1 p.47-49.
fn decode_longitude(d28: u8, m28: u8, h28: u8, offset_100: bool, west: bool) -> f64 {
    // Degrees (p.48 decode algorithm).
    let mut d = d28 as i32 - 28;
    if offset_100 {
        d += 100;
    }
    if (180..=189).contains(&d) {
        d -= 80;
    } else if (190..=199).contains(&d) {
        d -= 190;
    }
    // Minutes (p.49).
    let mut m = m28 as i32 - 28;
    if m >= 60 {
        m -= 60;
    }
    // Hundredths of minutes (p.49).
    let h = h28 as i32 - 28;

    let mag = d as f64 + (m as f64 + h as f64 / 100.0) / 60.0;
    if west {
        -mag
    } else {
        mag
    }
}

/// Decode the Mic-E speed (knots) and course (degrees) from the 3 info-field
/// bytes SP+28 / DC+28 / SE+28. APRS 1.0.1 p.49-52 ("Decoding the Speed and
/// Course", p.52).
fn decode_speed_course(sp: u8, dc: u8, se: u8) -> (i32, i32) {
    // SP+28: tens of knots = (SP-28)*10 (p.52).
    let sp_tens = (sp as i32 - 28) * 10;
    // DC+28: (DC-28)/10 -> quotient = units of speed, remainder = hundreds of
    // course (p.52).
    let dc_v = dc as i32 - 28;
    let units = dc_v / 10;
    let course_hundreds = dc_v % 10;
    // SE+28: tens+units of course = (SE-28) (p.52).
    let se_v = se as i32 - 28;

    let mut speed = sp_tens + units;
    let mut course = course_hundreds * 100 + se_v;
    // Final adjustments (p.52).
    if speed >= 800 {
        speed -= 800;
    }
    if course >= 400 {
        course -= 400;
    }
    (speed, course)
}

/// Decode a full Mic-E packet from the AX.25 destination callsign (6 plain
/// ASCII characters) and the information field. APRS 1.0.1 Chapter 10.
///
/// Returns `None` if the destination is not a valid Mic-E address or the info
/// field is shorter than the mandatory 9 bytes (p.47: "if the Information
/// field appears to be less than 9 bytes long, the packet must be ignored").
pub fn parse(dest: &str, info: &[u8]) -> Option<MicE> {
    // Info must start with a Mic-E data-type id and carry >= 9 bytes (p.46-47).
    if info.len() < 9 {
        return None;
    }
    let dti = info[0];
    // Current/old GPS data identifiers (p.46): grave-accent ` and apostrophe '
    // (also 0x1c / 0x1d for Rev.0 beta units).
    let is_mice_dti = matches!(dti, b'`' | b'\'' | 0x1c | 0x1d);
    if !is_mice_dti {
        return None;
    }

    let (lat, north, offset_100, west, msg_kind, msg_name, ambiguity) = decode_dest(dest)?;

    // Info layout: [dti][d+28][m+28][h+28][SP+28][DC+28][SE+28][sym code]
    // [sym table id] then optional telemetry/status. p.46.
    let d28 = info[1];
    let m28 = info[2];
    let h28 = info[3];
    let sp = info[4];
    let dc = info[5];
    let se = info[6];
    let sym_code = info[7] as char;
    let sym_table = info[8] as char;

    let lon = decode_longitude(d28, m28, h28, offset_100, west);
    let (speed, course) = decode_speed_course(sp, dc, se);

    // Optional trailing field: Mic-E telemetry (starts with ` or ' or 0x1d) or
    // status text. p.54.
    let mut fields = json!({
        "lat": lat,
        "lon": lon,
        "speed_knots": speed,
        "course_deg": course,
        "symbol_code": sym_code.to_string(),
        "symbol_table": sym_table.to_string(),
        "message_type": msg_name,
        "message_class": msg_kind,
        "mic_e": true,
        "north": north,
        "west": west,
        "long_offset_100": offset_100,
    });
    if ambiguity > 0 {
        fields["position_ambiguity"] = json!(ambiguity);
    }

    if info.len() > 9 {
        let trailing = &info[9..];
        let tflag = trailing[0];
        if matches!(tflag, b'`' | b'\'' | 0x1d) {
            // Mic-E telemetry data (p.54). ` => 2 hex channels, ' => 5 hex,
            // 0x1d => 5 binary channels.
            fields["status"] = json!(String::from_utf8_lossy(&trailing[1..]).to_string());
            fields["has_telemetry"] = json!(true);
        } else {
            // Mic-E status text (may carry a Maidenhead locator + altitude).
            let status = String::from_utf8_lossy(trailing).to_string();
            fields["status"] = json!(status);
        }
    }

    Some(MicE { fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 10, the destination-address
    /// worked example on p.44.
    ///
    /// "For a station at a latitude of 33 degrees 25.64 minutes north, in the
    /// western hemisphere, with longitude offset +0 degrees, and transmitting
    /// standard message identifier bits 1/0/0, the encoding of the first 6
    /// bytes of the Destination Address field is ... S 3 2 U 6 T" (p.44 table).
    ///
    /// We feed the literal destination "S32U6T" from the spec and assert the
    /// decoder recovers 33.4273°N, North, offset +0, West, and message bits
    /// 1/0/0 => Standard M3 (Returning, per the p.45 message-type table).
    #[test]
    fn dest_worked_example_p44() {
        let (lat, north, offset_100, west, kind, name, amb) = decode_dest("S32U6T").unwrap();
        // 33 deg 25.64 min = 33 + 25.64/60 = 33.42733...
        assert!((lat - 33.427333).abs() < 1e-5, "lat={lat}");
        assert!(north, "p.44: north");
        assert!(!offset_100, "p.44: longitude offset +0");
        assert!(west, "p.44: western hemisphere");
        // Message bits A/B/C = 1/0/0 Standard => M3 Returning (p.45 table).
        assert_eq!(kind, "standard");
        assert_eq!(name, "M3: Returning");
        assert_eq!(amb, 0);
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 p.46, "Some examples of message type
    /// encoding": destination first-3 "S32" => Standard 1/0/0 => M3 Returning;
    /// "234" => 0/0/0 => Emergency.
    #[test]
    fn message_type_examples_p46() {
        // "S32" -> S=Std(1), 3=0, 2=0 => 1/0/0 Standard M3.
        let (_, _, _, _, k, n, _) = decode_dest("S32U6T").unwrap();
        assert_eq!((k, n), ("standard", "M3: Returning"));
        // "234..." -> 2=0,3=0,4=0 => Emergency. Bytes 4-6 must still be valid
        // (digits encode South/+0/East), so "234567" is a complete address.
        let (_, _, _, _, k, n, _) = decode_dest("234567").unwrap();
        assert_eq!((k, n), ("emergency", "Emergency"));
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 10, the information-field worked
    /// example on p.53 ("Example of Decoding the Information Field Data").
    ///
    /// "If the first 9 bytes of the Information field contain `(_fn"Oj/, and
    /// the destination address specifies that the station is in the western
    /// hemisphere with a longitude offset of +100 degrees, then the data is
    /// decoded as follows: ... longitude is 112 degrees 7.74 minutes west ...
    /// final computed speed of 20 knots ... final value of 251 degrees for the
    /// course ... the jeep symbol from the Primary Symbol Table" (p.53).
    ///
    /// The 9 info bytes are: 0x60 '`', 0x28 '(', 0x5f '_', 0x66 'f', 0x6e 'n',
    /// 0x22 '"', 0x4f 'O', 0x6a 'j', 0x2f '/'.
    #[test]
    fn info_field_worked_example_p53() {
        // Western hemisphere, longitude offset +100 (from the destination).
        let lon = decode_longitude(b'(', b'_', b'f', true, true);
        // 112 deg 7.74 min west = -(112 + 7.74/60) = -112.129
        assert!((lon - (-112.129)).abs() < 1e-3, "lon={lon}");

        let (speed, course) = decode_speed_course(b'n', b'"', b'O');
        assert_eq!(speed, 20, "p.53: final computed speed 20 knots");
        assert_eq!(course, 251, "p.53: final value 251 degrees for course");
    }

    /// SPEC GROUND TRUTH — the full p.53 example through the top-level parse(),
    /// driven from the literal spec bytes. Destination must specify western
    /// hemisphere + offset +100; per the p.44 table, "P" gives North/+100/West
    /// for bytes 4-6 — but we need byte 5 = +100 and byte 6 = West, which any
    /// of P-Z provide. We build a destination that yields offset +100 / West.
    #[test]
    fn parse_full_mic_e_p53() {
        // Destination: lat digits arbitrary; we just need byte5 offset +100 and
        // byte6 West. Use "T7P3SY": byte5 'S' => +100, byte6 'Y' => West.
        let info = b"`(_fn\"Oj/";
        let m = parse("T7P3SY", info).expect("Mic-E parse");
        let lon = m.fields["lon"].as_f64().unwrap();
        assert!((lon - (-112.129)).abs() < 1e-3, "lon={lon}");
        assert_eq!(m.fields["speed_knots"], 20);
        assert_eq!(m.fields["course_deg"], 251);
        // p.53: jeep symbol from the Primary Symbol Table => code 'j', table '/'.
        assert_eq!(m.fields["symbol_code"], "j");
        assert_eq!(m.fields["symbol_table"], "/");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 p.52, "Example of Mic-E Speed and Course
    /// Encoding": speed 86 knots, course 194 degrees. SP+28 char 't' or '$';
    /// DC+28 char ']' or 'Y'; SE+28 char 'z'. We decode both valid SP+28
    /// encodings (p.50 note: two schemes exist) and assert 86 / 194.
    #[test]
    fn speed_course_example_p52() {
        // 't' = 0x74 = 116, ']' = 0x5d = 93, 'z' = 0x7a = 122.
        let (s1, c1) = decode_speed_course(b't', b']', b'z');
        assert_eq!(s1, 86);
        assert_eq!(c1, 194);
        // Alternate SP+28 '$' = 0x24 = 36, DC+28 'Y' = 0x59 = 89, SE+28 'z'.
        let (s2, c2) = decode_speed_course(b'$', b'Y', b'z');
        assert_eq!(s2, 86);
        assert_eq!(c2, 194);
    }

    /// A too-short info field (< 9 bytes) must be rejected (p.47: "the packet
    /// must be ignored").
    #[test]
    fn short_info_rejected() {
        assert!(parse("T7P3SY", b"`(_fn\"Oj").is_none());
    }

    /// Position ambiguity: trailing space latitude digits (encoded as Z in the
    /// destination) are counted. APRS 1.0.1 p.53-54: dest "T4SQZZ" masks the
    /// last two latitude digits.
    #[test]
    fn position_ambiguity_p54() {
        let (_, _, _, _, _, _, amb) = decode_dest("T4SQZZ").unwrap();
        assert_eq!(amb, 2, "p.54: last two latitude digits ambiguous");
    }
}
