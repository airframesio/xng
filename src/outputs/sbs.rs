//! SBS-1 ("BaseStation") output: the CSV line protocol dump1090/readsb
//! serve on TCP 30003, consumed by Virtual Radar Server, PlanePlotter,
//! and most ADS-B aggregator feeders. We serve it the same way: listen,
//! and stream MSG lines to every connected client.

use std::sync::Arc;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

/// A normalized aircraft state fix, extracted from any position-bearing body
/// and keyed on the ICAO hex (the only aircraft id SBS/BaseStation can carry).
/// This is the mode-agnostic adapter (XM-2.2): Mode S, UAT (978 ADS-B) and
/// HFDL aircraft positions all converge here, so HFDL/UAT aircraft reach
/// tar1090 / VRS / aggregator feeders over the same `:30003` stream.
struct AircraftFix {
    icao: String,
    callsign: Option<String>,
    altitude_ft: Option<i32>,
    lat: Option<f64>,
    lon: Option<f64>,
    speed_kt: Option<f64>,
    /// true when `speed_kt` is *airspeed* (Mode S BDS 6,0 "AS"), not ground
    /// speed — kept out of the velocity (MSG,4) classification but still shown.
    speed_is_airspeed: bool,
    track_deg: Option<f64>,
    vertical_rate_fpm: Option<i32>,
    squawk: Option<String>,
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

/// Extract a normalized aircraft fix from any supported body. Only bodies that
/// carry an ICAO hex *and* some reportable state map (Mode S, UAT ADS-B, HFDL
/// position HFNPDUs). ACARS/Aero ADS-C positions key on flight/registration,
/// not ICAO, so they have no SBS hex id and are not emitted here.
fn aircraft_fix(msg: &Message) -> Option<AircraftFix> {
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
            })
        }
        _ => None,
    }
}

/// Render a message as an SBS-1 ("BaseStation") MSG line. Any body that yields
/// an [`AircraftFix`] (Mode S / UAT / HFDL) maps; everything else returns None.
pub fn format_sbs(msg: &Message) -> Option<String> {
    let f = aircraft_fix(msg)?;
    let icao = &f.icao;
    // Transmission type: 1 ident, 3 airborne position, 4 velocity,
    // 5 surveillance altitude, 6 squawk.
    let tt = if f.lat.is_some() {
        3
    } else if f.speed_kt.is_some() && !f.speed_is_airspeed {
        4
    } else if f.callsign.is_some() {
        1
    } else if f.squawk.is_some() {
        6
    } else if f.altitude_ft.is_some() {
        5
    } else {
        return None;
    };
    let d = msg.timestamp.format("%Y/%m/%d");
    let t = msg.timestamp.format("%H:%M:%S%.3f");
    let fmt_f = |v: Option<f64>, p: usize| v.map(|x| format!("{x:.p$}")).unwrap_or_default();
    let fmt_i = |v: Option<i32>| v.map(|x| x.to_string()).unwrap_or_default();
    Some(format!(
        "MSG,{tt},1,1,{icao},1,{d},{t},{d},{t},{},{},{},{},{},{},{},{},,,,",
        f.callsign.as_deref().unwrap_or(""),
        fmt_i(f.altitude_ft),
        fmt_f(f.speed_kt, 1),
        fmt_f(f.track_deg, 1),
        fmt_f(f.lat, 5),
        fmt_f(f.lon, 5),
        fmt_i(f.vertical_rate_fpm),
        f.squawk.as_deref().unwrap_or(""),
    ))
}

/// Serve SBS lines on `addr` (e.g. `0.0.0.0:30003`).
pub async fn run(rx: broadcast::Receiver<Arc<Message>>, addr: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("SBS (BaseStation) output on {addr}");
    loop {
        let (mut sock, peer) = listener.accept().await?;
        tracing::info!("SBS client connected: {peer}");
        let mut rx = rx.resubscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let Some(line) = format_sbs(&msg) {
                            if sock.write_all(format!("{line}\r\n").as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_types::{DecodeQuality, Mode, Provenance, SignalQuality, StationIdentity};

    #[test]
    fn position_message_renders_msg3() {
        let msg = Message {
            mode: Mode::Adsb,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_090_000_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::ModeS {
                df: 17,
                icao: Some("40621D".into()),
                callsign: None,
                altitude_ft: Some(38_000),
                squawk: None,
                lat: Some(52.2572),
                lon: Some(3.91937),
                speed_kt: None,
                speed_type: None,
                track_deg: None,
                vertical_rate_fpm: None,
                comm_b: None,
                adsb_status: None,
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: xng_types::AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let line = format_sbs(&msg).unwrap();
        assert!(line.starts_with("MSG,3,1,1,40621D,1,"), "{line}");
        assert!(line.contains(",38000,"), "{line}");
        assert!(line.contains(",52.25720,3.91937,"), "{line}");
    }

    fn msg_with(mode: Mode, body: MessageBody) -> Message {
        Message {
            mode,
            timestamp: chrono::Utc::now(),
            frequency_hz: 0,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body,
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: xng_types::AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    // XM-2.2: a UAT 978 ADS-B state vector renders an SBS position line keyed
    // on the same ICAO hex Mode S uses, so 978 traffic reaches tar1090/VRS.
    #[test]
    fn uat_position_renders_sbs() {
        let body = MessageBody::Uat {
            kind: "adsb".into(),
            details: serde_json::json!({
                "address": "a1b2c3", "callsign": "N12345",
                "geometric_altitude": 9500, "ground_speed": 142.0,
                "true_track": 271.0, "lat": 37.6189, "lon": -122.3750,
            }),
        };
        let line = format_sbs(&msg_with(Mode::Uat, body)).unwrap();
        assert!(line.starts_with("MSG,3,1,1,A1B2C3,1,"), "{line}");
        assert!(line.contains(",N12345,"), "{line}");
        assert!(line.contains(",9500,"), "{line}");
        assert!(line.contains(",37.61890,-122.37500,"), "{line}");
    }

    // XM-2.2: an HFDL position HFNPDU (ICAO from the logon cache) renders too.
    #[test]
    fn hfdl_position_renders_sbs() {
        let body = MessageBody::Hfdl {
            kind: "hfnpdu".into(),
            details: serde_json::json!({
                "icao": "ABCDEF",
                "position": { "lat": 51.5, "lon": -0.12, "flight": "BAW123", "icao": "ABCDEF" },
            }),
        };
        let line = format_sbs(&msg_with(Mode::Hfdl, body)).unwrap();
        assert!(line.starts_with("MSG,3,1,1,ABCDEF,1,"), "{line}");
        assert!(line.contains(",BAW123,"), "{line}");
        assert!(line.contains(",51.50000,-0.12000,"), "{line}");
    }

    // A non-position body (or one without an ICAO) yields no SBS line.
    #[test]
    fn non_aircraft_body_has_no_sbs_line() {
        let body = MessageBody::Hfdl { kind: "logon".into(), details: serde_json::json!({ "who": {} }) };
        assert!(format_sbs(&msg_with(Mode::Hfdl, body)).is_none());
    }
}
