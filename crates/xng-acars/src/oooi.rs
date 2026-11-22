//! OOOI (OUT / OFF / ON / IN) event extraction from ACARS message text.
//!
//! Many ACARS labels carry the standard gate/wheels flight events plus the
//! departure / destination airports and an estimated time of arrival, in a
//! fixed per-label text layout. This module is a clean-room port of
//! f00b4r0/acarsdec's `label.c` (`DecodeLabel` + the per-label `label_*`
//! helpers) — facts only, reimplemented in safe Rust with bounds checks and
//! field validation that acarsdec's raw `memcpy`s omit.
//!
//! The emitted field names match acarsdec's `output.c` JSON exactly:
//! `depa` (departure airport, `sa`), `dsta` (destination, `da`), `eta`,
//! `gtout` (gate-out, `gout`), `gtin` (gate-in, `gin`), `wloff`
//! (wheels-off, `woff`), `wlin` (wheels-on, `won`).

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// Decoded OOOI fields. Empty fields are omitted from serialization; an
/// all-empty result is reported as `None` by [`decode`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Oooi {
    /// Departure airport (acarsdec `sa`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depa: Option<String>,
    /// Destination airport (acarsdec `da`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsta: Option<String>,
    /// Estimated time of arrival, HHMM UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<String>,
    /// Gate-out (OUT) time, HHMM UTC (acarsdec `gout`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gtout: Option<String>,
    /// Gate-in (IN) time, HHMM UTC (acarsdec `gin`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gtin: Option<String>,
    /// Wheels-off (OFF) time, HHMM UTC (acarsdec `woff`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wloff: Option<String>,
    /// Wheels-on (ON) time, HHMM UTC (acarsdec `won`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wlin: Option<String>,
}

impl Oooi {
    fn is_empty(&self) -> bool {
        self.depa.is_none()
            && self.dsta.is_none()
            && self.eta.is_none()
            && self.gtout.is_none()
            && self.gtin.is_none()
            && self.wloff.is_none()
            && self.wlin.is_none()
    }
}

/// A 4-character airport identifier: letters/digits only, no spaces or
/// padding. Returns `None` (the field is dropped) when the slice is missing
/// or implausible — acarsdec copies the bytes unconditionally; we validate.
fn airport(txt: &[u8], at: usize) -> Option<String> {
    let s = txt.get(at..at + 4)?;
    if s.iter().all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        Some(String::from_utf8_lossy(s).into_owned())
    } else {
        None
    }
}

/// A 4-digit HHMM time field. Validates the hours/minutes range.
fn time4(txt: &[u8], at: usize) -> Option<String> {
    let s = txt.get(at..at + 4)?;
    if !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let hh = (s[0] - b'0') * 10 + (s[1] - b'0');
    let mm = (s[2] - b'0') * 10 + (s[3] - b'0');
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(String::from_utf8_lossy(s).into_owned())
}

/// `txt[at] == c`?
fn at_is(txt: &[u8], at: usize, c: u8) -> bool {
    txt.get(at) == Some(&c)
}

fn starts(txt: &[u8], prefix: &[u8]) -> bool {
    txt.len() >= prefix.len() && &txt[..prefix.len()] == prefix
}

// --- OOOI report formats seen in Airframes production traffic ---
//
// Times in these reports are HHMM or HHMMSS; the fields stay HHMM like
// acarsdec's, so seconds are dropped. Same formats as acars-decoder-typescript
// (Label_80, Label_QE_Report, Label_QE_Station, Label_11Through14_OOOI,
// Label_13Through18_Slash) and microservice-event-digger's oooi_reports.go.

/// A 4- or 6-digit HHMM[SS] value, returned as HHMM.
fn clock(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() != 4 && b.len() != 6 {
        return None;
    }
    if b.len() == 6 && (!b[4].is_ascii_digit() || !b[5].is_ascii_digit() || (b[4] - b'0') > 5) {
        return None;
    }
    time4(b, 0)
}

/// A 4-character ICAO-style airport (`----` and other padding rejected).
fn icao(s: &str) -> Option<String> {
    if s.len() == 4 { airport(s.as_bytes(), 0) } else { None }
}

/// A 3-letter IATA station (`---` and `///` rejected).
fn iata(t: &[u8], at: usize) -> Option<String> {
    let s = t.get(at..at + 3)?;
    if s.iter().all(u8::is_ascii_uppercase) {
        Some(String::from_utf8_lossy(s).into_owned())
    } else {
        None
    }
}

/// Puts `time` (HHMM) in the field for `event` (OUT/OFF/ON/IN).
fn set_event(o: &mut Oooi, event: &str, time: Option<String>) {
    let Some(time) = time else { return };
    match event {
        "OUT" => o.gtout = Some(time),
        "OFF" => o.wloff = Some(time),
        "ON" | "ONN" => o.wlin = Some(time),
        "IN" | "INN" => o.gtin = Some(time),
        _ => {}
    }
}

fn has_time(o: &Oooi) -> bool {
    o.gtout.is_some() || o.wloff.is_some() || o.wlin.is_some() || o.gtin.is_some()
}

fn decode_report(label: &str, text: &str) -> Option<Oooi> {
    match label {
        "80" | "30" | "31" | "32" | "33" | "82" | "8D" => airline_report(text),
        "10" => delta_report(label, text).or_else(|| day_clock_report(text)),
        "11" | "12" | "13" | "14" => delta_report(label, text)
            .or_else(|| numbered_report(text))
            .or_else(|| united_report(text))
            .or_else(|| compact_report(text))
            .or_else(|| day_clock_report(text))
            .or_else(|| csv_version_report(text)),
        "15" => delta_report(label, text).or_else(|| united_report(text)),
        "16" | "17" | "18" => united_report(text),
        "C1" | "RA" => colon_times_report(text),
        "4T" => pushback_report(text),
        "22" => numbered_report(text).or_else(|| label22_csv(text)),
        "44" => label44_report(text),
        "QC" | "QD" | "QE" | "QF" => text_report(text).or_else(|| station_report(label, text)),
        _ => None,
    }
}

