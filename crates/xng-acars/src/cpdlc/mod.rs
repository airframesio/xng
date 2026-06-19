//! CPDLC (FANS-1/A) message identification, ported from MIT-licensed
//! libacars (asn1c-generated FANSAC* tables + asn1-format-cpdlc-text.c
//! labels; see ../PROVENANCE.md).
//!
//! Scope: unaligned-PER decode of the ATC message header (message id,
//! optional reference number, optional timestamp), the first message
//! element's CHOICE tag mapped to its human-readable template, and the
//! element arguments for the shapes we decode (rendered into the
//! template). Additional elements are walked while every preceding
//! element's argument shape decodes (UPER has no per-element length
//! prefix). Argument shapes not yet decoded (e.g. FANSPositionReport,
//! RouteClearance trackDetail) leave the bracketed template in place and
//! stop the walk.
//!
//! UPER layout (from the generated constraint tables):
//! - ATCDownlinkMessage / ATCUplinkMessage = SEQUENCE { header,
//!   first-element, additional-elements OPTIONAL } → 1 presence bit
//! - header = SEQUENCE { msgId (0..63, 6 bits), msgRef OPTIONAL
//!   (0..63, 6 bits), timestamp OPTIONAL } → 2 presence bits
//! - timestamp = hours (0..23, 5 bits), minutes (0..59, 6 bits),
//!   seconds (0..59, 6 bits)
//! - element = CHOICE: downlink (0..128, 8 bits), uplink (0..182,
//!   8 bits), non-extensible

use serde::Serialize;

