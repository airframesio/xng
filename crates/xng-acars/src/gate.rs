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
//! | American, Envoy, PSA, GoJet, Piedmont, Republic | 40, 41, C1, RA | `FLT 1658/22 AUS-DFW` / `GATE D21 ...`, `NEW ARRIVAL GATE NBR IS C33`, `FLT YX4704 RDU-LGA` |
//! | Envoy arriving | 40 | `FLT 4258 ARRIVING DCA` / `ARRIVAL GATE D43A` |
//! | Air Wisconsin | RA | `FLT 4940 IAD GATE A4C` |
//! | Frontier | RA | `FROM PHL OPS` ... `GATE  E8` / `GATE AVAIL AT ARRIVAL  Y`; ops free text `QUANPOCF9` ... `GATE D36` |
//! | Hawaiian | 3T, 40 | `804 ARR GATE 69A`, `HA0805 AT GATE A6` |
//! | JetBlue in range | 41, 42 | `INR,221156,  GATE:514` |
//! | Jetstar | C1 | `ARRIVAL GATE THIS FLT:43` |
//! | Alaska gate change | 21 | `GATE CHANGE IN LAX` / `NEW GATE: 210A` |
//! | PSA crew acknowledgement | 5Z | `/ACKFLT GATE F1` |
//! | Jazz gate data | C1 | `- JZA GATE DATA UPLINK` ... `FLIGHT: 8063/22Nov26/YYC/YVR` / `PLANNED ARRIVAL GATE IS C42` |
//! | Jetstar gate change | C1 | `JQ782/22 Nov MEL-ADL` / `GATE CHANGE MESSAGE - ARRIVAL` / `NEW GATE: 23` |
//! | WestJet | RA, 41, 42 | `WS 345 YXE-YYC` / `YYC GATE D80/OPS`, `ARR GATE IS NOW C26/OPS`, Encore `FTX01,WS 3564 YVR-YLW` |
//! | Shandong and others | RA | `SC8402 ARR:ZSJN` / `GATE:10` |
//! | LATAM | RA | `GATE ASSIGNMENT` / `PPOS:602L` |
//! | Delta crew P/F | RA, 25, HX | `CREW P/F` ... `ARR ATL GTE E15` |
//! | BA PAD11 arrival stand | 10, 11 | `:PAD11` / `ARRIVAL- GTNED BA1447` / `STAND 503  ETA 1357` |
//! | Virgin Australia bay | 3M | `VA1007, BAY: 45A`, `VA1007, ARR BAY: 45A` |
//! | Turkish parking position | RA | `- EXPECTED PARKING POSITION: F8R` |
//! | Finnair arriving stand | RA, 3E | `AY1074 22NOV26` / `ARRIVING STAND 131 ELDT 12:53` |
//! | Finnair boarding gate (departure gate) | RA | `AY230    19NOV  TKU STD0600  BOARD 0540 GATE 2` |
//! | Parking assigned | RA | `PARKING PREASSIGNED:138`, `PARKING ASSIGNED:138` |
//! | Jazz flight data (both gates) | C1 | `DEP YVR/06:20Z/GTE C40` / `ARR YYC/07:48Z/GTE A17` |
//! | Qatar arrival stand | RA | `QR899 BNE - DOH ARR STAND - C6` |
//! | Arrival lines | RA | `-PKG T21`, `ESTIMATED PARK IS G09`, `PARK BAY: 21`, `ARR GATE IS 17R`, `YOUR GATE IS 9`, `PLANNING GATE B37`, `ARR GATE B44 ASSIGNED` |
//! | Qantas bay | RA | `GDAY QF0011 BAY 221` |
//! | Avianca | RA | `GATE J-12 MIA` (gate J12) |
//! | Airport arrival gate line | RA | `EAU ARRIVAL GATE 1` |
//! | Alaska ETA acknowledgement | 21, RA | `FROM:LAX OPS` / `AS1831` / `ARRIVAL GATE:65B` |
//! | SAS passenger info | C1 | `ATT SK0932  ETA: 1414  GATE: C33` |
//! | Swiss / Edelweiss / Helvetic | 32, C1 | `ARR GATE A52`, else `ARR POS D12` |
//! | Air France arrival parking | C1 | `ARRIVAL PKG: K12 PKG TYPE: C` |
//! | KLM connection info | 32 | `P64AMSKWKL170542` ... `KL 1750` / `GATE  A04  POS  A62` (at AMS) |
//! | Korean Air spot | 33 | `SPOT NO : 606` |
//! | Aer Lingus stand | 48 | `STAND INFO = 400C` |
//! | Avianca dispatch | RA | `QUATLASAV` ... `GATE 13 EN SAL.`, `E7 PARK EN BOG` (until `FROM:`) |
//! | SkyWest AA in-range | RA | `IN-RANGE FROM AA` / `GATE: 1D` (only with an `IN-RANGE` line) |
//! | Allegiant in-range | RA | `--GATE ASSIGNMENT  PVU GATE 2 ASSIGNED` |
//! | Air Nostrum | RA | `PKN:330   GATE:K88   BELT:19` |
//! | Volaris | RA | `A SU LLEGADA GATE J16` |
//! | ITA connecting gates | RA | `ZRH/FCO ETA 1220` / `GATE A32 BAG CLAIM 23` (at FCO) |
//! | Stand number (China Southern and others) | RA | `STAND NO: 136` |
//! | China Eastern park bay | RA | `HI SIR, MU9062 SWA/HFE, PARK BAY 6.` (at HFE) |
//! | FedEx | RA | `EXPECT GATE: 2(SBD)` |
//! | Arrival gate colon | RA | `AIRSIDE ARRIVAL GATE IS: B12`, Horizon `ARRIVAL GATE:B9` |
//! | Mesa header gate | RA | `QUPHXACYV.1ARRIVAL GATE A1B` |
//!
//! A gate must contain a digit, so `GATE OPEN` or `GATE STATUS NOT AVAIL`
//! never match. Reading stops where a message moves on to other flights
//! (crew/pax connections, `NEXT FLT`, `NEXT LEG`, `NEXT SECTOR`, `NXM`,
//! `- NEXT FLIGHT`, `TURN FLT`, `AC TURN`, `END`, and for BA PAD11
//! `ON STAND-` and `CREW DEPT`). Stands, bays and parking positions are
//! reported as gates. A bare CR ends a line, as a CRLF does. The label RA
//! uplinks read by [`parse`] are left to it, Finnair connection lists are
//! never read (only Finnair's arriving stand and boarding gate); gate
//! requests (5Z `/C3 GATE REQ`) carry no gate and are not read. Also not
//! read: the Jazz `NEXT FLIGHT INFORMATION` gate (the next flight's), Air
//! Canada 4C `-GTE C001  C47` (meaning unclear) and crew free text.

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
    /// Gate the flight left from, when an arrival gate report also names it
    /// (Jazz flight data `DEP YVR/06:20Z/GTE C40`); it is at `departure`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure_gate: Option<String>,
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
            departure_gate: None,
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
        if let Some(d) = &self.departure_gate {
            s.push_str(&format!(" dep gate {d}"));
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

/// Lines as the digger's `raLines` splits them: CRLF and bare CR (Piedmont,
/// Air Europa) both end a line, then each line is trimmed.
fn split_lines(text: &str) -> Vec<&str> {
    text.split("\r\n").flat_map(|l| l.split(['\r', '\n'])).map(str::trim).collect()
}

fn parse_united(text: &str) -> Option<GateInfo> {
    let lines = split_lines(text);
    let mut lines = lines.into_iter();
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
// The flight can carry an airline prefix (Republic `FLT YX4704 RDU-LGA`).
re!(ROUTE_LINE, r"^(?:-\s*|[A-Z0-9]{9}[~.]\d)?FLT\s+(?:[A-Z]{2}|[A-Z]\d|\d[A-Z])?(\d{1,4})(?:/(\d{2}))?\s+([A-Z]{3})-([A-Z]{3})");
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
// WestJet Encore (41) starts the route with `FTX01,`: FTX01,WS 3564 YVR-YLW
re!(WESTJET_ROUTE, r"^(?:FTX\d{2},)?WS\s+(\d{1,4})\s+([A-Z]{3})-([A-Z]{3})\b");
gate_re!(WESTJET_GATE, r"^([A-Z]{3}) GATE\s+", r"/OPS");
gate_re!(WESTJET_ARR_GATE, r"^ARR GATE IS NOW\s+", r"/OPS");
// Shandong and others (RA): CURRENT FLIGHT: / SC8402 ARR:ZSJN / GATE:10
re!(ARR_ICAO_LINE, r"^([A-Z0-9]{2}\d{1,4}) ARR:([A-Z]{4})$");
gate_re!(COLON_GATE, r"^GATE:\s*", r"\s*$");
// LATAM (RA): GATE ASSIGNMENT / PPOS:602L (parking position) / NEXT LEG: ...
gate_re!(LATAM_PPOS, r"^PPOS:\s*", r"\s*$");
// Delta crew P/F (RA, 25): ARR ATL GTE E15
gate_re!(DELTA_ARR, r"^ARR ([A-Z]{3}) GTE\s+", r"\s*$");
// Virgin Australia (3M): VA1007, BAY: 45A or VA1007, ARR BAY: 45A
gate_re!(VIRGIN_BAY, r"^(VA\d{1,4}), (?:ARR )?BAY:\s*", r"\s*$");
// Turkish (RA): - EXPECTED PARKING POSITION: F8R
gate_re!(TURKISH_PARK, r"^- EXPECTED PARKING POSITION:\s*", r"\s*$");
// Finnair (RA): AY1074 22NOV26 / ARRIVING STAND 131 ELDT 12:53
gate_re!(FINNAIR_STAND, r"^ARRIVING STAND\s+", r"(?:\s|$)");
re!(FINNAIR_FLIGHT, r"^(AY\d{1,4})\s+\d{2}[A-Z]{3}\d{2}$");
// China Southern and others (RA): PARKING PREASSIGNED:138 (NA when none)
gate_re!(PARKING_ASSIGNED, r"^PARKING (?:PRE)?ASSIGNED:\s*", r"\s*$");
// BA PAD11 arrival uplink (10, 11): ARRIVAL- GMEDK BA2661 / STAND 36M  ETA 1341
re!(BA_ARRIVAL, r"^ARRIVAL-\s+[A-Z0-9]{5}\s+(BA\d{1,4})[A-Z]?\b");
gate_re!(BA_STAND, r"^STAND\s+", r"(?:\s|$)");
// Jazz flight data (C1): DEP YVR/06:20Z/GTE C40 / ARR YYC/07:48Z/GTE A17
gate_re!(JAZZ_DEP, r"^DEP ([A-Z]{3})/[0-9:]+Z/GTE\s+", r"\s*$");
gate_re!(JAZZ_ARR, r"^ARR ([A-Z]{3})/[0-9:]+Z/GTE\s+", r"\s*$");
// Finnair weight and balance (RA, 3E): AY230    19NOV  TKU STD0600  BOARD 0540 GATE 2
gate_re!(
    FINNAIR_BOARD,
    r"^(AY\d{1,4})\s+\d{2}[A-Z]{3}\s+([A-Z]{3})\s+STD\d{4}\s+(?:ETD\d{4}\s+)?BOARD\s+\d{4}\s+GATE\s+",
    r"(?:\s|$)"
);
// Alaska ETA acknowledgement (21): FROM:LAX OPS / AS1831 / ARRIVAL GATE:65B
re!(ALASKA_FROM, r"^FROM:\s*([A-Z]{3}) OPS$");
gate_re!(ALASKA_ARR_GATE, r"^ARRIVAL GATE:\s*", r"\s*$");
// SAS passenger info (C1): ATT SK0932  ETA: 1414  GATE: C33   TERM: 3
gate_re!(SAS, r"^ATT (SK\d{1,4})\s+ETA:\s*\d{4}\s+GATE:\s*", r"(?:\s|$)");
// Swiss/Edelweiss (32): ARR GATE A12, or ARR POS D12 when the gate is a bus
gate_re!(ARR_GATE_LINE, r"^ARR GATE\s+", r"\s*$");
gate_re!(ARR_POS_LINE, r"^ARR POS\s+", r"\s*$");
// Qatar (RA): QR899 BNE - DOH ARR STAND - C6
gate_re!(QATAR_STAND, r"^([A-Z0-9]{2}\d{1,4}) ([A-Z]{3}) - ([A-Z]{3}) ARR STAND - ", r"\s*$");
// Free-standing arrival lines (RA): -PKG 35 (Air Europa handling), ESTIMATED
// PARK IS G09 (EVA), PARK BAY: 21 and ARR GATE IS 17R (Qantas), YOUR GATE IS
// 9, PLANNING GATE B37 and ARR GATE B44 ASSIGNED (Ethiopian dispatch).
gate_re!(
    LOOSE_ARRIVAL,
    r"^(?:-PKG|ESTIMATED PARK IS|PARK BAY:|ARR GATE IS|YOUR GATE IS|PLANNING GATE|ARR GATE)\s*",
    r"(?:\s+ASSIGNED)?\s*$"
);
// KLM connection info (32): P64AMSKWKL170542 / KL 1750 / GATE  A04  POS  A62
re!(KLM_HEADER, r"^P\d{2}AMSKWKL\d{6}");
re!(KLM_FLIGHT, r"^KL\s+(\d{1,4})$");
static KLM_GATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^GATE\s+{GATE_TOKEN}\s+POS\s+{GATE_TOKEN}\s*$")).unwrap());
// Korean Air (33): SPOT NO : 606
gate_re!(KOREAN_SPOT, r"^SPOT NO\s*:\s*", r"\s*$");
// Aer Lingus (48): STAND INFO = 400C
gate_re!(AER_LINGUS_STAND, r"^STAND INFO\s*=\s*", r"\s*$");
// Avianca dispatch (RA, QUATLASAV): GATE E12, GATE 13 EN SAL., E7 PARK EN BOG
gate_re!(AVIANCA_GATE, r"^GATE\s+", r"(?:\s+EN\s+([A-Z]{3}))?(?:[\s.]|$)");
gate_re!(AVIANCA_PARK, r"^", r"\s+PARK EN ([A-Z]{3})\b");
// Qantas (RA): GDAY QF0011 BAY 221
gate_re!(QANTAS_BAY, r"^GDAY (QF\d{1,4}) BAY\s+", r"\s*$");
// SkyWest without the ARR GATE INFO header (RA): MSP ARRIVAL GATE D9
gate_re!(AIRPORT_ARRIVAL_GATE, r"^([A-Z]{3}) ARRIVAL GATE\s+", r"\s*$");
// Avianca (RA): GATE J-12 MIA is gate J12 at MIA
re!(DASHED_AIRPORT, r"^GATE ([A-Z])-(\d{1,3}[A-Z]?) ([A-Z]{3})$");
// Round 7 (RA unless noted): Frontier ops free text (QUANPOCF9) GATE D36;
// SkyWest AA in-range GATE: 1D; Allegiant --GATE ASSIGNMENT PVU GATE 2
// ASSIGNED; Air Nostrum PKN:330 GATE:K88 BELT:19; Volaris A SU LLEGADA GATE
// J16; ITA ZRH/FCO ETA 1220 / GATE A32 BAG CLAIM 23; China Southern STAND NO:
// 136; China Eastern HI SIR, MU9062 SWA/HFE, PARK BAY 6.; FedEx EXPECT GATE:
// 2(SBD); ARRIVAL GATE IS: B12; Horizon ARRIVAL GATE:B9; Mesa
// QUPHXACYV.1ARRIVAL GATE Z9; Air France (C1) ARRIVAL PKG: K12.
gate_re!(PLAIN_GATE_LINE, r"^GATE\s+", r"\s*$");
gate_re!(IN_RANGE_COLON, r"^GATE:\s*", r"\s*$");
gate_re!(ALLEGIANT, r"^--GATE ASSIGNMENT\s+([A-Z]{3}) GATE\s+", r"\s+ASSIGNED");
gate_re!(AIR_NOSTRUM, r"^PKN:\S*\s+GATE:\s*", r"(?:\s|$)");
gate_re!(VOLARIS, r"LLEGADA GATE\s+", r"\b");
re!(ITA_ROUTE, r"^([A-Z]{3})/([A-Z]{3}) ETA (\d{4})$");
re!(HHMM, r"^(?:[01]\d|2[0-3])[0-5]\d$");
gate_re!(ITA_GATE, r"^GATE\s+", r"\s+BAG CLAIM");
gate_re!(STAND_NO, r"^STAND NO:\s*", r"\s*$");
gate_re!(CHINA_EASTERN, r"^HI SIR, MU(\d{1,4}) [A-Z]{3}/([A-Z]{3}), PARK BAY\s+", r"\b");
gate_re!(FEDEX_EXPECT, r"^EXPECT GATE:\s*", r"\(([A-Z]{3})\)");
// Anchored, so chat (`SEEING ARRIVAL GATE: F3`) and `PREVIOUS ARRIVAL GATE:`
// lines are not read.
gate_re!(ARRIVAL_GATE_IS, r"^(?:AIRSIDE\s+)?ARRIVAL GATE(?: IS)?:\s*", r"\s*$");
// SkyWest AA in-range marker; the `GATE:` line must come after it.
re!(IN_RANGE_MARKER, r"^IN-RANGE FROM AA\b");
gate_re!(HEADER_ARR_GATE, r"^[A-Z0-9]{9}[.~]\dARRIVAL GATE\s+", r"\s*$");
gate_re!(AIR_FRANCE_PKG, r"^ARRIVAL PKG:\s*", r"\b");
// Where a message moves on to other flights (connections, the aircraft's
// next flight); gate lines after it are not this flight's.
re!(
    STOP_LINE,
    r"^(?:CREW CONNECTING|CONNECTING GATE\b|NEXT FLT|NEXT FLIGHT|NEXT LEG|NEXT SECTOR|NXM\b|- NEXT FLIGHT|ON STAND-|CREW DEPT|TURN FLT|CREW CONNECTIONS|PAX CONNECTIONS|AC TURN|END$)"
);
// Label RA uplinks read by [`parse`] (Republic's and SkyWest's are skipped by
// what `parse` reads, since their other messages are read here), and Finnair
// connection lists (other flights' departure gates).
re!(RA_HANDLED_HEADER, r"^(?:QUNDCULUA|QUHELASAY)");

/// Read a gate assignment from the families in the module docs (label B3
/// clearances and airline gate, stand and parking messages on 10, 11, 21,
/// 25, 32, 33, 3E, 3M, 3T, 40, 48, 41, 42, 5Z, C1, HX and RA);
/// `None` when the text is not one. On label RA the uplinks [`parse`] reads
/// are skipped, so call it first.
pub fn parse_report(label: &str, text: &str) -> Option<GateInfo> {
    let lines = split_lines(text);
    let first = lines[0];
    match label {
        "B3" => lines.iter().find_map(|l| {
            let c = PDC.captures(l)?;
            let mut g = report(GateKind::DepartureGate, "PDC", &c[3], &c[4]);
            g.flight_number = Some(c[1].to_owned());
            g.departure = Some(c[2].to_owned());
            Some(g)
        }),
        "10" | "11" if first == ":PAD11" => ba_pad11_stand(&lines),
        "25" | "HX" => delta_arr_gate(&lines),
        "3E" => finnair(&lines),
        "32" if KLM_HEADER.is_match(first) => klm_connection_info(&lines),
        "32" => arr_gate_pos(&lines),
        "33" => lines.iter().find_map(|l| {
            let c = KOREAN_SPOT.captures(l)?;
            Some(report(GateKind::ArrivalGate, "KOREAN_SPOT", &c[1], ""))
        }),
        "48" => lines.iter().find_map(|l| {
            let c = AER_LINGUS_STAND.captures(l)?;
            Some(report(GateKind::ArrivalGate, "AER_LINGUS_STAND", &c[1], ""))
        }),
        "3M" => lines.iter().find_map(|l| {
            let c = VIRGIN_BAY.captures(l)?;
            Some(with_flight(report(GateKind::ArrivalGate, "VIRGIN_AUSTRALIA_BAY", &c[2], ""), &c[1]))
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
        "21" => alaska_gate_change(&lines).or_else(|| alaska_eta_ack(&lines)),
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
            .or_else(|| jazz_flight_data(&lines))
            .or_else(|| {
                lines.iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
                    let c = AIR_FRANCE_PKG.captures(l)?;
                    Some(report(GateKind::ArrivalGate, "AIR_FRANCE_ARRIVAL_PKG", &c[1], ""))
                })
            })
            .or_else(|| arr_gate_pos(&lines))
            .or_else(|| {
                lines.iter().find_map(|l| {
                    let c = SAS.captures(l)?;
                    Some(with_flight(report(GateKind::ArrivalGate, "SAS_PASSENGER_INFO", &c[2], ""), &c[1]))
                })
            })
            .or_else(|| jetstar_gate_change(&lines))
            .or_else(|| route_gate(&lines)),
        "41" | "42" => {
            if !JETBLUE_INR.is_match(first) {
                return westjet_gate(&lines).or_else(|| route_gate(&lines));
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
            if first.starts_with("QUHELASAY") {
                // Finnair: the arriving stand and the boarding gate, not the
                // connection lists.
                return finnair(&lines);
            }
            if RA_HANDLED_HEADER.is_match(first) {
                return None;
            }
            // Republic's and SkyWest's ARRIVAL GATE uplinks are `parse`'s;
            // their other messages (`RAH MESSAGE` / `FLT YX4704 RDU-LGA` /
            // `GATE 29`, SkyWest in-range `GATE: 1D`) are read here.
            if parse_arrival_gate(text).is_some() {
                return None;
            }
            if let Some(g) = alaska_eta_ack(&lines).or_else(|| ra_gate_round7(&lines)) {
                return Some(g);
            }
            if first.starts_with("QUATLASAV") {
                if let Some(g) = avianca_dispatch(&lines) {
                    return Some(g);
                }
            }
            delta_arr_gate(&lines)
                .or_else(|| {
                    lines.iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
                        if let Some(c) = TURKISH_PARK.captures(l) {
                            return Some(report(GateKind::ArrivalGate, "TURKISH_PARKING_POSITION", &c[1], ""));
                        }
                        if let Some(c) = PARKING_ASSIGNED.captures(l) {
                            return Some(report(GateKind::ArrivalGate, "PARKING_ASSIGNED", &c[1], ""));
                        }
                        ra_arrival_line(l)
                    })
                })
                .or_else(|| lines.iter().find_map(|l| {
                    let c = FLIGHT_AIRPORT_GATE.captures(l)?;
                    Some(with_flight(report(GateKind::ArrivalGate, "FLT_AIRPORT_GATE", &c[3], &c[2]), &c[1]))
                }))
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

/// The free-standing RA arrival lines: Qatar `ARR STAND`, `-PKG T21` and
/// the other loose arrival lines, Avianca `GATE J-12 MIA`, Qantas `GDAY
/// QF0011 BAY 221` and SkyWest's `EAU ARRIVAL GATE 1` without its header.
fn ra_arrival_line(l: &str) -> Option<GateInfo> {
    if let Some(c) = QATAR_STAND.captures(l) {
        let mut g = with_flight(report(GateKind::ArrivalGate, "QATAR_ARR_STAND", &c[4], &c[3]), &c[1]);
        g.departure = Some(c[2].to_owned());
        return Some(g);
    }
    if let Some(c) = LOOSE_ARRIVAL.captures(l) {
        return Some(report(GateKind::ArrivalGate, "ARRIVAL_PARKING", &c[1], ""));
    }
    if let Some(c) = DASHED_AIRPORT.captures(l) {
        return Some(report(GateKind::ArrivalGate, "GATE_DASH_AIRPORT", &format!("{}{}", &c[1], &c[2]), &c[3]));
    }
    if let Some(c) = QANTAS_BAY.captures(l) {
        return Some(with_flight(report(GateKind::ArrivalGate, "QANTAS_BAY", &c[2], ""), &c[1]));
    }
    let c = AIRPORT_ARRIVAL_GATE.captures(l)?;
    Some(report(GateKind::ArrivalGate, "AIRPORT_ARRIVAL_GATE", &c[2], &c[1]))
}

/// The RA forms listed with `PLAIN_GATE_LINE`: the Mesa header gate, then
/// line by line (before the message moves on) Frontier ops free text (after
/// its `FROM PHL OPS` form), SkyWest in-range `GATE:`, ITA, Allegiant, Air
/// Nostrum, Volaris, `STAND NO:`, China Eastern, FedEx and `ARRIVAL GATE:`.
fn ra_gate_round7(lines: &[&str]) -> Option<GateInfo> {
    if let Some(c) = HEADER_ARR_GATE.captures(lines[0]) {
        return Some(report(GateKind::ArrivalGate, "HEADER_ARRIVAL_GATE", &c[1], ""));
    }
    let frontier = lines[0].starts_with("QUANPOCF9");
    if frontier {
        // FROM PHL OPS ... GATE AVAIL AT ARRIVAL also names the airport.
        if let Some(g) = frontier_gate(lines) {
            return Some(g);
        }
    }
    let mut in_range = false;
    let mut ita: Option<(&str, &str, &str)> = None;
    for l in lines.iter().take_while(|l| !STOP_LINE.is_match(l)) {
        let arrival = |format, gate: &str, airport: &str| Some(report(GateKind::ArrivalGate, format, gate, airport));
        if frontier {
            if let Some(c) = PLAIN_GATE_LINE.captures(l) {
                return arrival("FRONTIER_OPS", &c[1], "");
            }
        }
        if IN_RANGE_MARKER.is_match(l) {
            in_range = true;
            continue;
        }
        if in_range {
            if let Some(c) = IN_RANGE_COLON.captures(l) {
                return arrival("IN_RANGE_GATE", &c[1], "");
            }
        }
        if let Some(c) = ITA_ROUTE.captures(l) {
            ita = Some((c.get(1)?.as_str(), c.get(2)?.as_str(), c.get(3)?.as_str()));
        }
        if let (Some(c), Some((origin, airport, eta))) = (ITA_GATE.captures(l), ita) {
            let mut g = report(GateKind::ArrivalGate, "ITA_CONNECTING_GATES", &c[1], airport);
            g.departure = Some(origin.to_owned());
            if HHMM.is_match(eta) {
                g.eta = Some(eta.to_owned());
            }
            return Some(g);
        }
        if let Some(c) = ALLEGIANT.captures(l) {
            return arrival("ALLEGIANT_IN_RANGE", &c[2], &c[1]);
        }
        if let Some(c) = AIR_NOSTRUM.captures(l) {
            return arrival("AIR_NOSTRUM_PKN", &c[1], "");
        }
        if let Some(c) = VOLARIS.captures(l) {
            return arrival("VOLARIS_LLEGADA", &c[1], "");
        }
        if let Some(c) = STAND_NO.captures(l) {
            return arrival("STAND_NO", &c[1], "");
        }
        if let Some(c) = CHINA_EASTERN.captures(l) {
            return Some(with_flight(report(GateKind::ArrivalGate, "CHINA_EASTERN_PARK_BAY", &c[3], &c[2]), &c[1]));
        }
        if let Some(c) = FEDEX_EXPECT.captures(l) {
            return arrival("FEDEX_EXPECT_GATE", &c[1], &c[2]);
        }
        if let Some(c) = ARRIVAL_GATE_IS.captures(l) {
            return arrival("ARRIVAL_GATE_COLON", &c[1], "");
        }
    }
    None
}

/// KLM connection info: `KL 1750` and `GATE  A04  POS  A62`, the gate at
/// AMS, before the next flight (`NXM`).
fn klm_connection_info(lines: &[&str]) -> Option<GateInfo> {
    let mut flight = None;
    for l in lines.iter().take_while(|l| !STOP_LINE.is_match(l)) {
        if let Some(c) = KLM_FLIGHT.captures(l) {
            flight = Some(format!("KL{}", &c[1]));
        }
        if let Some(c) = KLM_GATE.captures(l) {
            let mut g = report(GateKind::ArrivalGate, "KLM_CONNECTION_INFO", &c[1], "AMS");
            g.flight_number = flight;
            return Some(g);
        }
    }
    None
}

/// Avianca dispatch (`QUATLASAV`): `GATE 13 EN SAL.`, `GATE E12` or `E7 PARK
/// EN BOG`, before the dispatcher's `FROM:` line.
fn avianca_dispatch(lines: &[&str]) -> Option<GateInfo> {
    lines.iter().take_while(|l| !l.starts_with("FROM:")).find_map(|l| {
        let c = AVIANCA_GATE.captures(l).or_else(|| AVIANCA_PARK.captures(l))?;
        let airport = c.get(2).map_or("", |m| m.as_str());
        Some(report(GateKind::ArrivalGate, "AVIANCA_DISPATCH", &c[1], airport))
    })
}

/// Swiss/Edelweiss: `ARR GATE A52`, else `ARR POS D12` (the gate is a bus),
/// before the connecting gates.
fn arr_gate_pos(lines: &[&str]) -> Option<GateInfo> {
    [&ARR_GATE_LINE, &ARR_POS_LINE].into_iter().find_map(|re| {
        lines.iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
            let c = re.captures(l)?;
            Some(report(GateKind::ArrivalGate, "ARR_GATE_POS", &c[1], ""))
        })
    })
}

/// Alaska ETA acknowledgement: `FROM:LAX OPS`, the flight and `ARRIVAL
/// GATE:65B`. The gate line alone is not enough (Horizon sends a bare
/// `ARRIVAL GATE:B9`): the message must also have an `ETA ACK`, `FROM:<IATA>
/// OPS` or AS flight line before the next flight.
fn alaska_eta_ack(lines: &[&str]) -> Option<GateInfo> {
    let mut airport = None;
    let mut flight = None;
    let mut ack = false;
    let mut gate = None;
    for l in lines.iter().take_while(|l| !STOP_LINE.is_match(l)) {
        if *l == "ETA ACK" {
            ack = true;
        }
        if let Some(c) = ALASKA_FROM.captures(l) {
            airport = Some(c[1].to_owned());
        }
        if let Some(c) = ALASKA_FLIGHT.captures(l) {
            flight = Some(c[1].to_owned());
        }
        if gate.is_none() {
            if let Some(c) = ALASKA_ARR_GATE.captures(l) {
                gate = Some(c[1].to_owned());
            }
        }
    }
    let gate = gate?;
    if !(ack || airport.is_some() || flight.is_some()) {
        return None;
    }
    let mut g = report(GateKind::ArrivalGate, "ALASKA_ETA_ACK", &gate, "");
    g.arrival = airport;
    g.flight_number = flight;
    Some(g)
}

/// Finnair: the boarding gate of a weight and balance message (a departure
/// gate), else the arriving stand.
fn finnair(lines: &[&str]) -> Option<GateInfo> {
    lines
        .iter()
        .find_map(|l| {
            let c = FINNAIR_BOARD.captures(l)?;
            let mut g = with_flight(report(GateKind::DepartureGate, "FINNAIR_BOARDING_GATE", &c[3], ""), &c[1]);
            g.departure = Some(c[2].to_owned());
            Some(g)
        })
        .or_else(|| finnair_stand(lines))
}

/// Delta crew P/F (`CREW P/F` or `PART  1 OF` line): `ARR ATL GTE E15`.
fn delta_arr_gate(lines: &[&str]) -> Option<GateInfo> {
    if !lines.iter().any(|l| l.starts_with("CREW P/F") || l.starts_with("PART  1 OF")) {
        return None;
    }
    lines.iter().find_map(|l| {
        let c = DELTA_ARR.captures(l)?;
        Some(report(GateKind::ArrivalGate, "DELTA_CREW_PF", &c[2], &c[1]))
    })
}

/// Finnair `AY1074 22NOV26` / `ARRIVING STAND 131 ELDT 12:53`; connection
/// lists name other flights' gates, so reading stops at `CONNECTION INFO`.
fn finnair_stand(lines: &[&str]) -> Option<GateInfo> {
    let mut flight = None;
    for l in lines {
        if let Some(c) = FINNAIR_FLIGHT.captures(l) {
            flight = Some(c[1].to_owned());
        }
        if let Some(c) = FINNAIR_STAND.captures(l) {
            let mut g = report(GateKind::ArrivalGate, "FINNAIR_ARRIVING_STAND", &c[1], "");
            g.flight_number = flight;
            return Some(g);
        }
        if l.starts_with("CONNECTION INFO") {
            return None;
        }
    }
    None
}

/// BA PAD11 arrival uplink: after `ARRIVAL- GTNED BA1447`, the first
/// `STAND 503  ETA 1357` line, before `ON STAND-` (the aircraft on the
/// stand) or `CREW DEPT` (the crew's next flight). The ETA itself is left
/// to the OOOI extractor.
fn ba_pad11_stand(lines: &[&str]) -> Option<GateInfo> {
    let (i, c) = lines.iter().enumerate().find_map(|(i, l)| Some((i, BA_ARRIVAL.captures(l)?)))?;
    lines[i + 1..].iter().take_while(|l| !STOP_LINE.is_match(l)).find_map(|l| {
        let g = BA_STAND.captures(l)?;
        Some(with_flight(report(GateKind::ArrivalGate, "BA_PAD11_STAND", &g[1], ""), &c[1]))
    })
}

/// Jazz flight data: `DEP YVR/06:20Z/GTE C40` and `ARR YYC/07:48Z/GTE A17`
/// give both gates (an arrival gate with `departure_gate`), or only the
/// departure gate when there is no arrival line.
fn jazz_flight_data(lines: &[&str]) -> Option<GateInfo> {
    let mut dep = None;
    for l in lines.iter().take_while(|l| !STOP_LINE.is_match(l)) {
        if let Some(c) = JAZZ_DEP.captures(l) {
            dep = Some(c);
        }
        if let Some(c) = JAZZ_ARR.captures(l) {
            let mut g = report(GateKind::ArrivalGate, "JAZZ_FLIGHT_DATA", &c[2], &c[1]);
            if let Some(d) = dep {
                g.departure = Some(d[1].to_owned());
                g.departure_gate = Some(d[2].to_owned());
            }
            return Some(g);
        }
    }
    let d = dep?;
    let mut g = report(GateKind::DepartureGate, "JAZZ_FLIGHT_DATA", &d[2], "");
    g.departure = Some(d[1].to_owned());
    Some(g)
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
            ("Delta crew P/F", "RA", "48   ATLE15\r\nCREW P/F        \r\nPART  1 OF  1 \r\nFLT 1160 - 22NOV1213 CREW P/F        \r\nARR ATL GTE E15\r\nFLT 1941 PVR 1025L\r\n                     NXT  DEPT  DEPT    \r\n NAME                FLT  TIME  FROM GTE\r\n", ArrivalGate, "DELTA_CREW_PF", None, Some("ATL"), "E15", None),
            ("Delta crew P/F on 25", "25", "48   JFKB22\r\nCREW P/F        \r\nPART  1 OF  1 \r\nFLT 0028 - 22NOV1236 CREW P/F        \r\nARR JFK GTE B22\r\n", ArrivalGate, "DELTA_CREW_PF", None, Some("JFK"), "B22", None),
            ("Virgin Australia bay", "3M", "VA1007, BAY: 45A\r\nGPU: NO\r\nPCA: \r\n", ArrivalGate, "VIRGIN_AUSTRALIA_BAY", None, None, "45A", Some("VA1007")),
            ("Turkish parking position", "RA", "QUISTBITK~1\r\n- EXPECTED PARKING POSITION: F8R\r\n- NEXT FLIGHT INFO:IST/PRG TK1769 14:45Z\r\n", ArrivalGate, "TURKISH_PARKING_POSITION", None, None, "F8R", None),
            ("Finnair arriving stand", "RA", "QUHELASAY~1\r\nAY1074 22NOV26\r\nARRIVING STAND 131 ELDT 12:53\r\nOHATR->AY1101 23NOV26 ETD - (STD 05:45)\r\n", ArrivalGate, "FINNAIR_ARRIVING_STAND", None, None, "131", Some("AY1074")),
            ("parking preassigned", "RA", "QUCANXMCZ~1RA221348\r\n\tB-309Y\r\n\tPARKING PREASSIGNED:138\r\n\tMETAR ZYTX 221330Z 19007MPS 9999 OVC040 04/M03 Q1020 NOSIG=\r\n", ArrivalGate, "PARKING_ASSIGNED", None, None, "138", None),
            ("BA PAD11 stand", "10", ":PAD11\r\nUTC\r\nARRIVAL- GTNED BA1447  \r\nSTAND 503  ETA 1357\r\nON STAND- BA424   \r\nETD 1350\r\n\r\nCREW DEPT- FOR INFO ONLY\r\nGTNED BA1372  TO X\r\nSTAND 503\r\n", ArrivalGate, "BA_PAD11_STAND", None, None, "503", Some("BA1447")),
            ("BA PAD11 stand with suffix", "11", ":PAD11\r\nUTC\r\nARRIVAL- GMEDK BA2661  \r\nSTAND 36M  ETA 1341 //\r\nON STAND- UNKNOWN //", ArrivalGate, "BA_PAD11_STAND", None, None, "36M", Some("BA2661")),
            ("Jazz flight data", "C1", JAZZ_FLIGHT_DATA, ArrivalGate, "JAZZ_FLIGHT_DATA", Some("YVR"), Some("YYC"), "A17", None),
            ("Finnair boarding gate", "RA", "QUHELASAY~1\r\nCUSTOMER PAX AND BAG DETAILS FOR WEIGHT AND BALANCE              \r\nFLIGHT INFO  AT7       REG OH-ATO   70Y                     PW   \r\nAY230    19NOV  TKU STD0600             BOARD 0540 GATE 2   AO   \r\n", DepartureGate, "FINNAIR_BOARDING_GATE", Some("TKU"), None, "2", Some("AY230")),
            ("Finnair arriving stand on 3E", "3E", "AY1074 22NOV26\r\nARRIVING STAND 131 ELDT 12:53\r\n", ArrivalGate, "FINNAIR_ARRIVING_STAND", None, None, "131", Some("AY1074")),
            ("Delta crew P/F on HX", "HX", "RA IV3 48   DTWA67\r\nCREW P/F        \r\nPART  1 OF  1 \r\nFLT 1849 - 19NOV0102 CREW P/F        \r\nARR DTW GTE A67\r\n", ArrivalGate, "DELTA_CREW_PF", None, Some("DTW"), "A67", None),
            ("Qatar arrival stand", "RA", "\r\nSTANDINFO\r\nQR899 BNE - DOH ARR STAND - C6\r\nALL DATES AND TIMES UTC\r\nA.N.R - QR283 AMS STD 11:55 221\r\n", ArrivalGate, "QATAR_ARR_STAND", Some("BNE"), Some("DOH"), "C6", Some("QR899")),
            ("Air Europa handling, bare CR lines", "RA", "QUPMIANUX~1\r\nMESSAGE FROM HANDLING\r\n-OPS/GFH MAD(STAFF 1)\r-PKG T21\r-BELT 10\r-TTOS\r FLT      DEST  PAX    GATE\r", ArrivalGate, "ARRIVAL_PARKING", None, None, "T21", None),
            ("EVA estimated park", "RA", "QUSPLKPBR~1RA101191445\r\nBR75\r\nESTIMATED PARK IS G09\r\nFINAL WL B INFRM BY ATC\r\n", ArrivalGate, "ARRIVAL_PARKING", None, None, "G09", None),
            ("Qantas park bay", "RA", "QUADLKIQF~1\r\nHI\r\nPARK BAY: 21\r\nBAGGAGE CAROUSEL: 4 \r\n", ArrivalGate, "ARRIVAL_PARKING", None, None, "21", None),
            ("Qantas arrival gate", "RA", ".AKLOSQF ~1 QANTAS AKL MSG\r\nGDAY QF153\r\nACARS ACK 03 WCHRS ON ARR\r\nARR GATE IS 17R\r\n", ArrivalGate, "ARRIVAL_PARKING", None, None, "17R", None),
            ("your gate is", "RA", "HELLO\r\nYOUR GATE IS 9\r\n", ArrivalGate, "ARRIVAL_PARKING", None, None, "9", None),
            ("Qantas bay with flight", "RA", ".LAXOSQF ~1 QANTAS LAX MSG\r\n\tGDAY QF0011 BAY 221\r\n\tBAGGAGE CAROUSEL TBA UPON\r\n", ArrivalGate, "QANTAS_BAY", None, None, "221", Some("QF0011")),
            ("Ethiopian planning gate", "RA", "QUJFKOWLX~1RA199190007\r\nHELLO ET516 CREW\r\nPLANNING GATE B37\r\nNO DELAYS\r\n", ArrivalGate, "ARRIVAL_PARKING", None, None, "B37", None),
            ("Avianca dashed gate", "RA", "QUATLASAV~1GATE \r\nBUENOS DIAS\r\nGATE J-12 MIA\r\nBUEN VUELO\r\n", ArrivalGate, "GATE_DASH_AIRPORT", None, Some("MIA"), "J12", None),
            ("SkyWest arrival gate line", "RA", "579\r\n\r\nEAU ARRIVAL GATE 1\r\n\r\n*END-MSG\r\n", ArrivalGate, "AIRPORT_ARRIVAL_GATE", None, Some("EAU"), "1", None),
            ("WestJet Encore on 41", "41", "FTX01,WS 3564 YVR-YLW\r\nYLW GATE 9/OPS", ArrivalGate, "WESTJET_OPS_GATE", Some("YVR"), Some("YLW"), "9", Some("3564")),
            ("Alaska ETA acknowledgement", "21", ",,\r\nETA ACK\r\nFROM:LAX OPS\r\nAS1831\r\nETA:0057\r\nARRIVAL GATE:65B \r\nEXT ELECT AVAIL W/INTERLOCK\r\nNEXT FLT:1397 PDX \r\n", ArrivalGate, "ALASKA_ETA_ACK", None, Some("LAX"), "65B", Some("AS1831")),
            ("SAS passenger info", "C1", ".HDQBQSK 191354\r\n\tAGM\r\n\tAN LN-RKS/FI SK932/MA 767I\r\n\tATT SK0932  ETA: 1414  GATE: C33   TERM: 3    BC: \r\n\tCONNECTIONS\r\n\t -VIE SK1693 0755  B9   3   Vienna\r\n", ArrivalGate, "SAS_PASSENGER_INFO", None, None, "C33", Some("SK0932")),
            ("Edelweiss arrival position", "32", "STA/ETA  1135LT\r\nARR TERM 2 BY BUS\r\nARR GATE GRZ\r\nARR POS  D12\r\nBAG BELT 21\r\nCONNECTING GATE INFO 19NOV1004Z\r\n       1220 B10 LX1270 Copenhagen\r\n", ArrivalGate, "ARR_GATE_POS", None, None, "D12", None),
            ("Swiss arrival gate", "32", "ARR GATE A52\r\nARR POS  A52\r\n", ArrivalGate, "ARR_GATE_POS", None, None, "A52", None),
            ("Virgin Australia arrival bay", "3M", "VA1007, ARR BAY: 45A\r\n", ArrivalGate, "VIRGIN_AUSTRALIA_BAY", None, None, "45A", Some("VA1007")),
            ("KLM connection info", "32", "P64AMSKWKL170542\r\nCONNECTION INFO.\r\nKL 1750 \r\nALL TIMES LOCAL\r\nGATE  A04  POS  A62 \r\nSCHED 0715 ETA  0715\r\nNXM   KL 1771  17NOV 0805  B30 A62\r\n", ArrivalGate, "KLM_CONNECTION_INFO", None, Some("AMS"), "A04", Some("KL1750")),
            ("Korean Air spot", "33", "SPOT NO : 606\r\n", ArrivalGate, "KOREAN_SPOT", None, None, "606", None),
            ("Aer Lingus stand", "48", "FLIGHT NUMBER = EI0122\r\n\tDATE = 17 NOV\r\n\tTIME = 0334\r\n\tTAIL = EI-FNH\r\n\tSTAND INFO = 400C\r\n", ArrivalGate, "AER_LINGUS_STAND", None, None, "400C", None),
            ("Republic RAH message", "RA", "QUHDQDDRP~1RAH MESSAGE\r\nAN N423YX\r\nFLT YX4704 RDU-LGA\r\nGATE 29 ETA 2213L \r\nTERMINATING FLIGHT\r\n", ArrivalGate, "FLT_ROUTE_GATE", Some("RDU"), Some("LGA"), "29", Some("4704")),
            ("Avianca dispatch gate", "RA", "QUATLASAV~1.\r\nBUENOS DIAS\r\nGATE 13 EN SAL. SLDS.\r\n\r\n------------\r\nFROM: DISP_X\r\n", ArrivalGate, "AVIANCA_DISPATCH", None, Some("SAL"), "13", None),
            ("Avianca dispatch park", "RA", "QUATLASAV~1PARK\n\tBN DIA CREW\n\tE7 PARK EN BOG\n\tSALU2 BN VLO\n\t\n\t------------\n\tFROM: DISP_X\n", ArrivalGate, "AVIANCA_DISPATCH", None, Some("BOG"), "E7", None),
            ("Avianca dispatch gate without airport", "RA", "QUATLASAV~1.\r\nGATE E12\r\nFROM: DISP_X\r\n", ArrivalGate, "AVIANCA_DISPATCH", None, None, "E12", None),
            ("Frontier ops free text", "RA", "QUANPOCF9~1\r\nGATE D36\r\n", ArrivalGate, "FRONTIER_OPS", None, None, "D36", None),
            ("SkyWest AA in-range", "RA", "QUSILITOO.1AA IN-RANGE     \r\nIN-RANGE FROM AA        \r\nGATE: 1D\r\nTERMINATING FLIGHT\r\n", ArrivalGate, "IN_RANGE_GATE", None, None, "1D", None),
            ("Allegiant in-range", "RA", "QUANPOCG4~1           \r\n*AUTOMATED MSG FLT 9201*****IN-RANGE NOTICE*****\r\n--GATE ASSIGNMENT       PVU GATE 2 ASSIGNED      \r\n--NEXT FLIGHT           3232 PVU-SNA 19-1700Z\r\n", ArrivalGate, "ALLEGIANT_IN_RANGE", None, Some("PVU"), "2", None),
            ("Air Nostrum PKN", "RA", "QUVLCASYW~1\r\nIB1059  SDR   K92\r\nPKN:330   GATE:K88   BELT:19  HALL:10\r\n", ArrivalGate, "AIR_NOSTRUM_PKN", None, None, "K88", None),
            ("Volaris llegada", "RA", "QUANPH1Y4~1HOLA CAP, BUEN DIA. A SU LLEGADA GATE J16 Y GPU DISPONIBLE, FAVOR DE CONFIRMAR E\r\nN TIERRA\r\n", ArrivalGate, "VOLARIS_LLEGADA", None, None, "J16", None),
            ("ITA connecting gates", "RA", "QUMGL1AAZ~1CONNECTING GATES\r\nEIHHI AZ0571/17 \r\nZRH/FCO ETA 1220 \r\nGATE A32 BAG CLAIM 23 \r\nFLT/DTE   TYP ETD   TO   \r\nAZ1781/17 INF 1315 PMO  \r\n", ArrivalGate, "ITA_CONNECTING_GATES", Some("ZRH"), Some("FCO"), "A32", None),
            ("China Southern stand no", "RA", "QUCANXMCZ~1RA170521\n\t//**TRANSFER PASSENGER INFORMATION***\n\tFLTNO         DEST     GATE  CDT DLY STAND\n\tNIL\n\tBAGGAGE CLAIM BELT: 01\n\tSTAND NO: 136\n", ArrivalGate, "STAND_NO", None, None, "136", None),
            ("China Eastern park bay", "RA", "HI SIR, MU9062 SWA/HFE, PARK BAY 6.FOR REFERENCE ONLY.", ArrivalGate, "CHINA_EASTERN_PARK_BAY", None, Some("HFE"), "6", Some("9062")),
            ("FedEx expect gate", "RA", "QUMEMXRFX~1(4511)\r\nRAMP INRANGE NOTICE\r\nEXPECT GATE: 2(SBD)\r\nGROUND POWER AVAILABLE\r\n", ArrivalGate, "FEDEX_EXPECT_GATE", None, Some("SBD"), "2", None),
            ("airside arrival gate", "RA", "QUANPOCPD~1 FREETEXT MSG\r\nAIRSIDE ARRIVAL GATE IS: B12\r\n", ArrivalGate, "ARRIVAL_GATE_COLON", None, None, "B12", None),
            ("Mesa header arrival gate", "RA", "QUPHXACYV.1ARRIVAL GATE A1B\r\n\r\n", ArrivalGate, "HEADER_ARRIVAL_GATE", None, None, "A1B", None),
            ("Alaska ETA acknowledgement on RA", "RA", "ETA ACK\r\nFROM:ANC OPS\r\nAS2301\r\nETA:0126\r\nARRIVAL GATE:C7 \r\n", ArrivalGate, "ALASKA_ETA_ACK", None, Some("ANC"), "C7", Some("AS2301")),
            ("Helvetic arrival position on C1", "C1", ".ZRHOWOA 191354\r\nAGM\r\nARR GATE GRZ\r\nARR POS  I12\r\nCONNECTING GATE INFO 19NOV1004Z\r\n", ArrivalGate, "ARR_GATE_POS", None, None, "I12", None),
            ("Air France arrival parking", "C1", ".PAROWAF 171200\r\nAGM\r\nARRIVAL PKG: K12 PKG TYPE: C TERM: E\r\n", ArrivalGate, "AIR_FRANCE_ARRIVAL_PKG", None, None, "K12", None),
            ("Ethiopian arrival gate assigned", "RA", "QUJFKOWLX~1\r\nSWR DSP JFK\r\nGD MORNING ET500 CREW\r\nARR GATE B44 ASSIGNED\r\nHAPPY LANDING\r\n", ArrivalGate, "ARRIVAL_PARKING", None, None, "B44", None),
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

    const JAZZ_FLIGHT_DATA: &str = ".YHZITQK 220419\r\nAGM\r\nAN CGJUZ/MA 395I\r\n-  DFR FLT 8074\r\n----\r\nFIN 726/CGJUZ/CRA\r\nDEP YVR/06:20Z/GTE C40\r\nARR YYC/07:48Z/GTE A17\r\nPAX 46/PAD 8/FUEL \r\n";

    #[test]
    fn jazz_flight_data_gives_both_gates() {
        let g = parse_report("C1", JAZZ_FLIGHT_DATA).unwrap();
        assert_eq!((g.departure_gate.as_deref(), g.gate.as_str()), (Some("C40"), "A17"));
        assert_eq!(g.summary(), "ARRIVAL GATE YVR->YYC dep gate C40 gate A17");
        let dep_only = parse_report("C1", "-  DFR FLT 8074\r\nDEP YVR/06:20Z/GTE C40\r\nPAX 46\r\n").unwrap();
        assert_eq!(dep_only.kind, GateKind::DepartureGate);
        assert_eq!((dep_only.departure.as_deref(), dep_only.gate.as_str()), (Some("YVR"), "C40"));
        assert_eq!(dep_only.departure_gate, None);
    }

    #[test]
    fn finnair_stand_but_not_connections() {
        // `parse` still reads neither; `parse_report` reads the stand and the
        // boarding gate (a departure gate), never the connection lists.
        assert!(parse_report("RA", FINNAIR_CONNECT).is_none());
        let g = parse_report("RA", FINNAIR_PAX).unwrap();
        assert_eq!((g.kind, g.departure.as_deref(), g.gate.as_str()), (GateKind::DepartureGate, Some("HEL"), "9"));
        assert_eq!(g.summary(), "DEPARTURE GATE AY1073 HEL gate 9");
        assert!(parse_report("RA", "QUHELASAY~1\r\nCONNECTION INFO\r\nARRIVING STAND 131\r\n").is_none());
    }

    #[test]
    fn round7_review_cases() {
        // Horizon's bare gate line is not an Alaska ETA acknowledgement.
        let g = parse_report("RA", "ARRIVAL GATE:B9\r\n").unwrap();
        assert_eq!((g.format, g.gate.as_str()), (Some("ARRIVAL_GATE_COLON"), "B9"));
        // A previous gate or chat line is not the arrival gate.
        assert!(parse_report("RA", "PREVIOUS ARRIVAL GATE: B4\r\n").is_none());
        assert!(parse_report("RA", "SEEING ARRIVAL GATE: F3\r\n").is_none());
        // An in-range marker of a later flight does not enable this GATE: line,
        // and one after a Shandong gate leaves its flight and airport.
        assert!(parse_report("RA", "QUSILITOO.1ENROUTE PLAN    \r\nGATE: 4\r\nNEXT FLIGHT\r\nIN-RANGE FROM AA\r\n").is_none());
        let g = parse_report("RA", "CURRENT FLIGHT:\r\nSC8402 ARR:ZSJN\r\nGATE:10\r\nWEATHER:\r\nIN-RANGE FROM AA\r\n").unwrap();
        assert_eq!((g.flight_number.as_deref(), g.arrival.as_deref(), g.gate.as_str()), (Some("SC8402"), Some("ZSJN"), "10"));
        // ITA keeps the origin and ETA, also with the title on its own line.
        for text in [
            "QUMGL1AAZ~1CONNECTING GATES\r\nZRH/FCO ETA 1220 \r\nGATE A32 BAG CLAIM 23 \r\n",
            "QUMGL1AAZ~1\r\nCONNECTING GATES\r\nZRH/FCO ETA 1220\r\nGATE A32 BAG CLAIM 23\r\n",
        ] {
            let g = parse_report("RA", text).unwrap();
            assert_eq!(
                (g.departure.as_deref(), g.arrival.as_deref(), g.gate.as_str(), g.eta.as_deref()),
                (Some("ZRH"), Some("FCO"), "A32", Some("1220"))
            );
        }
        // Air France parking after the next-flight line is not read.
        assert!(parse_report("C1", ".PAROWAF 171200\r\nAGM\r\nNEXT FLIGHT\r\nARRIVAL PKG: K12 PKG TYPE: C TERM: E\r\n").is_none());
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
            ("Republic ARRIVAL GATE stays with parse", "RA", RPA),
            ("KLM next flight gate", "32", "P64AMSKWKL170542\r\nCONNECTION INFO.\r\nKL 1750 \r\nNXM   KL 1771  17NOV 0805  B30 A62\r\nGATE  B30  POS  A62\r\n"),
            ("Avianca gate after FROM", "RA", "QUATLASAV~1.\r\nFROM: DISP_X\r\nGATE 13 EN SAL.\r\n"),
            ("Finnair connections", "RA", FINNAIR_CONNECT),
            ("Frontier chat about another airport's gate", "RA", "QUANPOCF9~1\r\nGOOD TO KNOW. RIDES STILL ROUGH? \r\nSEEING GATE F3 IN KMIA\r\n"),
            ("SkyWest gate without in-range", "RA", "QUSILITOO.1ENROUTE PLAN    \r\nGATE: 4\r\n"),
            // No gate for this flight, only for the connections and the next flight.
            ("gate status only", "40", "FLT 0278/21 PHL-EDI\r\nGATE STATUS NOT AVAIL\r\nNEXT FLT 0602 ACFT 3JF SKD 0900 GRR\r\nGATE B12\r\n"),
            ("connections only", "40", "FLT 4258/22 HPN-DCA\r\nCREW CONNECTING GATE INFO\r\nCREW SEAT FLT DEST GATE TIME\r\nGATE D35\r\n"),
            ("PDC without a gate", "B3", "/LSZH.DC1/RCD 011\r\nSWR72-LSZH-GATE -KIAD\r\n"),
            ("gate request downlink", "5Z", "/C3 GATE REQ       / KAUS KIAH\r\n"),
            ("JetBlue crew connection gate", "41", "INR,221156\r\nCAROUSEL:3\r\nCREW CONNECTIONS\r\n X\r\n   -BUF F:0802 ON TIME  1029L G:516\r\n"),
            ("route family on another label", "H1", "FLT 1658/22 AUS-DFW\r\nGATE D21\r\n"),
            ("PSA acknowledgement without a gate", "5Z", "/ACKFLT GATE E"),
            ("LATAM without a position", "RA", "QUSCLASLA~1\r\n\tGATE ASSIGNMENT\r\n\t\r\n\tPPOS:N/A\r\n\tBAG BELT:\r\n\tNEXT LEG: LA3091 FLN-CGH 22NOV 11:35Z"),
            ("Finnair delay summary", "RA", "QUHELASAY~1\r\nDELAY SUMMARY\r\nAY818/21NOV2026 ARN-HEL\r\nSTD 20:05\r\nATD 21:19\r\n"),
            ("parking not assigned", "RA", "QUCANXMCZ~1RA221348\r\n\tPARKING ASSIGNED:NA\r\n"),
            ("BA PAD11 release without a stand", "10", ":PAD11\r\n22NOV26 BA356    GEUUZ LHR-LYS\r\nETD 0700Z FLIGHT RELEASE OFP  5"),
            ("BA PAD11 next flight stand only", "10", ":PAD11\r\nUTC\r\nARRIVAL- GTNEF BA1421  \r\nETA 1343\r\nCREW DEPT- FOR INFO ONLY\r\nSTAND 506\r\n"),
            ("Jazz next flight gate", "C1", ".YHZITQK 190005\r\nAGM\r\nAN CFCJZ/MA 988I\r\n-  NEXT FLIGHT INFORMATION\r\nFLT: 8908\r\nDES: YYZ\r\nGATE:E02\r\n"),
            ("Air Canada GTE codes", "4C", "-GTE C001  C47 \r\n"),
            ("crew stand request free text", "10", "FTX01.OPC\r\nHI STAND PLANNING. WE\r\nHAVE STAND 515 BUT A/C\r\n"),
            ("Swiss connecting gates only", "32", "STA/ETA  1135LT\r\nCONNECTING GATE INFO 19NOV1004Z\r\nARR POS  D12\r\n"),
            ("stand on another label", "H1", ":PAD11\r\nUTC\r\nARRIVAL- GTNED BA1447  \r\nSTAND 503  ETA 1357\r\n"),
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
