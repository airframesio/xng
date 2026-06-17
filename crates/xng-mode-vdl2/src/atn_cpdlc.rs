//! ATN Baseline 1 CPDLC (protected mode) and CM logon decoding.
//!
//! Message structures are from the ICAO Doc 9880/9705 ASN.1 modules
//! (vendored as spec text in docs/asn1/, obtained via Wireshark's
//! transcription of the ICAO standard — module text only; no Wireshark
//! code consulted). Encoding is unaligned PER (ITU-T X.691) as profiled
//! by the ATN upper layers.
//!
//! v1 scope: ProtectedAircraftPDUs / ProtectedGroundPDUs walk, the
//! ATCUplink/DownlinkMessage header (msg id/ref, date-time, logical
//! ack), and element identification — the full 238-uplink/114-downlink
//! element tables with the standard phraseology, generated from the
//! module. Elements with arguments report the argument type and the
//! phrase; argument value rendering follows (the FANS-1/A path).

use serde_json::{Value, json};

include!("atn_cpdlc_tables.rs");

/// Unaligned-PER bit reader.
struct Per<'a> {
    bits: &'a [u8],
    pos: usize,
}

impl<'a> Per<'a> {
    fn new(bytes: &'a [u8], store: &'a mut Vec<u8>) -> Per<'a> {
        store.clear();
        store.extend(
            bytes.iter().flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1)),
        );
        Per { bits: store, pos: 0 }
    }

    fn bit(&mut self) -> Option<u8> {
        let b = *self.bits.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn uint(&mut self, n: usize) -> Option<u64> {
        if self.pos + n > self.bits.len() {
            return None;
        }
        let v = self.bits[self.pos..self.pos + n]
            .iter()
            .fold(0u64, |v, &b| (v << 1) | b as u64);
        self.pos += n;
        Some(v)
    }

    /// Constrained whole number (X.691 §10.5): bit-field of the minimal
    /// width for the range.
    fn constrained(&mut self, lo: i64, hi: i64) -> Option<i64> {
        let range = (hi - lo + 1) as u64;
        if range == 1 {
            return Some(lo);
        }
        let bits = 64 - (range - 1).leading_zeros() as usize;
        Some(lo + self.uint(bits)? as i64)
    }

    /// General length determinant (X.691 §10.9, unaligned): 0xxxxxxx or
    /// 10xxxxxx xxxxxxxx (fragmentation unsupported — not seen in CPDLC).
    fn length(&mut self) -> Option<usize> {
        if self.bit()? == 0 {
            return Some(self.uint(7)? as usize);
        }
        if self.bit()? == 0 {
            return Some(self.uint(14)? as usize);
        }
        None
    }

    /// IA5String with a constrained SIZE: 7 bits per character (UPER).
    fn ia5(&mut self, min: i64, max: i64) -> Option<String> {
        let n = self.constrained(min, max)? as usize;
        let mut s = String::with_capacity(n);
        for _ in 0..n {
            s.push(self.uint(7)? as u8 as char);
        }
        Some(s)
    }

    fn remaining_bytes(&mut self, nbits: usize) -> Option<Vec<u8>> {
        if self.pos + nbits > self.bits.len() {
            return None;
        }
        let out = self.bits[self.pos..self.pos + nbits]
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |v, (i, &b)| v | (b << (7 - i))))
            .collect();
        self.pos += nbits;
        Some(out)
    }
}

/// Try to decode an ATN-B1 protected-mode CPDLC APDU (either direction).
pub fn parse_apdu(bytes: &[u8]) -> Option<Value> {
    parse_pdus(bytes, true).or_else(|| parse_pdus(bytes, false))
}

fn parse_pdus(bytes: &[u8], downlink: bool) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    // CHOICE with extension marker: 1 extension bit + root index.
    if p.bit()? != 0 {
        return None; // extension alternatives: not decoded
    }
    // ProtectedAircraftPDUs: 4 root alternatives (2 bits);
    // ProtectedGroundPDUs: 6 root alternatives (3 bits).
    let (alts, idx_bits) = if downlink { (4u64, 2) } else { (6u64, 3) };
    let idx = p.uint(idx_bits)?;
    if idx >= alts {
        return None;
    }
    let kind = match (downlink, idx) {
        (_, 0) => "abort-user",
        (_, 1) => "abort-provider",
        (true, 2) => "startdown",
        (true, 3) => "send",
        (false, 2) => "startup",
        (false, 3) => "send",
        (false, 4) => "forward",
        (false, 5) => "forward-response",
        _ => return None,
    };
    let mut out = json!({
        "application": "CPDLC",
        "version": "ATN-B1",
        "direction": if downlink { "downlink" } else { "uplink" },
        "pdu": kind,
    });
    match kind {
        "send" | "startup" => {
            out["message"] = protected_message(&mut p, downlink)?;
        }
        "startdown" => {
            // ProtectedStartDownMessage: mode DEFAULT (presence bit) then
            // the protected message.
            if p.bit()? == 1 {
                out["mode"] = json!(if p.bit()? == 1 { "dsc" } else { "cpdlc" });
            }
            out["message"] = protected_message(&mut p, downlink)?;
        }
        "abort-user" | "abort-provider" => {
            // Extensible ENUMERATED: ext bit + root index.
            if p.bit()? == 0 {
                out["reason"] = json!(p.uint(3)?);
            }
        }
        _ => {}
    }
    Some(out)
}

/// ProtectedUplink/DownlinkMessage: extensible SEQUENCE with two
/// OPTIONAL components, the second being the PER-encoded
/// ATCUplink/DownlinkMessage in a BIT STRING.
fn protected_message(p: &mut Per, downlink: bool) -> Option<Value> {
    if p.bit()? != 0 {
        return None; // extension additions present: bail
    }
    let has_algo = p.bit()? == 1;
    let has_msg = p.bit()? == 1;
    if has_algo {
        // RELATIVE-OID: length determinant + octets (skipped).
        let n = p.length()?;
        p.remaining_bytes(n * 8)?;
    }
    if !has_msg {
        return Some(json!({ "empty": true }));
    }
    let nbits = p.length()?;
    let inner = p.remaining_bytes(nbits)?;
    // The BIT STRING length is in bits; the inner message is itself
    // PER, decoded from its own bit zero.
    atc_message(&inner, nbits, downlink)
}

/// ATCUplinkMessage / ATCDownlinkMessage.
fn atc_message(bytes: &[u8], nbits: usize, downlink: bool) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    p.bits = &p.bits[..nbits.min(p.bits.len())];

    // ATCMessageHeader: optional msgRef + defaulted logicalAck preamble.
    let has_ref = p.bit()? == 1;
    let has_ack = p.bit()? == 1;
    let msg_id = p.constrained(0, 63)?;
    let msg_ref = if has_ref { Some(p.constrained(0, 63)?) } else { None };
    // DateTimeGroup: Date{year 1996..2095, month 1..12, day 1..31} +
    // Timehhmmss{hours 0..23, minutes 0..59, seconds 0..59}.
    let (y, mo, d) = (
        p.constrained(1996, 2095)?,
        p.constrained(1, 12)?,
        p.constrained(1, 31)?,
    );
    let (h, mi, sec) = (
        p.constrained(0, 23)?,
        p.constrained(0, 59)?,
        p.constrained(0, 59)?,
    );
    let ack = if has_ack {
        if p.constrained(0, 1)? == 0 { "required" } else { "not-required" }
    } else {
        "not-required"
    };

    // MessageData: SEQUENCE {elementIds SIZE(1..5), constrainedData OPT}.
    let _has_constrained = p.bit()? == 1;
    let count = p.constrained(1, 5)? as usize;
    let table: &[(&str, &str, &str)] =
        if downlink { &DOWNLINK_ELEMENTS } else { &UPLINK_ELEMENTS };
    let idx_bits = 64 - (table.len() as u64 - 1).leading_zeros() as usize;

    let mut elements = Vec::new();
    let mut bailed = false;
    for k in 0..count {
        // Element CHOICE (not extensible in the module).
        let idx = p.uint(idx_bits)? as usize;
        let (name, arg_ty, phrase) = table.get(idx).copied()?;
        let mut el = json!({ "element": name, "phrase": phrase });
        if arg_ty != "NULL" {
            el["argument_type"] = json!(arg_ty);
            match read_argument(&mut p, arg_ty) {
                Some(vals) => {
                    el["text"] = json!(fill_phrase(phrase, &vals));
                    el["arguments"] = json!(vals);
                }
                None => {
                    // Unknown argument size: later elements unreachable.
                    if k + 1 < count {
                        el["note"] =
                            json!("remaining elements undecoded (argument type unsupported)");
                    }
                    bailed = true;
                    elements.push(el);
                    break;
                }
            }
        } else {
            el["text"] = json!(phrase);
        }
        elements.push(el);
    }

    let mut out = json!({
        "msg_id": msg_id,
        "msg_ref": msg_ref,
        "timestamp": format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{sec:02}Z"),
        "logical_ack": ack,
        "elements": elements,
    });
    // constrainedData (route clearances) sits after the element list —
    // reachable only when every element argument decoded.
    if _has_constrained && !bailed {
        // SEQUENCE { routeClearanceData SIZE(1..2) OPTIONAL, ... }:
        // extension bit + presence bit.
        if p.bit() == Some(0) && p.bit() == Some(1) {
            if let Some(n) = p.constrained(1, 2) {
                let mut rcs = Vec::new();
                for _ in 0..n {
                    match read_route_clearance(&mut p) {
                        Some(rc) => rcs.push(rc),
                        None => break,
                    }
                }
                if !rcs.is_empty() {
                    out["route_clearances"] = json!(rcs);
                }
            }
        }
    }
    Some(out)
}


