//! Military (US Air Mobility Command style) ARINC 702 reports.
//!
//! Clean-room reimplementation from airframes' own acars-decoder-typescript
//! (`utils/arinc_702_helper.ts`, `utils/mission_number.ts` and their tests;
//! facts only) and from real off-air messages seen on Airframes in November
//! 2026. These reports share one header across many ACARS labels (H1 with
//! the `MD` sublabel, and bare on 1B/1J/1M/20/80/4J/2J/2P/31…):
//!
//! ```text
//! [preamble]<KIND>/ID<tail>,<callsign>,<mission>/<IEI><data>/…<csum>
//! INI/ID25756T,RCH469,GBB22X10B322/MR0,1/AFCYFB,BGTL/TD,13306BD2
//! ```
//!
//! `<KIND>` is a three-letter imbedded message identifier (`INI`, `POS`,
//! `FTX`, `WXR`, `FPN`, `OOI`, `DIS`, `INR`, `SUM`, …) or a six-letter pair
//! such as `REQPOS`. `<mission>` is the 12-character AMC mission number
//! (may be empty). The trailing 4 hex characters are a CRC-16 over the
//! record from `<KIND>` on: CRC-16/IBM-SDLC with its nibbles reversed on
//! VHF/VDL2/HFDL/Iridium, CRC-16/GENIBUS on Inmarsat.

use crate::fpn::FlightPlan;
use crate::position::{decode_decimal_minutes, Position};
use serde::Serialize;

/// A 12-character AMC mission number, split into its fixed-width parts.
/// The meaning of individual prefix/suffix characters varies by mission
/// type and is not decoded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mission {
    pub value: String,
    /// Characters 1-3.
    pub prefix: String,
    /// Characters 4-7, the basic mission number.
    pub basic: String,
    /// Characters 8-9.
    pub suffix: String,
    /// Characters 10-12: day of year (1-366) the mission operates.
    pub day_of_year: u16,
}

/// Default values some avionics send when no mission number was entered.
const PLACEHOLDERS: [&str; 2] = ["COLLINSAM123", "ZZZ123456789"];

impl Mission {
    pub fn parse(s: &str) -> Option<Mission> {
        if s.len() != 12
            || PLACEHOLDERS.contains(&s)
            || !s
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return None;
        }
        let day: u16 = s[9..].parse().ok()?;
        if !(1..=366).contains(&day) || !s[9..].bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Mission {
            value: s.to_owned(),
            prefix: s[..3].to_owned(),
            basic: s[3..7].to_owned(),
            suffix: s[7..9].to_owned(),
            day_of_year: day,
        })
    }

    /// Grouped form, e.g. `AVY F913 YT 321`.
    pub fn grouped(&self) -> String {
        format!(
            "{} {} {} {}",
            self.prefix,
            self.basic,
            self.suffix,
            &self.value[9..]
        )
    }
}

/// A METAR/TAF returned in answer to a `WXR` request (`/MT` field).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WeatherReport {
    pub station: String,
    /// `HHMM` the report was issued.
    pub time: String,
    /// `METAR`, `TAF` or `UNKNOWN`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub report: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MilitaryReport {
    /// Message kind (`INI`, `POS`, `REQPOS`, …).
    pub kind: String,
    pub description: String,
    pub tail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    /// The mission field exactly as sent (absent when empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission: Option<Mission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrival: Option<String>,
    /// `/TD` planned departure, `DDHHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_departure: Option<String>,
    /// `/TD` estimated departure, `HHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_departure: Option<String>,
    /// `/DC` report time as ISO 8601 UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_time: Option<String>,
    /// `/OT` out-of-gate time, `DDHHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_time: Option<String>,
    /// `/OF` takeoff time, `DDHHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_time: Option<String>,
    /// `/ON` landing time, `DDHHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_time: Option<String>,
    /// `/IN` in-gate time, `DDHHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_time: Option<String>,
    /// `/ET` estimated arrival, `DDHHMM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude_ft: Option<u32>,
    /// `/FB` fuel on board (units as sent, usually hundreds of pounds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuel_on_board: Option<u32>,
    /// `/FX` free text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather: Option<WeatherReport>,
    /// Station asked for in a `REQWXR` (`/WX<DDHHMM>,<code>,<station>,…`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather_request_station: Option<String>,
    /// Route of an `FPN` report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_plan: Option<FlightPlan>,
    /// The trailing checksum matched one of the two ARINC 702 CRCs.
    pub crc_ok: bool,
}