mod tables;
use tables::{DOWNLINK_ELEMENTS, UPLINK_ELEMENTS};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CpdlcMessage {
    pub msg_id: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_ref: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// First message element's ASN.1 tag (e.g. "dM0NULL").
    pub element: String,
    /// Human-readable template for the element ("WILCO",
    /// "REQUEST [altitude]", ...). Bracketed arguments are not decoded
    /// in v1.
    pub text: String,
    /// Decoded element arguments in template order (when the element's
    /// argument structure is one we decode; see `decode`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Additional decoded elements beyond the first (reachable only
    /// while every preceding element's argument shape is decodable).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional: Vec<CpdlcElement>,
    /// The message carries additional elements beyond the first.
    pub more_elements: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CpdlcElement {
    pub element: String,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

/// FANSAltitude CHOICE (3-bit index; widths/offsets and the value
/// semantics — QNH/QFE in tens of feet, flight level metric in tens of
/// meters — from libacars's generated constraints and text formatters).
fn read_altitude(b: &mut Bits) -> Option<String> {
    Some(match b.read(3)? {
        0 => format!("{} ft", b.read(12)? * 10),        // QNH (0..2500) x10
        1 => format!("{} m", b.read(14)?),              // QNH meters
        2 => format!("{} ft QFE", b.read(12)? * 10),    // QFE (0..2100) x10
        3 => format!("{} m QFE", b.read(13)?),          // QFE meters
        4 => format!("{} ft", b.read(18)?),             // GNSS feet
        5 => format!("{} m", b.read(16)?),              // GNSS meters
        6 => format!("FL{}", 30 + b.read(10)?),         // flight level (30..600)
        7 => format!("{} m", (100 + b.read(11)?) * 10), // metric FL (100..2000) x10
        _ => unreachable!(),
    })
}

/// FANSSpeed CHOICE (3-bit index; widths/offsets and value semantics —
/// English speeds in tens of knots, metric in tens of km/h, Mach in
/// hundredths — from libacars's generated constraints and formatters).
fn read_speed(b: &mut Bits) -> Option<String> {
    Some(match b.read(3)? {
        0 => format!("{} kt", (7 + b.read(5)?) * 10),     // indicated (7..38)
        1 => format!("{} km/h", (10 + b.read(7)?) * 10),  // indicated metric
        2 => format!("{} kt", (7 + b.read(6)?) * 10),     // true (7..70)
        3 => format!("{} km/h", (10 + b.read(7)?) * 10),  // true metric
        4 => format!("{} kt GS", (7 + b.read(6)?) * 10),  // ground (7..70)
        5 => format!("{} km/h GS", (10 + b.read(8)?) * 10), // ground metric
        6 => format!("M{:.2}", (61 + b.read(5)?) as f32 / 100.0), // mach (61..92)
        7 => format!("M{:.2}", (93 + b.read(9)?) as f32 / 100.0), // mach large
        _ => unreachable!(),
    })
}

/// Constrained IA5 string: `len_bits`-bit length (offset from `min`),
/// then 7-bit characters.
fn read_ia5(b: &mut Bits, len_bits: usize, min: u32) -> Option<String> {
    let n = min + if len_bits > 0 { b.read(len_bits)? } else { 0 };
    let mut s = String::new();
    for _ in 0..n {
        let c = b.read(7)? as u8;
        if !(0x20..0x7F).contains(&c) {
            return None;
        }
        s.push(c as char);
    }
    Some(s)
}

/// FANSPosition CHOICE (3-bit index): fix name, navaid, airport,
/// latitude/longitude (degrees + optional tenths-of-minutes +
/// direction), or place-bearing-distance.
fn read_position(b: &mut Bits) -> Option<String> {
    match b.read(3)? {
        0 => read_ia5(b, 3, 1), // fixName SIZE(1..5)
        1 => read_ia5(b, 2, 1), // navaid SIZE(1..4)
        2 => read_ia5(b, 0, 4), // airport SIZE(4)
        3 => read_latlon(b),
        4 => read_place_bearing_distance(b),
        _ => None,
    }
}

/// FANSPlaceBearingDistance: SEQUENCE { fixName (IA5 SIZE 1..5),
/// latitudeLongitude OPTIONAL, degrees, distance }. One OPTIONAL → a
/// leading presence bit. Constraints + member order from the libacars
/// asn1c FANSPlaceBearingDistance tables (lat/lon is optional member 1).
fn read_place_bearing_distance(b: &mut Bits) -> Option<String> {
    let has_ll = b.read(1)? == 1;
    let fix = read_ia5(b, 3, 1)?; // FANSFixName SIZE(1..5)
    let ll = if has_ll { Some(read_latlon(b)?) } else { None };
    let deg = read_degrees(b)?;
    let dist = read_distance(b)?;
    Some(match ll {
        Some(ll) => format!("{fix} ({ll}) BRG {deg} DIST {dist}"),
        None => format!("{fix} BRG {deg} DIST {dist}"),
    })
}

fn read_latlon(b: &mut Bits) -> Option<String> {
    let lat_has_min = b.read(1)? == 1;
    let lat_deg = b.read(7)?;
    let lat_min = if lat_has_min { Some(b.read(10)?) } else { None };
    let ns = if b.read(1)? == 1 { 'S' } else { 'N' };
    let lon_has_min = b.read(1)? == 1;
    let lon_deg = b.read(8)?;
    let lon_min = if lon_has_min { Some(b.read(10)?) } else { None };
    let ew = if b.read(1)? == 1 { 'W' } else { 'E' };
    if lat_deg > 90 || lon_deg > 180 {
        return None;
    }
    let fmt = |deg: u32, min: Option<u32>, dir: char| match min {
        Some(m) => format!("{deg}°{:.1}'{dir}", m as f32 / 10.0),
        None => format!("{deg}°{dir}"),
    };
    Some(format!("{} {}", fmt(lat_deg, lat_min, ns), fmt(lon_deg, lon_min, ew)))
}

/// FANSPublishedIdentifier: fixName + OPTIONAL latitudeLongitude.
fn read_published(b: &mut Bits) -> Option<String> {
    let has_ll = b.read(1)? == 1;
    let name = read_ia5(b, 3, 1)?; // Fixname SIZE(1..5)
    if has_ll {
        Some(format!("{name} ({})", read_latlon(b)?))
    } else {
        Some(name)
    }
}

/// FANSRouteClearance: ten optional components (constraints from the
/// libacars asn1c tables). The trailing routeInformationAdditional is
/// reported present-but-undecoded; it is last, so the route itself is
/// always reachable.
fn read_route_clearance(b: &mut Bits) -> Option<String> {
    let present: Vec<bool> = (0..10).map(|_| b.read(1) == Some(1)).collect();
    let mut parts: Vec<String> = Vec::new();
    let read_runway = |b: &mut Bits| -> Option<String> {
        let dir = b.read(6)? + 1; // (1..36)
        let cfg = match b.read(2)? {
            0 => "L",
            1 => "R",
            2 => "C",
            _ => "",
        };
        Some(format!("RWY {dir:02}{cfg}"))
    };
    let read_procedure = |b: &mut Bits| -> Option<String> {
        let has_transition = b.read(1)? == 1;
        let ptype = match b.read(2)? {
            0 => "ARRIVAL",
            1 => "APPROACH",
            _ => "DEPARTURE",
        };
        let name = read_ia5(b, 3, 1)?; // FANSProcedure SIZE(1..6)
        let mut s = format!("{name} ({ptype})");
        if has_transition {
            s.push_str(&format!(" TRANS {}", read_ia5(b, 3, 1)?)); // SIZE(1..5)
        }
        Some(s)
    };
    if present[0] {
        parts.push(format!("DEP {}", read_ia5(b, 0, 4)?));
    }
    if present[1] {
        parts.push(format!("DEST {}", read_ia5(b, 0, 4)?));
    }
    if present[2] {
        parts.push(format!("DEP {}", read_runway(b)?));
    }
    if present[3] {
        parts.push(format!("SID {}", read_procedure(b)?));
    }
    if present[4] {
        parts.push(format!("ARR {}", read_runway(b)?));
    }
    if present[5] {
        parts.push(format!("APPROACH {}", read_procedure(b)?));
    }
    if present[6] {
        parts.push(format!("STAR {}", read_procedure(b)?));
    }
    if present[7] {
        parts.push(format!("INTERCEPT {}", read_ia5(b, 3, 1)?)); // SIZE(1..5)
    }
    if present[8] {
        // FANSRouteInformationSequence SIZE(1..128).
        let n = b.read(7)? as usize + 1;
        let mut legs = Vec::with_capacity(n);
        for _ in 0..n {
            let leg = match b.read(3)? {
                0 => read_published(b)?,
                1 => read_latlon(b)?,
                2 => format!(
                    "{} BRG {} / {} BRG {}",
                    read_published(b)?,
                    read_degrees(b)?,
                    read_published(b)?,
                    read_degrees(b)?
                ),
                3 => {
                    let pb = read_published(b)?;
                    let deg = read_degrees(b)?;
                    let dist = if b.read(1)? == 0 {
                        format!("{:.1} NM", b.read(14)? as f64 / 10.0)
                    } else {
                        format!("{} KM", b.read(10)? + 1)
                    };
                    format!("{pb} BRG {deg} DIST {dist}")
                }
                4 => read_ia5(b, 3, 1)?, // airway SIZE(1..5)
                _ => return None, // trackDetail: not decoded
            };
            legs.push(leg);
        }
        parts.push(format!("ROUTE {}", legs.join(" ")));
    }
    if present[9] {
        parts.push("[+additional data undecoded]".into());
    }
    Some(parts.join(", "))
}



/// FANSTime: hours (0..23, 5 bits) + minutes (0..59, 6 bits).
fn read_time(b: &mut Bits) -> Option<String> {
    let h = b.read(5)?;
    let m = b.read(6)?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}"))
}

/// Decode the element's arguments when its type (the tag's suffix after
/// `dMnn`/`uMnn`) is one of the simple shapes we handle. Returns None
/// for argument structures not decoded yet — the caller keeps the
/// bracketed template untouched.
/// FANSVerticalRate: CHOICE english (0..60, 100 ft/min) / metric
/// (0..200, 10 m/min) — constraints per libacars asn1c tables.
fn read_vertical_rate(b: &mut Bits) -> Option<String> {
    Some(if b.read(1)? == 0 {
        format!("{} FT/MIN", b.read(6)? * 100)
    } else {
        format!("{} M/MIN", b.read(8)? * 10)
    })
}

/// FANSDegrees: CHOICE magnetic/true, each INTEGER (1..360).
fn read_degrees(b: &mut Bits) -> Option<String> {
    let mag = b.read(1)? == 0;
    Some(format!("{}°{}", b.read(9)? + 1, if mag { "M" } else { "T" }))
}

/// FANSDirection: ENUMERATED (0..10).
fn read_direction(b: &mut Bits) -> Option<String> {
    const DIRS: [&str; 11] = [
        "LEFT", "RIGHT", "EITHER SIDE", "NORTH", "SOUTH", "EAST", "WEST",
        "NORTH-EAST", "NORTH-WEST", "SOUTH-EAST", "SOUTH-WEST",
    ];
    DIRS.get(b.read(4)? as usize).map(|s| s.to_string())
}

/// FANSFreeText: IA5String SIZE (1..256).
fn read_freetext(b: &mut Bits) -> Option<String> {
    let n = b.read(8)? as usize + 1;
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        s.push(b.read(7)? as u8 as char);
    }
    Some(s)
}