/// `1101 OFFRP  0001/21 KDCA/KSEA .N535AS\r\n/OUT 1258/OFF 1318/FOB 0391/ETA 1822`
/// (also SUMMRY, OUTRP, ONRP, INRP; JetBlue prefixes `/FB 0242/AD TJSJ/<pos>,`).
fn airline_report(text: &str) -> Option<Oooi> {
    let first = text.split(['\r', '\n']).next().unwrap_or("");
    let (kw_at, kw) = ["OUTRP", "OFFRP", "ONRP", "INRP", "SUMMRY"].iter().find_map(|kw| {
        first.match_indices(kw).find_map(|(i, _)| {
            let before_ok = i == 0 || first.as_bytes()[i - 1] == b' ';
            let after_ok = first.as_bytes().get(i + kw.len()) == Some(&b' ');
            (before_ok && after_ok).then_some((i, *kw))
        })
    })?;
    let after = &text[kw_at + kw.len()..];
    let mut words = after.split_whitespace();
    let flight_day = words.next()?;
    let route = words.next()?;
    if !flight_day.contains('/') {
        return None;
    }
    let (dep, arr) = route.split_once('/')?;
    let mut o = Oooi {
        depa: icao(dep),
        dsta: icao(arr),
        ..Oooi::default()
    };
    let rest = &after[after.find(route)? + route.len()..];
    for tag in rest.split('/') {
        let mut kv = tag.split_whitespace();
        let (Some(key), Some(val)) = (kv.next(), kv.next()) else { continue };
        match key {
            "OUT" | "OFF" | "ON" | "IN" => set_event(&mut o, key, clock(val)),
            "ETA" => o.eta = clock(val),
            _ => {}
        }
    }
    has_time(&o).then_some(o)
}

/// `OUT01    ETH309/22220151HKJKHAAB\r\n0151 ...` on labels 11-14, and
/// `OFF01YX5763/21212240KDCAKLGA224047  77` (HHMMSS inline) on label 22.
fn numbered_report(text: &str) -> Option<Oooi> {
    let text = strip_block_header(text);
    let event = ["OUT", "OFF", "ONN", "INN"].into_iter().find(|e| text.starts_with(&format!("{e}01")))?;
    let slash = text.find('/')?;
    let flight = &text[5..slash];
    let flight_ok = flight.bytes().all(|c| c == b' ' || c.is_ascii_alphanumeric());
    if flight.is_empty() || flight.len() > 10 || !flight_ok {
        return None;
    }
    let t = &text.as_bytes()[slash + 1..];
    // flight day, report day, report time
    if t.len() < 16 || !t[..8].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut o = Oooi {
        depa: airport(t, 8),
        dsta: airport(t, 12),
        ..Oooi::default()
    };
    o.depa.as_ref()?;
    let tail = text.get(slash + 17..)?;
    let time = if let Some(line) = tail.strip_prefix("\r\n").or_else(|| tail.strip_prefix('\n')) {
        let line = line.strip_prefix('\t').unwrap_or(line);
        line.get(..4).and_then(clock)
    } else {
        tail.get(..6).and_then(clock)
    };
    set_event(&mut o, event, time);
    has_time(&o).then_some(o)
}

/// Label 22 `OFF02\r\nKOAK,KLAS,1839,17.2`.
fn label22_csv(text: &str) -> Option<Oooi> {
    let body = text.strip_prefix("OFF02")?.trim_start_matches(['\r', '\n']);
    let f: Vec<&str> = body.split(',').collect();
    if f.len() < 3 {
        return None;
    }
    let mut o = Oooi { depa: icao(f[0]), dsta: icao(f[1]), ..Oooi::default() };
    set_event(&mut o, "OFF", clock(f[2]));
    has_time(&o).then_some(o)
}

/// Label 44 `OFF02,N35575W115081,KHND,KPSP,1121,0458,0530,016.8`
/// (position, departure, arrival, MMDD, event time; OFF also has an ETA).
fn label44_report(text: &str) -> Option<Oooi> {
    let body = text.strip_prefix("00").unwrap_or(text);
    let f: Vec<&str> = body.split(',').collect();
    if f.len() < 6 {
        return None;
    }
    let event = ["OFF", "ON", "IN"].into_iter().find(|e| {
        f[0].len() == e.len() + 2 && f[0].starts_with(e) && matches!(&f[0][e.len()..], "01" | "02" | "03")
    })?;
    let mut o = Oooi { depa: icao(f[2]), dsta: icao(f[3]), ..Oooi::default() };
    set_event(&mut o, event, clock(f[5]));
    if event == "OFF" {
        o.eta = f.get(6).and_then(|v| clock(v));
    }
    has_time(&o).then_some(o)
}

/// `-------------------OUT REPORT\r\nDEP RCKU,OUT 1118,FOB 106` on QE/QF/QC/QD,
/// also `M77ASC4873---OUT REPORT,DEP ZSQD,...` and `...INNRPT\n\tDES ZGSZ,...`.
fn text_report(text: &str) -> Option<Oooi> {
    let s = match text.find('-') {
        Some(i) if i == 0 || i == 10 => text[i..].trim_start_matches('-'),
        _ => text,
    };
    let event = ["OUT", "OFF", "ONN", "INN", "ON", "IN"].into_iter().find(|e| s.starts_with(e))?;
    let s = s[event.len()..].trim_start_matches(' ');
    let body = s.strip_prefix("REPORT").or_else(|| s.strip_prefix("RPT"))?;
    let mut o = Oooi::default();
    for field in body.split(',') {
        let mut kv = field.split_whitespace();
        let (Some(key), Some(val)) = (kv.next(), kv.next()) else { continue };
        match key {
            "DEP" => o.depa = icao(val),
            "DES" => o.dsta = icao(val),
            "ETA" => o.eta = clock(val),
            "OUT" | "OFF" | "ON" | "IN" => set_event(&mut o, key, clock(val)),
            _ => {}
        }
    }
    has_time(&o).then_some(o)
}

/// Short layout with 3-letter IATA stations (`---`/`///` when unknown):
/// QE `MEL2019       60WSI` (OUT, destination at 16), QF `PHL1412GSO...`
/// (OFF, destination at 7), QC `IAD2128` (ON), QD `SFO164100792` (IN).
/// The station goes in `depa` as in acarsdec's 4-character QC/QD layouts.
fn station_report(label: &str, text: &str) -> Option<Oooi> {
    let t = text.as_bytes();
    let head = t.get(..3)?;
    if !(head.iter().all(u8::is_ascii_uppercase) || head == b"---" || head == b"///") {
        return None;
    }
    let time = time4(t, 3)?;
    let mut o = Oooi { depa: iata(t, 0), ..Oooi::default() };
    match label {
        "QE" => {
            o.gtout = Some(time);
            o.dsta = iata(t, 16);
        }
        "QF" => {
            o.wloff = Some(time);
            o.dsta = iata(t, 7);
        }
        "QC" => o.wlin = Some(time),
        _ => o.gtin = Some(time),
    }
    Some(o)
}

