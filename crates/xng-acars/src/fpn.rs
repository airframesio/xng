//! ARINC 702 flight-plan (FPN) decoder for the H1 `FPN/` preamble.
//!
//! Clean-room reimplementation from airframes' own documentation and test
//! suite (facts only): acars-message-documentation `research/H1/FPN.md`
//! (format, key table, status codes, real example messages) and
//! acars-decoder-typescript `plugins/ARINC_702.ts` +
//! `plugins/Label_H1_FPN.test.ts` (field labels, the company-route /
//! waypoint rendering and the decimal-minute coordinate conversion). FPN
//! messages are key/value records:
//!
//! ```text
//! FPN/[SN<serial>/][FN<flight>/][TS<time>/]<status>:<key>:<val>:...<csum>
//! ```
//!
//! where `<status>` is `RI` (route inactive) or `RP` (route planned), the
//! trailing four characters are a (hex) message checksum, and the keys are
//! `DA` (origin), `AA` (destination), `CR` (company route), `R`
//! (departure runway), `D` (departure procedure), `A` (arrival
//! procedure), `AP` (approach procedure) and `F` (aircraft route / first
//! waypoint).

use crate::position::{decode_decimal_minutes, Position};
use serde::Serialize;

/// A waypoint in a route: its name and, when present, its decoded position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Waypoint {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlightPlan {
    /// `Route Inactive` / `Route Planned` (from the `RI`/`RP` status).
    pub route_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure_runway: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure_procedure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrival_procedure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approach_procedure: Option<String>,
    /// Decoded waypoints from the aircraft-route (`F`) fields, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub waypoints: Vec<Waypoint>,
    /// Trailing 4-character message checksum, lowercased with a `0x`
    /// prefix to match acars-decoder-typescript's rendering.
    pub checksum: String,
}

/// Parse a flight-plan message. `text` is the H1 message text; returns
/// `None` when it is not an `FPN/` record.
pub fn parse(text: &str) -> Option<FlightPlan> {
    // libacars/airframes substitute nothing here, but real messages carry
    // embedded CR/LF inside coordinates; strip them so a split coordinate
    // stays contiguous (acars-decoder-typescript test "FPN with newlines").
    let cleaned: String = text.chars().filter(|&c| c != '\r' && c != '\n').collect();
    let body = cleaned.strip_prefix("FPN/")?;
    if body.len() < 4 {
        return None;
    }
    // The last 4 characters are the checksum; everything before is the
    // record. Split that into ':'-separated fields.
    let (record, csum) = body.split_at(body.len() - 4);
    let record = record.strip_suffix(':').unwrap_or(record);

    let mut fields = record.split(':');
    let header = fields.next()?;

    // Header: "<status>" or "[SN.../][FN.../][TS.../]<status>", the parts
    // separated by '/'. The status (RI/RP) is the final part.
    let mut flight_number = None;
    let mut serial_number = None;
    let mut route_status = None;
    for part in header.split('/') {
        if let Some(fn_) = part.strip_prefix("FN") {
            flight_number = Some(fn_.to_string());
        } else if let Some(sn) = part.strip_prefix("SN") {
            // SN may carry a trailing ",time"; keep the leading token.
            serial_number = Some(sn.split(',').next().unwrap_or(sn).to_string());
        } else if part.starts_with("TS") {
            // Timestamp token: not surfaced as a structured time here.
        } else if part == "RI" {
            route_status = Some("Route Inactive");
        } else if part == "RP" {
            route_status = Some("Route Planned");
        }
    }
    let route_status = route_status?.to_string();

    let mut fp = FlightPlan {
        route_status,
        flight_number,
        serial_number,
        origin: None,
        destination: None,
        company_route: None,
        departure_runway: None,
        departure_procedure: None,
        arrival_procedure: None,
        approach_procedure: None,
        waypoints: Vec::new(),
        checksum: format!("0x{}", csum.to_lowercase()),
    };

    // Remaining fields are key:value pairs.
    let rest: Vec<&str> = fields.collect();
    let mut i = 0;
    while i + 1 < rest.len() {
        let key = rest[i];
        let val = rest[i + 1];
        match key {
            "DA" => fp.origin = Some(val.to_string()),
            "AA" => fp.destination = Some(val.to_string()),
            "CR" => fp.company_route = Some(val.to_string()),
            "R" => fp.departure_runway = Some(val.to_string()),
            "D" => fp.departure_procedure = Some(val.to_string()),
            "A" => fp.arrival_procedure = Some(val.to_string()),
            "AP" => fp.approach_procedure = Some(val.to_string()),
            "F" => fp.waypoints.extend(parse_route(val)),
            _ => {}
        }
        i += 2;
    }

    Some(fp)
}