fn description(kind: &str) -> String {
    let one = |k: &str| match k {
        "CRG" => "Cargo Report",
        "DIS" => "Position Report (DIS)",
        "ENG" => "Engine Report",
        "FPN" => "Flight Plan",
        "FTX" => "Free Text",
        "INI" => "Initial Report",
        "INR" => "In-Range Report",
        "OOI" => "OOOI Event Report",
        "POS" => "Position Report",
        "PWI" => "Pilot Weather Information",
        "REJ" => "Reject",
        "REQ" => "Request",
        "RES" => "Response",
        "SUM" => "Summary Report",
        "WXR" => "Weather Report",
        _ => "Military Report",
    };
    if kind.len() == 6 {
        format!("{} for {}", one(&kind[..3]), one(&kind[3..]))
    } else {
        one(kind).to_owned()
    }
}

fn crc16_ibm_sdlc_nibble_rev(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    let c = crc ^ 0xffff;
    ((c & 0xf) << 12) | (((c >> 4) & 0xf) << 8) | (((c >> 8) & 0xf) << 4) | ((c >> 12) & 0xf)
}

fn crc16_genibus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc ^ 0xffff
}

fn is_upper(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// `DDHHMM` (6 digits) or `None`.
/// `DDHHMM`, kept only when it is a real day of the month and time of day.
fn ddhhmm(s: &str) -> Option<String> {
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |i: usize| s[i..i + 2].parse::<u32>().unwrap_or(99);
    ((1..=31).contains(&n(0)) && n(2) <= 23 && n(4) <= 59).then(|| s.to_owned())
}

/// `TAF`, `METAR` (station then a `DDHHMMZ` group, optionally after
/// `METAR`/`SPECI`) or `UNKNOWN` for anything else, e.g. "no data" replies.
fn weather_kind(report: &str) -> &'static str {
    if report == "TAF" || report.starts_with("TAF ") {
        return "TAF";
    }
    let mut words = report.split_whitespace();
    let mut first = words.next().unwrap_or("");
    if first == "METAR" || first == "SPECI" {
        first = words.next().unwrap_or("");
    }
    let station = first.len() == 4
        && first.as_bytes()[0].is_ascii_uppercase()
        && first
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let time = words.next().is_some_and(|t| {
        t.len() == 7 && t.ends_with('Z') && t[..6].bytes().all(|b| b.is_ascii_digit())
    });
    if station && time {
        "METAR"
    } else {
        "UNKNOWN"
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// Parse a military ARINC 702 report. `text` is the message text with any
/// H1 sublabel already removed; returns `None` when it has no
/// `<KIND>/ID<tail>,` header near the start.
pub fn parse(text: &str) -> Option<MilitaryReport> {
    // Byte offsets below assume ASCII; a military report is always ASCII.
    if !text.is_ascii() {
        return None;
    }
    // Multi-block uplinks repeat the `- #MD` sublabel at every block join
    // (`…:V:OKA- #MDBU,240…`); the CRC covers the record without them.
    let cleaned: String = text
        .chars()
        .filter(|&c| c != '\r' && c != '\n')
        .collect::<String>()
        .replace("- #MD", "");
    let cleaned = cleaned.trim_end();
    // The header must sit near the start (after at most a short preamble
    // such as `M49AMCEN97` or `F82AMCFAMC#M2B`).
    let id_at = cleaned.find("/ID")?;
    if !(3..=24).contains(&id_at) {
        return None;
    }
    let before = &cleaned[..id_at];
    let kind_len = if before.len() >= 6
        && is_upper(&before[before.len() - 6..])
        && matches!(
            &before[before.len() - 6..before.len() - 3],
            "REQ" | "REJ" | "RES"
        ) {
        6
    } else {
        3
    };
    let kind = &before[before.len() - kind_len..];
    if !is_upper(kind) {
        return None;
    }
    let record = &cleaned[id_at - kind_len..];
    if record.len() < kind_len + 8 {
        return None;
    }
    let (data, csum) = record.split_at(record.len() - 4);
    let crc_ok = u16::from_str_radix(csum, 16).is_ok_and(|c| {
        crc16_ibm_sdlc_nibble_rev(data.as_bytes()) == c || crc16_genibus(data.as_bytes()) == c
    });
    // Without a valid CRC, keep the last 4 characters as data.
    let data = if crc_ok { data } else { record };

    let fields: Vec<&str> = data[kind_len + 1..].split('/').collect();
    let id = fields.first()?.strip_prefix("ID")?;
    let mut id_parts = id.split(',');
    let tail = id_parts.next()?;
    if tail.len() < 4 || tail.len() > 7 || !tail.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let callsign = id_parts.next().unwrap_or("");
    let mission_number = id_parts.next().unwrap_or("");

    let mut r = MilitaryReport {
        kind: kind.to_owned(),
        description: description(kind),
        tail: tail.to_owned(),
        callsign: non_empty(callsign),
        mission_number: non_empty(mission_number),
        mission: Mission::parse(mission_number),
        departure: None,
        arrival: None,
        planned_departure: None,
        estimated_departure: None,
        report_time: None,
        out_time: None,
        off_time: None,
        on_time: None,
        in_time: None,
        eta: None,
        position: None,
        altitude_ft: None,
        fuel_on_board: None,
        text: None,
        weather: None,
        weather_request_station: None,
        flight_plan: None,
        crc_ok,
    };

    for &f in fields.iter().skip(1) {
        if f.len() < 2 || !f.is_char_boundary(2) {
            continue;
        }
        let (iei, v) = f.split_at(2);
        let parts: Vec<&str> = v.split(',').collect();
        match iei {
            "AF" if parts.len() == 2 => {
                r.departure = non_empty(parts[0]);
                r.arrival = non_empty(parts[1]);
            }
            "TD" if parts.len() == 2 => {
                r.planned_departure = ddhhmm(parts[0]);
                r.estimated_departure = (parts[1].len() == 4
                    && parts[1].bytes().all(|b| b.is_ascii_digit()))
                .then(|| parts[1].to_owned());
            }
            "DC" if parts.len() == 2 && parts[0].len() == 8 && parts[1].len() == 6 => {
                let (d, t) = (parts[0], parts[1]);
                if d.bytes().chain(t.bytes()).all(|b| b.is_ascii_digit()) {
                    r.report_time = Some(format!(
                        "{}-{}-{}T{}:{}:{}Z",
                        &d[4..],
                        &d[2..4],
                        &d[..2],
                        &t[..2],
                        &t[2..4],
                        &t[4..]
                    ));
                }
            }
            "ET" => r.eta = ddhhmm(v),
            "OF" => r.off_time = ddhhmm(v),
            "OT" => r.out_time = ddhhmm(parts[0]),
            "ON" => r.on_time = ddhhmm(v),
            "IN" => r.in_time = ddhhmm(parts[0]),
            // engine start, fuel, engine stop, then out, off, on and in times,
            // each followed by the fuel on board at that moment
            "SM" if parts.len() == 11 => {
                r.out_time = ddhhmm(parts[3]);
                r.off_time = ddhhmm(parts[5]);
                r.on_time = ddhhmm(parts[7]);
                r.in_time = ddhhmm(parts[9]);
            }
            "WX" if parts.len() >= 3
                && parts[2].len() == 4
                && parts[2].bytes().all(|b| b.is_ascii_alphanumeric())
                && parts[2].as_bytes()[0].is_ascii_uppercase() =>
            {
                r.weather_request_station = Some(parts[2].to_owned());
            }
            "FB" => r.fuel_on_board = v.parse().ok(),
            "FX" => r.text = non_empty(v),
            "LL" => r.position = decode_decimal_minutes(v),
            "PS" => {
                r.position = decode_decimal_minutes(parts[0]);
                if parts.len() > 2 {
                    r.altitude_ft = parts[2]
                        .parse::<u32>()
                        .ok()
                        .and_then(|fl| fl.checked_mul(100));
                }
            }
            "MT" => {
                // `<station>,<HHMM>,<report>`. AMC free text never contains
                // '/' (slashes and commas are sent as '('), so the report ends
                // at the next field like any other.
                let mut it = v.splitn(3, ',');
                if let (Some(st), Some(t), Some(rep)) = (it.next(), it.next(), it.next()) {
                    let hhmm_ok = t.len() == 4
                        && t.bytes().all(|b| b.is_ascii_digit())
                        && t[..2].parse::<u32>().is_ok_and(|h| h <= 23)
                        && t[2..].parse::<u32>().is_ok_and(|m| m <= 59);
                    if hhmm_ok {
                        let rep = rep.trim();
                        r.weather = Some(WeatherReport {
                            station: st.to_owned(),
                            time: t.to_owned(),
                            kind: weather_kind(rep),
                            report: rep.to_owned(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if kind == "FPN" {
        r.flight_plan = crate::fpn::parse(&format!("FPN/{}", &record[kind_len + 1..]));
    }
    Some(r)
}

impl MilitaryReport {
    /// One-line console summary.
    pub fn summary(&self) -> String {
        let mut s = format!("MIL {}", self.kind);
        if let Some(c) = &self.callsign {
            s.push(' ');
            s.push_str(c);
        }
        match (&self.mission, &self.mission_number) {
            (Some(m), _) => s.push_str(&format!(" msn {}", m.grouped())),
            (None, Some(raw)) => s.push_str(&format!(" msn {raw}")),
            _ => {}
        }
        if self.departure.is_some() || self.arrival.is_some() {
            s.push_str(&format!(
                " {}->{}",
                self.departure.as_deref().unwrap_or("?"),
                self.arrival.as_deref().unwrap_or("?")
            ));
        }
        if let Some(w) = &self.weather {
            s.push_str(&format!(" {} {}", w.kind, w.station));
        }
        if let Some(st) = &self.weather_request_station {
            s.push_str(&format!(" wx? {st}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_full() {
        let r =
            parse("INI/ID88197A,RCH4913,AVYF913YT321/MR0,0/AFOTBH,ETAD/TD172129,0255E534").unwrap();
        assert!(r.crc_ok);
        assert_eq!(r.kind, "INI");
        assert_eq!(r.description, "Initial Report");
        assert_eq!(r.tail, "88197A");
        assert_eq!(r.callsign.as_deref(), Some("RCH4913"));
        let m = r.mission.as_ref().unwrap();
        assert_eq!(
            (m.prefix.as_str(), m.basic.as_str(), m.suffix.as_str()),
            ("AVY", "F913", "YT")
        );
        assert_eq!(m.day_of_year, 321);
        assert_eq!(m.grouped(), "AVY F913 YT 321");
        assert_eq!(r.departure.as_deref(), Some("OTBH"));
        assert_eq!(r.arrival.as_deref(), Some("ETAD"));
        assert_eq!(r.planned_departure.as_deref(), Some("172129"));
        assert_eq!(r.estimated_departure.as_deref(), Some("0255"));
        assert_eq!(
            r.summary(),
            "MIL INI RCH4913 msn AVY F913 YT 321 OTBH->ETAD"
        );
    }

    #[test]
    fn ini_empty_td_and_uplink_framing() {
        let r =
            parse("- #MDINI/ID55144A,RCH5010,XMZM3313C321/MR0,0/AFKBGR,KRIV/TD,085582A3").unwrap();
        assert!(r.crc_ok);
        assert_eq!(r.planned_departure, None);
        assert_eq!(r.estimated_departure.as_deref(), Some("0855"));
        assert_eq!(r.arrival.as_deref(), Some("KRIV"));
    }

    #[test]
    fn placeholder_mission_is_not_parsed() {
        let r =
            parse("INI/IDA97450,ASY822,COLLINSAM123/MR1,1/AFKBWI,KJFK/TD210314,03146209").unwrap();
        assert_eq!(r.mission_number.as_deref(), Some("COLLINSAM123"));
        assert_eq!(r.mission, None);
    }

    #[test]
    fn pos_with_preamble_and_report_time() {
        let r = parse("F35AMCFAMC#M2BPOS/ID66162A,RCH3245,PAM7887W2324/DC20112026,161626/MR71,/ET201743/PSN31364E124002,161625,350,SADLI,201621,PONIK,M48,259144,M767T449/CG362,,/FB636/VR320012").unwrap();
        assert_eq!(r.kind, "POS");
        assert_eq!(r.report_time.as_deref(), Some("2026-11-20T16:16:26Z"));
        assert_eq!(r.eta.as_deref(), Some("201743"));
        assert_eq!(r.altitude_ft, Some(35000));
        assert_eq!(r.fuel_on_board, Some(636));
        let p = r.position.unwrap();
        assert!((p.latitude - 31.6067).abs() < 1e-3, "{p:?}");
        assert!((p.longitude - 124.0033).abs() < 1e-3, "{p:?}");
    }

    #[test]
    fn pos_without_callsign_or_mission() {
        let r = parse("POS/ID23571S,,/DC19112026,233551/MR54,/ET192341/PSN64580W147209,233600,59,UYOVA,192338,WABIR,P4,14221,MT250/CG287,160,350/FB330/VR327EA3").unwrap();
        assert_eq!(r.callsign, None);
        assert_eq!(r.mission_number, None);
        assert!(r.position.unwrap().longitude < 0.0);
    }

    #[test]
    fn wxr_weather() {
        let r = parse("- #MDWXR/ID99207A,RCH3269,AAM7886W2324/MR4,120/MTKADW,1200,KADW 211200Z VRB06KT 9999 (7SM) SCT250 T07C A3027 REQUEST PIREP AS MISSION PERMITSE1E4").unwrap();
        let w = r.weather.unwrap();
        assert_eq!(w.station, "KADW");
        assert_eq!(w.kind, "METAR");
        assert!(w.report.ends_with("MISSION PERMITS"));
        let t = parse("- #MDWXR/ID66162A,RCH3245,PAM7887W2324/MR1,48/MTPAED,0311,TAF PAED 210311Z 03009KT 9999 FEW030 BKN050 BKN100 QNH2979INS T0C-AUTO RESPONSE BASED ON AVAILABLE DATA-CONTACT C2 AGENCY FOR ADDITIONAL INFO2C6B").unwrap();
        assert_eq!(t.weather.unwrap().kind, "TAF");
    }

    #[test]
    fn wxr_paren_substitution_and_unknown_text() {
        // AMC free text cannot contain '/' or ','; both are sent as '('.
        let r = parse("- #MDWXR/ID99207A,RCH3269,AAM7886W2324/MR4,120/MTKADW,1200,KADW 211200Z 1(2SM FG OVC002 07(02 A3027/FB123").unwrap();
        let w = r.weather.unwrap();
        assert_eq!(w.kind, "METAR");
        assert_eq!(w.report, "KADW 211200Z 1(2SM FG OVC002 07(02 A3027");
        assert_eq!(r.fuel_on_board, Some(123));
        let u =
            parse("- #MDWXR/ID99207A,RCH3269,AAM7886W2324/MR4,120/MTKADW,1200,NO WX AVBL FOR KADW")
                .unwrap();
        assert_eq!(u.weather.unwrap().kind, "UNKNOWN");
        let bad = parse("- #MDWXR/ID99207A,RCH3269,AAM7886W2324/MR4,120/MTKADW,2460,TEST").unwrap();
        assert!(bad.weather.is_none());
    }

    #[test]
    fn rejects_non_ascii_and_huge_altitude() {
        assert!(parse("éABCDE/ID1234,,").is_none());
        assert!(parse("FTX/ID1234,,/FX€ab").is_none());
        let r = parse("POS/ID23571S,,/PSN31364E124002,120000,42949673/FXx0000").unwrap();
        assert_eq!(r.altitude_ft, None);
    }

    #[test]
    fn ddhhmm_range() {
        assert_eq!(ddhhmm("210114").as_deref(), Some("210114"));
        assert_eq!(ddhhmm("219914"), None);
        assert_eq!(ddhhmm("001200"), None);
        assert_eq!(ddhhmm("211260"), None);
    }

    #[test]
    fn ooi_and_req_pair() {
        let r = parse("OOI/ID76027S,KECK43,8VZ095L6S324/DC21112026,011458/MR4,/OF210114/CG229,110,360/FB794/VR321B31").unwrap();
        assert_eq!(r.description, "OOOI Event Report");
        assert_eq!(r.off_time.as_deref(), Some("210114"));
        let q = parse("- #MDREQPOS/ID60005A,RCH4133,JAM7869P2324/MR1,/AU337EB").unwrap();
        assert_eq!(q.kind, "REQPOS");
        assert_eq!(q.description, "Request for Position Report");
    }

    #[test]
    fn ftx_text() {
        let r = parse("FTX/ID50007B,RCH4086,ABB02R70E037/MR6,/FX4 QTR PHILLY UP 37-6307A").unwrap();
        assert_eq!(r.kind, "FTX");
        assert!(r.text.as_deref().unwrap().starts_with("4 QTR PHILLY UP"));
    }

    #[test]
    fn fpn_carries_route() {
        let r = parse("FPN/ID00534A,RCH3216,AJM7031H1325/MR3,/RP:DA:KCHS:AA:KNYG:F:FILLI..SARKY.Q113..AARNN..THHMP..LOOPR:V:SARKY,280,AT3100,,/WD,,,,ECB3").unwrap();
        let fp = r.flight_plan.unwrap();
        assert_eq!(fp.origin.as_deref(), Some("KCHS"));
        assert_eq!(fp.destination.as_deref(), Some("KNYG"));
        assert_eq!(r.mission.unwrap().day_of_year, 325);
    }

    #[test]
    fn multi_block_uplink_joins_are_removed() {
        let r = parse("- #MDFTX/ID55139A,RCH117,XMZM120GF318/MR1,/FXAH - YOU ALREADY HAVE A QT YOU JUST WANT A QUICKER TURN. WILL BRING IT UP TO DO. WILL MOST LIKELY BE TABLED UNTIL YOU LAND AND IT WILL BE CONSIDERED AN EGO VS FUEL ONLY QT. FM- #MD B2 SENDS.600C").unwrap();
        assert!(r.crc_ok);
        assert!(r.text.unwrap().ends_with("FM B2 SENDS."));
    }

    #[test]
    fn ftx_paren_substitutions_are_kept() {
        // https://app.airframes.io/messages/7755431084 and 7931914752: AMC free
        // text never contains '/' or ','; both are sent as '('.
        let r = parse("FTX/ID543615,CANNY52,202609300515/MR7,/FXTO ESP AND GIBAJF502 FLT FOR TOMORROW WILL BE ACTIVATED.ETD WILL BE 0100Z SO FAR.SO ASC HQ IS NOW REQUESTING TO CANX 4TH PERIOD TODAY.BTW CONFIRM T(O FUEL OF JF502 FROM RJOH IS 115K.0D88").unwrap();
        assert!(r.crc_ok);
        assert!(r
            .text
            .unwrap()
            .ends_with("CONFIRM T(O FUEL OF JF502 FROM RJOH IS 115K."));
        assert!(r.mission.is_none());
        let r = parse("- #MDFTX/ID80002A,BRK71,COLLINSAM123/MR2,/FXMETAR LIPA 260855Z 07009KT 9999 FEW080 15(06 Q1021 RMK FEW VIS MIN 9999 WIND THR05 (((((KT WIND THR23 (((((KT A3015 BLU15ED").unwrap();
        assert!(r.crc_ok);
        assert_eq!(
            r.text.as_deref(),
            Some("METAR LIPA 260855Z 07009KT 9999 FEW080 15(06 Q1021 RMK FEW VIS MIN 9999 WIND THR05 (((((KT WIND THR23 (((((KT A3015 BLU")
        );
    }

    #[test]
    fn ftx_sigmet_relay() {
        // https://app.airframes.io/messages/7655317188: body verbatim, header
        // and checksum rebuilt around it (only the text was copied).
        let body = "CZQX SIGMET F2 VALID 231520(231920 CWUL-CZQX GANDER OCEANIC FIR(CTA SEV TURB OBS AT 1509Z WI 180NM WID LINEBTN (N5630W03900( - (N6130 W03200( FL310(360 MOV WNW 10KT WKN";
        let r = parse(&format!(
            "FTX/ID80002A,BRK71,COLLINSAM123/MR2,/FX{body}2B23"
        ))
        .unwrap();
        assert!(r.crc_ok);
        assert_eq!(r.text.as_deref(), Some(body));
    }

    #[test]
    fn on_in_and_summary_times() {
        let r = parse("OOI/ID66015S,RAMEN95,CPZ595L5P323/DC19112026,192310/MR8,/ON191923/CG254,112,360/FB990/VR32F49A").unwrap();
        assert_eq!(r.on_time.as_deref(), Some("191923"));
        let r = parse("OOI/ID50005B,RCH2151,PBC03810C314/DC16112026,121054/MR152,/IN161210,16/CG,,/FB/VR032AE97").unwrap();
        assert!(r.crc_ok);
        assert_eq!(r.in_time.as_deref(), Some("161210"));
        assert_eq!(r.fuel_on_board, None);
        let r = parse("F25ACR1225#M2BSUM/ID00403A,GOLD38,ZZZ123456789/DC17112026,181139/MR120,/SM170742,2204,171811,170755,2203,170805,2200,171751,325,171811,310/VR32A36A").unwrap();
        assert_eq!(r.description, "Summary Report");
        assert_eq!(r.out_time.as_deref(), Some("170755"));
        assert_eq!(r.off_time.as_deref(), Some("170805"));
        assert_eq!(r.on_time.as_deref(), Some("171751"));
        assert_eq!(r.in_time.as_deref(), Some("171811"));
    }

    #[test]
    fn weather_and_cargo_requests() {
        let r = parse("REQWXR/ID16093S,RCH5070,DS8903461322/DC21112026,102803/MR18,/WX211915,2,KWRI,,,,,,,,,,,/FB1162/VR32B6AD").unwrap();
        assert!(r.crc_ok);
        assert_eq!(r.description, "Request for Weather Report");
        assert_eq!(r.weather_request_station.as_deref(), Some("KWRI"));
        assert_eq!(r.fuel_on_board, Some(1162));
        let r = parse("REQWXR/ID16093S,RCH5070,DS8903461322/DC21112026,102803/MR18,/WX211915,2,,,,/FB1162/VR326817").unwrap();
        assert_eq!(r.weather_request_station, None);
        let r = parse("- #MDREQCRG/ID77174A,RCH341,AMZM2016C322/MR2,230F").unwrap();
        assert!(r.crc_ok);
        assert_eq!(r.description, "Request for Cargo Report");
    }

    #[test]
    fn rejects_non_military_text() {
        assert!(parse("POSN47312W122300,KSEA").is_none());
        assert!(parse("#DFB/M1 POSRPT").is_none());
        assert!(parse("SOME FREE TEXT WITH /ID IN IT LATER ON THE LINE").is_none());
    }
}
