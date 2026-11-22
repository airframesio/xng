//! Gate assignments: arrival gate uplinks on label RA, pre-departure
//! clearance departure gates, and airline gate messages on other labels.
//!
//! Port of the Airframes event digger's gate parsing
//! (microservice-event-digger `pkg/handlers/labelra_handler.go` for the
//! label RA formats in [`parse`], `pkg/handlers/gate_reports.go` for the
//! families in [`parse_report`]); keep the rules in step with it.
//!
//! ## Label RA ([`parse`])
//!
//! Three formats name the gate at the arrival airport:
//!
//! - United `GATE ASSIGN` / `GATE CHANGE`. The field lines are often
//!   indented with a tab, and a gate change repeats "GATE ASSIGN" inside a
//!   crew alert, so the text is read line by line. The flight/route line
//!   gives the flight and the ICAO origin/destination. A gate always has a
//!   digit, which tells it apart from the "GATE ASSIGN" line; when several
//!   gates appear, the last one wins.
//!
//!   ```text
//!   QUNDCULUA~1GATE ASSIGN
//!       UA0942/21 KIAD LPPT
//!       SENT: 09:14:42Z
//!       GATE 43    OPEN
//!   ```
//!
//! - Republic `ARRIVAL GATE`:
//!   `RPA N401YX YX4450 ORD MSN ETA = 0542 GATE = 16` is gate 16 at MSN.
//! - SkyWest `ARR GATE INFO`: `BFL ARRIVAL GATE 4` is gate 4 at BFL. The
//!   next-flight and crew block that follows is about another flight and
//!   is not read.
//!
//! Airports are as written: ICAO for United, IATA for Republic and SkyWest.
//! Finnair connection lists (`QUHELASAY~1`) name the departure gates of
//! other flights, so they are not read as arrival gates.
//!
//! ## Other gate reports ([`parse_report`])
//!
//! Each family is tied to the labels it was seen on:
//!
//! | Family | Labels | Example |
//! |---|---|---|
//! | Pre-departure clearance (departure gate) | B3 | `JZA110-CYWG-GATE 12-CYOW` |
//! | American, Envoy, PSA, GoJet, Piedmont | 40, 41, C1, RA | `FLT 1658/22 AUS-DFW` / `GATE D21 ...`, `NEW ARRIVAL GATE NBR IS C33` |
//! | Envoy arriving | 40 | `FLT 4258 ARRIVING DCA` / `ARRIVAL GATE D43A` |
//! | Air Wisconsin | RA | `FLT 4940 IAD GATE A4C` |
//! | Frontier | RA | `FROM PHL OPS` ... `GATE  E8` / `GATE AVAIL AT ARRIVAL  Y` |
//! | Hawaiian | 3T, 40 | `804 ARR GATE 69A`, `HA0805 AT GATE A6` |
//! | JetBlue in range | 41, 42 | `INR,221156,  GATE:514` |
//! | Jetstar | C1 | `ARRIVAL GATE THIS FLT:43` |
//! | Alaska gate change | 21 | `GATE CHANGE IN LAX` / `NEW GATE: 210A` |
//! | PSA crew acknowledgement | 5Z | `/ACKFLT GATE F1` |
//! | Jazz gate data | C1 | `- JZA GATE DATA UPLINK` ... `FLIGHT: 8063/22Nov26/YYC/YVR` / `PLANNED ARRIVAL GATE IS C42` |
//! | Jetstar gate change | C1 | `JQ782/22 Nov MEL-ADL` / `GATE CHANGE MESSAGE - ARRIVAL` / `NEW GATE: 23` |
//! | WestJet | RA | `WS 345 YXE-YYC` / `YYC GATE D80/OPS`, `ARR GATE IS NOW C26/OPS` |
//! | Shandong and others | RA | `SC8402 ARR:ZSJN` / `GATE:10` |
//! | LATAM | RA | `GATE ASSIGNMENT` / `PPOS:602L` |
//!
//! A gate must contain a digit, so `GATE OPEN` or `GATE STATUS NOT AVAIL`
//! never match. Reading stops where a message moves on to other flights
//! (crew/pax connections, `NEXT FLT`, `NEXT LEG`, `NEXT SECTOR`,
//! `TURN FLT`, `AC TURN`, `END`). The
//! label RA uplinks read by [`parse`] and Finnair connection lists are left
//! to it; gate requests (5Z `/C3 GATE REQ`) carry no gate and are not read.

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// United `GATE ASSIGN`.
    GateAssign,
    /// United `GATE CHANGE`.
    GateChange,
    /// Gate at the arrival airport: Republic `ARRIVAL GATE`, SkyWest
    /// `ARR GATE INFO` and the [`parse_report`] arrival families.
    ArrivalGate,
    /// Gate at the departure airport, from a pre-departure clearance.
    DepartureGate,
}

/// The gate assigned to the flight: at its arrival airport, or for
/// [`GateKind::DepartureGate`] at its departure airport.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateInfo {
    pub kind: GateKind,
    /// Message family of a [`parse_report`] gate, as the event digger names
    /// it (`PDC`, `FLT_ROUTE_GATE`, `JETBLUE_INR`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<&'static str>,
    /// The gate, e.g. `43`, `B47`, `C121`, `26B`, `D43A`.
    pub gate: String,
    /// `OPEN` or `AVAIL` (United).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_status: Option<String>,
    /// HH:MM the gate becomes available (United `AVAIL 06:30`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_available_time: Option<String>,
    /// Flight as written, e.g. `UA0942`, `YX4450`, `HA0805`, `1658`, or the
    /// callsign of a clearance (`JZA110`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_number: Option<String>,
    /// Day of month of the flight (United `UA0942/21`, `FLT 1658/22`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    /// Departure airport; for a departure gate, the airport the gate is at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure: Option<String>,
    /// Arrival airport, as written (ICAO for United and clearances, IATA
    /// otherwise); for an arrival gate, the airport the gate is at. Some
    /// airlines (Hawaiian, JetBlue, Jetstar) do not name it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrival: Option<String>,
    /// HH:MM:SS UTC the assignment was sent (United).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_time: Option<String>,
    /// Estimated on time, HHMM UTC (United `EON`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eon_time: Option<String>,
    /// Estimated arrival, HHMM (Republic `ETA =`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ramp_frequency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops_frequency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_power: Option<bool>,
    /// HH:MM planned on the ground (United `PLANNED GROUND TIME IS 02:13`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_ground_time: Option<String>,
}

impl GateInfo {
    fn new(kind: GateKind, gate: &str, arrival: &str) -> Self {
        Self {
            kind,
            format: None,
            gate: gate.to_owned(),
            gate_status: None,
            gate_available_time: None,
            flight_number: None,
            day: None,
            departure: None,
            arrival: (!arrival.is_empty()).then(|| arrival.to_owned()),
            sent_time: None,
            eon_time: None,
            eta: None,
            ramp_frequency: None,
            ops_frequency: None,
            ground_power: None,
            planned_ground_time: None,
        }
    }

