//! Label `RA` arrival-gate uplinks: the gate the aircraft will park at on
//! arrival. Ported from airframesio/microservice-event-digger
//! `pkg/handlers/labelra_handler.go`, which reads the same texts.
//!
//! Three layouts are seen in production:
//!
//! * United `GATE ASSIGN` / `GATE CHANGE`. The field lines are often tab
//!   indented, and a gate change repeats `GATE ASSIGN` inside a crew alert,
//!   so the text is read line by line:
//!
//!   ```text
//!   QUNDCULUA~1GATE ASSIGN
//!   	UA0942/21 KIAD LPPT
//!   	SENT: 09:14:42Z
//!   	GATE 43    OPEN
//!   	RAMP FREQ
//!   	OPS FREQ   131.575
//!   	EON        1003Z
//!   ```
//!
//!   The gate is at the destination (`LPPT`); `EON` is the expected on time
//!   there. A gate always has a digit, which tells it apart from the
//!   `GATE ASSIGN` / `GATE CHANGE` lines, and the last gate line wins.
//! * Republic `ARRIVAL GATE`:
//!   `RPA N401YX YX4450 ORD MSN ETA = 0542 GATE = 16` is gate 16 at MSN.
//! * SkyWest `ARR GATE INFO`: a line `BFL ARRIVAL GATE 4`. Everything after
//!   `NEXT FLIGHT INFORMATION` is about the aircraft's next flight.
//!
//! Republic and SkyWest name IATA airports; United names ICAO airports.
//! Finnair `CONNECTION INFO` lists name other flights' departure gates and
//! are not arrival gates, so they are not read.

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// An arrival gate assignment for the flight that receives the uplink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateAssignment {
    /// `united_gate_assign`, `united_gate_change`, `republic_arrival_gate`
    /// or `skywest_arr_gate_info`.
    pub format: &'static str,
    /// The arrival gate, e.g. `B47`, `43`, `16`.
    pub gate: String,
    /// The airport the gate is at: the destination (ICAO for United, IATA
    /// for Republic and SkyWest).
    pub airport: String,
    /// Gate status from the United layout (`OPEN`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Flight as written in the text, e.g. `UA0942`, `YX4450`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight: Option<String>,
    /// Day of month after the United flight number (`UA0942/21`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Registration named in the text (Republic).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<String>,
    /// United `SENT:` time, `hh:mm:ss` UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent: Option<String>,
    /// Expected arrival, `hhmm`: United `EON ...Z` (UTC) or Republic
    /// `ETA = hhmm`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ramp_frequency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops_frequency: Option<String>,
}

impl GateAssignment {
    fn new(format: &'static str, gate: &str, airport: &str) -> Self {
        GateAssignment {
            format,
            gate: gate.to_owned(),
            airport: airport.to_owned(),
            status: None,
            flight: None,
            day: None,
            departure: None,
            destination: None,
            registration: None,
            sent: None,
            eta: None,
            ramp_frequency: None,
            ops_frequency: None,
        }
    }

    /// One-line console summary, e.g. `GATE B47 KDEN UA1720 KMSP->KDEN`.
    pub fn summary(&self) -> String {
        let mut s = format!("GATE {} {}", self.gate, self.airport);
        if let Some(f) = &self.flight {
            s.push_str(&format!(" {f}"));
        }
        if let (Some(d), Some(a)) = (&self.departure, &self.destination) {
            s.push_str(&format!(" {d}->{a}"));
        }
        if let Some(e) = &self.eta {
            s.push_str(&format!(" ETA {e}"));
        }
        s
    }
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("gate regex")
}

// The gate itself: an optional concourse letter or three, digits, an
// optional suffix letter.
const GATE: &str = r"[A-Z]{0,3}\d+[A-Z]?";

