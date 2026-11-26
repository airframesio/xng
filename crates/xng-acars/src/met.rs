//! Winds-aloft / meteorological fields from ACARS position-weather reports.
//!
//! Scope note: the WMO-BUFR-class AMDAR binary schema (phase/roll-flag/
//! turbulence/humidity, NOAA `dcacar`) is **not** carried in any airframes
//! documented example and is intentionally out of scope here — there is no
//! real reference to verify a decoder against. What *is* documented and
//! verifiable is the free-text `4J` "POSWX" position-and-weather report
//! (airframes acars-message-documentation `research/4J.md`), which carries
//! the standard winds-aloft met set in slash-delimited IEI fields:
//!
//!   `/WND 334060` — wind 334° at 60 kt
//!   `/SAT -032`   — static air temperature −32 °C
//!   `/TAS 490`    — true airspeed 490 kt
//!   `/ALT 270`    — altitude / flight level FL270 (27000 ft)
//!
//! This module decodes that verifiable met set. Temperature uses airframes'
//! own convention (`M`→minus, `P`→plus; `research/H1/POS.md`,
//! acars-decoder-typescript `ResultFormatter.temperature`).

use serde::Serialize;

/// Winds-aloft / met fields extracted from a position-weather report.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Met {
    /// Wind direction in degrees true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_dir_deg: Option<u16>,
    /// Wind speed in knots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_speed_kt: Option<u16>,
    /// Static / outside air temperature in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<i16>,
    /// True airspeed in knots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_airspeed_kt: Option<u16>,
    /// Altitude in feet (flight level × 100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude_ft: Option<u32>,
}

impl Met {
    fn is_empty(&self) -> bool {
        self.wind_dir_deg.is_none()
            && self.wind_speed_kt.is_none()
            && self.temperature_c.is_none()
            && self.true_airspeed_kt.is_none()
            && self.altitude_ft.is_none()
    }
}

/// Parse airframes' temperature convention: a leading `M` (minus) / `P`
/// (plus) or an explicit sign, then digits. `M48` → −48, `-032` → −32.
fn parse_temp(s: &str) -> Option<i16> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let normalized = s.replace('M', "-").replace('P', "+");
    normalized.parse::<i16>().ok()
}

/// `/WND dddss` — direction (3 digits) + speed (2-3 digits).
fn parse_wind(s: &str) -> (Option<u16>, Option<u16>) {
    let s = s.trim();
    if s.len() < 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return (None, None);
    }
    let dir = s[..3].parse::<u16>().ok().filter(|&d| d <= 360);
    let speed = s[3..].parse::<u16>().ok();
    (dir, speed)
}

/// Decode the met fields from a `4J` POSWX position-weather report. Returns
/// `None` when no met field is present.
pub fn decode(label: &str, text: &str) -> Option<Met> {
    if label != "4J" {
        return None;
    }
    let mut m = Met::default();
    for part in text.split('/') {
        if let Some(v) = part.strip_prefix("WND ").or_else(|| part.strip_prefix("WND")) {
            let (dir, spd) = parse_wind(v);
            m.wind_dir_deg = dir;
            m.wind_speed_kt = spd;
        } else if let Some(v) = part.strip_prefix("SAT ").or_else(|| part.strip_prefix("SAT")) {
            m.temperature_c = parse_temp(v);
        } else if let Some(v) = part.strip_prefix("TAS ").or_else(|| part.strip_prefix("TAS")) {
            m.true_airspeed_kt = v.trim().parse().ok();
        } else if let Some(v) = part.strip_prefix("ALT ").or_else(|| part.strip_prefix("ALT")) {
            // Flight level in hundreds of feet (FL270 → 27000 ft).
            if let Ok(fl) = v.trim().parse::<u32>() {
                m.altitude_ft = Some(fl * 100);
            }
        }
    }
    if m.is_empty() { None } else { Some(m) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference string + expected values are the real documented example
    // from airframes' acars-message-documentation research/4J.md
    // (airframes.io message 880996538).
    const POSWX_4J: &str = "4J01 POSWX 0318/20 ETAD/ETAD .00318S\n\
        /POS N5043.5E01121.8/OVR 0817\n\
        /ALT 270/TFW 1342/TAS 490/SAT -032\n\
        /POS GOVEN /OVR 0835\n\
        /POS DILVI\n\
        /WND 334060/TRB /SKY DCC3";

    #[test]
    fn poswx_met_fields() {
        let m = decode("4J", POSWX_4J).unwrap();
        // research/4J.md: WND 334 deg / 060 kt, SAT -32, TAS 490, ALT FL270.
        assert_eq!(m.wind_dir_deg, Some(334));
        assert_eq!(m.wind_speed_kt, Some(60));
        assert_eq!(m.temperature_c, Some(-32));
        assert_eq!(m.true_airspeed_kt, Some(490));
        assert_eq!(m.altitude_ft, Some(27000));
    }

    #[test]
    fn temperature_sign_conventions() {
        // M = minus, P = plus, explicit sign all per airframes convention.
        assert_eq!(parse_temp("M48"), Some(-48));
        assert_eq!(parse_temp("-032"), Some(-32));
        assert_eq!(parse_temp("P15"), Some(15));
        assert_eq!(parse_temp("020"), Some(20));
        assert_eq!(parse_temp(""), None);
    }

    #[test]
    fn non_4j_is_none() {
        assert!(decode("H1", "#CFBFLR/something").is_none());
        assert!(decode("20", "POSN38160W077075").is_none());
    }

    #[test]
    fn report_without_met_is_none() {
        assert!(decode("4J", "POS/ID91459S,BANKR31,/DC03032024").is_none());
    }
}
