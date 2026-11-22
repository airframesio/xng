//! Weight and balance loadsheets sent over ACARS.
//!
//! Port of the Airframes reference parser (acars-decoder-typescript
//! `lib/utils/loadsheet.ts`); the rules and field names are shared with the
//! decoder, the event digger and the API, so keep them in step.
//!
//! Most airlines uplink the IATA AHM 517 "LOADSHEET" text, wrapped in their
//! own routing header and on many labels (RA, C1, 2A, 10, 3L, 3T, 22, ...):
//!
//! ```text
//! LOADSHEET FINAL 1359 EDNO1
//! AI160/21      21NOV26
//! LGW AMD VT-ALP   3/15
//! ZFW 214843 MAX 237682  L
//! TOF 73700
//! TOW 288543 MAX 312967
//! ...
//! ```
//!
//! Crews also acknowledge loadsheets ("LOADSHEET FINAL,EDNO 01,ACCEPT",
//! "LDSHT ACCEPT ACK") and request them ("LOADSHEET REQ"); those come back
//! with kind `acceptance` or `request`.
//!
//! Weights are whole numbers in the airline's unit (usually kg;
//! `weight_unit` is set only when the loadsheet says so). Airports are as
//! written (IATA on most loadsheets, ICAO on some).

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadsheetKind {
    #[default]
    Loadsheet,
    Acceptance,
    Request,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadsheetStatus {
    Final,
    Prelim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WeightUnit {
    #[serde(rename = "KG")]
    Kg,
    #[serde(rename = "LB")]
    Lb,
}

/// Which maximum limits the load (the `L` marker on a weight line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Limiting {
    Zfw,
    Tow,
    Law,
}

/// A decoded loadsheet, loadsheet acceptance or loadsheet request.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Loadsheet {
    pub kind: LoadsheetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<LoadsheetStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<u32>,
    /// Issue time as sent, HHMM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_number: Option<String>,
    /// Day of month after the flight number (AI160/21).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_day: Option<u32>,
    /// Flight date, YYYY-MM-DD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrival: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<String>,
    /// Crew as sent, cockpit/cabin (3/15) or cockpit/extra/cabin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_unit: Option<WeightUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zfw: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zfw_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tof: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tow: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tow_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tif: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub law: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub law_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limiting: Option<Limiting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underload: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dow: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bw: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bi: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dli: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub li_zfw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub li_tow: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub li_law: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac_zfw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac_tow: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac_law: Option<f64>,
    /// Passengers by class or zone as sent (4/33/302).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pax_breakdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pax_total: Option<i64>,
    /// Infants (PAX 339 PLUS 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub infants: Option<i64>,
    /// Total on board, crew included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tob: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notoc: Option<bool>,
}

impl Loadsheet {
    fn new(kind: LoadsheetKind) -> Self {
        Loadsheet { kind, ..Default::default() }
    }

    /// One-line summary for console output.
    pub fn summary(&self) -> String {
        let mut s = String::from(match self.kind {
            LoadsheetKind::Acceptance => "LOADSHEET ACCEPT",
            LoadsheetKind::Request => "LOADSHEET REQ",
            LoadsheetKind::Loadsheet => "LOADSHEET",
        });
        match self.status {
            Some(LoadsheetStatus::Final) => s.push_str(" final"),
            Some(LoadsheetStatus::Prelim) => s.push_str(" prelim"),
            None => {}
        }
        if let Some(e) = self.edition {
            s.push_str(&format!(" ed{e}"));
        }
        if let Some(f) = &self.flight_number {
            s.push_str(&format!(" {f}"));
        }
        if let (Some(d), Some(a)) = (&self.departure, &self.arrival) {
            s.push_str(&format!(" {d}->{a}"));
        }
        if let Some(r) = &self.registration {
            s.push_str(&format!(" {r}"));
        }
        if let Some(z) = self.zfw {
            s.push_str(&format!(" ZFW {z}"));
        }
        if let Some(t) = self.tow {
            s.push_str(&format!(" TOW {t}"));
        }
        if let Some(p) = self.pax_total {
            s.push_str(&format!(" PAX {p}"));
        }
        s
    }
}