/// FANSDistanceOffset: CHOICE(1 bit) of distanceOffsetNm (INTEGER 1..128,
/// 7 bits, integer nm) / distanceOffsetKm (INTEGER 1..256, 8 bits,
/// integer km). Constraints + units from libacars asn1c tables and
/// asn1-format-cpdlc-text.c (FANSDistanceOffsetNm " nm", DistanceMetric
/// " km").
fn read_distance_offset(b: &mut Bits) -> Option<String> {
    Some(if b.read(1)? == 0 {
        format!("{} nm", b.read(7)? + 1)
    } else {
        format!("{} km", b.read(8)? + 1)
    })
}

/// FANSDistance: CHOICE(1 bit) of distanceNm (INTEGER 0..9999, 14 bits,
/// tenths of nm via FANSDistanceEnglish 0.1) / distanceKm (INTEGER
/// 1..1024, 10 bits, integer km via FANSDistanceMetric).
fn read_distance(b: &mut Bits) -> Option<String> {
    Some(if b.read(1)? == 0 {
        format!("{:.1} nm", b.read(14)? as f64 / 10.0)
    } else {
        format!("{} km", b.read(10)? + 1)
    })
}

/// FANSFrequency: CHOICE(2 bits) of hf (INTEGER 2850..28000 kHz, 15 bits,
/// rendered in kHz) / vhf (117000..138000 kHz, 15 bits) / uhf
/// (225000..399975 kHz, 18 bits) — vhf/uhf rendered in MHz — /
/// satchannel (NumericString SIZE 12, 4 bits per char). Constraints +
/// formatters from libacars asn1c tables and asn1-format-cpdlc-text.c.
fn read_frequency(b: &mut Bits) -> Option<String> {
    match b.read(2)? {
        0 => Some(format!("{} kHz", 2850 + b.read(15)?)),
        1 => Some(format!("{:.3} MHz", (117000 + b.read(15)?) as f64 / 1000.0)),
        2 => Some(format!("{:.3} MHz", (225000 + b.read(18)?) as f64 / 1000.0)),
        3 => {
            // NumericString SIZE(12): 12 chars, each a 4-bit constrained
            // alphabet index into " 0123456789" (NumericString PER
            // alphabet; per-char index range 32..57 in the asn1c table).
            const NUM: &[u8] = b" 0123456789";
            let mut s = String::with_capacity(12);
            for _ in 0..12 {
                let idx = b.read(4)? as usize;
                s.push(*NUM.get(idx).unwrap_or(&b'?') as char);
            }
            Some(s.trim().to_string())
        }
        _ => unreachable!(),
    }
}

/// FANSBeaconCode: SEQUENCE OF SIZE(4..4) FANSBeaconCodeOctalDigit
/// (INTEGER 0..7, 3 bits). Fixed size → no length count. Rendered as the
/// 4-digit squawk (libacars FANSBeaconCode formatter "%ld%ld%ld%ld").
fn read_beacon_code(b: &mut Bits) -> Option<String> {
    let mut s = String::with_capacity(4);
    for _ in 0..4 {
        let d = b.read(3)?;
        if d > 7 {
            return None;
        }
        s.push((b'0' + d as u8) as char);
    }
    Some(s)
}

/// FANSProcedureName: SEQUENCE { procedureType (ENUM 0..2, 2 bits),
/// procedure (IA5 SIZE 1..6, 3-bit length offset 1), procedureTransition
/// OPTIONAL (IA5 SIZE 1..6) }. One OPTIONAL → a leading presence bit.
/// Constraints from libacars asn1c tables.
fn read_procedure_name(b: &mut Bits) -> Option<String> {
    let has_transition = b.read(1)? == 1;
    let ptype = match b.read(2)? {
        0 => "ARRIVAL",
        1 => "APPROACH",
        2 => "DEPARTURE",
        _ => return None,
    };
    let name = read_ia5(b, 3, 1)?; // FANSProcedure SIZE(1..6)
    let mut s = format!("{name} ({ptype})");
    if has_transition {
        s.push_str(&format!(" TRANS {}", read_ia5(b, 3, 1)?)); // SIZE(1..6)
    }
    Some(s)
}

