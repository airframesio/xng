//! Arrival gate uplinks on label RA.
//!
//! Port of the Airframes event digger's label RA gate parsing
//! (microservice-event-digger `pkg/handlers/labelra_handler.go`); keep the
//! rules in step with it. Three formats name the gate at the arrival
//! airport:
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
    /// Republic `ARRIVAL GATE` / SkyWest `ARR GATE INFO`.
    ArrivalGate,
}

/// The gate assigned at the flight's arrival airport.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateInfo {
    pub kind: GateKind,
    /// Gate at the arrival airport, e.g. `43`, `B47`, `C121`, `26B`.
    pub gate: String,
    /// `OPEN` or `AVAIL` (United).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_status: Option<String>,
    /// HH:MM the gate becomes available (United `AVAIL 06:30`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_available_time: Option<String>,
    /// Flight as written, e.g. `UA0942`, `YX4450`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_number: Option<String>,
    /// Day of month of the flight (United `UA0942/21`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure: Option<String>,
    /// Airport the gate is at (ICAO for United, IATA otherwise).
    pub arrival: String,
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
            gate: gate.to_owned(),
            gate_status: None,
            gate_available_time: None,
            flight_number: None,
            day: None,
            departure: None,
            arrival: arrival.to_owned(),
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
        });
        if let Some(f) = &self.flight_number {
            s.push_str(&format!(" {f}"));
        }
        match &self.departure {
            Some(d) => s.push_str(&format!(" {d}->{}", self.arrival)),
            None => s.push_str(&format!(" {}", self.arrival)),
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
    out.arrival = flight[4].to_owned();
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
        assert_eq!(g.arrival, "KDEN");
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
        assert_eq!(g.arrival, "LPPT");
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
        assert_eq!(g.arrival, "PHNL");
        assert_eq!(g.gate, "G5");
        assert_eq!(g.gate_status.as_deref(), Some("AVAIL"));
        assert_eq!(g.gate_available_time.as_deref(), Some("06:30"));
    }

    #[test]
    fn united_gate_change_skips_crew_alert_line() {
        let g = parse(UA_CHANGE).unwrap();
        assert_eq!(g.kind, GateKind::GateChange);
        assert_eq!(g.departure.as_deref(), Some("KSTL"));
        assert_eq!(g.arrival, "KSFO");
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
        assert_eq!(g.arrival, "MSN");
        assert_eq!(g.eta.as_deref(), Some("0542"));
        assert_eq!(g.gate, "16");
        let g = parse(RPA_L27).unwrap();
        assert_eq!((g.arrival.as_str(), g.gate.as_str()), ("ORD", "L27"));
    }

    #[test]
    fn skywest_arrival_gate() {
        for (text, airport, gate) in
            [(SKW, "BFL", "4"), (SKW_SIL, "FAT", "7"), (SKW_SAME_LINE, "LAX", "26B")]
        {
            let g = parse(text).unwrap_or_else(|| panic!("no gate in {text:?}"));
            assert_eq!(g.kind, GateKind::ArrivalGate);
            assert_eq!((g.arrival.as_str(), g.gate.as_str()), (airport, gate));
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
}