// `\d` in the `regex` crate also matches non-ASCII digits, so fields that
// are later byte-sliced (issue time, CSV date) use `[0-9]`.
// The `regex` crate has no look-around; where the reference parser uses
// `(?![\d.])` after a number, these patterns use `(?:[^\d.]|$)`, and a
// trailing `(?=\s|$)` becomes `(?:\s|$)` (only captures are read).
fn re(p: &str) -> Regex {
    Regex::new(p).expect("loadsheet regex")
}

static HEADER_WORD: LazyLock<Regex> =
    LazyLock::new(|| re(r"LOADSHEET|LOAD SHEET|L O A D S H E E T|\bLS\s+(?:FINAL|PRELIM)"));
/// Lines that mention a loadsheet without starting one.
static NOT_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"END (?:OF )?(?:ACARS )?LOADSHEET|LOADSHEET END|AMENDMENTS|WILL FOLLOW|NO FINAL LOADSHEET|AWAITING|CANCELLED|CHECKS PERFORMED|ACCEPT|\bACK|RCVD|\bREQ\b|REQUEST|,")
});
static STATUS: LazyLock<Regex> = LazyLock::new(|| re(r"\b(FINAL|PRELIM)(?:INARY)?"));
static EDITION: LazyLock<Regex> = LazyLock::new(|| re(r"\bED(?:NO?|N0)?\s*0*(\d{1,2})\b"));
/// Flight number, optional `/day`, then the rest of the line, which must be
/// empty or start with a space or `/`.
static FLIGHT: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([A-Z0-9]{2}[A-Z]?\d{1,4}[A-Z]?)(?:/(\d{1,2}))?((?:[\s/].*)?)$"));
static FLIGHT_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Z0-9]{2}[A-Z]?\d"));
static DATE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(\d{2})(JAN|FEB|MAR|APR|MAY|JUN|JUL|AUG|SEP|OCT|NOV|DEC)(\d{2})\b"));
static ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^([A-Z]{3,4})(?:\s+|/|-)([A-Z]{3,4})(?:\s+([A-Z0-9]{1,2}-?[A-Z0-9]{2,6}))?(?:\s+(\d{1,2}/\d{1,2}(?:/\d{1,2})?))?(?:\s|$)")
});
static ROUTE_JOINED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^([A-Z]{3})([A-Z]{3})\s+([A-Z0-9]{1,2}-?[A-Z0-9]{2,6})(?:\s+(\d{1,2}/\d{1,2}(?:/\d{1,2})?))?(?:\s|$)")
});
static ACCEPT_CSV: LazyLock<Regex> = LazyLock::new(|| {
    re(r",([A-Z]{3,4}),([A-Z]{3,4}),([0-9]{6}),([0-9]{4}),LOADSHEET (FINAL|PRELIM),EDNO\s*(\d+),ACCEPT")
});
static LEADING_JUNK: LazyLock<Regex> = LazyLock::new(|| re(r"^[\s\-.*/]+"));
static TIME_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9]{4}Z?$"));
static EDITION_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{3}$"));
static NAMED_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:LOADSHEET|LOAD SHEET)\s+([A-Z0-9]{2}[A-Z]?\d{1,4}[A-Z]?)\s+REF\b")
});
static FINAL_NO: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:FINAL|PRELIM)0*(\d{1,2})\s"));
static FLIGHT_LINE_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:FINAL|PRELIM)\d*\s+"));
static SECTOR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^([A-Z0-9]{2}\d{1,4})/([A-Z]{3})-([A-Z]{3})/(\d{2})([A-Z]{3})(\d{2})$")
});
static REG_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([A-Z0-9]{2}\d{1,4})/([A-Z]{2}[A-Z0-9]{3,4})/$"));
static LEADING_DATE: LazyLock<Regex> = LazyLock::new(|| re(r"^/?\d{2}[A-Z]{3}\d{2}\s*"));
static FLT_ROUTE: LazyLock<Regex> = LazyLock::new(|| re(r"^FLT\s+([A-Z]{3,4})-0?([A-Z]{3,4})\b"));
static DTE: LazyLock<Regex> = LazyLock::new(|| re(r"^DTE\s+(\d{4}-\d{2}-\d{2})$"));
static ZFW: LazyLock<Regex> = LazyLock::new(|| re(r"^ZFW(?: ACTUAL)?:?\s+(\d+)(?:[^\d.]|$)"));
static TOW: LazyLock<Regex> = LazyLock::new(|| re(r"^TOW(?: ACTUAL)?:?\s+(\d+)(?:[^\d.]|$)"));
static LAW: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:LAW|LDW|LGW|LW)(?: ACTUAL)?:?\s+(\d+)(?:[^\d.]|$)"));
/// A bare `KEY number` line, checked on the last line for a cut-off value.
static BARE_KEY_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Z]+:?\s+([0-9]+)$"));
static MAX: LazyLock<Regex> = LazyLock::new(|| re(r"\bM(?:AX)?\s*(\d+)"));
static LIMIT_MARK: LazyLock<Regex> = LazyLock::new(|| re(r"[\s\d]L$"));
static WEIGHT_START: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:ZFW|TOW)(?: ACTUAL)?:?\s+\d"));
static TOF: LazyLock<Regex> = LazyLock::new(|| re(r"^TOF\s+(\d+)(?:[^\d.]|$)"));
static TIF: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:TIF|TF|BO)\s+(\d+)(?:[^\d.]|$)"));
static UNDLD: LazyLock<Regex> = LazyLock::new(|| re(r"^UNDLD\s+(\d+)(?:[^\d.]|$)"));
static DOW: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|\s)DOW\s+(\d+)(?:[^\d.]|$)"));
static BW: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|\s)BW\s*(\d+)(?:[^\d.]|$)"));
static CREW: LazyLock<Regex> = LazyLock::new(|| re(r"^CREW\s+(\d{1,2}/\d{1,2}(?:/\d{1,2})?)\b"));
static TOB: LazyLock<Regex> = LazyLock::new(|| re(r"^TOB\s+(\d+)(?:[^\d.]|$)"));
static TYPE: LazyLock<Regex> = LazyLock::new(|| re(r"^TYPE\s+([A-Z0-9-]{3,10})$"));
static PAX_TTL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:^|\s)PAX[\s/]+(\d+(?:/\s*\d+)*)\s+TTL\s+(\d+)"));
static INFANTS: LazyLock<Regex> = LazyLock::new(|| re(r"^PAX\s+\d+\s+PLUS\s+(\d+)"));
static NOTOC: LazyLock<Regex> = LazyLock::new(|| re(r"NOTOC:?\s*(YES|NO)\b"));
static UNIT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:WEIGHTS IN|WGHTS|WT IN|WTS IN|UOM[:/ ]|LOADSHEET -)\s*(KG|LB|KILO)")
});