    pub fn summary(&self) -> String {
        let mut s = String::from(match self.kind {
            GateKind::GateAssign => "GATE ASSIGN",
            GateKind::GateChange => "GATE CHANGE",
            GateKind::ArrivalGate => "ARRIVAL GATE",
            GateKind::DepartureGate => "DEPARTURE GATE",
        });
        if let Some(f) = &self.flight_number {
            s.push_str(&format!(" {f}"));
        }
        match (&self.departure, &self.arrival) {
            (Some(d), Some(a)) => s.push_str(&format!(" {d}->{a}")),
            (Some(a), None) | (None, Some(a)) => s.push_str(&format!(" {a}")),
            (None, None) => {}
        }
        s.push_str(&format!(" gate {}", self.gate));
        if let Some(st) = &self.gate_status {
            s.push_str(&format!(" {st}"));
        }
        if let Some(t) = &self.gate_available_time {
            s.push_str(&format!(" {t}"));
        }
        s
    }
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

re!(UA_HEADER, r"^[A-Z0-9~.]+?\s*GATE (ASSIGN|CHANGE)\b");
re!(UA_FLIGHT, r"^([A-Z]{2}\d{1,4})/(\d{2})\s+([A-Z0-9]{4})\s+([A-Z0-9]{4})\b");
re!(UA_SENT, r"^SENT:\s*(\d{2}:\d{2}:\d{2})Z");
// The gate always has a digit, which tells it apart from the "GATE ASSIGN"
// and "GATE CHANGE" lines. `AVAIL` gates carry the time they free up.
re!(UA_GATE, r"^GATE\s+([A-Z]{0,3}\d+[A-Z]?)(?:\s+([A-Z]+)(?:\s+(\d{2}:\d{2}))?)?$");
re!(UA_EON, r"^EON\s+(\d{4})Z");
re!(UA_RAMP, r"^RAMP FREQ\s+(\d{3}\.\d{1,3})");
re!(UA_OPS, r"^OPS FREQ\s+(\d{3}\.\d{1,3})");
re!(UA_POWER, r"^GROUND PWR AVAIL -(YES|NO)-");
re!(UA_GROUND_TIME, r"^IS (\d{2}:\d{2})$");
re!(
    RPA_LINE,
    r"\b([A-Z0-9]{2}\d{1,4})\s+([A-Z]{3})\s+([A-Z]{3})\s+ETA\s*=\s*(\d{4})\s+GATE\s*=\s*([A-Z]{0,3}\d+[A-Z]?)\b"
);
// The airport line usually follows the header; some uplinks put it on the
// header line ("ARR GATE INFO   LAX ARRIVAL GATE 26B").
re!(
    SKW_LINE,
    r"(?m)(?:^|ARR GATE INFO)[ \t]*([A-Z]{3}) ARRIVAL GATE[ \t]+([A-Z]{0,3}\d+[A-Z]?)[ \t\r]*$"
);

/// Parse a label RA gate uplink; `None` when the text is not one.
pub fn parse(text: &str) -> Option<GateInfo> {
    parse_united(text).or_else(|| parse_arrival_gate(text))
}

fn parse_united(text: &str) -> Option<GateInfo> {
    let mut lines = text.split('\n').map(str::trim);
    let kind = match &UA_HEADER.captures(lines.next()?)?[1] {
        "CHANGE" => GateKind::GateChange,
        _ => GateKind::GateAssign,
    };
    let mut flight = None;
    let mut sent = None;
    let mut gate = None;
    let mut out = GateInfo::new(kind, "", "");
    let mut after_ground_time = false;
    for l in lines {
        // The rest is about the aircraft's next flight, whose gate lines
        // must not replace this flight's arrival gate.
        if l.starts_with("NEXT FLT") {
            break;
        }
        if flight.is_none() {
            flight = UA_FLIGHT.captures(l);
            continue;
        }
        if sent.is_none() {
            if let Some(c) = UA_SENT.captures(l) {
                sent = Some(c[1].to_owned());
                continue;
            }
        }
        // A gate change can name the gate more than once; the last wins.
        if let Some(c) = UA_GATE.captures(l) {
            gate = Some(c);
        } else if let Some(c) = UA_EON.captures(l) {
            out.eon_time = Some(c[1].to_owned());
        } else if let Some(c) = UA_RAMP.captures(l) {
            out.ramp_frequency = Some(c[1].to_owned());
        } else if let Some(c) = UA_OPS.captures(l) {
            out.ops_frequency = Some(c[1].to_owned());
        } else if let Some(c) = UA_POWER.captures(l) {
            out.ground_power = Some(&c[1] == "YES");
        } else if l == "PLANNED GROUND TIME" {
            after_ground_time = true;
            continue;
        } else if after_ground_time {
            if let Some(c) = UA_GROUND_TIME.captures(l) {
                out.planned_ground_time = Some(c[1].to_owned());
            }
        }
        after_ground_time = false;
    }
    let (flight, gate) = (flight?, gate?);
    out.gate = gate[1].to_owned();
    out.gate_status = gate.get(2).map(|m| m.as_str().to_owned());
    out.gate_available_time = gate.get(3).map(|m| m.as_str().to_owned());
    out.flight_number = Some(flight[1].to_owned());
    out.day = Some(flight[2].to_owned());
    out.departure = Some(flight[3].to_owned());
    out.arrival = Some(flight[4].to_owned());
    out.sent_time = sent;
    Some(out)
}

fn parse_arrival_gate(text: &str) -> Option<GateInfo> {
    if let Some(c) = RPA_LINE.captures(text) {
        let mut g = GateInfo::new(GateKind::ArrivalGate, &c[5], &c[3]);
        g.flight_number = Some(c[1].to_owned());
        g.departure = Some(c[2].to_owned());
        g.eta = Some(c[4].to_owned());
        return Some(g);
    }
    if text.contains("ARR GATE INFO") {
        let c = SKW_LINE.captures(text)?;
        return Some(GateInfo::new(GateKind::ArrivalGate, &c[2], &c[1]));
    }
    None
}

// --- Other gate reports (event digger `gate_reports.go`) ---

/// A gate as airlines write it: 4, 43, D21, D43A, 210A, A4C, S113, SEC9,
/// 221R. It needs a digit, so words like OPEN never match.
const GATE_TOKEN: &str = r"([A-Z]{0,3}\d{1,4}[A-Z]?)";

macro_rules! gate_re {
    ($name:ident, $pre:expr, $post:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(&format!("{}{}{}", $pre, GATE_TOKEN, $post)).unwrap());
    };
}