/// PublishedIdentifier: fixName | navaid (both name + optional latlon).
fn read_published(p: &mut Per) -> Option<String> {
    let navaid = p.bit()? == 1;
    let has_ll = p.bit()? == 1;
    let name = if navaid { p.ia5(1, 4)? } else { p.ia5(1, 5)? };
    if has_ll {
        let ll = read_latlon(p)?;
        Some(format!("{name} ({ll})"))
    } else {
        Some(name)
    }
}

fn read_distance(p: &mut Per) -> Option<String> {
    Some(if p.bit()? == 0 {
        format!("{:.1} NM", p.constrained(0, 9999)? as f64 / 10.0)
    } else {
        format!("{:.2} KM", p.constrained(0, 8000)? as f64 / 4.0)
    })
}

/// ProcedureName: type + IA5(1..20) + optional transition IA5(1..5).
fn read_procedure(p: &mut Per) -> Option<String> {
    let has_transition = p.bit()? == 1;
    let ptype = match p.uint(2)? {
        0 => "ARRIVAL",
        1 => "APPROACH",
        2 => "DEPARTURE",
        _ => return None,
    };
    let name = p.ia5(1, 20)?;
    let mut s = format!("{name} ({ptype})");
    if has_transition {
        s.push_str(&format!(" TRANSITION {}", p.ia5(1, 5)?));
    }
    Some(s)
}

/// Runway: direction (1..36) + L/R/C suffix.
fn read_runway(p: &mut Per) -> Option<String> {
    let dir = p.constrained(1, 36)?;
    let cfg = match p.uint(2)? {
        0 => "L",
        1 => "R",
        2 => "C",
        _ => "",
    };
    Some(format!("RWY {dir:02}{cfg}"))
}

/// One RouteInformation leg.
fn read_route_information(p: &mut Per) -> Option<String> {
    match p.uint(3)? {
        0 => read_published(p),
        1 => read_latlon(p),
        2 => {
            // PlaceBearingPlaceBearing: SEQUENCE SIZE(2) OF PlaceBearing.
            let a = format!("{} BRG {}", read_published(p)?, read_degrees(p)?);
            let b = format!("{} BRG {}", read_published(p)?, read_degrees(p)?);
            Some(format!("{a} / {b}"))
        }
        3 => Some(format!(
            "{} BRG {} DIST {}",
            read_published(p)?,
            read_degrees(p)?,
            read_distance(p)?
        )),
        4 => p.ia5(2, 7), // ATS route designator
        _ => None,
    }
}

/// RouteClearance: 9 optional components; the route itself is the list
/// of RouteInformation legs. The routeInformationAdditional tail (hold
/// patterns, RTA, intercepts) is reported present-but-undecoded — it is
/// last in the structure, so everything before it is safe to decode.
fn read_route_clearance(p: &mut Per) -> Option<Value> {
    let present: Vec<bool> = (0..9).map(|_| p.bit() == Some(1)).collect();
    let mut out = serde_json::Map::new();
    if present[0] {
        out.insert("departure_airport".into(), json!(p.ia5(4, 4)?));
    }
    if present[1] {
        out.insert("destination_airport".into(), json!(p.ia5(4, 4)?));
    }
    if present[2] {
        out.insert("departure_runway".into(), json!(read_runway(p)?));
    }
    if present[3] {
        out.insert("departure_procedure".into(), json!(read_procedure(p)?));
    }
    if present[4] {
        out.insert("arrival_runway".into(), json!(read_runway(p)?));
    }
    if present[5] {
        out.insert("approach_procedure".into(), json!(read_procedure(p)?));
    }
    if present[6] {
        out.insert("arrival_procedure".into(), json!(read_procedure(p)?));
    }
    if present[7] {
        let n = p.constrained(1, 128)? as usize;
        let mut legs = Vec::with_capacity(n);
        for _ in 0..n {
            legs.push(read_route_information(p)?);
        }
        out.insert("route".into(), json!(legs));
    }
    if present[8] {
        out.insert("additional".into(), json!("present (undecoded)"));
    }
    Some(Value::Object(out))
}