/// United slash reports: `/14 OFF EVENT      / KIAD KMCO 21 145718/TIME 1457`,
/// `/17 POST RPT       / KORD KAUS 21 214146/TIME 2140` (IN),
/// `/18 POST TIMES SECS/ KSFO KDFW 20 042934/IN TIME0428` (IN) and
/// `/18 FLT TIMES SECS / DGAA KIAD 20 094735\r\n/M13 OUT TIME 221610...`.
fn united_report(text: &str) -> Option<Oooi> {
    let parts: Vec<&str> = text.splitn(4, '/').collect();
    if parts.len() < 3 || !parts[0].is_empty() {
        return None;
    }
    let kind: Vec<&str> = parts[1].split_whitespace().collect();
    let route: Vec<&str> = parts[2].split_whitespace().collect();
    if kind.len() < 2 || route.len() < 2 || !matches!(kind[0], "13" | "14" | "15" | "16" | "17" | "18") {
        return None;
    }
    let mut o = Oooi { depa: icao(route[0]), dsta: icao(route[1]), ..Oooi::default() };
    let reported = || {
        let v = parts.get(3)?.trim_start();
        let v = v.strip_prefix("IN ").unwrap_or(v).strip_prefix("TIME")?.trim_start();
        v.get(..4).and_then(clock)
    };
    match kind[1..] {
        [e @ ("OUT" | "OFF" | "ON" | "IN"), "EVENT"] => set_event(&mut o, e, reported()),
        ["POST", "RPT"] if kind[0] == "17" => set_event(&mut o, "IN", reported()),
        ["POST", "TIMES", "SECS"] if kind[0] == "18" => set_event(&mut o, "IN", reported()),
        ["FLT", "TIMES", "SECS"] if kind[0] == "18" => {
            for line in text.lines() {
                let w: Vec<&str> = line.split_whitespace().collect();
                if let [m, e, "TIME", v] = w[..] {
                    if matches!(m, "/M13" | "/M14" | "/M15" | "/M16") {
                        set_event(&mut o, e, clock(v));
                    }
                }
            }
        }
        _ => return None,
    }
    has_time(&o).then_some(o)
}

// --- More airline layouts (production samples 2026-11-21/22) ---

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid OOOI regex")
}

/// Drops a C-Band `M<nn>A<airline><flight>` block header
/// (`M99ANH0218INN01...`, `M70ADL0065201915 ...`).
fn strip_block_header(text: &str) -> &str {
    let t = text.as_bytes();
    let is_header = t.len() > 10
        && t[0].is_ascii_uppercase()
        && t[1].is_ascii_digit()
        && t[2].is_ascii_digit()
        && t[3].is_ascii_uppercase()
        && t[4..6].iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && t[6..10].iter().all(u8::is_ascii_digit);
    if is_header { &text[10..] } else { text }
}

static COMPACT_OUT: LazyLock<Regex> = LazyLock::new(|| re(r"^OUT01(\d{4})\d{4}([A-Z]{4})([A-Z]{4})"));
static COMPACT_OFF: LazyLock<Regex> =
    LazyLock::new(|| re(r"^OFF01(\d{4})DIA\d{2}([A-Z]{4})([A-Z]{4})(\d{4})?"));
static COMPACT_ON_IN: LazyLock<Regex> = LazyLock::new(|| re(r"^(ONN|INN)01(\d{4})([A-Z]{4}|-{4})"));

/// British Airways, the numbered form without the flight and `/`:
/// 11 `OUT0105400525LSZHEGLL`, 12 `OFF010247DIA02KEWREGLL0859BAB708...` (ETA),
/// 13 `ONN010223CYVR`, 14 `INN010611EGLL 484 480  56  54` / `INN011053----`.
fn compact_report(text: &str) -> Option<Oooi> {
    let mut o = Oooi::default();
    if let Some(c) = COMPACT_OUT.captures(text) {
        o.depa = icao(&c[2]);
        o.dsta = icao(&c[3]);
        set_event(&mut o, "OUT", clock(&c[1]));
    } else if let Some(c) = COMPACT_OFF.captures(text) {
        o.depa = icao(&c[2]);
        o.dsta = icao(&c[3]);
        o.eta = c.get(4).and_then(|m| clock(m.as_str()));
        set_event(&mut o, "OFF", clock(&c[1]));
    } else if let Some(c) = COMPACT_ON_IN.captures(text) {
        o.dsta = icao(&c[3]);
        set_event(&mut o, &c[1], clock(&c[2]));
    }
    has_time(&o).then_some(o)
}

static DAY_CLOCK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(OUT|OFF|ONN|INN)\d{2}A(0[1-9]|[12]\d|3[01])(\d{6})([A-Z]{4})(?:([A-Z]{4})\d{6}|\d{4}([A-Z]{4})?)")
});

/// JAL (label 10) and ANA (label 11): event, version, `A`, day, HHMMSS:
/// `OUT04A21204409KSFO2044RJTT`, `ONN04A21200226YSSY2002`,
/// `OFF01A21070436RJSSRJCC070421`. OUT/OFF name the departure then the
/// arrival, ON/IN the arrival.
fn day_clock_report(text: &str) -> Option<Oooi> {
    let c = DAY_CLOCK.captures(strip_block_header(text))?;
    let mut o = Oooi::default();
    let second = c.get(5).or(c.get(6)).and_then(|m| icao(m.as_str()));
    if matches!(&c[1], "OUT" | "OFF") {
        o.depa = icao(&c[4]);
        o.dsta = second;
    } else {
        o.dsta = icao(&c[4]);
    }
    set_event(&mut o, &c[1], clock(&c[3]));
    has_time(&o).then_some(o)
}

static DELTA_HEADER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(0[1-9]|[12]\d|3[01])(?:[01]\d|2[0-3])(1[0-5]) ([A-Z0-9]{4}) ([A-Z0-9]{4})\d\r?\n\t?"));
static DELTA_OUT: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}(\d{4})\d{4}\d{8}"));
static DELTA_OFF: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}(\d{4})\d{4}[\d ]{12}(\d{4})\d{4}"));
static DELTA_ON: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}(\d{4})\d{4} *(?:\r?\n|$)"));
static DELTA_IN: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}(\d{4})[\d ]{4}[\d ]{4} *$"));
static DELTA_SUMMARY: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}(\d{4})[\d ]{4}\S"));