// FLT 1658/22 AUS-DFW (American, Envoy, PSA, GoJet, Piedmont). The line can
// follow an RA header or a dash (`QUSBYDDPT~1FLT 5966 SYR-PHL...`,
// `- FLT 0174 RDU-LHR`), and Piedmont runs the gate into the route:
// `FLT 5966 SYR-PHLGATE F37 ETA 707L`.
re!(ROUTE_LINE, r"^(?:-\s*|[A-Z0-9]{9}[~.]\d)?FLT\s+(\d{1,4})(?:/(\d{2}))?\s+([A-Z]{3})-([A-Z]{3})");
gate_re!(INLINE_GATE, r"GATE\s+", r"(?:\s|$)");
// GATE D21 AVAILABILITY PLN ETA 0721, GATE D43APCA Y ELEC Y, GATE 9
gate_re!(LEADING_GATE, r"^GATE\s+", r"(?:\s|PCA|$)");
// NEW ARRIVAL GATE NBR IS  C33
gate_re!(NEW_ARRIVAL, r"^NEW ARRIVAL GATE NBR IS\s+", r"\s*$");
// Envoy: FLT 4258   ARRIVING DCA / ARRIVAL GATE D43A  BAG CLAIM AREA 9
re!(ENVOY_ARRIVING, r"^FLT\s+\d{1,4}\s+ARRIVING\s+([A-Z]{3})\b");
gate_re!(ENVOY_GATE, r"^ARRIVAL GATE\s+", r"(?:\s|$)");
// United Express (Air Wisconsin): FLT 4940 IAD GATE A4C
gate_re!(FLIGHT_AIRPORT_GATE, r"^FLT\s+(\d{1,4})\s+([A-Z]{3})\s+GATE\s+", r"\s*$");
// Hawaiian: ARR GATE MSG / 899 ARR GATE C5, and HA1195 AT GATE A19
gate_re!(HAWAIIAN_ARR, r"^(\d{1,4})\s+ARR GATE\s+", r"\s*$");
gate_re!(HAWAIIAN_AT_GATE, r"^(HA\d{1,4})\s+AT GATE\s+", r"\s*$");
// JetBlue in range: INR,221156,  GATE:514
re!(JETBLUE_INR, r"^INR,\d{6}\b");
gate_re!(JETBLUE_GATE, r"GATE:\s*", r"\s*$");
// Jetstar: ARRIVAL GATE THIS FLT:43
gate_re!(JETSTAR, r"^ARRIVAL GATE THIS FLT:\s*", r"\s*$");
// Alaska: GATE CHANGE IN LAX / AS1636 / NEW GATE: 210A
re!(ALASKA_CHANGE, r"^GATE CHANGE IN ([A-Z]{3})$");
re!(ALASKA_FLIGHT, r"^(AS\d{1,4})$");
gate_re!(ALASKA_GATE, r"^NEW GATE:\s*", r"\s*$");
// Frontier: FROM PHL OPS / ... / GATE  E8 / GATE AVAIL AT ARRIVAL  Y
re!(FRONTIER_FROM, r"^FROM ([A-Z]{3}) OPS$");
gate_re!(FRONTIER_GATE, r"^GATE\s+", r"$");
// Pre-departure clearance (label B3): JZA110-CYWG-GATE 12-CYOW,
// BAW22G -EGKK-GATE 27-MDPC. The gate is at the departure airport.
gate_re!(PDC, r"^([A-Z0-9]{2,8})\s*-([A-Z]{4})-GATE\s+", r"-([A-Z]{4})\b");
// PSA crew acknowledgement of the arrival gate (label 5Z): /ACKFLT GATE F1
gate_re!(PSA_ACK, r"^/ACKFLT GATE\s+", r"\s*$");
// Jazz (C1): JZA GATE DATA UPLINK / FLIGHT: 8063/22Nov26/YYC/YVR /
// PLANNED ARRIVAL GATE IS C42
re!(JAZZ_FLIGHT, r"^FLIGHT:\s*(\d{1,4})/\d{2}[A-Za-z]{3}\d{2}/([A-Z]{3})/([A-Z]{3})");
gate_re!(JAZZ_PLANNED, r"^PLANNED ARRIVAL GATE IS\s+", r"\s*$");
// Jetstar gate change (C1): JQ782/22 Nov MEL-ADL / GATE CHANGE MESSAGE -
// ARRIVAL / PREVIOUS GATE: 25 / NEW GATE: 23
re!(JETSTAR_ROUTE, r"^([A-Z0-9]{2}\d{1,4})/(\d{2}) [A-Za-z]{3} ([A-Z]{3})-([A-Z]{3})\b");
// WestJet (RA): WS 345 YXE-YYC / YYC GATE D80/OPS, or ARR GATE IS NOW C26/OPS
re!(WESTJET_ROUTE, r"^WS\s+(\d{1,4})\s+([A-Z]{3})-([A-Z]{3})\b");
gate_re!(WESTJET_GATE, r"^([A-Z]{3}) GATE\s+", r"/OPS");
gate_re!(WESTJET_ARR_GATE, r"^ARR GATE IS NOW\s+", r"/OPS");
// Shandong and others (RA): CURRENT FLIGHT: / SC8402 ARR:ZSJN / GATE:10
re!(ARR_ICAO_LINE, r"^([A-Z0-9]{2}\d{1,4}) ARR:([A-Z]{4})$");
gate_re!(COLON_GATE, r"^GATE:\s*", r"\s*$");
// LATAM (RA): GATE ASSIGNMENT / PPOS:602L (parking position) / NEXT LEG: ...
gate_re!(LATAM_PPOS, r"^PPOS:\s*", r"\s*$");
// Where a message moves on to other flights (connections, the aircraft's
// next flight); gate lines after it are not this flight's.
re!(
    STOP_LINE,
    r"^(?:CREW CONNECTING|CONNECTING GATE|NEXT FLT|NEXT FLIGHT|NEXT LEG|NEXT SECTOR|TURN FLT|CREW CONNECTIONS|PAX CONNECTIONS|AC TURN|END$)"
);
// Label RA uplinks read by [`parse`], and Finnair connection lists (other
// flights' departure gates).
re!(RA_HANDLED_HEADER, r"^(?:QUNDCULUA|QUHDQDDRP|QUHDQITOO|QUSILITOO|QUHELASAY)");