#[derive(Clone, Copy)]
enum Index {
    Doi,
    Bi,
    Dli,
    LiZfw,
    LiTow,
    LiLaw,
    MacZfw,
    MacTow,
    MacLaw,
}

impl Index {
    fn slot(self, ls: &mut Loadsheet) -> &mut Option<f64> {
        match self {
            Index::Doi => &mut ls.doi,
            Index::Bi => &mut ls.bi,
            Index::Dli => &mut ls.dli,
            Index::LiZfw => &mut ls.li_zfw,
            Index::LiTow => &mut ls.li_tow,
            Index::LiLaw => &mut ls.li_law,
            Index::MacZfw => &mut ls.mac_zfw,
            Index::MacTow => &mut ls.mac_tow,
            Index::MacLaw => &mut ls.mac_law,
        }
    }
}

/// MACZFW 32.5, MAC ZFW: 30.58, LIZFW 33.0, DOI -28.7.
static INDEX_PATTERNS: LazyLock<Vec<(Regex, Index)>> = LazyLock::new(|| {
    [
        ("DOI", Index::Doi),
        ("BI", Index::Bi),
        ("DLI", Index::Dli),
        ("LI ?ZFW", Index::LiZfw),
        ("LI ?TOW", Index::LiTow),
        ("LI ?LAW", Index::LiLaw),
        ("MAC ?ZFW", Index::MacZfw),
        ("MAC ?TOW", Index::MacTow),
        ("MAC ?LAW", Index::MacLaw),
    ]
    .into_iter()
    .map(|(key, idx)| (re(&format!(r"(?:^|\s){key}:?\s+(-?\d+(?:\.\d+)?)")), idx))
    .collect()
});

