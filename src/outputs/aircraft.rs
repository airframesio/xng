//! Mode-agnostic aircraft-state adapter (XM-2.2). A position-bearing body from
//! any source — Mode S, UAT (978 ADS-B), or HFDL — is reduced to one normalized
//! [`AircraftFix`] keyed on the ICAO hex (the only aircraft id the 1090
//! ecosystem can carry), so the SBS, Beast, and `aircraft.json` outputs all feed
//! from a single extractor instead of four per-mode wirings.

use serde_json::Value;
use xng_types::{Message, MessageBody};

/// A normalized aircraft state fix. ACARS positions (ADS-C reports and
/// free-text position reports, from any carrier) are represented when the
/// ICAO address is known: from the ADS-C airframe-id group, else from the
/// tail through the loaded aircraft database (`--aircraft-db`).
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
    /// ADS-B with a non-ICAO / anonymous (or non-aircraft) address — distinct
    /// so the 1090 re-encode marks it non-ICAO (DF18 CF=1) instead of asserting
    /// the 24-bit value is a real ICAO.
    AdsbOther,
    /// TIS-B with an ICAO address.
    TisB,
    /// TIS-B with a non-ICAO address (track-file id) — distinct so the 1090
    /// re-encode preserves the non-ICAO fact (DF18 CF=5, not CF=2).
    TisBOther,
    AdsR,
    /// A position the aircraft reported over ACARS (ADS-C or free text). Not
    /// ADS-B, and often minutes old, so it goes to SBS only: Beast would
    /// re-encode it as a 1090 squitter that feeders forward as real ADS-B.
    Acars,
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
            // UAT address qualifier (DO-282B 2.2.4.5.1.2) → 1090 re-encode
            // provenance. ICAO-addressed sources keep their class: adsb_icao →
            // native ADS-B, tisb_icao → TIS-B, and adsr_other (qualifier 6, an
            // ADS-R target) carries the rebroadcast aircraft's *real* ICAO, so
            // it stays ADS-R (DF18 CF=6). The non-ICAO addresses get a non-ICAO
            // CF so a receiver can't read the 24-bit value back as a real ICAO:
            // tisb_trackfile → CF=5 (lossless), and adsb_other (qualifier 1,
            // self-assigned/anonymous) → ADS-B non-ICAO (DF18 CF=1). vehicle/
            // fixed_beacon fall through to native ADS-B as before (non-aircraft,
            // out of scope).
            source: match details.get("address_qualifier").and_then(Value::as_str) {
                Some("tisb_icao") => AircraftSource::TisB,
                Some("tisb_trackfile") => AircraftSource::TisBOther,
                Some("adsb_other") => AircraftSource::AdsbOther,
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
                // HFDL-7: dumphfdl's --freq-as-squawk convention — convey the
                // HFDL channel frequency (kHz) in the Basestation/Beast squawk
                // slot so SBS consumers see which channel carried the fix.
                // Opt-in via env (matches the codebase's XNG_* flag pattern);
                // off by default (a real squawk would be more useful if HFDL
                // carried one, but it does not).
                squawk: if std::env::var("XNG_HFDL_FREQ_AS_SQUAWK").is_ok() {
                    Some((msg.frequency_hz / 1000).to_string())
                } else {
                    None
                },
                source: AircraftSource::Adsb,
            })
        }
        MessageBody::Acars(a) => acars_fix(a),
        _ => None,
    }
}

/// An aircraft fix from an ACARS message's decoded application layer: the
/// ADS-C basic report (with earth-reference track/speed/vertical rate and the
/// flight-id callsign when sent), else a free-text position report (kept only
/// for downlinks by `xng_acars`). Predicted and projected positions are not
/// where the aircraft is and are ignored.
fn acars_fix(a: &xng_types::AcarsCore) -> Option<AircraftFix> {
    let app = a.app.as_ref()?;
    let structured = xng_acars::structured_app(app);
    let mut fix = AircraftFix {
        icao: String::new(),
        callsign: a.flight.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from),
        altitude_ft: None,
        lat: None,
        lon: None,
        speed_kt: None,
        speed_is_airspeed: false,
        track_deg: None,
        vertical_rate_fpm: None,
        squawk: None,
        source: AircraftSource::Acars,
    };
    let mut icao = None;
    if structured.get("app").and_then(Value::as_str) == Some("adsc") {
        for t in structured.get("tags").and_then(Value::as_array).into_iter().flatten() {
            match t.get("tag").and_then(Value::as_str) {
                Some("report") => {
                    // Motion belongs to the report it follows: a second report
                    // in the same downlink must not inherit the first one's.
                    fix.track_deg = None;
                    fix.speed_kt = None;
                    fix.vertical_rate_fpm = None;
                    fix.lat = jf(t, "lat");
                    fix.lon = jf(t, "lon");
                    fix.altitude_ft = ji(t, "alt_ft");
                }
                Some("earth_ref") => {
                    if t.get("true_track_valid").and_then(Value::as_bool) == Some(true) {
                        fix.track_deg = jf(t, "true_track_deg");
                    }
                    fix.speed_kt = jf(t, "ground_speed_kt");
                    fix.vertical_rate_fpm = ji(t, "vert_speed_fpm");
                }
                Some("flight_id") => {
                    if let Some(id) = js(t, "id") {
                        fix.callsign = Some(id);
                    }
                }
                Some("airframe_id") => icao = js(t, "icao").and_then(|s| norm_icao(&s)),
                _ => {}
            }
        }
    }
    if fix.lat.is_none() {
        let p = app.get("position")?;
        fix.lat = jf(p, "latitude");
        fix.lon = jf(p, "longitude");
    }
    let (lat, lon) = (fix.lat?, fix.lon?);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    fix.icao = icao.or_else(|| {
        let tail = a.tail.as_deref()?;
        super::dbinfo::AircraftDb::icao_for_reg(tail).map(|i| format!("{i:06X}"))
    })?;
    Some(fix)
}