/// Read a gate assignment from the families in the module docs (label B3
/// clearances and airline gate messages on 40, 41, 42, C1, 21, 3T, 5Z and
/// RA);
/// `None` when the text is not one. On label RA the uplinks [`parse`] reads
/// are skipped, so call it first.
pub fn parse_report(label: &str, text: &str) -> Option<GateInfo> {
    // Lines as the digger splits them: CRs dropped (which is how Piedmont's
    // CR-only lines run the gate into the route line), then trimmed.
    let text = text.replace('\r', "");
    let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
    let first = lines[0];
    match label {
        "B3" => lines.iter().find_map(|l| {
            let c = PDC.captures(l)?;
            let mut g = report(GateKind::DepartureGate, "PDC", &c[3], &c[4]);
            g.flight_number = Some(c[1].to_owned());
            g.departure = Some(c[2].to_owned());
            Some(g)
        }),
        "3T" => {
            if !lines.iter().any(|l| l.starts_with("ARR GATE MSG")) {
                return None;
            }
            lines.iter().find_map(|l| {
                let c = HAWAIIAN_ARR.captures(l)?;
                Some(with_flight(report(GateKind::ArrivalGate, "HAWAIIAN_ARR_GATE", &c[2], ""), &c[1]))
            })
        }
        "21" => alaska_gate_change(&lines),
        "5Z" => {
            let c = PSA_ACK.captures(first)?;
            Some(report(GateKind::ArrivalGate, "PSA_ACK_GATE", &c[1], ""))
        }
        "C1" => lines
            .iter()
            .find_map(|l| {
                let c = JETSTAR.captures(l)?;
                Some(report(GateKind::ArrivalGate, "JETSTAR_ARRIVAL_GATE", &c[1], ""))
            })
            .or_else(|| jazz_gate(&lines))
            .or_else(|| jetstar_gate_change(&lines))
            .or_else(|| route_gate(&lines)),
        "41" | "42" => {
            if !JETBLUE_INR.is_match(first) {
                return route_gate(&lines);
            }
            lines.iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
                let c = JETBLUE_GATE.captures(l)?;
                Some(report(GateKind::ArrivalGate, "JETBLUE_INR", &c[1], ""))
            })
        }
        "40" => lines
            .iter()
            .find_map(|l| {
                let c = HAWAIIAN_AT_GATE.captures(l)?;
                Some(with_flight(report(GateKind::ArrivalGate, "HAWAIIAN_AT_GATE", &c[2], ""), &c[1]))
            })
            .or_else(|| envoy_arriving(&lines))
            .or_else(|| route_gate(&lines)),
        "RA" => {
            if RA_HANDLED_HEADER.is_match(first) {
                return None;
            }
            lines
                .iter()
                .find_map(|l| {
                    let c = FLIGHT_AIRPORT_GATE.captures(l)?;
                    Some(with_flight(report(GateKind::ArrivalGate, "FLT_AIRPORT_GATE", &c[3], &c[2]), &c[1]))
                })
                .or_else(|| frontier_gate(&lines))
                .or_else(|| westjet_gate(&lines))
                .or_else(|| arr_icao_gate(&lines))
                .or_else(|| latam_ppos(&lines))
                .or_else(|| route_gate(&lines))
        }
        _ => None,
    }
}

fn report(kind: GateKind, format: &'static str, gate: &str, arrival: &str) -> GateInfo {
    let mut g = GateInfo::new(kind, gate, arrival);
    g.format = Some(format);
    g
}

fn with_flight(mut g: GateInfo, flight: &str) -> GateInfo {
    g.flight_number = Some(flight.to_owned());
    g
}

/// `FLT 1658/22 AUS-DFW` and the first gate line after it, before the
/// message moves on to connections or the next flight. Only the first route
/// line is read.
fn route_gate(lines: &[&str]) -> Option<GateInfo> {
    let (i, c) = lines.iter().enumerate().find_map(|(i, l)| Some((i, ROUTE_LINE.captures(l)?)))?;
    let rest = &lines[i][c.get(0)?.end()..];
    let gate = INLINE_GATE.captures(rest).map(|g| g[1].to_owned()).or_else(|| {
        lines[i + 1..].iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
            LEADING_GATE.captures(l).or_else(|| NEW_ARRIVAL.captures(l)).map(|g| g[1].to_owned())
        })
    })?;
    let mut g = with_flight(report(GateKind::ArrivalGate, "FLT_ROUTE_GATE", &gate, &c[4]), &c[1]);
    g.day = c.get(2).map(|m| m.as_str().to_owned());
    g.departure = Some(c[3].to_owned());
    Some(g)
}

/// Envoy `FLT 4258 ARRIVING DCA` and the `ARRIVAL GATE` line after it.
fn envoy_arriving(lines: &[&str]) -> Option<GateInfo> {
    let (i, c) = lines.iter().enumerate().find_map(|(i, l)| Some((i, ENVOY_ARRIVING.captures(l)?)))?;
    lines[i + 1..].iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
        let g = ENVOY_GATE.captures(l)?;
        Some(report(GateKind::ArrivalGate, "ENVOY_ARRIVING", &g[1], &c[1]))
    })
}

/// Alaska `GATE CHANGE IN LAX`, then the flight and `NEW GATE:` lines.
fn alaska_gate_change(lines: &[&str]) -> Option<GateInfo> {
    let i = lines.iter().position(|l| ALASKA_CHANGE.is_match(l))?;
    let change = ALASKA_CHANGE.captures(lines[i])?;
    let airport = &change[1];
    let mut flight = None;
    for l in &lines[i + 1..] {
        if let Some(c) = ALASKA_FLIGHT.captures(l) {
            flight = Some(c[1].to_owned());
        }
        if let Some(c) = ALASKA_GATE.captures(l) {
            let mut g = report(GateKind::GateChange, "ALASKA_GATE_CHANGE", &c[1], airport);
            g.flight_number = flight;
            return Some(g);
        }
    }
    None
}

/// Jazz `JZA GATE DATA UPLINK`: the `FLIGHT:` line and `PLANNED ARRIVAL
/// GATE IS C42`.
fn jazz_gate(lines: &[&str]) -> Option<GateInfo> {
    if !lines.iter().any(|l| l.starts_with("- JZA GATE DATA UPLINK") || l.starts_with("-  JZA GATE DATA UPLINK")) {
        return None;
    }
    let mut flight = None;
    for l in lines {
        if let Some(c) = JAZZ_FLIGHT.captures(l) {
            flight = Some(c);
        }
        if let Some(c) = JAZZ_PLANNED.captures(l) {
            let mut g = report(GateKind::ArrivalGate, "JAZZ_GATE_DATA", &c[1], "");
            if let Some(f) = flight {
                g.flight_number = Some(f[1].to_owned());
                g.departure = Some(f[2].to_owned());
                g.arrival = Some(f[3].to_owned());
            }
            return Some(g);
        }
    }
    None
}

/// Jetstar `GATE CHANGE MESSAGE - ARRIVAL`: the `JQ782/22 Nov MEL-ADL` route
/// line and `NEW GATE: 23`.
fn jetstar_gate_change(lines: &[&str]) -> Option<GateInfo> {
    if !lines.iter().any(|l| l.starts_with("GATE CHANGE MESSAGE - ARRIVAL")) {
        return None;
    }
    let mut route = None;
    for l in lines {
        if let Some(c) = JETSTAR_ROUTE.captures(l) {
            route = Some(c);
        }
        if let Some(c) = ALASKA_GATE.captures(l) {
            let mut g = report(GateKind::GateChange, "JETSTAR_GATE_CHANGE", &c[1], "");
            if let Some(r) = route {
                g.flight_number = Some(r[1].to_owned());
                g.day = Some(r[2].to_owned());
                g.departure = Some(r[3].to_owned());
                g.arrival = Some(r[4].to_owned());
            }
            return Some(g);
        }
    }
    None
}