static ACCEPT_CSV_FLIGHT: LazyLock<Regex> =
    LazyLock::new(|| re(r"([A-Z]{2,3}\d{1,4}[A-Z]?),[A-Z]{3,4},"));
static ACCEPT_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:LOADSHEET|LDSHT|LOAD SHEET)(?:\s+(?:FINAL|PRELIM))?(?:\s+\d{1,2})?\s+(?:ACCEPT|ACKNOW|RCVD)")
});
static ACK_LOADSHEET: LazyLock<Regex> = LazyLock::new(|| re(r"\bACK LOADSHEET"));
static ACCEPT_OF: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:ACKNOWLEDGEMENT|ACCEPTANCE) OF (?:THE )?(?:FINAL|PRELIM) LOADSHEET")
});
static ACCEPT_EDNO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"EDN[O0]?\s*0*(\d{1,2})\b|LOADSHEET\s*0*(\d{1,2})\b|LOADSHEET ID:\s*(?:FINAL|PRELIM)0*(\d{1,2})\b")
});
static ACCEPT_FLIGHT: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:FLIGHT NUMBER:\s*|FOR\s+)([A-Z0-9]{2}[A-Z]?\d{1,4}[A-Z]?)\b"));
static ACCEPT_SECTOR: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:SECTOR:\s*|\s)([A-Z]{3,4})[-/]([A-Z]{3,4})\b"));
static ACCEPT_SECTOR_START: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Z]{4})/([A-Z]{4})\s"));
static REQUEST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:LOADSHEET|LDSHT|LOAD SHEET)\s+(?:REQ|REQUEST)\b(?:.*?/ORIG\s+([A-Z]{3,4}))?")
});

const MONTHS: [&str; 12] =
    ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

/// Cheap check: could this text hold a loadsheet?
pub fn may_be_loadsheet(text: &str) -> bool {
    text.contains("LOADSHEET")
        || text.contains("LOAD SHEET")
        || text.contains("LDSHT")
        || text.contains("LS FINAL")
}

fn clean_lines(text: &str) -> Vec<String> {
    text.replace('\r', "")
        .split('\n')
        .map(|l| {
            let l = l.replace('\t', " ");
            LEADING_JUNK.replace(&l, "").trim_end().to_owned()
        })
        .collect()
}

fn flight_date(dd: &str, mon: &str, yy: &str) -> Option<String> {
    let m = MONTHS.iter().position(|&x| x == mon)?;
    let d: u32 = dd.parse().ok()?;
    if !(1..=31).contains(&d) {
        return None;
    }
    Some(format!("20{yy}-{:02}-{dd}", m + 1))
}

fn cap<'a>(c: &regex::Captures<'a>, i: usize) -> Option<&'a str> {
    c.get(i).map(|m| m.as_str())
}

fn int<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.parse().ok()
}

fn status_of(s: &str) -> LoadsheetStatus {
    if s == "FINAL" {
        LoadsheetStatus::Final
    } else {
        LoadsheetStatus::Prelim
    }
}

fn parse_header(ls: &mut Loadsheet, header: &str, next: &[&str]) {
    let first = next.first().copied().unwrap_or("");
    if let Some(c) = STATUS.captures(header).or_else(|| STATUS.captures(first)) {
        ls.status = Some(status_of(&c[1]));
    }
    let start = HEADER_WORD.find(header).map_or(0, |m| m.start());
    let rest = &header[start..];
    if let Some(c) = EDITION.captures(rest) {
        ls.edition = int(&c[1]);
    }
    let stripped = EDITION.replace(rest, " ");
    let stripped = DATE.replace(&stripped, " ");
    for t in stripped.split_whitespace().skip(1) {
        if TIME_TOKEN.is_match(t) && ls.time.is_none() {
            ls.time = Some(t[..4].to_owned());
        } else if EDITION_TOKEN.is_match(t) && ls.edition.is_none() {
            ls.edition = int(t);
        }
    }
    // LOADSHEET  QY6731   REF 2NM0H
    if let Some(c) = NAMED_HEADER.captures(rest) {
        ls.flight_number = Some(c[1].to_owned());
    }
    if ls.edition.is_none() {
        if let Some(c) = FINAL_NO.captures(first) {
            ls.edition = int(&c[1]);
        }
    }
    if ls.edition.is_none() {
        // EDNO on one of the next lines (EI132/21 21NOV26 EDNO 001).
        for line in next.iter().take(3) {
            if let Some(c) = EDITION.captures(line) {
                ls.edition = int(&c[1]);
                break;
            }
        }
    }
}