/// Render a decoded argument into the phrase template's placeholder.
fn fill_phrase(phrase: &str, vals: &[String]) -> String {
    let mut out = String::new();
    let mut vi = 0;
    let mut rest = phrase;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) => {
                if let Some(v) = vals.get(vi) {
                    out.push_str(v);
                } else {
                    out.push_str(&rest[i..i + j + 1]);
                }
                vi += 1;
                rest = &rest[i + j + 1..];
            }
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode an element argument by its ASN.1 type name. Returns the
/// human-readable value strings (one per phrase placeholder) — `None`
/// when the type is not yet supported (decoding must stop there, since
/// the argument's size is then unknown).
fn read_argument(p: &mut Per, ty: &str) -> Option<Vec<String>> {
    Some(match ty {
        "Level" => vec![read_level(p)?],
        "LevelLevel" => vec![read_level(p)?, read_level(p)?],
        "Time" => vec![read_time(p)?],
        "TimeTime" => vec![read_time(p)?, read_time(p)?],
        "Position" => vec![read_position(p)?],
        "PositionPosition" => vec![read_position(p)?, read_position(p)?],
        "Speed" => vec![read_speed(p)?],
        "SpeedSpeed" => vec![read_speed(p)?, read_speed(p)?],
        "Degrees" => vec![read_degrees(p)?],
        "Airport" => vec![p.ia5(4, 4)?],
        "LevelPosition" => vec![read_level(p)?, read_position(p)?],
        "LevelTime" => vec![read_level(p)?, read_time(p)?],
        "LevelSpeed" => vec![read_level(p)?, read_speed(p)?, read_speed(p)?],
        "PositionLevel" => vec![read_position(p)?, read_level(p)?],
        "PositionTime" => vec![read_position(p)?, read_time(p)?],
        "PositionSpeed" => vec![read_position(p)?, read_speed(p)?],
        "PositionDegrees" => vec![read_position(p)?, read_degrees(p)?],
        "TimeLevel" => vec![read_time(p)?, read_level(p)?],
        "TimePosition" => vec![read_time(p)?, read_position(p)?],
        "DirectionDegrees" => vec![read_direction(p)?, read_degrees(p)?],
        // The clearance itself rides in constrainedData; elements carry
        // an index into it.
        "RouteClearanceIndex" => {
            vec![format!("(route clearance #{})", p.constrained(1, 2)?)]
        }
        "PositionRouteClearanceIndex" => vec![
            read_position(p)?,
            format!("(route clearance #{})", p.constrained(1, 2)?),
        ],

        // --- VDL2-1.1: extended argument-type coverage (ICAO Doc 9880 module,
        // docs/asn1/atn-cpdlc.asn; unaligned PER per ITU-T X.691). ---

        // Frequency CHOICE (4 alts) and its two-component compounds.
        "Frequency" => vec![read_frequency(p)?],
        "UnitNameFrequency" => vec![read_unit_name(p)?, read_frequency(p)?],
        "PositionUnitNameFrequency" => {
            vec![read_position(p)?, read_unit_name(p)?, read_frequency(p)?]
        }
        "TimeUnitNameFrequency" => {
            vec![read_time(p)?, read_unit_name(p)?, read_frequency(p)?]
        }

        // Altimeter CHOICE (english/metric) and its compounds.
        "Altimeter" => vec![read_altimeter(p)?],
        "FacilityDesignation" => vec![read_facility_designation(p)?],
        "Facility" => vec![read_facility(p)?],
        "FacilityDesignationAltimeter" => {
            vec![read_facility_designation(p)?, read_altimeter(p)?]
        }
        "FacilityDesignationATISCode" => {
            vec![read_facility_designation(p)?, read_atis_code(p)?]
        }

        // Codes, text, ATIS, simple enums.
        "ATISCode" => vec![read_atis_code(p)?],
        "Code" => vec![read_code(p)?],
        "FreeText" => vec![read_free_text(p)?],
        "VersionNumber" => vec![format!("v{}", p.constrained(0, 15)?)],
        "TrafficType" => vec![read_traffic_type(p)?],
        "ClearanceType" => vec![read_clearance_type(p)?],
        "ErrorInformation" => vec![read_error_information(p)?],

        // Procedure / runway compounds.
        "ProcedureName" => vec![read_procedure(p)?],
        "PositionProcedureName" => vec![read_position(p)?, read_procedure(p)?],
        "RunwayRVR" => vec![read_runway(p)?, read_rvr(p)?],

        // Speed-type triples and the level/speed/position compounds whose
        // shapes were previously unknown.
        "SpeedTypeSpeedTypeSpeedType" => read_speed_type_triple(p)?,
        "SpeedTypeSpeedTypeSpeedTypeSpeed" => {
            let mut v = read_speed_type_triple(p)?;
            v.push(read_speed(p)?);
            v
        }
        // SpeedSpeed is a SEQUENCE SIZE(2) OF Speed.
        "LevelSpeedSpeed" => vec![read_level(p)?, read_speed(p)?, read_speed(p)?],
        "PositionSpeedSpeed" => {
            vec![read_position(p)?, read_speed(p)?, read_speed(p)?]
        }
        "TimeSpeed" => vec![read_time(p)?, read_speed(p)?],
        "SpeedTime" => vec![read_speed(p)?, read_time(p)?],
        "TimeSpeedSpeed" => vec![read_time(p)?, read_speed(p)?, read_speed(p)?],
        "PositionLevelLevel" => {
            vec![read_position(p)?, read_level(p)?, read_level(p)?]
        }
        "PositionLevelSpeed" => {
            // PositionLevelSpeed { positionlevel PositionLevel, speed Speed }.
            vec![read_position(p)?, read_level(p)?, read_speed(p)?]
        }
        "PositionTimeTime" => {
            // PositionTimeTime { position, times TimeTime } (2 times).
            vec![read_position(p)?, read_time(p)?, read_time(p)?]
        }
        "PositionTimeLevel" => {
            // PositionTimeLevel { positionTime PositionTime, level }.
            vec![read_position(p)?, read_time(p)?, read_level(p)?]
        }
        "TimePositionLevel" => {
            // TimePositionLevel { timeposition TimePosition, level }.
            vec![read_time(p)?, read_position(p)?, read_level(p)?]
        }
        "TimePositionLevelSpeed" => {
            // { timeposition TimePosition, levelspeed LevelSpeed }; LevelSpeed
            // carries a SpeedSpeed (two speeds).
            vec![
                read_time(p)?,
                read_position(p)?,
                read_level(p)?,
                read_speed(p)?,
                read_speed(p)?,
            ]
        }

        // Distance/direction offset family.
        "DistanceSpecifiedDirection" => {
            let (d, dir) = read_distance_specified_direction(p)?;
            vec![d, dir]
        }
        "PositionDistanceSpecifiedDirection" => {
            let pos = read_position(p)?;
            let (d, dir) = read_distance_specified_direction(p)?;
            vec![pos, d, dir]
        }
        "TimeDistanceSpecifiedDirection" => {
            let t = read_time(p)?;
            let (d, dir) = read_distance_specified_direction(p)?;
            vec![t, d, dir]
        }
        "DistanceSpecifiedDirectionTime" => {
            let (d, dir) = read_distance_specified_direction(p)?;
            vec![d, dir, read_time(p)?]
        }

        // To/From distance reports.
        "ToFromPosition" => vec![read_tofrom(p)?, read_position(p)?],
        "TimeToFromPosition" => {
            vec![read_time(p)?, read_tofrom(p)?, read_position(p)?]
        }
        "TimeDistanceToFromPosition" => vec![
            read_time(p)?,
            read_distance(p)?,
            read_tofrom(p)?,
            read_position(p)?,
        ],

        // Vertical rate and remaining-fuel/POB.
        "VerticalRate" => vec![read_vertical_rate(p)?],
        "RemainingFuelPersonsOnBoard" => {
            // RemainingFuel ::= Time; PersonsOnBoard ::= INTEGER(1..1024).
            vec![read_time(p)?, format!("{}", p.constrained(1, 1024)?)]
        }

        // HoldClearance: one OPTIONAL (legType) → one presence bit.
        "HoldClearance" => read_hold_clearance(p)?,
        // DepartureClearance: two OPTIONALs (flightInformation,
        // furtherInstructions); the mandatory head decodes cleanly, the
        // optional tail is reported present-but-undecoded (it is last).
        "DepartureClearance" => read_departure_clearance(p)?,
        // PositionReport: 3 mandatory + 19 OPTIONAL fields. Decode the
        // mandatory head; succeed only when every optional is absent (else
        // the field sizes downstream are unknown — stop the element walk).
        "PositionReport" => read_position_report(p)?,

        _ => return None,
    })
}

fn read_level(p: &mut Per) -> Option<String> {
    // Level ::= CHOICE {singleLevel LevelType, blockLevel SEQ SIZE(2)}.
    if p.bit()? == 0 {
        read_level_type(p)
    } else {
        Some(format!("{} TO {}", read_level_type(p)?, read_level_type(p)?))
    }
}

fn read_level_type(p: &mut Per) -> Option<String> {
    Some(match p.uint(2)? {
        0 => format!("{} FT", p.constrained(-60, 7000)? * 10),
        1 => format!("{} M", p.constrained(-30, 25_000)?),
        2 => format!("FL{}", p.constrained(30, 700)?),
        _ => format!("{} M", p.constrained(100, 2500)? * 10),
    })
}

fn read_time(p: &mut Per) -> Option<String> {
    Some(format!("{:02}:{:02}", p.constrained(0, 23)?, p.constrained(0, 59)?))
}

fn read_speed(p: &mut Per) -> Option<String> {
    Some(match p.uint(3)? {
        0 => format!("{} KT IAS", p.constrained(0, 400)?),
        1 => format!("{} KM/H IAS", p.constrained(0, 800)?),
        2 => format!("{} KT TAS", p.constrained(0, 2000)?),
        3 => format!("{} KM/H TAS", p.constrained(0, 4000)?),
        4 => format!("{} KT GS", p.constrained(-50, 2000)?),
        5 => format!("{} KM/H GS", p.constrained(-100, 4000)?),
        6 => format!("M{:.3}", p.constrained(500, 4000)? as f64 / 1000.0),
        _ => return None,
    })
}

fn read_degrees(p: &mut Per) -> Option<String> {
    let mag = p.bit()? == 0;
    Some(format!(
        "{}°{}",
        p.constrained(1, 360)?,
        if mag { "M" } else { "T" }
    ))
}