/// WestJet `WS 345 YXE-YYC`, then `YYC GATE D80/OPS` (the gate's airport) or
/// `ARR GATE IS NOW C26/OPS` (at the route's destination).
fn westjet_gate(lines: &[&str]) -> Option<GateInfo> {
    let i = lines.iter().position(|l| WESTJET_ROUTE.is_match(l))?;
    let route = WESTJET_ROUTE.captures(lines[i])?;
    lines[i + 1..].iter().find_map(|l| {
        let (gate, airport) = match WESTJET_GATE.captures(l) {
            Some(c) => (c[2].to_owned(), c[1].to_owned()),
            None => (WESTJET_ARR_GATE.captures(l)?[1].to_owned(), route[3].to_owned()),
        };
        let mut g = with_flight(report(GateKind::ArrivalGate, "WESTJET_OPS_GATE", &gate, &airport), &route[1]);
        g.departure = Some(route[2].to_owned());
        Some(g)
    })
}

/// `SC8402 ARR:ZSJN` and the `GATE:10` line after it.
fn arr_icao_gate(lines: &[&str]) -> Option<GateInfo> {
    let (i, c) = lines.iter().enumerate().find_map(|(i, l)| Some((i, ARR_ICAO_LINE.captures(l)?)))?;
    lines[i + 1..].iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
        let g = COLON_GATE.captures(l)?;
        Some(with_flight(report(GateKind::ArrivalGate, "ARR_ICAO_GATE", &g[1], &c[2]), &c[1]))
    })
}

/// LATAM `GATE ASSIGNMENT` with a `PPOS:602L` parking position (`PPOS:N/A`
/// when there is none).
fn latam_ppos(lines: &[&str]) -> Option<GateInfo> {
    if !lines.iter().any(|l| l.starts_with("GATE ASSIGNMENT")) {
        return None;
    }
    lines.iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
        let c = LATAM_PPOS.captures(l)?;
        Some(report(GateKind::ArrivalGate, "LATAM_GATE_ASSIGNMENT", &c[1], ""))
    })
}