fn parse_flight_line(ls: &mut Loadsheet, raw: &str) -> bool {
    let line = FLIGHT_LINE_PREFIX.replace(raw, "");
    let line = line.as_ref();
    let Some(m) = FLIGHT.captures(line) else { return false };
    if !FLIGHT_PREFIX.is_match(&m[1]) {
        return false;
    }
    // AR1134/EZE-MAD/21NOV26
    if let Some(s) = SECTOR_LINE.captures(line) {
        ls.flight_number = Some(s[1].to_owned());
        ls.departure = Some(s[2].to_owned());
        ls.arrival = Some(s[3].to_owned());
        if let Some(d) = flight_date(&s[4], &s[5], &s[6]) {
            ls.date = Some(d);
        }
        return true;
    }
    // AR1740/LVFPS/
    if let Some(r) = REG_LINE.captures(line) {
        ls.flight_number = Some(r[1].to_owned());
        ls.registration = Some(r[2].to_owned());
        return true;
    }
    let date = DATE.captures(line);
    // Needs a day or a date to tell a flight line from any other line.
    let day = cap(&m, 2);
    if day.is_none() && date.is_none() {
        return false;
    }
    ls.flight_number = Some(m[1].to_owned());
    if let Some(d) = day {
        ls.flight_day = int(d);
    }
    if let Some(d) = date.and_then(|d| flight_date(&d[1], &d[2], &d[3])) {
        ls.date = Some(d);
    }
    // Some airlines put the route on the flight line (SQ7394 21NOV26 AMS LHR 9V-SFP).
    let rest = DATE.replace(&m[3], " ");
    let rest = EDITION.replace(&rest, " ");
    let rest = rest.trim();
    if !rest.is_empty() {
        let rest = LEADING_DATE.replace(rest, "");
        parse_route_line(ls, &rest);
    }
    true
}

fn parse_route_line(ls: &mut Loadsheet, line: &str) -> bool {
    // CDGDZA F-OLRB 3/9: airports written together.
    let Some(m) = ROUTE.captures(line).or_else(|| ROUTE_JOINED.captures(line)) else {
        return false;
    };
    ls.departure = Some(m[1].to_owned());
    ls.arrival = Some(m[2].to_owned());
    if let Some(r) = cap(&m, 3) {
        ls.registration = Some(r.to_owned());
    }
    if let Some(c) = cap(&m, 4) {
        ls.crew = Some(c.to_owned());
    }
    true
}

fn parse_weight(ls: &mut Loadsheet, line: &str, which: Limiting) {
    let (re, value, max) = match which {
        Limiting::Zfw => (&*ZFW, &mut ls.zfw, &mut ls.zfw_max),
        Limiting::Tow => (&*TOW, &mut ls.tow, &mut ls.tow_max),
        Limiting::Law => (&*LAW, &mut ls.law, &mut ls.law_max),
    };
    if value.is_some() {
        return;
    }
    let Some(c) = re.captures(line) else { return };
    *value = int(&c[1]);
    if let Some(mx) = MAX.captures(line) {
        *max = int(&mx[1]);
    }
    // The first limiting weight wins when several are marked.
    if ls.limiting.is_none() && LIMIT_MARK.is_match(line) {
        ls.limiting = Some(which);
    }
}

fn set_int(slot: &mut Option<i64>, re: &Regex, line: &str) {
    if slot.is_none() {
        if let Some(c) = re.captures(line) {
            *slot = int(&c[1]);
        }
    }
}