/// FANSAltimeter: CHOICE(1 bit) of altimeterEnglish (INTEGER 2200..3200,
/// 10 bits, inHg ×0.01, 2 decimals) / altimeterMetric (7500..12500,
/// 13 bits, hPa ×0.1, 1 decimal). Constraints + formatters from libacars.
fn read_altimeter(b: &mut Bits) -> Option<String> {
    Some(if b.read(1)? == 0 {
        format!("{:.2} inHg", (2200 + b.read(10)?) as f64 / 100.0)
    } else {
        format!("{:.1} hPa", (7500 + b.read(13)?) as f64 / 10.0)
    })
}

/// FANSATISCode: IA5String SIZE(1..1) — exactly one 7-bit char, no length
/// bits. Constraint from libacars asn1c table.
fn read_atis_code(b: &mut Bits) -> Option<String> {
    read_ia5(b, 0, 1)
}

/// FANSRemainingFuel: SEQUENCE { hours (0..23, 5 bits), minutes (0..59,
/// 6 bits) } — endurance as HH:MM (libacars renders it via the FANSTime
/// formatter "%02ld:%02ld").
fn read_remaining_fuel(b: &mut Bits) -> Option<String> {
    let h = b.read(5)?;
    let m = b.read(6)?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}"))
}

/// FANSRemainingSouls: INTEGER 1..1024 (10 bits). Persons on board.
fn read_remaining_souls(b: &mut Bits) -> Option<String> {
    Some(format!("{}", b.read(10)? + 1))
}

/// FANSErrorInformation: ENUMERATED 0..16 (5 bits). Labels from the
/// libacars FANSErrorInformation enum (asn1c FANSErrorInformation.c).
fn read_error_information(b: &mut Bits) -> Option<String> {
    const ERRS: [&str; 17] = [
        "application error",
        "duplicate message identification number",
        "unrecognized message reference number",
        "end service with pending messages",
        "end service with no valid response",
        "insufficient message storage capacity",
        "no available message identification number",
        "commanded termination",
        "insufficient data",
        "unexpected data",
        "invalid data",
        "reserved error message 1",
        "reserved error message 2",
        "reserved error message 3",
        "reserved error message 4",
        "reserved error message 5",
        "reserved error message 6",
    ];
    ERRS.get(b.read(5)? as usize).map(|s| s.to_string())
}

/// FANSVersionNumber: INTEGER 0..15 (4 bits).
fn read_version_number(b: &mut Bits) -> Option<String> {
    Some(format!("{}", b.read(4)?))
}

/// FANSICAOfacilitydesignation: IA5String SIZE(4..4) — 4 fixed 7-bit
/// chars, no length bits. Constraint from libacars asn1c table.
fn read_icao_facility_designation(b: &mut Bits) -> Option<String> {
    read_ia5(b, 0, 4)
}

/// FANSTp4table: ENUMERATED 0..1 (1 bit): labelA / labelB.
fn read_tp4table(b: &mut Bits) -> Option<String> {
    Some(match b.read(1)? {
        0 => "label A".to_string(),
        _ => "label B".to_string(),
    })
}

/// FANSToFrom: ENUMERATED 0..1 (1 bit): to / from.
fn read_tofrom(b: &mut Bits) -> Option<String> {
    Some(match b.read(1)? {
        0 => "TO".to_string(),
        _ => "FROM".to_string(),
    })
}

/// FANSICAOFacilityIdentification: CHOICE(1 bit) of
/// iCAOfacilitydesignation (IA5 SIZE 4) / iCAOfacilityname (IA5 SIZE
/// 3..18, 4-bit length offset 3).
fn read_icao_facility_id(b: &mut Bits) -> Option<String> {
    match b.read(1)? {
        0 => read_ia5(b, 0, 4), // designation SIZE(4)
        _ => read_ia5(b, 4, 3), // name SIZE(3..18)
    }
}

/// FANSICAOUnitName: SEQUENCE { iCAOFacilityIdentification,
/// iCAOFacilityFunction (ENUM 0..7, 3 bits) }. Function labels from the
/// libacars FANSICAOFacilityFunction enum.
fn read_icao_unit_name(b: &mut Bits) -> Option<String> {
    let id = read_icao_facility_id(b)?;
    const FUNCS: [&str; 8] = [
        "center",
        "approach",
        "tower",
        "final",
        "ground control",
        "clearance delivery",
        "departure",
        "control",
    ];
    let func = FUNCS.get(b.read(3)? as usize)?;
    Some(format!("{id} {func}"))
}

/// FANSICAOUnitNameFrequency: SEQUENCE { iCAOUnitName, frequency }.
fn read_icao_unit_name_frequency(b: &mut Bits) -> Option<(String, String)> {
    let unit = read_icao_unit_name(b)?;
    let freq = read_frequency(b)?;
    Some((unit, freq))
}