/// Delta: `<DD><HH><label> <dep> <arr><n>` then four-digit fields:
/// 13 OUT `210513 KLAX KATL6\r\n07830550038005490542` (flight, OUT, fuel, ...),
/// 10 OFF `...\r\n07830603037905520549054205500550` (flight, OFF, fuel, three
/// times, OUT, OUT), 15 ON `...\r\n081805460086` (flight, ON, fuel),
/// 14 IN `...\r\n0694062900810084`, 11 IN with the ON time last on the line.
/// Checked on flights that sent several: DL816 L15 0045, L14 0056, L11
/// `0056 ... 0045`; NW783 L13 0550 repeated as the OUT of its L10 at 0603.
fn delta_report(label: &str, text: &str) -> Option<Oooi> {
    let text = strip_block_header(text);
    let h = DELTA_HEADER.captures(text)?;
    if &h[2] != label {
        return None;
    }
    let body = &text[h.get(0)?.end()..];
    let mut o = Oooi { depa: icao(&h[3]), dsta: icao(&h[4]), ..Oooi::default() };
    match label {
        "13" => set_event(&mut o, "OUT", clock(&DELTA_OUT.captures(body)?[1])),
        "10" => {
            let c = DELTA_OFF.captures(body)?;
            set_event(&mut o, "OFF", clock(&c[1]));
            set_event(&mut o, "OUT", clock(&c[2]));
        }
        "15" => set_event(&mut o, "ON", clock(&DELTA_ON.captures(body)?[1])),
        "14" => {
            let line = body.split(['\r', '\n']).next().unwrap_or("");
            set_event(&mut o, "IN", clock(&DELTA_IN.captures(line)?[1]));
        }
        "11" => {
            let line = body.split(['\r', '\n']).next().unwrap_or("").trim_end_matches(' ');
            set_event(&mut o, "IN", clock(&DELTA_SUMMARY.captures(line)?[1]));
            if line.len() >= 16 {
                set_event(&mut o, "ON", line.get(line.len() - 4..).and_then(clock));
            }
        }
        _ => return None,
    }
    has_time(&o).then_some(o)
}

static CSV_VERSION: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(OUT|ONE),V\d{2},[^,]*,([A-Z0-9-]{4}),([A-Z0-9-]{4}),(\d{4})\b"));

/// Cathay Pacific `OUT,V02,CX 837 20261121 1,CYVR,VHHH,2044,2036,----, 1179`
/// and `ONE,V02,CX 105 20261122 1,VHHH,YMML,0124,   96`.
fn csv_version_report(text: &str) -> Option<Oooi> {
    let c = CSV_VERSION.captures(text)?;
    let event = match &c[1] {
        "OUT" => "OUT",
        _ => "ON",
    };
    let mut o = Oooi { depa: icao(&c[2]), dsta: icao(&c[3]), ..Oooi::default() };
    set_event(&mut o, event, clock(&c[4]));
    has_time(&o).then_some(o)
}

static COLON_TIMES: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\bOUT:\s*([\d-]{4})\s+OFF:\s*([\d-]{4})\s*\r?\n\s*IN:\s*([\d-]{4})\s+ON:\s*([\d-]{4})")
});

/// SkyWest end of flight report (uplink, C1 or RA):
/// `OUT: 0538    OFF: 0551\r\n  IN: 0712     ON: 0649\r\nDOOR: 0713`.
fn colon_times_report(text: &str) -> Option<Oooi> {
    let c = COLON_TIMES.captures(text)?;
    let mut o = Oooi::default();
    set_event(&mut o, "OUT", clock(&c[1]));
    set_event(&mut o, "OFF", clock(&c[2]));
    set_event(&mut o, "IN", clock(&c[3]));
    set_event(&mut o, "ON", clock(&c[4]));
    has_time(&o).then_some(o)
}

static PUSHBACK: LazyLock<Regex> =
    LazyLock::new(|| re(r"^PSHBK [A-Z0-9]{3,8}/(?:0[1-9]|[12]\d|3[01])/(?:0[1-9]|[12]\d|3[01]) ([A-Z]{3}) (\d{4})Z"));

/// Air Canada pushback `PSHBK AC0281/21/22 YVR 0603Z/    Z/    Z/0605Z`
/// (label 4T) as the OUT time; the station is IATA, so it goes in `depa`
/// like the QE/QF short layouts.
fn pushback_report(text: &str) -> Option<Oooi> {
    let c = PUSHBACK.captures(text)?;
    let mut o = Oooi { depa: Some(c[1].to_string()), ..Oooi::default() };
    set_event(&mut o, "OUT", clock(&c[2]));
    has_time(&o).then_some(o)
}