fn parse_body(ls: &mut Loadsheet, lines: &[String]) {
    let mut flight_seen = false;
    let mut route_seen = false;
    let mut since_flight = 0;
    let last = lines.iter().rposition(|l| !l.trim().is_empty());
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Long loadsheets are split over several messages, and the split can
        // fall inside a value ("TOW 2" for "TOW 288113"). A bare key and
        // number on the last line is read only when it is about as long as
        // the ZFW.
        if Some(i) == last && i > 0 {
            if let Some(c) = BARE_KEY_NUMBER.captures(line) {
                let zfw_digits = ls.zfw.map(|z| z.to_string().len());
                if zfw_digits.is_none_or(|d| c[1].len() + 1 < d) {
                    continue;
                }
            }
        }
        if flight_seen {
            since_flight += 1;
        }
        if !flight_seen && i < 4 && parse_flight_line(ls, line) {
            flight_seen = true;
            route_seen = ls.departure.is_some();
            continue;
        }
        if flight_seen && !route_seen && since_flight <= 2 && parse_route_line(ls, line) {
            route_seen = true;
            continue;
        }
        if ls.departure.is_none() {
            // FLT  AMS-LHR  A306EAT / DAEAT, DTE  2026-11-21
            if let Some(c) = FLT_ROUTE.captures(line) {
                ls.departure = Some(c[1].to_owned());
                ls.arrival = Some(c[2].to_owned());
            }
        }
        if ls.date.is_none() {
            if let Some(c) = DTE.captures(line) {
                ls.date = Some(c[1].to_owned());
            }
        }
        parse_weight(ls, line, Limiting::Zfw);
        parse_weight(ls, line, Limiting::Tow);
        parse_weight(ls, line, Limiting::Law);
        set_int(&mut ls.tof, &TOF, line);
        set_int(&mut ls.tif, &TIF, line);
        set_int(&mut ls.underload, &UNDLD, line);
        set_int(&mut ls.dow, &DOW, line);
        set_int(&mut ls.bw, &BW, line);
        if ls.crew.is_none() {
            if let Some(c) = CREW.captures(line) {
                ls.crew = Some(c[1].to_owned());
            }
        }
        set_int(&mut ls.tob, &TOB, line);
        if ls.aircraft_type.is_none() {
            if let Some(c) = TYPE.captures(line) {
                ls.aircraft_type = Some(c[1].to_owned());
            }
        }
        if ls.pax_total.is_none() {
            if let Some(c) = PAX_TTL.captures(line) {
                ls.pax_breakdown = Some(c[1].chars().filter(|c| !c.is_whitespace()).collect());
                ls.pax_total = int(&c[2]);
            }
        }
        set_int(&mut ls.infants, &INFANTS, line);
        for (re, idx) in INDEX_PATTERNS.iter() {
            let slot = idx.slot(ls);
            if slot.is_none() {
                if let Some(c) = re.captures(line) {
                    *slot = c[1].parse().ok();
                }
            }
        }
        if ls.notoc.is_none() {
            if let Some(c) = NOTOC.captures(line) {
                ls.notoc = Some(&c[1] == "YES");
            }
        }
        if ls.weight_unit.is_none() {
            if let Some(c) = UNIT.captures(line) {
                ls.weight_unit = Some(if &c[1] == "LB" { WeightUnit::Lb } else { WeightUnit::Kg });
            }
        }
    }
}

fn parse_acceptance(text: &str) -> Option<Loadsheet> {
    if let Some(c) = ACCEPT_CSV.captures(text) {
        let mut ls = Loadsheet::new(LoadsheetKind::Acceptance);
        ls.status = Some(status_of(&c[5]));
        ls.edition = int(&c[6]);
        ls.time = Some(c[4].to_owned());
        ls.departure = Some(c[1].to_owned());
        ls.arrival = Some(c[2].to_owned());
        if let Some(f) = ACCEPT_CSV_FLIGHT.captures(text) {
            ls.flight_number = Some(f[1].to_owned());
        }
        let d = &c[3];
        ls.date = Some(format!("20{}-{}-{}", &d[4..6], &d[2..4], &d[0..2]));
        return Some(ls);
    }
    if ACCEPT_WORDS.is_match(text) || ACK_LOADSHEET.is_match(text) || ACCEPT_OF.is_match(text) {
        let mut ls = Loadsheet::new(LoadsheetKind::Acceptance);
        if let Some(c) = STATUS.captures(text) {
            ls.status = Some(status_of(&c[1]));
        }
        if let Some(c) = ACCEPT_EDNO.captures(text) {
            ls.edition = cap(&c, 1).or_else(|| cap(&c, 2)).or_else(|| cap(&c, 3)).and_then(int);
        }
        if let Some(c) = ACCEPT_FLIGHT.captures(text) {
            ls.flight_number = Some(c[1].to_owned());
        }
        if let Some(c) = ACCEPT_SECTOR.captures(text).or_else(|| ACCEPT_SECTOR_START.captures(text)) {
            ls.departure = Some(c[1].to_owned());
            ls.arrival = Some(c[2].to_owned());
        }
        return Some(ls);
    }
    None
}

