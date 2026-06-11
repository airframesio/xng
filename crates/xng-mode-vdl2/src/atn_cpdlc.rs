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