static UNITED_HEADER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[A-Z0-9~.]*\s*GATE (ASSIGN|CHANGE)\b"));
static UNITED_FLIGHT: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([A-Z]{2}\d{1,4})/(\d{2})\s+([A-Z0-9]{4})\s+([A-Z0-9]{4})\b"));
static UNITED_SENT: LazyLock<Regex> = LazyLock::new(|| re(r"^SENT:\s*(\d{2}:\d{2}:\d{2})Z"));
static UNITED_GATE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^GATE\s+({GATE})(?:\s+([A-Z]+))?$")));
static UNITED_EON: LazyLock<Regex> = LazyLock::new(|| re(r"^EON\s+(\d{4})Z"));
static UNITED_RAMP: LazyLock<Regex> = LazyLock::new(|| re(r"^RAMP FREQ\s+(\d+\.\d+)"));
static UNITED_OPS: LazyLock<Regex> = LazyLock::new(|| re(r"^OPS FREQ\s+(\d+\.\d+)"));

static REPUBLIC: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?:\b([A-Z]\d[A-Z0-9]{{1,4}})\s+)?\b([A-Z0-9]{{2}}\d{{1,4}})\s+([A-Z]{{3}})\s+([A-Z]{{3}})\s+ETA\s*=\s*(\d{{4}})\s+GATE\s*=\s*({GATE})\b"
    ))
});

static SKYWEST: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^([A-Z]{{3}}) ARRIVAL GATE\s+({GATE})$")));

/// The message's lines with surrounding blanks (tabs, `\r`) removed.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(str::trim)
}

/// United `GATE ASSIGN` / `GATE CHANGE`.
fn united(text: &str) -> Option<GateAssignment> {
    let mut it = lines(text);
    let head = UNITED_HEADER.captures(it.next()?)?;
    let format = if &head[1] == "CHANGE" { "united_gate_change" } else { "united_gate_assign" };

    let mut flight = None;
    let mut sent = None;
    let mut gate: Option<(String, Option<String>)> = None;
    let (mut eta, mut ramp, mut ops) = (None, None, None);
    for l in it {
        if flight.is_none() {
            // The flight and route follow the header; nothing before them
            // is read.
            flight = UNITED_FLIGHT.captures(l);
            continue;
        }
        if sent.is_none() {
            if let Some(c) = UNITED_SENT.captures(l) {
                sent = Some(c[1].to_owned());
                continue;
            }
        }
        if let Some(c) = UNITED_GATE.captures(l) {
            gate = Some((c[1].to_owned(), c.get(2).map(|m| m.as_str().to_owned())));
        } else if let Some(c) = UNITED_EON.captures(l) {
            eta.get_or_insert_with(|| c[1].to_owned());
        } else if let Some(c) = UNITED_RAMP.captures(l) {
            ramp.get_or_insert_with(|| c[1].to_owned());
        } else if let Some(c) = UNITED_OPS.captures(l) {
            ops.get_or_insert_with(|| c[1].to_owned());
        }
    }
    let flight = flight?;
    let (gate, status) = gate?;
    let mut g = GateAssignment::new(format, &gate, &flight[4]);
    g.status = status;
    g.flight = Some(flight[1].to_owned());
    g.day = Some(flight[2].to_owned());
    g.departure = Some(flight[3].to_owned());
    g.destination = Some(flight[4].to_owned());
    g.sent = sent;
    g.eta = eta;
    g.ramp_frequency = ramp;
    g.ops_frequency = ops;
    Some(g)
}

/// Republic `... YX4450 ORD MSN ETA = 0542 GATE = 16`.
fn republic(text: &str) -> Option<GateAssignment> {
    let c = REPUBLIC.captures(text)?;
    let mut g = GateAssignment::new("republic_arrival_gate", &c[6], &c[4]);
    g.registration = c.get(1).map(|m| m.as_str().to_owned());
    g.flight = Some(c[2].to_owned());
    g.departure = Some(c[3].to_owned());
    g.destination = Some(c[4].to_owned());
    g.eta = Some(c[5].to_owned());
    Some(g)
}