/// Parse a loadsheet, loadsheet acceptance or loadsheet request from ACARS
/// text. `None` when the text holds none of them.
pub fn parse(text: &str) -> Option<Loadsheet> {
    if text.is_empty() || !may_be_loadsheet(text) {
        return None;
    }
    let lines = clean_lines(text);

    // Several header lines can appear (a routing line, then the real one):
    // use the last header before the first weight line.
    let mut header_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim();
        if WEIGHT_START.is_match(line) {
            break;
        }
        if HEADER_WORD.is_match(line) && !NOT_HEADER.is_match(line) {
            header_idx = Some(i);
        }
    }

    if let Some(h) = header_idx {
        let mut ls = Loadsheet::new(LoadsheetKind::Loadsheet);
        let body = &lines[h + 1..];
        let next: Vec<&str> =
            body.iter().map(String::as_str).filter(|l| !l.trim().is_empty()).collect();
        parse_header(&mut ls, &lines[h], &next);
        parse_body(&mut ls, body);
        // A loadsheet names its flight or gives its weights; a header alone
        // (a continuation block, a cover note) is not decoded.
        if ls.flight_number.is_some() || ls.zfw.is_some() || ls.tow.is_some() {
            return Some(ls);
        }
    }

    if let Some(ls) = parse_acceptance(text) {
        return Some(ls);
    }

    REQUEST.captures(text).map(|c| {
        let mut ls = Loadsheet::new(LoadsheetKind::Request);
        ls.departure = cap(&c, 1).map(str::to_owned);
        ls
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheap_check() {
        assert!(may_be_loadsheet("LDSHT ACCEPT ACK"));
        assert!(!may_be_loadsheet("POSN43220W075123"));
        assert_eq!(parse("REQ POS"), None);
    }

    #[test]
    fn ahm517_final() {
        let ls = parse(
            "LOADSHEET FINAL 1359 EDNO1\nAI160/21      21NOV26\nLGW AMD VT-ALP   3/15\n\
             ZFW 214843 MAX 237682  L\nTOW 288543 MAX 312967\nPAX/4/33/302 TTL 342\nMACZFW   32.5",
        )
        .unwrap();
        assert_eq!(ls.kind, LoadsheetKind::Loadsheet);
        assert_eq!(ls.status, Some(LoadsheetStatus::Final));
        assert_eq!(ls.edition, Some(1));
        assert_eq!(ls.time.as_deref(), Some("1359"));
        assert_eq!(ls.flight_number.as_deref(), Some("AI160"));
        assert_eq!(ls.date.as_deref(), Some("2026-11-21"));
        assert_eq!(ls.registration.as_deref(), Some("VT-ALP"));
        assert_eq!(ls.zfw_max, Some(237682));
        assert_eq!(ls.limiting, Some(Limiting::Zfw));
        assert_eq!(ls.pax_total, Some(342));
        assert_eq!(ls.mac_zfw, Some(32.5));
    }

    #[test]
    fn non_ascii_digits_do_not_panic() {
        let ls = parse("LOADSHEET FINAL \u{0967}\u{0968}\u{0969}\u{096A}\nAI160/21 21NOV26\nZFW 123").unwrap();
        assert_eq!(ls.time, None);
        // Not a valid CSV acceptance date: must simply not panic.
        let _ = parse("QTR1,HKJK,OTHH,\u{0967}\u{0968}\u{0969}\u{096A}\u{0967}\u{0968},1401,LOADSHEET FINAL,EDNO 01,ACCEPT");
    }

    #[test]
    fn tonne_decimals_skipped() {
        // No flight and no whole-number weights: not decoded.
        assert_eq!(parse("LOADSHEET FINAL 0900 EDNO2\nZFW 61.4 MAX 62.5\nTOW 70.2"), None);
    }
}