/// Parse a route value into waypoints. Routes are `.`/`..`-delimited tokens
/// (`..` = direct-to, `.` = along-airway/to). Each waypoint token may carry
/// a `name,N12345W123456` position annotation. Pure airway identifiers (no
/// embedded coordinate, between two waypoints) are surfaced as named
/// waypoints without a position.
fn parse_route(route: &str) -> Vec<Waypoint> {
    let mut out = Vec::new();
    for token in route.split('.') {
        if token.is_empty() {
            continue; // the second dot of a `..` direct-to separator
        }
        // A token may be "NAME,COORD" or just "NAME"/"COORD".
        let (name, position) = if let Some((n, coord)) = token.split_once(',') {
            (n.to_string(), decode_decimal_minutes(coord))
        } else {
            (token.to_string(), decode_decimal_minutes(token))
        };
        out.push(Waypoint { name, position });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    // Reference messages and expected fields are the real documented
    // examples from airframes' acars-decoder-typescript
    // `Label_H1_FPN.test.ts` and acars-message-documentation
    // `research/H1/FPN.md`.

    #[test]
    fn landing_route_inactive() {
        // Label_H1_FPN.test.ts "FPN landing".
        let fp = parse(
            "FPN/RI:DA:KEWR:AA:KDFW:CR:EWRDFW01(17L)..SAAME.J6.HVQ.Q68.LITTR..MEEOW..FEWWW:A:SEEVR4.FEWWW:F:VECTOR..DISCO..RIVET:AP:ILS 17L.RIVET:F:TACKEC8B5",
        )
        .expect("FPN parses");
        assert_eq!(fp.route_status, "Route Inactive");
        assert_eq!(fp.origin.as_deref(), Some("KEWR"));
        assert_eq!(fp.destination.as_deref(), Some("KDFW"));
        assert_eq!(
            fp.company_route.as_deref(),
            Some("EWRDFW01(17L)..SAAME.J6.HVQ.Q68.LITTR..MEEOW..FEWWW")
        );
        assert_eq!(fp.arrival_procedure.as_deref(), Some("SEEVR4.FEWWW"));
        assert_eq!(fp.approach_procedure.as_deref(), Some("ILS 17L.RIVET"));
        assert_eq!(fp.checksum, "0xc8b5");
    }

    #[test]
    fn full_flight_with_flight_number_and_coords() {
        // Label_H1_FPN.test.ts "FPN full flight".
        let fp = parse(
            "FPN/FNAAL1956/RP:DA:KPHL:AA:KPHX:CR:PHLPHX61:R:27L(26O):D:PHL3:A:EAGUL6.ZUN:AP:ILS26..AIR,N40010W080490.J110.BOWRR..VLA,N39056W089097..STL,N38516W090289..GIBSN,N38430W092244..TYGER,N38410W094050..GCK,N37551W100435..DIXAN,N36169W105573..ZUN,N34579W109093293B",
        )
        .expect("FPN parses");
        assert_eq!(fp.route_status, "Route Planned");
        assert_eq!(fp.flight_number.as_deref(), Some("AAL1956"));
        assert_eq!(fp.origin.as_deref(), Some("KPHL"));
        assert_eq!(fp.destination.as_deref(), Some("KPHX"));
        assert_eq!(fp.company_route.as_deref(), Some("PHLPHX61"));
        assert_eq!(fp.departure_runway.as_deref(), Some("27L(26O)"));
        assert_eq!(fp.departure_procedure.as_deref(), Some("PHL3"));
        assert_eq!(fp.arrival_procedure.as_deref(), Some("EAGUL6.ZUN"));
        assert_eq!(fp.checksum, "0x293b");
        // The approach value carries waypoints; it is surfaced verbatim
        // (the AP field) — coordinate decoding is exercised via the F field
        // in the in-flight test below.
        assert!(fp.approach_procedure.as_deref().unwrap().contains("AIR,N40010W080490"));
    }

    #[test]
    fn in_flight_waypoints_decode_coordinates() {
        // Label_H1_FPN.test.ts "FPN in-flight": the F (aircraft-route)
        // waypoints decode to decimal-minute positions matching the TS
        // decoder (KAYEX -> 36.487 N, 120.948 W ...).
        let fp = parse(
            "FPN/FNUAL1187/RP:DA:KSFO:AA:KPHX:F:KAYEX,N36292W120569..LOSHN,N35509W120000..BOILE,N34253W118016..BLH,N33358W114457DDFB",
        )
        .expect("FPN parses");
        assert_eq!(fp.flight_number.as_deref(), Some("UAL1187"));
        assert_eq!(fp.origin.as_deref(), Some("KSFO"));
        assert_eq!(fp.destination.as_deref(), Some("KPHX"));
        assert_eq!(fp.checksum, "0xddfb");

        let names: Vec<&str> = fp.waypoints.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["KAYEX", "LOSHN", "BOILE", "BLH"]);
        let kayex = fp.waypoints[0].position.expect("KAYEX has a position");
        assert!(close(kayex.latitude, 36.487), "lat {}", kayex.latitude);
        assert!(close(kayex.longitude, -120.948), "lon {}", kayex.longitude);
        let blh = fp.waypoints[3].position.expect("BLH has a position");
        assert!(close(blh.latitude, 33.597), "lat {}", blh.latitude);
        assert!(close(blh.longitude, -114.762), "lon {}", blh.longitude);
    }

    #[test]
    fn serial_number_and_route_inactive_with_newlines() {
        // Label_H1_FPN.test.ts "FPN with newlines": SN + FN header, an
        // embedded CR/LF inside a coordinate, RI status.
        let fp = parse(
            "FPN/SN2125/FNQFA780/RI:DA:YPPH:CR:PERMEL001:AA:YMML..MEMUP,S33451E\r\n120525.Y53.WENDY0560",
        )
        .expect("FPN parses");
        assert_eq!(fp.route_status, "Route Inactive");
        assert_eq!(fp.flight_number.as_deref(), Some("QFA780"));
        assert_eq!(fp.serial_number.as_deref(), Some("2125"));
        assert_eq!(fp.origin.as_deref(), Some("YPPH"));
        assert_eq!(fp.company_route.as_deref(), Some("PERMEL001"));
        assert_eq!(fp.checksum, "0x0560");
    }

    #[test]
    fn rejects_non_fpn() {
        assert!(parse("POSN43312W123174,EASON").is_none());
        assert!(parse("#DFB engine data").is_none());
        assert!(parse("FPN/").is_none());
    }
}