/// SkyWest `ARR GATE INFO` / `BFL ARRIVAL GATE 4`.
fn skywest(text: &str) -> Option<GateAssignment> {
    if !text.contains("ARR GATE INFO") {
        return None;
    }
    // What follows is about the aircraft's next flight.
    let current = text.split("NEXT FLIGHT INFORMATION").next().unwrap_or(text);
    lines(current).find_map(|l| {
        let c = SKYWEST.captures(l)?;
        let mut g = GateAssignment::new("skywest_arr_gate_info", &c[2], &c[1]);
        g.destination = Some(c[1].to_owned());
        Some(g)
    })
}

/// Read an arrival gate from a label `RA` uplink text, or `None` when the
/// text is not one of the gate layouts.
pub fn parse(text: &str) -> Option<GateAssignment> {
    if !text.contains("GATE") {
        return None;
    }
    united(text).or_else(|| republic(text)).or_else(|| skywest(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Texts from production RA uplinks (shortened, names removed), the same
    // as event-digger's labelra_gate_test.go.
    const PLAIN: &str = "QUNDCULUA~1GATE ASSIGN\r\nUA1720/09 KMSP KDEN   \r\nSENT: 10:41:16Z \r\nGATE B47   OPEN\r\nRAMP FREQ  130.950\r\nOPS FREQ   \r\nEON        1208Z\r\nGROUND PWR AVAIL -YES-\r\n\r\nPLANNED GROUND TIME \r\nIS 01:36\r\nNEXT FLT UA1127/09\r\nKDEN KSN";
    const TABS: &str = "QUNDCULUA~1GATE ASSIGN\r\n\tUA0942/21 KIAD LPPT\r\n\tSENT: 09:14:42Z\r\n\tGATE 43    OPEN\r\n\tRAMP FREQ  \r\n\tOPS FREQ   131.575\r\n\tEON        1003Z\r\n";
    const CHANGE: &str = "QUNDCULUA~1GATE CHANGE\r\nUA0214/21 KSTL KSFO\r\nSENT: 05:22:32Z\r\n\r\n**** CREW ALERT ****\r\nGATE ASSIGN\r\nHAS CHANGED\r\nGATE D7    OPEN\r\nRAMP FREQ  119.225\r\n";
    const REPUBLIC_TEXT: &str = "QUHDQDDRP~1ARRIVAL GATE\r\nINFORMATION\r\nRPA N401YX YX4450 ORD MSN ETA = 0542 GATE = 16\r\nAIRCRAFT TURNS TO - YX,NEX \r\n";
    const SKYWEST_TEXT: &str = "QUHDQITOO.1ARR GATE INFO   \r\nBFL ARRIVAL GATE 4\r\nNEXT FLIGHT INFORMATION-\r\n** ALL TIMES LOCAL **\r\nAC N939SW\r\n N-A 4634 BFL-SFO 220700\r\nCA X\r\n";
    const SKYWEST_DEN: &str = "QUSILITOO.1ARR GATE INFO   \r\nDEN ARRIVAL GATE A28\r\nNEXT FLIGHT INFORMATION-\r\n";
    const FINNAIR_CONNECT: &str = "QUHELASAY~1\r\nCURRENT FLIGHT\r\nFLIGHT  ESTIMATED ARRIVAL TIME\r\nAY1014   EIBT  UTC  \r\n\r\nCONNECTION INFO\r\nFLIGHT DATE     TIME  TO  GATE \r\nAY0957 20261122 09:35 CPH 31\r\n";

    #[test]
    fn united_plain() {
        let g = parse(PLAIN).unwrap();
        assert_eq!(g.format, "united_gate_assign");
        assert_eq!((g.gate.as_str(), g.airport.as_str()), ("B47", "KDEN"));
        assert_eq!(g.status.as_deref(), Some("OPEN"));
        assert_eq!(g.flight.as_deref(), Some("UA1720"));
        assert_eq!(g.day.as_deref(), Some("09"));
        assert_eq!(g.departure.as_deref(), Some("KMSP"));
        assert_eq!(g.destination.as_deref(), Some("KDEN"));
        assert_eq!(g.sent.as_deref(), Some("10:41:16"));
        assert_eq!(g.eta.as_deref(), Some("1208"));
        assert_eq!(g.ramp_frequency.as_deref(), Some("130.950"));
        assert_eq!(g.ops_frequency, None);
        assert_eq!(g.summary(), "GATE B47 KDEN UA1720 KMSP->KDEN ETA 1208");
    }

    #[test]
    fn united_tab_indented() {
        let g = parse(TABS).unwrap();
        assert_eq!((g.gate.as_str(), g.airport.as_str()), ("43", "LPPT"));
        assert_eq!(g.flight.as_deref(), Some("UA0942"));
        assert_eq!(g.departure.as_deref(), Some("KIAD"));
        assert_eq!(g.sent.as_deref(), Some("09:14:42"));
        assert_eq!(g.eta.as_deref(), Some("1003"));
        assert_eq!(g.ramp_frequency, None);
        assert_eq!(g.ops_frequency.as_deref(), Some("131.575"));
    }

    #[test]
    fn united_gate_change_reads_the_gate_not_the_alert() {
        let g = parse(CHANGE).unwrap();
        assert_eq!(g.format, "united_gate_change");
        assert_eq!((g.gate.as_str(), g.airport.as_str()), ("D7", "KSFO"));
        assert_eq!(g.status.as_deref(), Some("OPEN"));
        assert_eq!(g.departure.as_deref(), Some("KSTL"));
    }

    #[test]
    fn united_last_gate_line_wins() {
        let t = "QUNDCULUA~1GATE CHANGE\r\nUA0214/21 KSTL KSFO\r\nSENT: 05:22:32Z\r\nGATE D5    OPEN\r\n**** CREW ALERT ****\r\nGATE ASSIGN\r\nHAS CHANGED\r\nGATE D7    OPEN\r\n";
        assert_eq!(parse(t).unwrap().gate, "D7");
    }

    #[test]
    fn united_without_gate_or_flight_is_not_read() {
        assert_eq!(parse("QUNDCULUA~1GATE ASSIGN\r\nUA0214/21 KSTL KSFO\r\nSENT: 05:22:32Z\r\n"), None);
        assert_eq!(parse("QUNDCULUA~1GATE ASSIGN\r\nGATE D7    OPEN\r\n"), None);
    }

    #[test]
    fn republic_arrival_gate() {
        let g = parse(REPUBLIC_TEXT).unwrap();
        assert_eq!(g.format, "republic_arrival_gate");
        assert_eq!((g.gate.as_str(), g.airport.as_str()), ("16", "MSN"));
        assert_eq!(g.registration.as_deref(), Some("N401YX"));
        assert_eq!(g.flight.as_deref(), Some("YX4450"));
        assert_eq!(g.departure.as_deref(), Some("ORD"));
        assert_eq!(g.destination.as_deref(), Some("MSN"));
        assert_eq!(g.eta.as_deref(), Some("0542"));
    }

    #[test]
    fn skywest_arr_gate_info() {
        let g = parse(SKYWEST_TEXT).unwrap();
        assert_eq!(g.format, "skywest_arr_gate_info");
        assert_eq!((g.gate.as_str(), g.airport.as_str()), ("4", "BFL"));
        assert_eq!(g.flight, None);
        let g = parse(SKYWEST_DEN).unwrap();
        assert_eq!((g.gate.as_str(), g.airport.as_str()), ("A28", "DEN"));
    }

    #[test]
    fn skywest_next_flight_is_not_read() {
        let t = "QUHDQITOO.1ARR GATE INFO   \r\nNEXT FLIGHT INFORMATION-\r\nSFO ARRIVAL GATE 9\r\n";
        assert_eq!(parse(t), None);
    }

    #[test]
    fn finnair_connection_gates_are_not_arrival_gates() {
        assert_eq!(parse(FINNAIR_CONNECT), None);
    }

    #[test]
    fn layouts_do_not_cross_match() {
        assert_eq!(united(REPUBLIC_TEXT), None);
        assert_eq!(united(SKYWEST_TEXT), None);
        assert_eq!(united(FINNAIR_CONNECT), None);
        assert_eq!(super::republic(PLAIN), None);
        assert_eq!(super::skywest(PLAIN), None);
    }
}