fn read_direction(p: &mut Per) -> Option<String> {
    const DIRS: [&str; 11] = [
        "LEFT", "RIGHT", "EITHER SIDE", "NORTH", "SOUTH", "EAST", "WEST",
        "NORTH-EAST", "NORTH-WEST", "SOUTH-EAST", "SOUTH-WEST",
    ];
    DIRS.get(p.constrained(0, 10)? as usize).map(|s| s.to_string())
}

fn read_position(p: &mut Per) -> Option<String> {
    match p.uint(3)? {
        0 => {
            // FixName: Fix IA5(1..5) + optional latlon.
            let has_ll = p.bit()? == 1;
            let name = p.ia5(1, 5)?;
            if has_ll {
                let ll = read_latlon(p)?;
                Some(format!("{name} ({ll})"))
            } else {
                Some(name)
            }
        }
        1 => {
            let has_ll = p.bit()? == 1;
            let name = p.ia5(1, 4)?;
            if has_ll {
                let ll = read_latlon(p)?;
                Some(format!("{name} ({ll})"))
            } else {
                Some(name)
            }
        }
        2 => p.ia5(4, 4),
        3 => read_latlon(p),
        _ => None, // placeBearingDistance: not yet
    }
}

fn read_latlon(p: &mut Per) -> Option<String> {
    // LatitudeLongitude: both components OPTIONAL.
    let has_lat = p.bit()? == 1;
    let has_lon = p.bit()? == 1;
    let mut parts = Vec::new();
    if has_lat {
        let v = read_lat_or_lon(p, 90_000, 89)?;
        let dir = if p.bit()? == 0 { "N" } else { "S" };
        parts.push(format!("{v}{dir}"));
    }
    if has_lon {
        let v = read_lat_or_lon(p, 180_000, 179)?;
        let dir = if p.bit()? == 0 { "E" } else { "W" };
        parts.push(format!("{v}{dir}"));
    }
    Some(parts.join(" "))
}

