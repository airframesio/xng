//! Mode-agnostic aircraft-state adapter (XM-2.2). A position-bearing body from
//! any source — Mode S, UAT (978 ADS-B), or HFDL — is reduced to one normalized
//! [`AircraftFix`] keyed on the ICAO hex (the only aircraft id the 1090
//! ecosystem can carry), so the SBS, Beast, and `aircraft.json` outputs all feed
//! from a single extractor instead of four per-mode wirings.

use serde_json::Value;
use xng_types::{Message, MessageBody};

/// A normalized aircraft state fix. ACARS/Aero ADS-C positions key on
/// flight/registration (no ICAO hex) and so are not represented here.
pub(crate) struct AircraftFix {
    pub icao: String,
    pub callsign: Option<String>,
    pub altitude_ft: Option<i32>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub speed_kt: Option<f64>,
    /// true when `speed_kt` is *airspeed* (Mode S BDS 6,0 "AS"), not ground
    /// speed — kept out of the velocity (SBS MSG,4) classification but shown.
    pub speed_is_airspeed: bool,
    pub track_deg: Option<f64>,
    pub vertical_rate_fpm: Option<i32>,
    pub squawk: Option<String>,
    /// Provenance class for synthesized 1090 frames (Beast): native ADS-B vs
    /// a TIS-B / ADS-R rebroadcast (from a UAT 978 address qualifier). Drives
    /// DF17-vs-DF18 selection so replotted UAT traffic keeps its source class.
    pub source: AircraftSource,
}

/// How an aircraft fix was originated, for re-encoding onto 1090 (NEW-P0-1.3).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AircraftSource {
    #[default]
    Adsb,
    TisB,
    AdsR,
}

fn jf(d: &Value, k: &str) -> Option<f64> {
    d.get(k).and_then(Value::as_f64)
}
fn ji(d: &Value, k: &str) -> Option<i32> {
    d.get(k).and_then(Value::as_i64).map(|x| x as i32)
}
fn js(d: &Value, k: &str) -> Option<String> {
    d.get(k).and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
/// Validate + canonicalize a 24-bit ICAO hex address.
fn norm_icao(s: &str) -> Option<String> {
    let s = s.trim().to_uppercase();
    (s.len() == 6 && s != "000000" && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// Extract a normalized aircraft fix from any supported body (Mode S, UAT
/// ADS-B, HFDL position HFNPDUs). `None` for bodies with no ICAO-keyed state.
pub(crate) fn aircraft_fix(msg: &Message) -> Option<AircraftFix> {
    match &msg.body {
        MessageBody::ModeS {
            icao,
            callsign,
            altitude_ft,
            squawk,
            lat,
            lon,
            speed_kt,
            speed_type,
            track_deg,
            vertical_rate_fpm,
            ..
        } => Some(AircraftFix {
            icao: norm_icao(icao.as_deref()?)?,
            callsign: callsign.clone(),
            altitude_ft: *altitude_ft,
            lat: *lat,
            lon: *lon,
            speed_kt: *speed_kt,
            speed_is_airspeed: speed_type.as_deref() == Some("AS"),
            track_deg: *track_deg,
            vertical_rate_fpm: *vertical_rate_fpm,
            squawk: squawk.clone(),
            source: AircraftSource::Adsb,
        }),
        // UAT 978 MHz ADS-B downlink — a real aircraft state vector.
        MessageBody::Uat { kind, details } if kind == "adsb" => Some(AircraftFix {
            icao: norm_icao(details.get("address").and_then(Value::as_str)?)?,
            callsign: js(details, "callsign"),
            altitude_ft: ji(details, "geometric_altitude").or_else(|| ji(details, "altitude")),
            lat: jf(details, "lat"),
            lon: jf(details, "lon"),
            speed_kt: jf(details, "ground_speed"),
            speed_is_airspeed: false,
            track_deg: jf(details, "true_track"),
            vertical_rate_fpm: ji(details, "vertical_rate"),
            squawk: None,
            // UAT address qualifier → 1090 rebroadcast provenance: tisb_* →
            // TIS-B, adsr_other → ADS-R, everything else is native ADS-B.
            source: match details.get("address_qualifier").and_then(Value::as_str) {
                Some("tisb_icao") | Some("tisb_trackfile") => AircraftSource::TisB,
                Some("adsr_other") => AircraftSource::AdsR,
                _ => AircraftSource::Adsb,
            },
        }),
        // HFDL position HFNPDU: lat/lon (+ flight), ICAO back-filled from the
        // logon cache (on the nested `position`, else top-level `icao`).
        MessageBody::Hfdl { details, .. } => {
            let pos = details.get("position")?;
            let icao = norm_icao(
                pos.get("icao")
                    .and_then(Value::as_str)
                    .or_else(|| details.get("icao").and_then(Value::as_str))?,
            )?;
            Some(AircraftFix {
                icao,
                callsign: js(pos, "flight"),
                altitude_ft: None,
                lat: jf(pos, "lat"),
                lon: jf(pos, "lon"),
                speed_kt: None,
                speed_is_airspeed: false,
                track_deg: None,
                vertical_rate_fpm: None,
                squawk: None,
                source: AircraftSource::Adsb,
            })
        }
        _ => None,
    }
}