/// Frontier `FROM PHL OPS` with a `GATE  E8` line; only when the uplink also
/// says whether the gate is available at arrival.
fn frontier_gate(lines: &[&str]) -> Option<GateInfo> {
    let airport = lines.iter().find_map(|l| FRONTIER_FROM.captures(l))?;
    if !lines.iter().any(|l| l.starts_with("GATE AVAIL AT ARRIVAL")) {
        return None;
    }
    lines.iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
        let c = FRONTIER_GATE.captures(l)?;
        Some(report(GateKind::ArrivalGate, "FRONTIER_OPS", &c[1], &airport[1]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Production RA uplinks, shortened, names removed.
    const UA_PLAIN: &str = "QUNDCULUA~1GATE ASSIGN\r\nUA1720/09 KMSP KDEN   \r\nSENT: 10:41:16Z \r\nGATE B47   OPEN\r\nRAMP FREQ  130.950\r\nOPS FREQ   \r\nEON        1208Z\r\nGROUND PWR AVAIL -YES-\r\n\r\nPLANNED GROUND TIME \r\nIS 01:36\r\nNEXT FLT UA1127/09\r\nKDEN KSN";
    const UA_TABS: &str = "QUNDCULUA~1GATE ASSIGN\n\tUA0942/21 KIAD LPPT\n\tSENT: 09:14:42Z\n\tGATE 43    OPEN\n\tRAMP FREQ  \n\tOPS FREQ   131.575\n\tEON        1003Z\n\tGROUND PWR AVAIL -NO-\n\t\n\tPLANNED GROUND TIME\n\tIS 02:13\n\tNEXT FLT UA0943/22\n\tLPPT KIAD ETD 1220Z";
    const UA_AVAIL: &str = "QUNDCULUA~1GATE ASSIGN\n\tUA1170/21 KLAX PHNL\n\tSENT: 05:36:45Z\n\tGATE G5    AVAIL 06:30\n\tRAMP FREQ  121.9\n\tOPS FREQ   130.300\n\tEON        0639Z\n\tGROUND PWR AVAIL -YES-\n\t\n\tPLANNED GROUND TIME\n\tIS 02:05\n\tNEXT FLT UA1169/21\n\tPHNL KLAX ETD 0850Z";
    const UA_CHANGE: &str = "QUNDCULUA~1GATE CHANGE\r\nUA0214/21 KSTL KSFO\r\nSENT: 05:22:32Z\r\n/////////////////////\r\n**** CREW ALERT ****\r\nGATE ASSIGN\r\n***ACCOMPLISH\r\nTERMINATION CHECKLIST***\r\nGATE D7    OPEN\r\nRAMP FREQ  119.225\r\nOPS FREQ   129.100\r\nEON        0521Z\r\nGROUND PWR AVAIL -YES-\r\n*ALERT*\r\nREVIEW SFO OPS ALERTS,\r\n\r\nPLANNED GROUND TIME\r\nIS 09:58\r\nNEXT FLT UA0494/22\r\nKSFO KSEA ETD 1525Z\r";
    const RPA: &str = "QUHDQDDRP~1ARRIVAL GATE\r\nINFORMATION\r\nRPA N401YX YX4450 ORD MSN ETA = 0542 GATE = 16\r\nAIRCRAFT TURNS TO - YX,NEX ,NE-,NE ETD-\r\n\r\nNOTE - NEXT FLT SUBJECT TO CHANGE\r\n";
    const RPA_L27: &str = "QUHDQDDRP~1ARRIVAL GATE\r\nINFORMATION\r\nRPA N116HQ YX4711 DSM ORD ETA = 0236 GATE = L27\r\nAIRCRAFT TURNS TO - YX4529 ORD-MCI ETD-0440\r\nNOTE - NEXT FLT SUBJECT TO CHANGE\r\n";
    const SKW: &str = "QUHDQITOO.1ARR GATE INFO   \r\nBFL ARRIVAL GATE 4\r\nNEXT FLIGHT INFORMATION-\r\n** ALL TIMES LOCAL **\r\nAC N939SW\r\n N-A 4634 BFL-SFO 220700\r\nCA X\r\n N-A 4634 BFL-SFO 220700\r\n\x07\r\n\r\n";
    const SKW_SIL: &str = "QUSILITOO.1ARR GATE INFO   \r\nFAT ARRIVAL GATE 7\r\nNEXT FLIGHT INFORMATION-\r\n** ALL TIMES LOCAL **\r\nAC N442SW\r\n N-A 5417 FAT-SFO 220817\r\nCA X\r\n";
    const SKW_SAME_LINE: &str = "QUHDQITOO.1ARR GATE INFO   LAX ARRIVAL GATE 26B\r\n*APU ON AFTER LANDING*\r\nNEXT FLIGHT INFORMATION-\r\n** ALL TIMES LOCAL **\r\nAC N305SY\r\n 26B 3856 LAX-SJC 220735\r\n";
    const SKW_NO_GATE: &str = "QUHDQITOO.1ARR GATE INFO   DFW ARRIVAL GATE \r\nNEXT FLIGHT INFORMATION-\r\n** ALL TIMES LOCAL **\r\nAC N707SK\r\n N-A 6313 DFW-BPT 220005\r\n";
    const FINNAIR_CONNECT: &str = "QUHELASAY~1\r\nCURRENT FLIGHT\r\nFLIGHT  ESTIMATED ARRIVAL TIME\r\nAY1014   EIBT  UTC  \r\n\r\nCONNECTION INFO\r\nFLIGHT DATE     TIME  TO  GATE \r\nAY0957 20261122 09:35 CPH 31\r\n";
    const FINNAIR_PAX: &str = "QUHELASAY~1\r\nCUSTOMER PAX AND BAG DETAILS FOR WEIGHT AND BALANCE             \r\nFLIGHT INFO  AT7       REG OH-ATR   72Y                     PW  \r\nAY1073   22NOV  HEL STD1210             BOARD 1150 GATE 9   AO  \r\n";

    #[test]
    fn united_gate_assign_plain() {
        let g = parse(UA_PLAIN).unwrap();
        assert_eq!(g.kind, GateKind::GateAssign);
        assert_eq!(g.flight_number.as_deref(), Some("UA1720"));
        assert_eq!(g.day.as_deref(), Some("09"));
        assert_eq!(g.departure.as_deref(), Some("KMSP"));
        assert_eq!(g.arrival.as_deref(), Some("KDEN"));
        assert_eq!(g.sent_time.as_deref(), Some("10:41:16"));
        assert_eq!(g.gate, "B47");
        assert_eq!(g.gate_status.as_deref(), Some("OPEN"));
        assert_eq!(g.gate_available_time, None);
        assert_eq!(g.eon_time.as_deref(), Some("1208"));
        assert_eq!(g.ramp_frequency.as_deref(), Some("130.950"));
        assert_eq!(g.ops_frequency, None);
        assert_eq!(g.ground_power, Some(true));
        assert_eq!(g.planned_ground_time.as_deref(), Some("01:36"));
    }

    #[test]
    fn united_gate_assign_tab_indented() {
        let g = parse(UA_TABS).unwrap();
        assert_eq!(g.flight_number.as_deref(), Some("UA0942"));
        assert_eq!(g.departure.as_deref(), Some("KIAD"));
        assert_eq!(g.arrival.as_deref(), Some("LPPT"));
        assert_eq!(g.sent_time.as_deref(), Some("09:14:42"));
        assert_eq!(g.gate, "43");
        assert_eq!(g.gate_status.as_deref(), Some("OPEN"));
        assert_eq!(g.ramp_frequency, None);
        assert_eq!(g.ops_frequency.as_deref(), Some("131.575"));
        assert_eq!(g.eon_time.as_deref(), Some("1003"));
        assert_eq!(g.ground_power, Some(false));
        assert_eq!(g.planned_ground_time.as_deref(), Some("02:13"));
    }

    #[test]
    fn united_gate_available_later() {
        let g = parse(UA_AVAIL).unwrap();
        assert_eq!(g.arrival.as_deref(), Some("PHNL"));
        assert_eq!(g.gate, "G5");
        assert_eq!(g.gate_status.as_deref(), Some("AVAIL"));
        assert_eq!(g.gate_available_time.as_deref(), Some("06:30"));
    }

    #[test]
    fn united_gate_change_skips_crew_alert_line() {
        let g = parse(UA_CHANGE).unwrap();
        assert_eq!(g.kind, GateKind::GateChange);
        assert_eq!(g.departure.as_deref(), Some("KSTL"));
        assert_eq!(g.arrival.as_deref(), Some("KSFO"));
        assert_eq!(g.gate, "D7");
        assert_eq!(g.gate_status.as_deref(), Some("OPEN"));
        assert_eq!(g.planned_ground_time.as_deref(), Some("09:58"));
    }

    #[test]
    fn united_last_gate_wins() {
        let t = "QUNDCULUA~1GATE CHANGE\r\nUA0214/21 KSTL KSFO\r\nSENT: 05:22:32Z\r\nGATE D5    OPEN\r\nGATE ASSIGN\r\nGATE D7    OPEN\r\n";
        assert_eq!(parse(t).unwrap().gate, "D7");
    }

    #[test]
    fn united_without_gate_is_not_read() {
        let t = "QUNDCULUA~1GATE ASSIGN\r\nUA0214/21 KSTL KSFO\r\nSENT: 05:22:32Z\r\nGATE ASSIGN\r\n";
        assert!(parse(t).is_none());
    }

    #[test]
    fn republic_arrival_gate() {
        let g = parse(RPA).unwrap();
        assert_eq!(g.kind, GateKind::ArrivalGate);
        assert_eq!(g.flight_number.as_deref(), Some("YX4450"));
        assert_eq!(g.departure.as_deref(), Some("ORD"));
        assert_eq!(g.arrival.as_deref(), Some("MSN"));
        assert_eq!(g.eta.as_deref(), Some("0542"));
        assert_eq!(g.gate, "16");
        let g = parse(RPA_L27).unwrap();
        assert_eq!((g.arrival.as_deref(), g.gate.as_str()), (Some("ORD"), "L27"));
    }

    #[test]
    fn skywest_arrival_gate() {
        for (text, airport, gate) in
            [(SKW, "BFL", "4"), (SKW_SIL, "FAT", "7"), (SKW_SAME_LINE, "LAX", "26B")]
        {
            let g = parse(text).unwrap_or_else(|| panic!("no gate in {text:?}"));
            assert_eq!(g.kind, GateKind::ArrivalGate);
            assert_eq!((g.arrival.as_deref(), g.gate.as_str()), (Some(airport), gate));
            // The next-flight block is another flight's.
            assert_eq!(g.flight_number, None);
            assert_eq!(g.departure, None);
        }
        assert!(parse(SKW_NO_GATE).is_none());
    }

    #[test]
    fn finnair_connection_gates_are_not_arrival_gates() {
        assert!(parse(FINNAIR_CONNECT).is_none());
        assert!(parse(FINNAIR_PAX).is_none());
    }

    #[test]
    fn other_text_is_not_a_gate() {
        assert!(parse("QUATLASAV~1.\r\nBUENOS DIAS\r\nGATE 18 EN SAL. SLDS.\r\n").is_none());
        assert!(parse("QUNDCULUA~1CREW LIST\r\nUA0942/21 KIAD LPPT\r\n").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn summary_and_json() {
        assert_eq!(parse(UA_AVAIL).unwrap().summary(), "GATE ASSIGN UA1170 KLAX->PHNL gate G5 AVAIL 06:30");
        assert_eq!(parse(SKW).unwrap().summary(), "ARRIVAL GATE BFL gate 4");
        let j = serde_json::to_value(parse(RPA).unwrap()).unwrap();
        assert_eq!(j["kind"], "arrival_gate");
        assert_eq!(j["arrival"], "MSN");
        assert_eq!(j["gate"], "16");
        assert!(j.get("gate_status").is_none());
    }

    #[test]
    fn united_next_flight_gate_is_not_read() {
        let t = "QUNDCULUA~1GATE ASSIGN\r\nUA1720/09 KMSP KDEN\r\nSENT: 10:41:16Z\r\nGATE B47   OPEN\r\nNEXT FLT UA1127/09\r\nKDEN KSNA\r\nGATE B12   OPEN\r\n";
        assert_eq!(parse(t).unwrap().gate, "B47");
    }

    // Production texts from 2026-11-22 (shortened, names removed), the
    // event digger's `gate_reports_test.go` cases.
    #[test]
    fn report_formats() {
        use GateKind::*;
        // (name, label, text, kind, format, departure, arrival, gate, flight)
        #[allow(clippy::type_complexity)]
        let cases: &[(&str, &str, &str, GateKind, &str, Option<&str>, Option<&str>, &str, Option<&str>)] = &[
            ("PDC Jazz", "B3", "/CYWG.DC1/RCD 024\r\nJZA110-CYWG-GATE 12-CYOW\r\nATIS O\r\n-TYP/CRJ9912E", DepartureGate, "PDC", Some("CYWG"), Some("CYOW"), "12", Some("JZA110")),
            ("PDC British Airways", "B3", "/EGKK./RCD 046\r\nBAW22G -EGKK-GATE 27-MDPC\r\nATIS B\r\n-TYP/B772", DepartureGate, "PDC", Some("EGKK"), Some("MDPC"), "27", Some("BAW22G")),
            ("American availability", "40", "FLT 1658/22 AUS-DFW\r\nGATE D21 AVAILABILITY PLN ETA 0721\r\nGATE OCCUPIED ACFT 3LM SKD 0700\r\nNEXT FLT 0602 ACFT 3JF SKD 0900 GRR\r\n\r\nAUTOMATED FLIGHT INFORMATION", ArrivalGate, "FLT_ROUTE_GATE", Some("AUS"), Some("DFW"), "D21", Some("1658")),
            ("American services", "40", "FLT 1658/22 AUS-DFW\r\n\r\nGATE D21 PCA Y ELEC Y DGS Y  \r\n                         \r\nCREW CONNECTING GATE INFO\r\nCREW SEAT FLT DEST GATE TIME\r\nC F       0389MEX  A14  0837\r\nEND", ArrivalGate, "FLT_ROUTE_GATE", Some("AUS"), Some("DFW"), "D21", Some("1658")),
            ("American new arrival gate", "40", "FLT 2857 DFW-DCA \r\nNEW ARRIVAL GATE NBR IS  C33 \r\n ", ArrivalGate, "FLT_ROUTE_GATE", Some("DFW"), Some("DCA"), "C33", Some("2857")),
            ("American on label C1", "C1", ".TULYVAA 220939\r\nAGM\r\nAN N758AN/FI AA0174/GL LGW\r\n- FLT 0174 RDU-LHR \r\nNEW ARRIVAL GATE NBR IS  320 \r\n \r\n", ArrivalGate, "FLT_ROUTE_GATE", Some("RDU"), Some("LHR"), "320", Some("0174")),
            ("American on label RA, tab indented", "RA", "QUTULYVAA010170FTM\n\tFLT 0170/22 HND-LAX\n\tGATE 153 AVAILABILITY PLN ETA 0534\n\tGATE OPEN\n\tNEXT FLT 0027 ACFT 8LM SKD 0950 HND", ArrivalGate, "FLT_ROUTE_GATE", Some("HND"), Some("LAX"), "153", Some("0170")),
            ("Envoy gate run into PCA", "40", "FLT 4258/22 HPN-DCA  BAG 9   \r\n \r\nGATE D43APCA Y ELEC Y DGS Y  \r\n                         \r\nCREW CONNECTING GATE INFO\r\nCREW SEAT FLT DEST GATE TIME\r\nC F 1 2   4252VPS  D35  1012\r\nEND", ArrivalGate, "FLT_ROUTE_GATE", Some("HPN"), Some("DCA"), "D43A", Some("4258")),
            ("Envoy arriving", "40", "FLT 4258   ARRIVING DCA \r\nARRIVAL GATE D43A  BAG CLAIM AREA 9   \r\nCONNECTING GATE INFORMATION\r\nDESTINATION      DESTINATION      GATE  \r\nDALLAS/FT WORTH  ................ C32 AA\r\nEND", ArrivalGate, "ENVOY_ARRIVING", None, Some("DCA"), "D43A", None),
            ("PSA", "41", "FLT 5106 DTW-PHL\r\nGATE F18 ETA 0743L \r\nTURN FLT 5636 DTW DEP 0835L\r\nFREQ=130.100 PWR=Y AIR=Y DOC=N\r\n", ArrivalGate, "FLT_ROUTE_GATE", Some("DTW"), Some("PHL"), "F18", Some("5106")),
            ("GoJet", "RA", "QUSTLAVG7~1GATE INFO\r\nFLT 4542 PWM-IAD\r\nGATE C22\r\n", ArrivalGate, "FLT_ROUTE_GATE", Some("PWM"), Some("IAD"), "C22", Some("4542")),
            ("Piedmont, CR line ends", "RA", "QUSBYDDPT~1FLT 5966 SYR-PHL\rGATE F37 ETA 707L\rTURN\r\nFLT 5799 ORF DEP 840L MOGT\r\n", ArrivalGate, "FLT_ROUTE_GATE", Some("SYR"), Some("PHL"), "F37", Some("5966")),
            ("Air Wisconsin", "RA", "FLT 4940 IAD GATE A4C\r\n", ArrivalGate, "FLT_AIRPORT_GATE", None, Some("IAD"), "A4C", Some("4940")),
            ("Frontier", "RA", "QUANPOCF9~1\r\nFROM PHL OPS\r\n\r\nETA 1233\r\nWHEELCHAIRS 2\r\nGATE  E8\r\nGATE AVAIL AT ARRIVAL  Y\r\nGND PWR  Y\r\nNEXT FLIGHT  2507\r\nNEXT DEST  MCO\r\n", ArrivalGate, "FRONTIER_OPS", None, Some("PHL"), "E8", None),
            ("Hawaiian arrival gate", "3T", "ARR GATE MSG   \r\n804 ARR GATE 69A\r\nGND PWR AVL - YES\r\nPARKING LINE - 69A\r\nNXT FLT 0801\r\nDST HNL\r\n", ArrivalGate, "HAWAIIAN_ARR_GATE", None, None, "69A", Some("804")),
            ("Hawaiian at gate", "40", "HA0805 AT GATE A6\r\nSHUT DOWN APU\r\nONCE PWR AVAIL.", ArrivalGate, "HAWAIIAN_AT_GATE", None, None, "A6", Some("HA0805")),
            ("JetBlue in range", "41", "INR,221156,  GATE:514\r\nCAROUSEL:3\r\nAC TURN: PUJ ON TIME   0959L\r\nCREW CONNECTIONS\r\n X\r\n   -BUF F:0802 ON TIME  1029L G:516\r\n", ArrivalGate, "JETBLUE_INR", None, None, "514", None),
            ("JetBlue gate on its own line", "42", "INR,221235\r\n\r\nGATE:A1\r\nCAROUSEL: N/A\r\nAC TURN: BOS ON TIME   0940L\r\n", ArrivalGate, "JETBLUE_INR", None, None, "A1", None),
            ("Jetstar", "C1", ".MELOJJQ 221217 \r\nAGM\r\nAN VH-VGO/MA 727A\r\n-  /\r\nACK RCPT OF TAC MSG.\r\nARRIVAL GATE THIS FLT:43\r\nARRIVAL REMARKS      :N/A\r\n", ArrivalGate, "JETSTAR_ARRIVAL_GATE", None, None, "43", None),
            ("PSA crew acknowledgement", "5Z", "/ACKFLT GATE F1", ArrivalGate, "PSA_ACK_GATE", None, None, "F1", None),
            ("Jazz gate data", "C1", ".YHZITQK 221242\r\n\r\nAGM\r\n\r\nAN CGJUZ/MA 429I\r\n\r\n-  JZA GATE DATA UPLINK\r\n\r\n----\r\n\r\nFLIGHT: 8063/22Nov26/YYC/YVR\r\n\r\nPLANNED ARRIVAL GATE IS C42\r\n", ArrivalGate, "JAZZ_GATE_DATA", Some("YYC"), Some("YVR"), "C42", Some("8063")),
            ("Jetstar gate change", "C1", ".MELOJJQ 221152 \r\n\r\nAGM\r\n\r\nAN VH-VGN/MA 708A\r\n\r\n-  /\r\n\r\nVH-VGN   \r\n\r\nJQ782/22 Nov MEL-ADL\r\n\r\nGATE CHANGE MESSAGE - ARRIVAL\r\n\r\nPREVIOUS GATE: 25\r\n\r\nNEW GATE: 23\r\n\r\nGPU AVAILABLE", GateChange, "JETSTAR_GATE_CHANGE", Some("MEL"), Some("ADL"), "23", Some("JQ782")),
            ("WestJet ops gate", "RA", "WS 345 YXE-YYC\r\n\r\nYYC GATE D80/OPS", ArrivalGate, "WESTJET_OPS_GATE", Some("YXE"), Some("YYC"), "D80", Some("345")),
            ("WestJet gate change", "RA", "///GATECHANGE///\r\n\r\nWS 736 YVR-YYZ\r\n\r\nARR GATE IS NOW C26/OPS", ArrivalGate, "WESTJET_OPS_GATE", Some("YVR"), Some("YYZ"), "C26", Some("736")),
            ("Shandong current flight", "RA", "QUTAOUOSC~10000\r\n\tCURRENT FLIGHT:\r\n\tSC8402 ARR:ZSJN\r\n\tGATE:10\r\n\tWEATHER:\r\n", ArrivalGate, "ARR_ICAO_GATE", None, Some("ZSJN"), "10", Some("SC8402")),
            ("LATAM parking position", "RA", "QUSCLASLA~1\r\n\r\nGATE ASSIGNMENT\r\n\r\n\r\n\r\nPPOS:602L\r\n\r\nBAG BELT:\r\n\r\nNEXT LEG: LA8142 GRU-AEP 22NOV 14:05Z", ArrivalGate, "LATAM_GATE_ASSIGNMENT", None, None, "602L", None),
            ("Alaska gate change", "21", ",,1\r\nGATE UPDATE\r\nGATE CHANGE IN LAX\r\nAS1636\r\nNEW GATE: 210A\r\n\r\n", GateChange, "ALASKA_GATE_CHANGE", None, Some("LAX"), "210A", Some("AS1636")),
        ];
        for &(name, label, text, kind, format, departure, arrival, gate, flight) in cases {
            let g = parse_report(label, text).unwrap_or_else(|| panic!("{name}: no gate"));
            assert_eq!(
                (g.kind, g.format, g.departure.as_deref(), g.arrival.as_deref(), g.gate.as_str(), g.flight_number.as_deref()),
                (kind, Some(format), departure, arrival, gate, flight),
                "{name}"
            );
        }
    }

    #[test]
    fn report_day_of_flight() {
        let g = parse_report("40", "FLT 1658/22 AUS-DFW\r\nGATE D21 AVAILABILITY PLN ETA 0721\r\n").unwrap();
        assert_eq!(g.day.as_deref(), Some("22"));
        let g = parse_report("41", "FLT 5106 DTW-PHL\r\nGATE F18 ETA 0743L \r\n").unwrap();
        assert_eq!(g.day, None);
    }

    #[test]
    fn report_ignores() {
        let cases = [
            // Read by `parse`.
            ("United gate assign", "RA", UA_TABS),
            ("SkyWest", "RA", SKW),
            ("Finnair connections", "RA", FINNAIR_CONNECT),
            // No gate for this flight, only for the connections and the next flight.
            ("gate status only", "40", "FLT 0278/21 PHL-EDI\r\nGATE STATUS NOT AVAIL\r\nNEXT FLT 0602 ACFT 3JF SKD 0900 GRR\r\nGATE B12\r\n"),
            ("connections only", "40", "FLT 4258/22 HPN-DCA\r\nCREW CONNECTING GATE INFO\r\nCREW SEAT FLT DEST GATE TIME\r\nGATE D35\r\n"),
            ("PDC without a gate", "B3", "/LSZH.DC1/RCD 011\r\nSWR72-LSZH-GATE -KIAD\r\n"),
            ("gate request downlink", "5Z", "/C3 GATE REQ       / KAUS KIAH\r\n"),
            ("JetBlue crew connection gate", "41", "INR,221156\r\nCAROUSEL:3\r\nCREW CONNECTIONS\r\n X\r\n   -BUF F:0802 ON TIME  1029L G:516\r\n"),
            ("route family on another label", "H1", "FLT 1658/22 AUS-DFW\r\nGATE D21\r\n"),
            ("PSA acknowledgement without a gate", "5Z", "/ACKFLT GATE E"),
            ("LATAM without a position", "RA", "QUSCLASLA~1\r\n\tGATE ASSIGNMENT\r\n\t\r\n\tPPOS:N/A\r\n\tBAG BELT:\r\n\tNEXT LEG: LA3091 FLN-CGH 22NOV 11:35Z"),
            ("empty", "40", ""),
        ];
        for (name, label, text) in cases {
            assert!(parse_report(label, text).is_none(), "{name}: {:?}", parse_report(label, text));
        }
    }

    #[test]
    fn report_summary_and_json() {
        let pdc = parse_report("B3", "/CYWG.DC1/RCD 024\r\nJZA110-CYWG-GATE 12-CYOW\r\n").unwrap();
        assert_eq!(pdc.summary(), "DEPARTURE GATE JZA110 CYWG->CYOW gate 12");
        let j = serde_json::to_value(&pdc).unwrap();
        assert_eq!(j["kind"], "departure_gate");
        assert_eq!(j["format"], "PDC");
        assert_eq!(j["departure"], "CYWG");
        let jetstar = parse_report("C1", "ARRIVAL GATE THIS FLT:43\r\n").unwrap();
        assert_eq!(jetstar.summary(), "ARRIVAL GATE gate 43");
        assert!(serde_json::to_value(&jetstar).unwrap().get("arrival").is_none());
        // The RA uplinks keep their JSON shape (no `format`).
        assert!(serde_json::to_value(parse(SKW).unwrap()).unwrap().get("format").is_none());
    }
}