fn read_args(tag: &str, b: &mut Bits) -> Option<Vec<String>> {
    let ty = tag.trim_start_matches(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == 'M');
    match ty {
        "NULL" => Some(Vec::new()),
        "Altitude" => Some(vec![read_altitude(b)?]),
        // SEQUENCE SIZE(2..2): fixed size, no length bits.
        "AltitudeAltitude" => Some(vec![read_altitude(b)?, read_altitude(b)?]),
        "Time" => Some(vec![read_time(b)?]),
        "Speed" => Some(vec![read_speed(b)?]),
        "SpeedSpeed" => Some(vec![read_speed(b)?, read_speed(b)?]),
        "Position" => Some(vec![read_position(b)?]),
        "PositionPosition" => Some(vec![read_position(b)?, read_position(b)?]),
        "PositionAltitude" => Some(vec![read_position(b)?, read_altitude(b)?]),
        "AltitudePosition" => Some(vec![read_altitude(b)?, read_position(b)?]),
        "TimeAltitude" => Some(vec![read_time(b)?, read_altitude(b)?]),
        "AltitudeTime" => Some(vec![read_altitude(b)?, read_time(b)?]),
        "PositionTime" => Some(vec![read_position(b)?, read_time(b)?]),
        "TimePosition" => Some(vec![read_time(b)?, read_position(b)?]),
        "PositionSpeed" => Some(vec![read_position(b)?, read_speed(b)?]),
        "TimeSpeed" => Some(vec![read_time(b)?, read_speed(b)?]),
        "AltitudeSpeed" => Some(vec![read_altitude(b)?, read_speed(b)?]),
        "PositionTimeAltitude" => {
            Some(vec![read_position(b)?, read_time(b)?, read_altitude(b)?])
        }
        "VerticalRate" => Some(vec![read_vertical_rate(b)?]),
        "Degrees" => Some(vec![read_degrees(b)?]),
        "DirectionDegrees" => Some(vec![read_direction(b)?, read_degrees(b)?]),
        "PositionDegrees" => Some(vec![read_position(b)?, read_degrees(b)?]),
        "FreeText" => Some(vec![read_freetext(b)?]),
        "RouteClearance" => Some(vec![read_route_clearance(b)?]),
        "PositionRouteClearance" => {
            Some(vec![read_position(b)?, read_route_clearance(b)?])
        }
        // --- ACARS-3.1: additional argument shapes ---
        "DistanceOffsetDirection" => {
            Some(vec![read_distance_offset(b)?, read_direction(b)?])
        }
        "PositionDistanceOffsetDirection" => Some(vec![
            read_position(b)?,
            read_distance_offset(b)?,
            read_direction(b)?,
        ]),
        "TimeDistanceOffsetDirection" => Some(vec![
            read_time(b)?,
            read_distance_offset(b)?,
            read_direction(b)?,
        ]),
        "Frequency" => Some(vec![read_frequency(b)?]),
        "BeaconCode" => Some(vec![read_beacon_code(b)?]),
        "ProcedureName" => Some(vec![read_procedure_name(b)?]),
        "PositionProcedureName" => {
            Some(vec![read_position(b)?, read_procedure_name(b)?])
        }
        "Altimeter" => Some(vec![read_altimeter(b)?]),
        "ATISCode" => Some(vec![read_atis_code(b)?]),
        "RemainingFuelRemainingSouls" => {
            Some(vec![read_remaining_fuel(b)?, read_remaining_souls(b)?])
        }
        "ErrorInformation" => Some(vec![read_error_information(b)?]),
        "VersionNumber" => Some(vec![read_version_number(b)?]),
        "ICAOfacilitydesignation" => {
            Some(vec![read_icao_facility_designation(b)?])
        }
        "ICAOfacilitydesignationTp4table" => Some(vec![
            read_icao_facility_designation(b)?,
            read_tp4table(b)?,
        ]),
        "ToFromPosition" => Some(vec![read_tofrom(b)?, read_position(b)?]),
        "TimeDistanceToFromPosition" => Some(vec![
            read_time(b)?,
            read_distance(b)?,
            read_tofrom(b)?,
            read_position(b)?,
        ]),
        "ICAOunitnameFrequency" => {
            let (unit, freq) = read_icao_unit_name_frequency(b)?;
            Some(vec![unit, freq])
        }
        "PositionICAOunitnameFrequency" => {
            let pos = read_position(b)?;
            let (unit, freq) = read_icao_unit_name_frequency(b)?;
            Some(vec![pos, unit, freq])
        }
        "TimeICAOunitnameFrequency" => {
            let time = read_time(b)?;
            let (unit, freq) = read_icao_unit_name_frequency(b)?;
            Some(vec![time, unit, freq])
        }
        _ => None,
    }
}

/// Substitute decoded arguments into the bracketed template slots.
fn render(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for a in args {
        let Some(start) = out.find('[') else { break };
        let Some(end) = out[start..].find(']') else { break };
        out.replace_range(start..start + end + 1, a);
    }
    out
}