fn read_lat_or_lon(p: &mut Per, max_milli: i64, max_whole: i64) -> Option<String> {
    Some(match p.uint(2)? {
        0 => format!("{:.3}°", p.constrained(0, max_milli)? as f64 / 1000.0),
        1 => {
            let d = p.constrained(0, max_whole)?;
            let m = p.constrained(0, 5999)? as f64 / 100.0;
            format!("{d}°{m:.2}'")
        }
        2 => {
            let d = p.constrained(0, max_whole)?;
            let m = p.constrained(0, 59)?;
            let s = p.constrained(0, 59)?;
            format!("{d}°{m}'{s}\"")
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// VDL2-1.1: argument-type readers added for the extended CPDLC coverage.
// Every encoding is taken directly from the ICAO Doc 9880 ASN.1 module
// (docs/asn1/atn-cpdlc.asn) and unaligned PER (ITU-T X.691): CHOICE indices
// are the minimal bit-field over the (non-extensible) root alternatives,
// constrained INTEGERs are minimal-width offsets, ENUMERATEDs with a "..."
// extension marker carry a leading extension bit then the root index.
// ---------------------------------------------------------------------------

/// Frequency ::= CHOICE { hf [0] INTEGER(2850..28000) kHz,
/// vhf [1] INTEGER(23600..27398) (118.000..136.990 MHz, step 0.005),
/// uhf [2] INTEGER(9000..15999) (225.000..399.975 MHz, step 0.025),
/// satchannel [3] NumericString(SIZE(12)) }. Non-extensible CHOICE → 2 bits.
fn read_frequency(p: &mut Per) -> Option<String> {
    Some(match p.uint(2)? {
        0 => format!("{} kHz", p.constrained(2850, 28000)?),
        1 => format!("{:.3} MHz", p.constrained(23600, 27398)? as f64 * 0.005),
        2 => format!("{:.3} MHz", p.constrained(9000, 15999)? as f64 * 0.025),
        // NumericString(SIZE(12)): fixed size, 4 bits/char (UPER permitted
        // alphabet "0123456789" has 10 symbols → 4 bits each, X.691 §27.5.7).
        _ => {
            let mut s = String::with_capacity(12);
            for _ in 0..12 {
                s.push((b'0' + p.uint(4)? as u8) as char);
            }
            format!("SAT {s}")
        }
    })
}

/// Altimeter ::= CHOICE { english [0] INTEGER(2200..3200) (in·0.01),
/// metric [1] INTEGER(7500..12500) (hPa·0.1) }. 2-alt CHOICE → 1 bit.
fn read_altimeter(p: &mut Per) -> Option<String> {
    Some(if p.bit()? == 0 {
        format!("{:.2} inHg", p.constrained(2200, 3200)? as f64 / 100.0)
    } else {
        format!("{:.1} hPa", p.constrained(7500, 12500)? as f64 / 10.0)
    })
}

/// ATISCode ::= IA5String(SIZE(1)) — fixed size, 7 bits, no length field.
fn read_atis_code(p: &mut Per) -> Option<String> {
    Some((p.uint(7)? as u8 as char).to_string())
}

/// FacilityDesignation ::= IA5String(SIZE(4..8)).
fn read_facility_designation(p: &mut Per) -> Option<String> {
    p.ia5(4, 8)
}

/// Facility ::= CHOICE { noFacility [0] NULL, facilityDesignation [1] }.
fn read_facility(p: &mut Per) -> Option<String> {
    if p.bit()? == 0 {
        Some("(none)".to_string())
    } else {
        read_facility_designation(p)
    }
}

/// Code ::= SEQUENCE SIZE(4) OF CodeOctalDigit (INTEGER 0..7) → a transponder
/// squawk: four 3-bit octal digits, no length field (fixed SIZE).
fn read_code(p: &mut Per) -> Option<String> {
    let mut s = String::with_capacity(4);
    for _ in 0..4 {
        s.push((b'0' + p.constrained(0, 7)? as u8) as char);
    }
    Some(s)
}

/// FreeText ::= IA5String(SIZE(1..256)).
fn read_free_text(p: &mut Per) -> Option<String> {
    p.ia5(1, 256)
}

/// TrafficType ::= ENUMERATED { noneSpecified..diverging, ... } (extensible).
fn read_traffic_type(p: &mut Per) -> Option<String> {
    if p.bit()? != 0 {
        return Some("(extended)".to_string());
    }
    const TT: [&str; 6] = [
        "NONE", "OPPOSITE DIRECTION", "SAME DIRECTION", "CONVERGING",
        "CROSSING", "DIVERGING",
    ];
    TT.get(p.constrained(0, 5)? as usize).map(|s| s.to_string())
}

/// ClearanceType ::= ENUMERATED { noneSpecified..downstream, ... }
/// (12 root values, extensible).
fn read_clearance_type(p: &mut Per) -> Option<String> {
    if p.bit()? != 0 {
        return Some("(extended)".to_string());
    }
    const CT: [&str; 12] = [
        "NONE", "APPROACH", "DEPARTURE", "FURTHER", "START-UP", "PUSHBACK",
        "TAXI", "TAKE-OFF", "LANDING", "OCEANIC", "EN-ROUTE", "DOWNSTREAM",
    ];
    CT.get(p.constrained(0, 11)? as usize).map(|s| s.to_string())
}

/// ErrorInformation ::= ENUMERATED (5 root values, extensible).
fn read_error_information(p: &mut Per) -> Option<String> {
    if p.bit()? != 0 {
        return Some("(extended)".to_string());
    }
    const EI: [&str; 5] = [
        "UNRECOGNIZED MSG REFERENCE NUMBER",
        "LOGICAL ACKNOWLEDGMENT NOT ACCEPTED",
        "INSUFFICIENT RESOURCES",
        "INVALID MESSAGE ELEMENT COMBINATION",
        "INVALID MESSAGE ELEMENT",
    ];
    EI.get(p.constrained(0, 4)? as usize).map(|s| s.to_string())
}

/// ToFrom ::= ENUMERATED { to (0), from (1) } (non-extensible) → 1 bit.
fn read_tofrom(p: &mut Per) -> Option<String> {
    Some(if p.constrained(0, 1)? == 0 { "TO" } else { "FROM" }.to_string())
}

/// UnitName ::= SEQUENCE { facilityDesignation [0], facilityName [1]
/// FacilityName(SIZE(3..18)) OPTIONAL, facilityFunction [2] }.
/// FacilityFunction ::= ENUMERATED (9 root values, extensible).
fn read_unit_name(p: &mut Per) -> Option<String> {
    let has_name = p.bit()? == 1;
    let designation = read_facility_designation(p)?;
    let name = if has_name { Some(p.ia5(3, 18)?) } else { None };
    let function = read_facility_function(p)?;
    Some(match name {
        Some(n) => format!("{designation} {n} {function}"),
        None => format!("{designation} {function}"),
    })
}

fn read_facility_function(p: &mut Per) -> Option<String> {
    if p.bit()? != 0 {
        return Some("(extended)".to_string());
    }
    const FF: [&str; 9] = [
        "CENTER", "APPROACH", "TOWER", "FINAL", "GROUND", "DELIVERY",
        "DEPARTURE", "CONTROL", "RADIO",
    ];
    FF.get(p.constrained(0, 8)? as usize).map(|s| s.to_string())
}

/// RVR ::= CHOICE { feet [0] INTEGER(0..6100), meters [1] INTEGER(0..1500) }.
fn read_rvr(p: &mut Per) -> Option<String> {
    Some(if p.bit()? == 0 {
        format!("{} FT", p.constrained(0, 6100)?)
    } else {
        format!("{} M", p.constrained(0, 1500)?)
    })
}

/// VerticalRate ::= CHOICE { english [0] INTEGER(0..3000) (·10 ft/min),
/// metric [1] INTEGER(0..1000) (·10 m/min) }.
fn read_vertical_rate(p: &mut Per) -> Option<String> {
    Some(if p.bit()? == 0 {
        format!("{} FPM", p.constrained(0, 3000)? * 10)
    } else {
        format!("{} M/MIN", p.constrained(0, 1000)? * 10)
    })
}

/// SpeedType ::= ENUMERATED (9 root values, extensible).
fn read_speed_type(p: &mut Per) -> Option<String> {
    if p.bit()? != 0 {
        return Some("(extended)".to_string());
    }
    const ST: [&str; 9] = [
        "NONE", "INDICATED", "TRUE", "GROUND", "MACH", "APPROACH", "CRUISE",
        "MINIMUM", "MAXIMUM",
    ];
    ST.get(p.constrained(0, 8)? as usize).map(|s| s.to_string())
}

/// SpeedTypeSpeedTypeSpeedType ::= SEQUENCE SIZE(3) OF SpeedType.
fn read_speed_type_triple(p: &mut Per) -> Option<Vec<String>> {
    Some(vec![read_speed_type(p)?, read_speed_type(p)?, read_speed_type(p)?])
}

/// DistanceSpecified ::= CHOICE { nm [0] INTEGER(1..250), km [1]
/// INTEGER(1..500) }; DistanceSpecifiedDirection adds a Direction enum.
/// Returns (distance, direction).
fn read_distance_specified_direction(p: &mut Per) -> Option<(String, String)> {
    let dist = if p.bit()? == 0 {
        format!("{} NM", p.constrained(1, 250)?)
    } else {
        format!("{} KM", p.constrained(1, 500)?)
    };
    Some((dist, read_direction(p)?))
}

/// HoldClearance ::= SEQUENCE { position, level, degrees, direction,
/// legType OPTIONAL }. One OPTIONAL → one leading presence bit.
fn read_hold_clearance(p: &mut Per) -> Option<Vec<String>> {
    let has_leg = p.bit()? == 1;
    let position = read_position(p)?;
    let level = read_level(p)?;
    let degrees = read_degrees(p)?;
    let direction = read_direction(p)?;
    let leg = if has_leg { read_leg_type(p)? } else { "(none)".to_string() };
    Some(vec![position, level, degrees, direction, leg])
}

/// LegType ::= CHOICE { legDistance [0] LegDistance, legTime [1] LegTime }.
/// LegDistance ::= CHOICE { english [0] INTEGER(0..50) NM, metric [1]
/// INTEGER(1..128) km }; LegTime ::= INTEGER(0..10) min.
fn read_leg_type(p: &mut Per) -> Option<String> {
    Some(if p.bit()? == 0 {
        if p.bit()? == 0 {
            format!("{} NM", p.constrained(0, 50)?)
        } else {
            format!("{} KM", p.constrained(1, 128)?)
        }
    } else {
        format!("{} MIN", p.constrained(0, 10)?)
    })
}

/// DepartureClearance ::= SEQUENCE { aircraftFlightIdentification [0]
/// IA5(2..8), clearanceLimit [1] Position, flightInformation [2] OPTIONAL,
/// furtherInstructions [3] OPTIONAL }. Two OPTIONALs → two presence bits.
/// The optional tail (FlightInformation / FurtherInstructions) is deeply
/// nested and sits last; we decode the mandatory head and flag the tail.
fn read_departure_clearance(p: &mut Per) -> Option<Vec<String>> {
    let has_flight_info = p.bit()? == 1;
    let has_further = p.bit()? == 1;
    let flight_id = p.ia5(2, 8)?;
    let limit = read_position(p)?;
    let mut s = format!("{flight_id} CLEARED TO {limit}");
    if has_flight_info || has_further {
        s.push_str(" (+flight-info/further-instructions present, undecoded)");
    }
    Some(vec![s])
}

/// PositionReport ::= SEQUENCE { positioncurrent [0], timeatpositioncurrent
/// [1], level [2], then 19 OPTIONAL fields [3..21] }. Decode the 3 mandatory
/// fields; succeed only when every optional is absent (the optional field
/// sizes are otherwise unknown, which would corrupt the element walk).
fn read_position_report(p: &mut Per) -> Option<Vec<String>> {
    // 19 OPTIONAL components → 19 leading presence bits.
    let mut any_optional = false;
    for _ in 0..19 {
        if p.bit()? == 1 {
            any_optional = true;
        }
    }
    let position = read_position(p)?;
    let time = read_time(p)?;
    let level = read_level(p)?;
    let mut s = format!("{position} {time} {level}");
    if any_optional {
        // Optional fields present but their full decode is staged; the
        // element walk cannot safely continue past an unsized tail.
        s.push_str(" (+optional fields present, undecoded)");
        // Signal "stop here" to the caller: a position report with optional
        // fields is necessarily the message's terminal element in practice,
        // but to stay correct we return None so later elements aren't
        // mis-decoded from an unknown offset.
        return None;
    }
    Some(vec![s])
}

/// CM ground-generated messages: identify the dialogue type (logon
/// response, update, contact request, abort, forward).
pub fn parse_cm_ground(bytes: &[u8]) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    // CMGroundMessage CHOICE (extensible, 6 root): ext bit + 3 bits.
    if p.bit()? != 0 {
        return None;
    }
    let kind = match p.uint(3)? {
        0 => "logon-response",
        1 => "update",
        2 => "contact-request",
        3 => "forward-request",
        4 => "abort",
        5 => "forward-response",
        _ => return None,
    };
    let mut out = json!({ "application": "CM", "pdu": kind });
    if matches!(kind, "logon-response" | "update") {
        // Two OPTIONAL application lists; report presence only (the
        // per-entry TSAP addresses are variable-size — staged).
        let air = p.bit()? == 1;
        let ground = p.bit()? == 1;
        out["air_apps_present"] = json!(air);
        out["ground_apps_present"] = json!(ground);
    }
    Some(out)
}

/// CM (context management) logon request — the dialogue that precedes
/// CPDLC; identifies the flight.
pub fn parse_cm_logon(bytes: &[u8]) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    // CMAircraftMessage CHOICE (extensible, 3 root): ext bit + 2 bits.
    if p.bit()? != 0 {
        return None;
    }
    if p.uint(2)? != 0 {
        return None; // only cmLogonRequest decoded for now
    }
    // CMLogonRequest: 6 OPTIONAL components → presence bitmap.
    let present: Vec<bool> = (0..6).map(|_| p.bit() == Some(1)).collect();
    let flight_id = p.ia5(2, 8)?;
    Some(json!({
        "application": "CM",
        "pdu": "logon-request",
        "flight_id": flight_id,
        "optional_fields_present": present.iter().filter(|&&b| b).count(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cm_ground_logon_response() {
        // CMGroundMessage CHOICE: ext=0, index=0 (logon-response),
        // both OPTIONAL application lists absent.
        let v = parse_cm_ground(&[0b0_000_0_0_00]).unwrap();
        assert_eq!(v["application"], "CM");
        assert_eq!(v["pdu"], "logon-response");
        assert_eq!(v["air_apps_present"], false);
        assert_eq!(v["ground_apps_present"], false);
    }

    #[test]
    fn cm_ground_contact_request() {
        // ext=0, index=2 (contact-request): no presence bits read.
        let v = parse_cm_ground(&[0b0_010_0000]).unwrap();
        assert_eq!(v["pdu"], "contact-request");
        assert!(v.get("air_apps_present").is_none());
    }

    /// Bit-builder for synthetic UPER vectors.
    struct Bits(Vec<u8>);
    impl Bits {
        fn new() -> Self {
            Bits(Vec::new())
        }
        fn push(&mut self, v: u64, n: usize) {
            for k in (0..n).rev() {
                self.0.push(((v >> k) & 1) as u8);
            }
        }
        fn bytes(&self) -> Vec<u8> {
            self.0
                .chunks(8)
                .map(|c| {
                    c.iter().enumerate().fold(0u8, |v, (i, &b)| v | (b << (7 - i)))
                })
                .collect()
        }
    }

    fn build_downlink_wilco() -> Vec<u8> {
        // Inner ATCDownlinkMessage: header + one dM0NULL (WILCO).
        let mut m = Bits::new();
        m.push(0, 1); // msgRef absent
        m.push(0, 1); // logicalAck default
        m.push(12, 6); // msg id 12
        m.push((2026 - 1996) as u64, 7); // year
        m.push(6 - 1, 4); // month (1..12)
        m.push(11 - 1, 5); // day (1..31)
        m.push(1, 5); // hours
        m.push(22, 6); // minutes
        m.push(33, 6); // seconds
        m.push(0, 1); // no constrainedData
        m.push(0, 3); // element count 1 (range 1..5 → 3 bits, offset 0)
        m.push(0, 7); // CHOICE index 0 = dM0NULL (114 → 7 bits)
        let inner_bits = m.0.len();

        // Outer ProtectedAircraftPDUs: send → ProtectedDownlinkMessage.
        let mut o = Bits::new();
        o.push(0, 1); // choice not extended
        o.push(3, 2); // send
        o.push(0, 1); // sequence not extended
        o.push(0, 1); // no algorithmIdentifier
        o.push(1, 1); // protectedMessage present
        // BIT STRING length determinant (short form).
        o.push(0, 1);
        o.push(inner_bits as u64, 7);
        o.0.extend(&m.0);
        // integrityCheck BIT STRING: zero-length is fine for the test.
        o.push(0, 1);
        o.push(0, 7);
        o.bytes()
    }

    #[test]
    fn downlink_wilco_decodes() {
        let v = parse_apdu(&build_downlink_wilco()).expect("apdu");
        assert_eq!(v["application"], "CPDLC");
        assert_eq!(v["direction"], "downlink");
        assert_eq!(v["pdu"], "send");
        let msg = &v["message"];
        assert_eq!(msg["msg_id"], 12);
        assert_eq!(msg["timestamp"], "2026-06-11T01:22:33Z");
        assert_eq!(msg["elements"][0]["element"], "dM0NULL");
        assert_eq!(msg["elements"][0]["phrase"], "WILCO");
    }

    #[test]
    fn uplink_element_with_argument_reports_type() {
        // Uplink: uM20Level "CLIMB TO [level]".
        let mut m = Bits::new();
        m.push(0, 1);
        m.push(0, 1);
        m.push(5, 6);
        m.push(30, 7);
        m.push(0, 4);
        m.push(0, 5);
        m.push(10, 5);
        m.push(0, 6);
        m.push(0, 6);
        m.push(0, 1);
        m.push(0, 3); // 1 element
        m.push(20, 8); // uM20 (238 → 8 bits)
        // Level argument: singleLevel, flight level, FL360.
        m.push(0, 1); // CHOICE: singleLevel
        m.push(2, 2); // LevelType: levelFlightLevel
        m.push(360 - 30, 10); // INTEGER (30..700) → 10 bits
        let inner_bits = m.0.len();
        let mut o = Bits::new();
        o.push(0, 1);
        o.push(3, 3); // ground PDUs: 3 bits, send
        o.push(0, 1);
        o.push(0, 1);
        o.push(1, 1);
        o.push(0, 1);
        o.push(inner_bits as u64, 7);
        o.0.extend(&m.0);
        o.push(0, 1);
        o.push(0, 7);
        let v = parse_pdus(&o.bytes(), false).expect("apdu");
        let el = &v["message"]["elements"][0];
        assert_eq!(el["element"], "uM20Level");
        assert_eq!(el["phrase"], "CLIMB TO [level]");
        assert_eq!(el["argument_type"], "Level");
        assert_eq!(el["text"], "CLIMB TO FL360");
    }

    #[test]
    fn cm_logon_request_flight_id_decodes() {
        let mut b = Bits::new();
        b.push(0, 1); // not extended
        b.push(0, 2); // cmLogonRequest
        b.push(0, 6); // six absent optionals
        // AircraftFlightIdentification IA5 SIZE(2..8): "UAL123" len 6.
        b.push(4, 3); // 6 - 2
        for c in b"UAL123" {
            b.push(*c as u64, 7);
        }
        let v = parse_cm_logon(&b.bytes()).expect("cm");
        assert_eq!(v["pdu"], "logon-request");
        assert_eq!(v["flight_id"], "UAL123");
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;

    #[test]
    fn cleared_route_decodes() {
        // Inner ATCUplinkMessage: uM80 CLEARED [routeClearance] with the
        // clearance in constrainedData: dest KSFO, route J501 → OAK.
        struct B(Vec<u8>);
        impl B {
            fn push(&mut self, v: u64, n: usize) {
                for k in (0..n).rev() {
                    self.0.push(((v >> k) & 1) as u8);
                }
            }
            fn ia5(&mut self, s: &str) {
                for c in s.bytes() {
                    self.push(c as u64, 7);
                }
            }
        }
        let mut m = B(Vec::new());
        m.push(0, 1); // no msgRef
        m.push(0, 1); // default logicalAck
        m.push(3, 6); // msg id
        m.push(30, 7); // year 2026
        m.push(5, 4); // June (1..12)
        m.push(10, 5); // day 11 (1..31)
        m.push(2, 5);
        m.push(0, 6);
        m.push(0, 6);
        m.push(1, 1); // constrainedData PRESENT
        m.push(0, 3); // one element
        m.push(80, 8); // uM80RouteClearance (index arg)
        m.push(0, 1); // RouteClearanceIndex = 1
        // constrainedData: ext 0, routeClearanceData present, count 1.
        m.push(0, 1);
        m.push(1, 1);
        m.push(0, 1);
        // RouteClearance presence: destination airport + route list.
        m.push(0b010000010, 9);
        m.ia5("KSFO"); // Airport: fixed SIZE(4), no length bits
        m.push(1, 7); // route count 2 (1..128)
        m.push(4, 3); // leg 1: ATS route designator
        m.push(2, 3); // IA5(2..7) length 4
        m.ia5("J501");
        m.push(0, 3); // leg 2: publishedIdentifier
        m.push(0, 1); // fixName
        m.push(0, 1); // no latlon
        m.push(2, 3); // Fix IA5(1..5) length 3
        m.ia5("OAK");
        let inner_bits = m.0.len();

        let mut o = B(Vec::new());
        o.push(0, 1);
        o.push(3, 3); // ground: send
        o.push(0, 1);
        o.push(0, 1);
        o.push(1, 1);
        // Length determinant: long form (the message exceeds 127 bits).
        if inner_bits < 128 {
            o.push(0, 1);
            o.push(inner_bits as u64, 7);
        } else {
            o.push(0b10, 2);
            o.push(inner_bits as u64, 14);
        }
        o.0.extend(&m.0);
        o.push(0, 1);
        o.push(0, 7);
        let bytes: Vec<u8> = o
            .0
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |v, (i, &b)| v | (b << (7 - i))))
            .collect();

        let v = parse_pdus(&bytes, false).expect("apdu");
        let msg = &v["message"];
        assert_eq!(msg["elements"][0]["element"], "uM80RouteClearance");
        let rc = &msg["route_clearances"][0];
        assert_eq!(rc["destination_airport"], "KSFO");
        assert_eq!(rc["route"][0], "J501");
        assert_eq!(rc["route"][1], "OAK");
    }
}

/// VDL2-1.1 argument-type decode tests.
///
/// Oracle: the ICAO Doc 9880 ASN.1 module (docs/asn1/atn-cpdlc.asn) — each
/// vector is the unaligned-PER (ITU-T X.691) encoding of a worked example
/// built bit-by-bit from the type's definition, with the expected
/// human-readable rendering derived from the published value constraints
/// (resolution / unit comments in the module). No encode→decode loopback:
/// the vectors are hand-assembled bit strings, decoded by the production
/// `read_argument` path, and asserted against spec-derived strings.
#[cfg(test)]
mod arg_tests {
    use super::*;

    /// Minimal MSB-first bit builder for hand-assembling PER vectors.
    struct Bb(Vec<u8>);
    impl Bb {
        fn new() -> Self {
            Bb(Vec::new())
        }
        fn push(&mut self, v: u64, n: usize) {
            for k in (0..n).rev() {
                self.0.push(((v >> k) & 1) as u8);
            }
        }
        fn ia5(&mut self, s: &str) {
            for c in s.bytes() {
                self.push(c as u64, 7);
            }
        }
        fn bytes(&self) -> Vec<u8> {
            self.0
                .chunks(8)
                .map(|c| c.iter().enumerate().fold(0u8, |v, (i, &b)| v | (b << (7 - i))))
                .collect()
        }
    }

    /// Decode `ty` from the assembled bits and return the rendered values.
    fn decode(b: &Bb, ty: &str) -> Vec<String> {
        let bytes = b.bytes();
        let mut store = Vec::new();
        let mut p = Per::new(&bytes, &mut store);
        read_argument(&mut p, ty).expect("argument decodes")
    }

    #[test]
    fn frequency_vhf_decodes() {
        // Frequency CHOICE index 1 (vhf, 2 bits) + INTEGER(23600..27398),
        // 12 bits. 121.500 MHz → raw 24300 → offset 700.
        let mut b = Bb::new();
        b.push(1, 2);
        b.push(24300 - 23600, 12);
        assert_eq!(decode(&b, "Frequency"), vec!["121.500 MHz"]);
    }

    #[test]
    fn frequency_hf_and_uhf_and_sat() {
        // HF: index 0, INTEGER(2850..28000), 15 bits, 8825 kHz.
        let mut b = Bb::new();
        b.push(0, 2);
        b.push(8825 - 2850, 15);
        assert_eq!(decode(&b, "Frequency"), vec!["8825 kHz"]);
        // UHF: index 2, INTEGER(9000..15999), 13 bits, 243.000 MHz → raw 9720.
        let mut b = Bb::new();
        b.push(2, 2);
        b.push(9720 - 9000, 13);
        assert_eq!(decode(&b, "Frequency"), vec!["243.000 MHz"]);
        // SAT: index 3, NumericString(SIZE 12), 4 bits/char.
        let mut b = Bb::new();
        b.push(3, 2);
        for c in "123456789012".chars() {
            b.push((c as u8 - b'0') as u64, 4);
        }
        assert_eq!(decode(&b, "Frequency"), vec!["SAT 123456789012"]);
    }

    #[test]
    fn altimeter_english_and_metric() {
        // english: CHOICE bit 0, INTEGER(2200..3200) 10 bits, 2992 → 29.92".
        let mut b = Bb::new();
        b.push(0, 1);
        b.push(2992 - 2200, 10);
        assert_eq!(decode(&b, "Altimeter"), vec!["29.92 inHg"]);
        // metric: CHOICE bit 1, INTEGER(7500..12500), 13 bits, 10132 → 1013.2.
        let mut b = Bb::new();
        b.push(1, 1);
        b.push(10132 - 7500, 13);
        assert_eq!(decode(&b, "Altimeter"), vec!["1013.2 hPa"]);
    }

    #[test]
    fn code_squawk_four_octal_digits() {
        // Code: SEQUENCE SIZE(4) OF INTEGER(0..7) → 7600 octal.
        let mut b = Bb::new();
        for d in [7u64, 6, 0, 0] {
            b.push(d, 3);
        }
        assert_eq!(decode(&b, "Code"), vec!["7600"]);
    }

    #[test]
    fn atis_code_single_char() {
        // ATISCode: IA5String(SIZE 1) → 7 bits, 'B'.
        let mut b = Bb::new();
        b.push(b'B' as u64, 7);
        assert_eq!(decode(&b, "ATISCode"), vec!["B"]);
    }

    #[test]
    fn free_text_length_prefixed() {
        // FreeText: IA5String(SIZE 1..256). Length determinant for the
        // constrained range 1..256 needs ceil(log2(256))=8 bits (offset).
        // "HELLO" len 5 → offset 4.
        let mut b = Bb::new();
        b.push(5 - 1, 8);
        b.ia5("HELLO");
        assert_eq!(decode(&b, "FreeText"), vec!["HELLO"]);
    }

    #[test]
    fn facility_designation_and_facility() {
        // FacilityDesignation: IA5(4..8). Length 5 → range 4..8 → 3 bits.
        let mut b = Bb::new();
        b.push(5 - 4, 3);
        b.ia5("KZAKZ");
        assert_eq!(decode(&b, "FacilityDesignation"), vec!["KZAKZ"]);
        // Facility CHOICE: noFacility (bit 0).
        let mut b = Bb::new();
        b.push(0, 1);
        assert_eq!(decode(&b, "Facility"), vec!["(none)"]);
    }

    #[test]
    fn unit_name_frequency_full() {
        // UnitNameFrequency = UnitName + Frequency.
        // UnitName: facilityName present (bit 1), designation IA5(4..8)
        // "KZAK" (len 4 → 0), name IA5(3..18) "OAKLAND" (len 7 → range
        // 3..18 → 4 bits → offset 4), function CENTER (ext 0 + index 0,
        // 9 root → 4 bits). Frequency vhf 134.150 → raw 26830.
        let mut b = Bb::new();
        b.push(1, 1); // facilityName present
        b.push(4 - 4, 3); // designation len 4
        b.ia5("KZAK");
        b.push(7 - 3, 4); // name len 7
        b.ia5("OAKLAND");
        b.push(0, 1); // function not extended
        b.push(0, 4); // function = CENTER
        b.push(1, 2); // frequency vhf
        b.push(26830 - 23600, 12); // 134.150 MHz
        assert_eq!(
            decode(&b, "UnitNameFrequency"),
            vec!["KZAK OAKLAND CENTER", "134.150 MHz"]
        );
    }

    #[test]
    fn vertical_rate_english() {
        // VerticalRate CHOICE bit 0, INTEGER(0..3000) 12 bits, 50 → 500 fpm.
        let mut b = Bb::new();
        b.push(0, 1);
        b.push(50, 12);
        assert_eq!(decode(&b, "VerticalRate"), vec!["500 FPM"]);
    }

    #[test]
    fn distance_specified_direction_nm() {
        // DistanceSpecifiedDirection: DistanceSpecified CHOICE bit 0 (nm),
        // INTEGER(1..250) 8 bits, 20 → offset 19; Direction LEFT (0),
        // 11 values non-ext → constrained(0..10) → 4 bits.
        let mut b = Bb::new();
        b.push(0, 1); // nm
        b.push(20 - 1, 8);
        b.push(0, 4); // LEFT
        assert_eq!(decode(&b, "DistanceSpecifiedDirection"), vec!["20 NM", "LEFT"]);
    }

    #[test]
    fn traffic_type_enum() {
        // TrafficType ENUM (extensible): ext 0 + index 1 (oppositeDirection),
        // 6 root → constrained(0..5) → 3 bits.
        let mut b = Bb::new();
        b.push(0, 1); // not extended
        b.push(1, 3);
        assert_eq!(decode(&b, "TrafficType"), vec!["OPPOSITE DIRECTION"]);
    }

    #[test]
    fn clearance_type_enum() {
        // ClearanceType ENUM (extensible): ext 0 + index 2 (departure),
        // 12 root → constrained(0..11) → 4 bits.
        let mut b = Bb::new();
        b.push(0, 1);
        b.push(2, 4);
        assert_eq!(decode(&b, "ClearanceType"), vec!["DEPARTURE"]);
    }

    #[test]
    fn error_information_enum() {
        // ErrorInformation ENUM (extensible): ext 0 + index 4
        // (invalidMessageElement), 5 root → constrained(0..4) → 3 bits.
        let mut b = Bb::new();
        b.push(0, 1);
        b.push(4, 3);
        assert_eq!(decode(&b, "ErrorInformation"), vec!["INVALID MESSAGE ELEMENT"]);
    }

    #[test]
    fn speed_type_triple_and_speed() {
        // SpeedTypeSpeedTypeSpeedTypeSpeed: three SpeedType then a Speed.
        // SpeedType ENUM (extensible): ext 0 + index, 9 root → 4 bits.
        // INDICATED(1), TRUE(2), MACH(4); then Speed mach M0.840.
        let mut b = Bb::new();
        for idx in [1u64, 2, 4] {
            b.push(0, 1); // not extended
            b.push(idx, 4);
        }
        // Speed CHOICE index 6 (mach, 7 alts → 3 bits), INTEGER(500..4000),
        // bits = ceil(log2(3501)) = 12. M0.840 → raw 840 → offset 340.
        b.push(6, 3);
        b.push(840 - 500, 12);
        assert_eq!(
            decode(&b, "SpeedTypeSpeedTypeSpeedTypeSpeed"),
            vec!["INDICATED", "TRUE", "MACH", "M0.840"]
        );
    }

    #[test]
    fn position_time_time_two_times() {
        // PositionTimeTime: position (airport KSFO) + two Times.
        // Position CHOICE index 2 (airport, 5 alts → 3 bits), IA5(4) fixed.
        // Time: hours(0..23) 5 bits, minutes(0..59) 6 bits.
        let mut b = Bb::new();
        b.push(2, 3); // airport
        b.ia5("KSFO");
        b.push(10, 5);
        b.push(30, 6); // 10:30
        b.push(11, 5);
        b.push(45, 6); // 11:45
        assert_eq!(
            decode(&b, "PositionTimeTime"),
            vec!["KSFO", "10:30", "11:45"]
        );
    }

    #[test]
    fn tofrom_position() {
        // ToFromPosition: ToFrom enum (to/from, 1 bit) + Position.
        // FROM (1), then airport "EGLL".
        let mut b = Bb::new();
        b.push(1, 1); // FROM
        b.push(2, 3); // airport
        b.ia5("EGLL");
        assert_eq!(decode(&b, "ToFromPosition"), vec!["FROM", "EGLL"]);
    }

    #[test]
    fn hold_clearance_full() {
        // HoldClearance: legType OPTIONAL present.
        // presence bit 1; position airport "KSFO"; level FL250;
        // degrees magnetic 270; direction RIGHT; legType legTime 5 min.
        let mut b = Bb::new();
        b.push(1, 1); // legType present
        b.push(2, 3); // position: airport
        b.ia5("KSFO");
        b.push(0, 1); // level: singleLevel
        b.push(2, 2); // levelFlightLevel
        b.push(250 - 30, 10); // FL250
        b.push(0, 1); // degrees: magnetic
        b.push(270 - 1, 9); // INTEGER(1..360) 9 bits → 270
        b.push(1, 4); // direction RIGHT (1)
        b.push(1, 1); // legType: legTime
        b.push(5, 4); // INTEGER(0..10) → 5 min
        assert_eq!(
            decode(&b, "HoldClearance"),
            vec!["KSFO", "FL250", "270°M", "RIGHT", "5 MIN"]
        );
    }

    #[test]
    fn departure_clearance_head() {
        // DepartureClearance: both OPTIONALs absent. flight id "DLH456"
        // IA5(2..8) len 6 → range 2..8 → 3 bits → offset 4; clearance limit
        // airport "EDDF".
        let mut b = Bb::new();
        b.push(0, 1); // flightInformation absent
        b.push(0, 1); // furtherInstructions absent
        b.push(6 - 2, 3); // flight id len 6
        b.ia5("DLH456");
        b.push(2, 3); // position: airport
        b.ia5("EDDF");
        assert_eq!(
            decode(&b, "DepartureClearance"),
            vec!["DLH456 CLEARED TO EDDF"]
        );
    }

    #[test]
    fn runway_rvr() {
        // RunwayRVR: Runway (dir 1..36 → 6 bits, config 2 bits) + RVR.
        // RWY 27L: dir 27 → offset 26; config LEFT(0). RVR feet 1200.
        let mut b = Bb::new();
        b.push(27 - 1, 6);
        b.push(0, 2); // L
        b.push(0, 1); // RVR feet
        b.push(1200, 13); // INTEGER(0..6100) → 13 bits
        assert_eq!(decode(&b, "RunwayRVR"), vec!["RWY 27L", "1200 FT"]);
    }

    #[test]
    fn position_report_mandatory_only() {
        // PositionReport with every optional absent: 19 presence bits = 0,
        // then position (airport KSFO), time 12:00, level FL350.
        let mut b = Bb::new();
        b.push(0, 19); // all 19 optionals absent
        b.push(2, 3); // airport
        b.ia5("KSFO");
        b.push(12, 5);
        b.push(0, 6); // 12:00
        b.push(0, 1); // level singleLevel
        b.push(2, 2); // FL
        b.push(350 - 30, 10);
        assert_eq!(
            decode(&b, "PositionReport"),
            vec!["KSFO 12:00 FL350"]
        );
    }

    #[test]
    fn remaining_fuel_persons_on_board() {
        // RemainingFuelPersonsOnBoard: RemainingFuel(=Time) 02:30 +
        // PersonsOnBoard INTEGER(1..1024) → 150 → offset 149, 10 bits.
        let mut b = Bb::new();
        b.push(2, 5);
        b.push(30, 6); // 02:30
        b.push(150 - 1, 10);
        assert_eq!(
            decode(&b, "RemainingFuelPersonsOnBoard"),
            vec!["02:30", "150"]
        );
    }

    #[test]
    fn version_number() {
        // VersionNumber INTEGER(0..15) → 4 bits, value 5.
        let mut b = Bb::new();
        b.push(5, 4);
        assert_eq!(decode(&b, "VersionNumber"), vec!["v5"]);
    }

    /// End-to-end: a ground uM166 DUE TO [traffictype] TRAFFIC element walk
    /// no longer stops at the (previously unsupported) TrafficType argument.
    #[test]
    fn uplink_traffic_type_element_walks() {
        let mut m = Bb::new();
        m.push(0, 1); // no msgRef
        m.push(0, 1); // default logicalAck
        m.push(7, 6); // msg id
        m.push(30, 7); // year 2026
        m.push(5, 4); // June
        m.push(10, 5); // day 11
        m.push(0, 5);
        m.push(0, 6);
        m.push(0, 6);
        m.push(0, 1); // no constrainedData
        m.push(0, 3); // 1 element
        m.push(166, 8); // uM166TrafficType
        m.push(0, 1); // TrafficType: not extended
        m.push(3, 3); // index 3 = converging
        let inner_bits = m.0.len();
        let mut o = Bb::new();
        o.push(0, 1);
        o.push(3, 3); // ground: send
        o.push(0, 1);
        o.push(0, 1);
        o.push(1, 1);
        o.push(0, 1);
        o.push(inner_bits as u64, 7);
        o.0.extend(&m.0);
        o.push(0, 1);
        o.push(0, 7);
        let v = parse_pdus(&o.bytes(), false).expect("apdu");
        let el = &v["message"]["elements"][0];
        assert_eq!(el["element"], "uM166TrafficType");
        assert_eq!(el["argument_type"], "TrafficType");
        assert_eq!(el["text"], "DUE TO CONVERGINGTRAFFIC");
    }
}