/// Extract OOOI fields for `label` from message `text`. Returns `None` when
/// the label is not OOOI-bearing or no plausible field is found.
pub fn decode(label: &str, text: &str) -> Option<Oooi> {
    // Report formats seen in production that acarsdec's fixed offsets do
    // not cover; fall through to those offsets when none matches.
    if let Some(o) = decode_report(label, text) {
        return Some(o);
    }

    let t = text.as_bytes();
    let mut o = Oooi::default();

    match label {
        // --- Q-series gate/wheels events (acarsdec label_q*) ---
        "Q1" => {
            o.depa = airport(t, 0);
            o.gtout = time4(t, 4);
            o.wloff = time4(t, 8);
            o.wlin = time4(t, 12);
            o.gtin = time4(t, 16);
            o.dsta = airport(t, 24);
        }
        "Q2" => {
            o.depa = airport(t, 0);
            o.eta = time4(t, 4);
        }
        "QA" => {
            o.depa = airport(t, 0);
            o.gtout = time4(t, 4);
        }
        "QB" => {
            o.depa = airport(t, 0);
            o.wloff = time4(t, 4);
        }
        "QC" => {
            o.depa = airport(t, 0);
            o.wlin = time4(t, 4);
        }
        "QD" => {
            o.depa = airport(t, 0);
            o.gtin = time4(t, 4);
        }
        "QE" => {
            o.depa = airport(t, 0);
            o.gtout = time4(t, 4);
            o.dsta = airport(t, 8);
        }
        "QF" => {
            o.depa = airport(t, 0);
            o.wloff = time4(t, 4);
            o.dsta = airport(t, 8);
        }
        "QG" => {
            o.depa = airport(t, 0);
            o.gtout = time4(t, 4);
            o.gtin = time4(t, 8);
        }
        "QH" => {
            o.depa = airport(t, 0);
            o.gtout = time4(t, 4);
        }
        "QK" => {
            o.depa = airport(t, 0);
            o.wlin = time4(t, 4);
            o.dsta = airport(t, 8);
        }
        "QL" => {
            o.dsta = airport(t, 0);
            o.gtin = time4(t, 8);
            o.depa = airport(t, 13);
        }
        "QM" => {
            o.dsta = airport(t, 0);
            o.depa = airport(t, 8);
        }
        "QN" => {
            o.dsta = airport(t, 4);
            o.eta = time4(t, 8);
        }
        "QP" => {
            o.depa = airport(t, 0);
            o.dsta = airport(t, 4);
            o.gtout = time4(t, 8);
        }
        "QQ" => {
            o.depa = airport(t, 0);
            o.dsta = airport(t, 4);
            o.wloff = time4(t, 8);
        }
        "QR" => {
            o.depa = airport(t, 0);
            o.dsta = airport(t, 4);
            o.wlin = time4(t, 8);
        }
        "QS" => {
            o.depa = airport(t, 0);
            o.dsta = airport(t, 4);
            o.gtin = time4(t, 8);
        }
        "QT" => {
            o.depa = airport(t, 0);
            o.dsta = airport(t, 4);
            o.gtout = time4(t, 8);
            o.gtin = time4(t, 12);
        }

        // --- Airline-application labels with embedded OOOI (acarsdec) ---
        "10" => {
            if !starts(t, b"ARR01") {
                return None;
            }
            o.dsta = airport(t, 12);
            o.eta = time4(t, 16);
        }
        "11" => {
            if t.get(13..17) != Some(b"/DS ") {
                return None;
            }
            o.dsta = airport(t, 17);
            if t.get(21..26) != Some(b"/ETA ") {
                return None;
            }
            o.eta = time4(t, 26);
        }
        "12" | "1G" | "83" => {
            if !at_is(t, 4, b',') {
                return None;
            }
            o.depa = airport(t, 0);
            o.dsta = airport(t, 5);
        }
        "15" => {
            if !starts(t, b"FST01") {
                return None;
            }
            o.depa = airport(t, 5);
            o.dsta = airport(t, 9);
        }
        "17" => {
            if !starts(t, b"ETA ") {
                return None;
            }
            o.eta = time4(t, 4);
            if !at_is(t, 8, b',') {
                return None;
            }
            o.depa = airport(t, 9);
            if !at_is(t, 13, b',') {
                return None;
            }
            o.dsta = airport(t, 14);
        }
        "20" => {
            if !starts(t, b"RST") {
                return None;
            }
            o.depa = airport(t, 22);
            o.dsta = airport(t, 26);
        }
        "21" => {
            if !at_is(t, 6, b',') {
                return None;
            }
            o.depa = airport(t, 7);
            if !at_is(t, 11, b',') {
                return None;
            }
            o.dsta = airport(t, 12);
        }
        "2N" => {
            if !starts(t, b"TKO01") || !at_is(t, 11, b'/') {
                return None;
            }
            o.depa = airport(t, 20);
            o.dsta = airport(t, 24);
        }
        "2Z" => {
            o.dsta = airport(t, 0);
        }
        "33" => {
            if !at_is(t, 0, b',') || !at_is(t, 20, b',') {
                return None;
            }
            o.depa = airport(t, 21);
            if !at_is(t, 25, b',') {
                return None;
            }
            o.dsta = airport(t, 26);
        }
        "39" => {
            if !starts(t, b"GTA01") || !at_is(t, 15, b'/') {
                return None;
            }
            o.depa = airport(t, 24);
            o.dsta = airport(t, 28);
        }
        "45" => {
            if !at_is(t, 0, b'A') {
                return None;
            }
            o.dsta = airport(t, 1);
        }
        "80" => {
            if t.get(6..11) != Some(b"/DEST") || !at_is(t, 11, b'/') {
                return None;
            }
            o.dsta = airport(t, 12);
        }
        "8D" => {
            if !at_is(t, 4, b',') || !at_is(t, 35, b',') {
                return None;
            }
            o.depa = airport(t, 36);
            if !at_is(t, 40, b',') {
                return None;
            }
            o.dsta = airport(t, 41);
        }
        "8E" | "8S" => {
            if !at_is(t, 4, b',') {
                return None;
            }
            o.dsta = airport(t, 0);
            o.eta = time4(t, 5);
        }

        _ => return None,
    }

    if o.is_empty() { None } else { Some(o) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference strings are the real documented examples from airframes'
    // acars-message-documentation; field offsets/semantics are acarsdec's
    // label.c. We assert the airport codes and times against the airports
    // stated in those docs (cross-verified, not invented).

    #[test]
    fn qf_off_destination_report() {
        // QF layout (acarsdec label_qf): depa(4) wloff(4) dsta(4), 4-char
        // ICAO codes. (The research/QF.md "EWR2210ATL" example uses 3-char
        // FAA codes, which do not fit acarsdec's 4-char-ICAO offsets and is
        // correctly rejected by field validation.)
        let o = decode("QF", "KEWR2210KATL").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KEWR"));
        assert_eq!(o.wloff.as_deref(), Some("2210"));
        assert_eq!(o.dsta.as_deref(), Some("KATL"));
    }

    #[test]
    fn qf_three_char_iata_station_layout() {
        // research/QF.md "EWR2210ATL": the 3-letter-station layout, which
        // is also most of the production QF traffic ("PHL1412GSOOS GSO/...").
        let o = decode("QF", "EWR2210ATL").unwrap();
        assert_eq!(o.depa.as_deref(), Some("EWR"));
        assert_eq!(o.wloff.as_deref(), Some("2210"));
        assert_eq!(o.dsta.as_deref(), Some("ATL"));
    }

    #[test]
    fn qq_off_report() {
        // research/QQ.md "without position": "KEWRKSWF20041942" —
        // origin KEWR, dest KSWF, then 2004 (HH:MM).
        let o = decode("QQ", "KEWRKSWF20041942").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KEWR"));
        assert_eq!(o.dsta.as_deref(), Some("KSWF"));
        assert_eq!(o.wloff.as_deref(), Some("2004"));
    }

    #[test]
    fn qq_off_report_with_destination_only() {
        // research/QQ.md: "KEWRKDFW1829OS KDFW ..." — KEWR → KDFW, 18:29.
        let o = decode("QQ", "KEWRKDFW1829OS KDFW /FUL0306/MO 1816/APH 0000000").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KEWR"));
        assert_eq!(o.dsta.as_deref(), Some("KDFW"));
        assert_eq!(o.wloff.as_deref(), Some("1829"));
    }

    #[test]
    fn q2_eta_report() {
        // QA-family layout: 4-char airport then HHMM ETA.
        let o = decode("Q2", "KSFO0830").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KSFO"));
        assert_eq!(o.eta.as_deref(), Some("0830"));
    }

    #[test]
    fn qp_out_report_full() {
        // QP layout (acarsdec label_qp): depa(4) dsta(4) gtout(4).
        let o = decode("QP", "KLAXKJFK1305").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KLAX"));
        assert_eq!(o.dsta.as_deref(), Some("KJFK"));
        assert_eq!(o.gtout.as_deref(), Some("1305"));
    }

    #[test]
    fn qs_in_report_full() {
        let o = decode("QS", "KLAXKJFK2247").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KLAX"));
        assert_eq!(o.dsta.as_deref(), Some("KJFK"));
        assert_eq!(o.gtin.as_deref(), Some("2247"));
    }

    #[test]
    fn label_20_rst() {
        // acarsdec label_20: requires "RST" prefix, then depa@22, dsta@26.
        // Construct a 30-char RST record with KORD/KSFO at those offsets.
        let txt = "RST0000000000000000000KORDKSFO";
        let o = decode("20", txt).unwrap();
        assert_eq!(o.depa.as_deref(), Some("KORD"));
        assert_eq!(o.dsta.as_deref(), Some("KSFO"));
    }

    #[test]
    fn non_oooi_label_is_none() {
        assert!(decode("H1", "#DFB engine data").is_none());
        assert!(decode("SA", "0EV121314V").is_none());
        // QF with garbage that fails validation entirely.
        assert!(decode("QF", "??").is_none());
    }

    #[test]
    fn empty_link_test_yields_nothing() {
        // Q0 is not OOOI-bearing.
        assert!(decode("Q0", "").is_none());
    }

    // --- Report formats from Airframes production traffic (Nov 21-22 2026) ---

    fn times(o: &Oooi) -> [Option<&str>; 4] {
        [o.gtout.as_deref(), o.wloff.as_deref(), o.wlin.as_deref(), o.gtin.as_deref()]
    }

    #[test]
    fn airline_offrp_label_80() {
        let o = decode(
            "80",
            "1101 OFFRP  0001/21 KDCA/KSEA .N535AS\r\n/OUT 1258/OFF 1318/FOB 0391/ETA 1822/CAB 1254",
        )
        .unwrap();
        assert_eq!(o.depa.as_deref(), Some("KDCA"));
        assert_eq!(o.dsta.as_deref(), Some("KSEA"));
        assert_eq!(times(&o), [Some("1258"), Some("1318"), None, None]);
        assert_eq!(o.eta.as_deref(), Some("1822"));
    }

    #[test]
    fn airline_summary_label_80() {
        let o = decode(
            "80",
            "3501 SUMMRY 3390/20 KPHX/KILN .N337AZ\r\n/OUT 1600/FOB 0541\r\n/OFF 1615/FOB ----\r\n/ON  1930/FOB ----\r\n/IN  1937/FOB 0224",
        )
        .unwrap();
        assert_eq!(times(&o), [Some("1600"), Some("1615"), Some("1930"), Some("1937")]);
    }

    #[test]
    fn airline_single_line_outrp() {
        let o = decode("80", "1001 OUTRP 1925/21 KBOS/MMGL .XA-VSX/OUT 1245/FOB  171").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KBOS"));
        assert_eq!(o.dsta.as_deref(), Some("MMGL"));
        assert_eq!(o.gtout.as_deref(), Some("1245"));
    }

    #[test]
    fn airline_reports_on_jetblue_labels() {
        let out = decode(
            "30",
            " /FB 0069/AD ----/N 42.372,W 71.028,1001 OUTRP  0940/22 KBOS/---- .N2086J\r\n/OUT 0348/FOB 0069",
        )
        .unwrap();
        assert_eq!(out.depa.as_deref(), Some("KBOS"));
        assert_eq!(out.dsta, None);
        assert_eq!(out.gtout.as_deref(), Some("0348"));

        let off = decode(
            "31",
            " /FB 0242/AD TJSJ/N 28.420,W 81.307,1101 OFFRP  2133/22 KMCO/TJSJ .N506JB\r\n/OUT 0328/OFF 0349/FOB 0242/ETA 0618",
        )
        .unwrap();
        assert_eq!(times(&off), [Some("0328"), Some("0349"), None, None]);
        assert_eq!(off.eta.as_deref(), Some("0618"));

        let on = decode(
            "32",
            "/FB 0131/AD KBOS/N4222.1,W07100.2,1201 ONRP   0150/22 KSAV/KBOS .N3189J\r\n/ON  0328/FOB 0131",
        )
        .unwrap();
        assert_eq!(on.wlin.as_deref(), Some("0328"));

        let inn = decode(
            "33",
            " /FB 0080/AD KBOS/N 42.371,W 71.006,1301 INRP   2166/21 KRSW/KBOS .N558JB\r\n/ON  1547/IN  1552/FOB 0080",
        )
        .unwrap();
        assert_eq!(inn.depa.as_deref(), Some("KRSW"));
        assert_eq!(times(&inn), [None, None, Some("1547"), Some("1552")]);
    }

    #[test]
    fn airline_inrp_label_8d_and_outrp_label_82() {
        let o = decode("8D", "13H3 INRP   2513/21 EKCH/EGPH .OY-KBR\r\n/ON 1825/IN 1828").unwrap();
        assert_eq!(o.depa.as_deref(), Some("EKCH"));
        assert_eq!(times(&o), [None, None, Some("1825"), Some("1828")]);

        let o = decode("82", "1001 OUTRP  0602/21 YSSY/RKSI .HL7626\r\n/OUT 2347/FOB 3084").unwrap();
        assert_eq!(o.gtout.as_deref(), Some("2347"));
    }

    #[test]
    fn label_80_position_report_is_not_oooi() {
        assert!(decode("80", "3N01 POSRPT 0581/27 KIAD/MSLP .N962AV\r\n/POS N38541W077244").is_none());
    }

    #[test]
    fn numbered_reports_labels_11_to_14() {
        let o = decode("11", "OUT01    ETH309/22220151HKJKHAAB\r\n0151 16510600").unwrap();
        assert_eq!(o.depa.as_deref(), Some("HKJK"));
        assert_eq!(o.dsta.as_deref(), Some("HAAB"));
        assert_eq!(o.gtout.as_deref(), Some("0151"));

        let o = decode("12", "OFF01    CSN654/21212021EGKKZHCC\r\n2021 5490936  98").unwrap();
        assert_eq!(o.wloff.as_deref(), Some("2021"));

        // the ON time is the first four digits, not 002101
        let o = decode("13", "ONN01   WJA2251/21220021MMPRCYYC\r\n00210167").unwrap();
        assert_eq!(o.wlin.as_deref(), Some("0021"));

        let o = decode("14", "INN01UAE179    /21211019OMDBEPWA\r\n1017 1761011").unwrap();
        assert_eq!(o.gtin.as_deref(), Some("1017"));

        // British Airways sends it without the flight and slash
        let o = decode("13", "ONN011317EGJB").unwrap();
        assert_eq!(o.dsta.as_deref(), Some("EGJB"));
        assert_eq!(o.wlin.as_deref(), Some("1317"));
    }

    #[test]
    fn british_airways_compact_reports() {
        let o = decode("11", "OUT0105400525LSZHEGLL").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("LSZH"), Some("EGLL")));
        assert_eq!(o.gtout.as_deref(), Some("0540"));

        let o = decode("12", "OFF010247DIA02KEWREGLL0859BAB7080926130450READY ").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("KEWR"), Some("EGLL")));
        assert_eq!(o.wloff.as_deref(), Some("0247"));
        assert_eq!(o.eta.as_deref(), Some("0859"));

        let o = decode("14", "INN010611EGLL 484 480  56  54").unwrap();
        assert_eq!(o.dsta.as_deref(), Some("EGLL"));
        assert_eq!(o.gtin.as_deref(), Some("0611"));

        let o = decode("14", "INN011053----").unwrap();
        assert_eq!(o.dsta, None);
        assert_eq!(o.gtin.as_deref(), Some("1053"));

        assert!(decode("16", "MVT01063100EGLLELLX000048000048021").is_none());
    }

    #[test]
    fn jal_and_ana_day_clock_reports() {
        let o = decode("10", "OUT04A21204409KSFO2044RJTT").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("KSFO"), Some("RJTT")));
        assert_eq!(o.gtout.as_deref(), Some("2044"));

        let o = decode("10", "ONN04A21200226YSSY2002").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (None, Some("YSSY")));
        assert_eq!(o.wlin.as_deref(), Some("2002"));

        let o = decode("10", "M34AJL0111OUT04A22003718RJTT0037RJOO").unwrap();
        assert_eq!(o.gtout.as_deref(), Some("0037"));

        let o = decode("11", "OFF01A21070436RJSSRJCC070421").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("RJSS"), Some("RJCC")));
        assert_eq!(o.wloff.as_deref(), Some("0704"));

        let o = decode("14", "M99ANH0218INN01ANA218    /21212126EDDMRJTT\n\t2124192 ").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("EDDM"), Some("RJTT")));
        assert_eq!(o.gtin.as_deref(), Some("2124"));

        assert!(decode("10", "ETA04M22062329   VTBS090161  340Y").is_none());
    }

    #[test]
    fn delta_reports() {
        let o = decode("13", "210513 KLAX KATL6\r\n07830550038005490542            ").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("KLAX"), Some("KATL")));
        assert_eq!(o.gtout.as_deref(), Some("0550"));
        assert_eq!(o.wloff, None);

        let o = decode("10", "210610 KLAX KATL6\r\n07830603037905520549054205500550            ").unwrap();
        assert_eq!(o.wloff.as_deref(), Some("0603"));
        assert_eq!(o.gtout.as_deref(), Some("0550"));

        let o = decode("10", "212010 KORD KATL6\r\n2894200502051942        19411941000000000000").unwrap();
        assert_eq!(o.wloff.as_deref(), Some("2005"));
        assert_eq!(o.gtout.as_deref(), Some("1941"));

        let o = decode("15", "210015 KSAT KBOS6\r\n081600450059\r\n000000903094901036000000000000001160000148").unwrap();
        assert_eq!(o.wlin.as_deref(), Some("0045"));

        let o = decode("15", "M70ADL0065201915 KLAX NZAA6\n\t006519470293\n\t      391944456540488833007827   ").unwrap();
        assert_eq!(o.dsta.as_deref(), Some("NZAA"));
        assert_eq!(o.wlin.as_deref(), Some("1947"));

        let o = decode("14", "210014 KSAT KBOS6\r\n0816005600560059").unwrap();
        assert_eq!(o.gtin.as_deref(), Some("0056"));

        let o = decode("14", "212214 KJFK KSFO6\r\n03632222        ").unwrap();
        assert_eq!(o.gtin.as_deref(), Some("2222"));

        let o = decode("11", "210111 KSAT KBOS6\r\n081600560056111390  0       0011600001490045\r\n\r\n").unwrap();
        assert_eq!((o.gtin.as_deref(), o.wlin.as_deref()), (Some("0056"), Some("0045")));

        let o = decode("11", "210511 KSLC KBOS8\r\n07020454    111200  0                   0449\r\n").unwrap();
        assert_eq!((o.gtin.as_deref(), o.wlin.as_deref()), (Some("0454"), Some("0449")));

        // a header for another label, a delay remark, a position report
        assert!(decode("13", "210514 KDTW KLAX6\r\n0694062900810084").is_none());
        assert!(decode("12", "212212 KSFO KATL6\r\n2253\r\nLONG LINE OF DEPARTURES\r\n").is_none());
        assert!(decode("17", "210617 KDTW EDDF7\r\n/FN 0086\r\nAGPED   06103900727-602750290039682").is_none());
    }

    #[test]
    fn cathay_skywest_and_air_canada_reports() {
        let o = decode("11", "OUT,V02,CX 837 20261121 1,CYVR,VHHH,2044,2036,----, 1179").unwrap();
        assert_eq!((o.depa.as_deref(), o.dsta.as_deref()), (Some("CYVR"), Some("VHHH")));
        assert_eq!(o.gtout.as_deref(), Some("2044"));

        let o = decode("13", "ONE,V02,CX 105 20261122 1,VHHH,YMML,0124,   96").unwrap();
        assert_eq!(o.wlin.as_deref(), Some("0124"));

        let o = decode(
            "RA",
            "SOULS: 69\r\nCARGO:  1280 ZFW: 64946\r\n\r\n OUT: 0353    OFF: 0409\r\n  IN: 0601     ON: 0551\r\nDOOR: 0603\r\n",
        )
        .unwrap();
        assert_eq!(o.gtout.as_deref(), Some("0353"));
        assert_eq!(o.wloff.as_deref(), Some("0409"));
        assert_eq!(o.wlin.as_deref(), Some("0551"));
        assert_eq!(o.gtin.as_deref(), Some("0601"));

        let o = decode("4T", "PSHBK AC0281/21/22 YVR 0603Z/    Z/    Z/0605Z").unwrap();
        assert_eq!(o.depa.as_deref(), Some("YVR"));
        assert_eq!(o.gtout.as_deref(), Some("0603"));
        assert!(decode("4T", "ETA AC0404/21/21 YUL 1510Z").is_none());
    }

    #[test]
    fn impossible_days_and_unknown_codes_are_not_reports() {
        assert!(decode("13", "992513 KLAX KATL6\r\n07830550038005490542").is_none());
        assert!(decode("13", "219913 KLAX KATL6\r\n07830550038005490542").is_none());
        assert!(decode("10", "OUT04A99204409KSFO2044RJTT").is_none());
        assert!(decode("13", "INC,V02,CX 140 20261122 1,YPPH,VHHH,1556").is_none());
        assert!(decode("13", "ONA,V02,CX 105 20261122 1,VHHH,YMML,0124").is_none());
        assert!(decode("4T", "PSHBK AC0281/99/00 YVR 0000Z/    Z/    Z/0605Z").is_none());
    }

    #[test]
    fn numbered_report_with_non_ascii_tail_does_not_panic() {
        assert!(decode("11", "OUT01ETH309/22220151HKJKHAAé0151").is_none());
    }

    #[test]
    fn label_22_off_reports() {
        let o = decode("22", "OFF01YX5763/21212240KDCAKLGA224047  77").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KDCA"));
        assert_eq!(o.dsta.as_deref(), Some("KLGA"));
        assert_eq!(o.wloff.as_deref(), Some("2240"));

        let o = decode("22", "OFF02\r\nKOAK,KLAS,1839,17.2").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KOAK"));
        assert_eq!(o.wloff.as_deref(), Some("1839"));
    }

    #[test]
    fn label_44_off_report() {
        let o = decode("44", "OFF02,N35575W115081,KHND,KPSP,1121,0458,0530,016.8").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KHND"));
        assert_eq!(o.dsta.as_deref(), Some("KPSP"));
        assert_eq!(o.wloff.as_deref(), Some("0458"));
        assert_eq!(o.eta.as_deref(), Some("0530"));
        assert!(decode("44", "/FB 0083/AD KRDU/N3823.5,W07507.3,INA03,KRDU,2337").is_none());
    }

    #[test]
    fn q_series_text_reports() {
        let o = decode("QE", "-------------------OUT REPORT\r\nDEP RCKU,OUT 1118,FOB 106").unwrap();
        assert_eq!(o.depa.as_deref(), Some("RCKU"));
        assert_eq!(o.gtout.as_deref(), Some("1118"));

        let o = decode("QE", "M77ASC4873-------------------OUT REPORT,DEP ZSQD,OUT 2350,FOB 110").unwrap();
        assert_eq!(o.depa.as_deref(), Some("ZSQD"));
        assert_eq!(o.gtout.as_deref(), Some("2350"));

        let o = decode("QF", "----------OFF REPORT\r\nDEP RCKU,OFF 1124,DES RCSS,FOB 106").unwrap();
        assert_eq!(o.dsta.as_deref(), Some("RCSS"));
        assert_eq!(o.wloff.as_deref(), Some("1124"));

        let o = decode("QC", "-------ON REPORT\r\nDES RCKU,ON 0843,FOB 111").unwrap();
        assert_eq!(o.dsta.as_deref(), Some("RCKU"));
        assert_eq!(o.wlin.as_deref(), Some("0843"));

        let o = decode("QD", "M55AZH9110------------INNRPT\n\tDES ZGSZ,N 22380E113495,IN 165807,FOB ,DEP ZBAA")
            .unwrap();
        assert_eq!(o.depa.as_deref(), Some("ZBAA"));
        assert_eq!(o.dsta.as_deref(), Some("ZGSZ"));
        assert_eq!(o.gtin.as_deref(), Some("1658"));
    }

    #[test]
    fn q_series_iata_station_reports() {
        let o = decode("QE", "MEL2019       60WSI").unwrap();
        assert_eq!(o.depa.as_deref(), Some("MEL"));
        assert_eq!(o.dsta.as_deref(), Some("WSI"));
        assert_eq!(o.gtout.as_deref(), Some("2019"));

        let o = decode("QE", "---1116       29---").unwrap();
        assert_eq!(o.depa, None);
        assert_eq!(o.dsta, None);
        assert_eq!(o.gtout.as_deref(), Some("1116"));

        let o = decode("QF", "PHL1412GSOOS GSO/FUL0628").unwrap();
        assert_eq!(o.dsta.as_deref(), Some("GSO"));

        let o = decode("QC", "IAD2128").unwrap();
        assert_eq!(o.depa.as_deref(), Some("IAD"));
        assert_eq!(o.wlin.as_deref(), Some("2128"));

        let o = decode("QD", "SFO164100792").unwrap();
        assert_eq!(o.gtin.as_deref(), Some("1641"));

        // the 4-character ICAO layout still decodes as before
        let o = decode("QC", "KIAD2128").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KIAD"));
        assert_eq!(o.wlin.as_deref(), Some("2128"));
    }

    #[test]
    fn q_series_blank_time_gives_no_time() {
        assert!(decode("QP", "KSEAKORD    0276     ").is_none_or(|o| o.gtout.is_none()));
    }

    #[test]
    fn united_slash_reports() {
        let o = decode("14", "/14 OFF EVENT      / KIAD KMCO 21 145718/TIME 1457\r\n/AU 17542332").unwrap();
        assert_eq!(o.depa.as_deref(), Some("KIAD"));
        assert_eq!(o.dsta.as_deref(), Some("KMCO"));
        assert_eq!(o.wloff.as_deref(), Some("1457"));

        let o = decode("17", "/17 POST RPT       / KORD KAUS 21 214146/TIME 2140\r\n/FOB 0068").unwrap();
        assert_eq!(o.gtin.as_deref(), Some("2140"));

        let o = decode("18", "/18 POST TIMES SECS/ KSFO KDFW 20 042934/IN TIME0428/PDOOR 042934").unwrap();
        assert_eq!(o.gtin.as_deref(), Some("0428"));

        let o = decode(
            "18",
            "/18 FLT TIMES SECS / DGAA KIAD 20 094735\r\n/M11 PDOOR TIME 220923\r\n/M13 OUT TIME 221610\r\n/M14 OFF TIME 223552\r\n/M15 ON TIME 093824\r\n/M16 IN TIME 094635\r\n/M17 POS",
        )
        .unwrap();
        assert_eq!(o.depa.as_deref(), Some("DGAA"));
        assert_eq!(times(&o), [Some("2216"), Some("2235"), Some("0938"), Some("0946")]);

        assert!(decode("12", "/12 CREW EVENT     / KMSP KORD 21 222646/TIME 2226").is_none());
    }
}