/// Decode a FANS-1/A ATC message body (the octets after the ARINC 622
/// IMI + aircraft registration, before the CRC).
pub fn decode(body: &[u8], downlink: bool) -> Option<CpdlcMessage> {
    let mut b = Bits { data: body, pos: 0 };
    let has_more = b.read(1)? == 1;
    let has_ref = b.read(1)? == 1;
    let has_ts = b.read(1)? == 1;
    let msg_id = b.read(6)? as u8;
    let msg_ref = if has_ref { Some(b.read(6)? as u8) } else { None };
    let timestamp = if has_ts {
        let h = b.read(5)?;
        let m = b.read(6)?;
        let s = b.read(6)?;
        if h > 23 || m > 59 || s > 59 {
            return None;
        }
        Some(format!("{h:02}:{m:02}:{s:02}"))
    } else {
        None
    };
    let table: &[(&str, &str)] =
        if downlink { &DOWNLINK_ELEMENTS } else { &UPLINK_ELEMENTS };
    let idx = b.read(8)? as usize;
    let (tag, label) = table.get(idx).copied()?;
    let first_args = read_args(tag, &mut b);
    let args = first_args.clone().unwrap_or_default();
    let text = if args.is_empty() { label.to_string() } else { render(label, &args) };

    // Additional elements (SEQUENCE SIZE(1..4), 2-bit count) are only
    // reachable while every preceding element's argument shape decodes
    // (UPER has no per-element length prefix).
    let mut additional = Vec::new();
    if has_more && first_args.is_some() {
        if let Some(count) = b.read(2).map(|v| v + 1) {
            for _ in 0..count {
                let Some(idx) = b.read(8).map(|v| v as usize) else { break };
                let Some((tag, label)) = table.get(idx).copied() else { break };
                let elem_args = read_args(tag, &mut b);
                let a = elem_args.clone().unwrap_or_default();
                additional.push(CpdlcElement {
                    element: tag.to_string(),
                    text: if a.is_empty() { label.to_string() } else { render(label, &a) },
                    args: a,
                });
                if elem_args.is_none() {
                    break; // cannot advance past an undecoded shape
                }
            }
        }
    }
    Some(CpdlcMessage {
        msg_id,
        msg_ref,
        timestamp,
        element: tag.to_string(),
        text,
        args,
        additional,
        more_elements: has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a UPER body for testing (mirrors the decoder's layout).
    fn build(
        msg_id: u8,
        msg_ref: Option<u8>,
        ts: Option<(u32, u32, u32)>,
        elem: u32,
        more: bool,
    ) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        let mut push = |v: u32, n: usize| {
            for k in (0..n).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        };
        push(more as u32, 1);
        push(msg_ref.is_some() as u32, 1);
        push(ts.is_some() as u32, 1);
        push(msg_id as u32, 6);
        if let Some(r) = msg_ref {
            push(r as u32, 6);
        }
        if let Some((h, m, s)) = ts {
            push(h, 5);
            push(m, 6);
            push(s, 6);
        }
        push(elem, 8);
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, &v) in bits.iter().enumerate() {
            out[i / 8] |= v << (7 - i % 8);
        }
        out
    }

    #[test]
    fn wilco_downlink() {
        let body = build(12, Some(5), Some((14, 32, 7)), 0, false);
        let m = decode(&body, true).unwrap();
        assert_eq!(m.msg_id, 12);
        assert_eq!(m.msg_ref, Some(5));
        assert_eq!(m.timestamp.as_deref(), Some("14:32:07"));
        assert_eq!(m.element, "dM0NULL");
        assert_eq!(m.text, "WILCO");
        assert!(!m.more_elements);
    }

    #[test]
    fn uplink_unable_and_altitude_request() {
        let m = decode(&build(3, None, None, 0, false), false).unwrap();
        assert_eq!(m.element, "uM0NULL");
        assert_eq!(m.text, "UNABLE");
        let m = decode(&build(7, None, None, 6, true), true).unwrap();
        assert_eq!(m.element, "dM6Altitude");
        assert_eq!(m.text, "REQUEST [altitude]");
        assert!(m.more_elements);
    }

    #[test]
    fn altitude_arguments_render() {
        // dM9Altitude = "REQUEST CLIMB TO [altitude]"; arg = flight level
        // CHOICE (index 6) + FL360 (offset 330 from lower bound 30).
        let mut body = build(11, None, None, 9, false);
        // Append the altitude arg bits: 3-bit choice 6, 10-bit offset 330.
        let mut bits: Vec<u8> = Vec::new();
        for k in (0..3).rev() {
            bits.push(((6 >> k) & 1) as u8);
        }
        for k in (0..10).rev() {
            bits.push(((330u32 >> k) & 1) as u8);
        }
        // The header for this build is 3+6+8 = 17 bits; continue packing
        // from bit 17.
        let mut all = body.clone();
        all.resize(5, 0);
        for (i, &v) in bits.iter().enumerate() {
            let p = 17 + i;
            all[p / 8] |= v << (7 - p % 8);
        }
        body = all;
        let m = decode(&body, true).unwrap();
        assert_eq!(m.element, "dM9Altitude");
        assert_eq!(m.args, vec!["FL360"]);
        assert_eq!(m.text, "REQUEST CLIMB TO FL360");
    }

    #[test]
    fn undecoded_argument_keeps_template() {
        // dM21Frequency = "REQUEST VOICE CONTACT [frequency]" —
        // frequency args are not decoded; the template must survive.
        let m = decode(&build(2, None, None, 21, false), true).unwrap();
        assert_eq!(m.element, "dM21Frequency");
        assert!(m.args.is_empty());
        assert!(m.text.contains("[frequency]"), "{}", m.text);
    }

    #[test]
    fn position_arguments_render() {
        let mut bits: Vec<u8> = Vec::new();
        let mut push = |v: u32, n: usize, bits: &mut Vec<u8>| {
            for k in (0..n).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        };
        // dM22Position = "REQUEST DIRECT TO [position]", fixName "TULSA".
        push(0, 1, &mut bits); // no extra elements
        push(0, 1, &mut bits); // no msgRef
        push(0, 1, &mut bits); // no timestamp
        push(9, 6, &mut bits); // msg id
        push(22, 8, &mut bits); // dM22Position
        push(0, 3, &mut bits); // position CHOICE: fixName
        push(4, 3, &mut bits); // length 5 (offset from 1)
        for c in b"TULSA" {
            push(*c as u32, 7, &mut bits);
        }
        let mut body = vec![0u8; bits.len().div_ceil(8)];
        for (i, &v) in bits.iter().enumerate() {
            body[i / 8] |= v << (7 - i % 8);
        }
        let m = decode(&body, true).unwrap();
        assert_eq!(m.text, "REQUEST DIRECT TO TULSA");

        // Same element with a latitude/longitude: 52°18.5'N 4°46.0'E.
        let mut bits: Vec<u8> = Vec::new();
        let mut push = |v: u32, n: usize, bits: &mut Vec<u8>| {
            for k in (0..n).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        };
        push(0, 1, &mut bits);
        push(0, 1, &mut bits);
        push(0, 1, &mut bits);
        push(10, 6, &mut bits);
        push(22, 8, &mut bits);
        push(3, 3, &mut bits); // latitudeLongitude
        push(1, 1, &mut bits); // lat minutes present
        push(52, 7, &mut bits);
        push(185, 10, &mut bits);
        push(0, 1, &mut bits); // N
        push(1, 1, &mut bits); // lon minutes present
        push(4, 8, &mut bits);
        push(460, 10, &mut bits);
        push(0, 1, &mut bits); // E
        let mut body = vec![0u8; bits.len().div_ceil(8)];
        for (i, &v) in bits.iter().enumerate() {
            body[i / 8] |= v << (7 - i % 8);
        }
        let m = decode(&body, true).unwrap();
        assert_eq!(m.text, "REQUEST DIRECT TO 52°18.5'N 4°46.0'E");
    }

    #[test]
    fn speed_and_multi_element() {
        // dM18Speed = "REQUEST [speed]" with Mach 0.84 (choice 6,
        // offset 84-61=23), followed by one additional element:
        // dM0NULL (WILCO). Header: more=1.
        let mut bits: Vec<u8> = Vec::new();
        let mut push = |v: u32, n: usize, bits: &mut Vec<u8>| {
            for k in (0..n).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        };
        push(1, 1, &mut bits); // has additional elements
        push(0, 1, &mut bits); // no msgRef
        push(0, 1, &mut bits); // no timestamp
        push(22, 6, &mut bits); // msg id
        push(18, 8, &mut bits); // dM18Speed
        push(6, 3, &mut bits); // speed CHOICE: mach
        push(23, 5, &mut bits); // M0.84
        push(0, 2, &mut bits); // seq count - 1 = 0 -> one element
        push(0, 8, &mut bits); // dM0NULL
        let mut body = vec![0u8; bits.len().div_ceil(8)];
        for (i, &v) in bits.iter().enumerate() {
            body[i / 8] |= v << (7 - i % 8);
        }
        let m = decode(&body, true).unwrap();
        assert_eq!(m.element, "dM18Speed");
        assert_eq!(m.text, "REQUEST M0.84");
        assert_eq!(m.additional.len(), 1);
        assert_eq!(m.additional[0].element, "dM0NULL");
        assert_eq!(m.additional[0].text, "WILCO");
    }

    #[test]
    fn rejects_out_of_range() {
        // Element index beyond the downlink table.
        assert!(decode(&build(1, None, None, 200, false), true).is_none());
        assert!(decode(&[], true).is_none());
    }
}

#[cfg(test)]
mod composite_tests {
    use super::*;

    struct Builder(Vec<u8>, usize);
    impl Builder {
        fn new() -> Self {
            Builder(vec![0u8; 64], 0)
        }
        fn push(&mut self, v: u32, n: usize) {
            for k in (0..n).rev() {
                let bit = ((v >> k) & 1) as u8;
                self.0[self.1 / 8] |= bit << (7 - self.1 % 8);
                self.1 += 1;
            }
        }
    }

    /// dM11PositionAltitude: "AT [position] REQUEST CLIMB TO [altitude]".
    #[test]
    fn position_altitude_composite_renders() {
        let mut b = Builder::new();
        b.push(0, 1); // no more elements
        b.push(0, 1); // no msg ref
        b.push(0, 1); // no timestamp
        b.push(7, 6); // msg id
        b.push(11, 8); // dM11
        // FANSPosition CHOICE: fixname (index 0 of 5 → 3 bits), IA5 1..5.
        b.push(0, 3);
        b.push(3, 3); // length 4 (offset from 1)
        for c in b"OAKEY" [..4].iter() {
            b.push(*c as u32, 7);
        }
        // FANSAltitude CHOICE index for flight level (per existing
        // read_altitude): use the same encoding the roundtrip tests use —
        // altitudeFlightLevel is choice 4 of 8 (3 bits) value (30..600).
        b.push(4, 3);
        b.push(360 - 30, 10);
        let m = decode(&b.0, true).expect("decode");
        assert_eq!(m.element, "dM11PositionAltitude");
        assert!(m.text.contains("AT OAKE"), "{}", m.text);
        assert!(m.text.contains("CLIMB TO"), "{}", m.text);
    }

    /// FreeText downlink renders verbatim.
    #[test]
    fn freetext_renders() {
        let mut b = Builder::new();
        b.push(0, 1);
        b.push(0, 1);
        b.push(0, 1);
        b.push(9, 6);
        b.push(67, 8); // dM67FreeText
        let msg = b"DUE WX";
        b.push((msg.len() - 1) as u32, 8);
        for c in msg {
            b.push(*c as u32, 7);
        }
        let m = decode(&b.0, true).expect("decode");
        assert_eq!(m.element, "dM67FreeText");
        assert_eq!(m.text, "DUE WX");
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;

    struct Builder(Vec<u8>, usize);
    impl Builder {
        fn new() -> Self {
            Builder(vec![0u8; 96], 0)
        }
        fn push(&mut self, v: u32, n: usize) {
            for k in (0..n).rev() {
                let bit = ((v >> k) & 1) as u8;
                self.0[self.1 / 8] |= bit << (7 - self.1 % 8);
                self.1 += 1;
            }
        }
        fn ia5(&mut self, s: &str) {
            for c in s.bytes() {
                self.push(c as u32, 7);
            }
        }
    }

    /// dM40RouteClearance "ASSIGNED ROUTE [routeclearance]" with a
    /// destination airport and a two-leg route.
    #[test]
    fn assigned_route_decodes() {
        let mut b = Builder::new();
        b.push(0, 1); // single element
        b.push(0, 1); // no msg ref
        b.push(0, 1); // no timestamp
        b.push(22, 6); // msg id
        b.push(40, 8); // dM40RouteClearance
        // presence map: destination airport (bit 1) + route list (bit 8)
        b.push(0b0100000010, 10);
        b.ia5("KSFO"); // airport SIZE(4): no length bits
        b.push(1, 7); // 2 legs (SIZE 1..128)
        b.push(4, 3); // airway identifier
        b.push(3, 3); // SIZE(1..5) length 4
        b.ia5("J501");
        b.push(0, 3); // published identifier
        b.push(0, 1); // no latlon
        b.push(2, 3); // fixname length 3
        b.ia5("OAK");
        let m = decode(&b.0, true).expect("decode");
        assert_eq!(m.element, "dM40RouteClearance");
        assert!(m.text.contains("DEST KSFO"), "{}", m.text);
        assert!(m.text.contains("ROUTE J501 OAK"), "{}", m.text);
    }
}


#[cfg(test)]
mod arg_reader_tests {
    //! ACARS-3.1 argument-reader vectors. Each body is a spec-derived
    //! FANS-1/A UPER message whose bit layout follows the libacars asn1c
    //! PER constraints (the external oracle); the EXPECTED decode of every
    //! one was independently confirmed by running the same body, wrapped in
    //! a valid ARINC-622 envelope, through the libacars reference decoder
    //! (`decode_acars_apps`) — these are NOT encode/decode loopbacks. The
    //! reference output is quoted next to each case.
    use super::*;

    fn dec(hex: &str, downlink: bool) -> CpdlcMessage {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        decode(&bytes, downlink).expect("decodes")
    }

    #[test]
    fn distance_offset_direction() {
        // libacars: "REQUEST OFFSET ... Offset: 20 nm / Direction: right".
        let m = dec("01878988", true);
        assert_eq!(m.element, "dM15DistanceOffsetDirection");
        assert_eq!(m.args, vec!["20 nm", "RIGHT"]);
        assert_eq!(m.text, "REQUEST OFFSET 20 nm RIGHT OF ROUTE");
    }

    #[test]
    fn beacon_code() {
        // libacars: "SQUAWKING ... Code: 2401".
        let m = dec("0197a808", true);
        assert_eq!(m.element, "dM47BeaconCode");
        assert_eq!(m.args, vec!["2401"]);
    }

    #[test]
    fn frequency_vhf() {
        // libacars: "REQUEST VOICE CONTACT ... VHF: 132.025 MHz".
        let m = dec("018aaeac40", true);
        assert_eq!(m.element, "dM21Frequency");
        assert_eq!(m.args, vec!["132.025 MHz"]);
    }

    #[test]
    fn frequency_hf() {
        // libacars renders HF in kHz: "8891 kHz" (CHECK STUCK MICROPHONE).
        let m = dec("01ce85e640", false);
        assert_eq!(m.element, "uM157Frequency");
        assert_eq!(m.args, vec!["8891 kHz"]);
    }

    #[test]
    fn procedure_name() {
        // libacars: "REQUEST ... Procedure type: arrival / name: ROBN1".
        let m = dec("018b894a7c29cc40", true);
        assert_eq!(m.element, "dM23ProcedureName");
        assert_eq!(m.args, vec!["ROBN1 (ARRIVAL)"]);
    }

    #[test]
    fn altimeter_english() {
        // libacars: "ALTIMETER ... Altimeter: 29.92 inHg".
        let m = dec("01ccb180", false);
        assert_eq!(m.element, "uM153Altimeter");
        assert_eq!(m.args, vec!["29.92 inHg"]);
    }

    #[test]
    fn atis_code() {
        // libacars: "ATIS ... ATIS code: B".
        let m = dec("01cf42", false);
        assert_eq!(m.element, "uM158ATISCode");
        assert_eq!(m.args, vec!["B"]);
    }

    #[test]
    fn remaining_fuel_and_souls() {
        // libacars: "Remaining fuel: 03:45 / Persons on board: 250".
        let m = dec("019c8ed3e4", true);
        assert_eq!(m.element, "dM57RemainingFuelRemainingSouls");
        assert_eq!(m.args, vec!["03:45", "250"]);
        assert_eq!(m.text, "03:45 OF FUEL REMAINING AND 250 SOULS ON BOARD");
    }

    #[test]
    fn error_information() {
        // libacars: "ERROR ... Error information: invalidData" (enum 10).
        let m = dec("019f28", true);
        assert_eq!(m.element, "dM62ErrorInformation");
        assert_eq!(m.args, vec!["invalid data"]);
    }

    #[test]
    fn version_number() {
        let m = dec("01a488", true);
        assert_eq!(m.element, "dM73VersionNumber");
        assert_eq!(m.args, vec!["1"]);
    }

    #[test]
    fn icao_facility_designation() {
        // libacars: "[icaofacilitydesignation]" -> KZAK.
        let m = dec("01a04bb50658", true);
        assert_eq!(m.element, "dM64ICAOfacilitydesignation");
        assert_eq!(m.args, vec!["KZAK"]);
    }

    #[test]
    fn icao_facility_designation_tp4table() {
        // libacars: "KZAK" + "TP4 table: labelB".
        let m = dec("01d1cbb5065c", false);
        assert_eq!(m.element, "uM163ICAOfacilitydesignationTp4table");
        assert_eq!(m.args, vec!["KZAK", "label B"]);
    }

    #[test]
    fn tofrom_position() {
        // libacars: "REPORT DISTANCE ... To/From: from / Fix: ABC".
        let m = dec("01dac2830a18", false);
        assert_eq!(m.element, "uM181ToFromPosition");
        assert_eq!(m.args, vec!["FROM", "ABC"]);
    }

    #[test]
    fn time_distance_tofrom_position() {
        // libacars: "AT ... Time 14:30 / Distance 12.5 nm / to / Fix XYZ".
        let m = dec("01a739e00fa0ac59b4", true);
        assert_eq!(m.element, "dM78TimeDistanceToFromPosition");
        assert_eq!(m.args, vec!["14:30", "12.5 nm", "TO", "XYZ"]);
    }

    #[test]
    fn icao_unitname_frequency() {
        // libacars: "CONTACT ... designation KZAK / function center /
        // VHF 121.500 MHz".
        let m = dec("01baa5da832c246500", false);
        assert_eq!(m.element, "uM117ICAOunitnameFrequency");
        assert_eq!(m.args, vec!["KZAK center", "121.500 MHz"]);
    }

    #[test]
    fn position_place_bearing_distance() {
        // libacars: "REQUEST DIRECT TO ... Fix FOO / Degrees (magnetic)
        // 270 deg / Distance 50.0 nm".
        let m = dec("018b428d3e7a1a07d0", true);
        assert_eq!(m.element, "dM22Position");
        assert_eq!(m.args, vec!["FOO BRG 270°M DIST 50.0 nm"]);
    }
}
