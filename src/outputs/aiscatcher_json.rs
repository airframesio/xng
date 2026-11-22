//! AIS-catcher HTTP JSON output (FEED-2.3): xng AIS messages in the shape
//! AIS-catcher posts with `-H … PROTOCOL AIRFRAMES`/`jsonaiscatcher`, which
//! the Airframes AIS intake (`http://feed.airframes.io:5599/`) takes in
//! batches: `{protocol, encodetime, stationid, receiver, device, msgs:[…]}`.
//!
//! xng's decoded field names (`sog_kt`, `nav_status`, `heading_deg`, …) are
//! mapped to AIS-catcher's scaled names (`speed`, `status`, `heading`, …).
//! Types matter: the intake's model reads booleans as `bool` and the codes
//! and dimensions as integers, and rejects a whole batch on a mismatch.

use serde_json::{json, Map, Value};
use xng_types::{Message, MessageBody, Mode};

/// xng `nav_status` text → AIS navigation status code (ITU-R M.1371).
const NAV_STATUS: [&str; 16] = [
    "under way (engine)",
    "at anchor",
    "not under command",
    "restricted manoeuvrability",
    "constrained by draught",
    "moored",
    "aground",
    "engaged in fishing",
    "under way (sailing)",
    "reserved (HSC)",
    "reserved (WIG)",
    "reserved",
    "reserved",
    "reserved",
    "AIS-SART",
    "undefined",
];

/// xng `epfd` text → EPFD code.
fn epfd_code(name: &str) -> Option<u64> {
    Some(match name {
        "undefined" => 0,
        "GPS" => 1,
        "GLONASS" => 2,
        "GPS+GLONASS" => 3,
        "Loran-C" => 4,
        "Chayka" => 5,
        "integrated" => 6,
        "surveyed" => 7,
        "Galileo" => 8,
        "internal GNSS" => 15,
        _ => return None,
    })
}

/// The AIVDM radio channel letter (sentence field 5), if any.
fn channel(nmea: &[String]) -> Option<String> {
    let first = nmea.first()?;
    let ch = first.split(',').nth(4)?;
    (!ch.is_empty()).then(|| ch.to_string())
}

/// Render one AIS message as an AIS-catcher `msgs[]` entry. `None` for
/// non-AIS messages or ones without a type and MMSI.
pub fn format_aiscatcher(msg: &Message) -> Option<Value> {
    if msg.mode != Mode::Ais {
        return None;
    }
    let MessageBody::Ais {
        nmea,
        msg_type,
        mmsi,
        details,
    } = &msg.body
    else {
        return None;
    };
    let (msg_type, mmsi) = ((*msg_type)?, (*mmsi)?);
    let empty = Map::new();
    let d = details
        .as_ref()
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut m = Map::new();
    m.insert("class".into(), json!("AIS"));
    m.insert("device".into(), json!("xng"));
    m.insert(
        "rxtime".into(),
        json!(msg.timestamp.format("%Y%m%d%H%M%S").to_string()),
    );
    m.insert("scaled".into(), json!(true));
    if let Some(c) = channel(nmea) {
        m.insert("channel".into(), json!(c));
    }
    m.insert("nmea".into(), json!(nmea));
    if let Some(l) = msg.signal.rssi_db {
        m.insert(
            "signalpower".into(),
            json!((l as f64 * 10.0).round() / 10.0),
        );
    }
    m.insert("type".into(), json!(msg_type));
    m.insert("mmsi".into(), json!(mmsi));

    // Copy a field under AIS-catcher's name, keeping it a float / an integer
    // / a bool / a string as the intake's model expects.
    let mut float = |from: &str, to: &str| {
        if let Some(v) = d.get(from).and_then(Value::as_f64) {
            m.insert(to.into(), json!(v));
        }
    };
    float("lat", "lat");
    float("lon", "lon");
    float("sog_kt", "speed");
    float("cog_deg", "course");
    float("draught_m", "draught");
    let mut int = |from: &str, to: &str| {
        if let Some(v) = d
            .get(from)
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        {
            m.insert(to.into(), json!(v));
        }
    };
    int("heading_deg", "heading");
    int("timestamp_sec", "second");
    int("maneuver", "maneuver");
    int("ais_version", "ais_version");
    int("imo", "imo");
    int("ship_type", "shiptype");
    int("to_bow", "to_bow");
    int("to_stern", "to_stern");
    int("to_port", "to_port");
    int("to_starboard", "to_starboard");
    int("aton_type", "aid_type");
    int("model", "model");
    int("serial", "serial");
    let mut boolean = |from: &str, to: &str| {
        if let Some(v) = d.get(from).and_then(Value::as_bool) {
            m.insert(to.into(), json!(v));
        }
    };
    boolean("position_accuracy", "accuracy");
    boolean("raim", "raim");
    boolean("off_position", "off_position");
    boolean("virtual_aid", "virtual_aid");
    boolean("dte_ready", "dte");
    let mut text = |from: &str, to: &str| {
        if let Some(v) = d
            .get(from)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            m.insert(to.into(), json!(v));
        }
    };
    text("callsign", "callsign");
    text("destination", "destination");
    text("vendor_id", "vendorid");
    // Type 21 names the aid to navigation `name`; every other type `shipname`.
    text("name", if msg_type == 21 { "name" } else { "shipname" });

    if let Some(rot) = d.get("rot_deg_min").and_then(Value::as_f64) {
        // AIS-catcher's scaled `turn` is whole degrees per minute.
        m.insert("turn".into(), json!(rot.round() as i64));
    }
    if let Some(s) = d.get("nav_status").and_then(Value::as_str) {
        if let Some(code) = NAV_STATUS.iter().position(|n| *n == s) {
            m.insert("status".into(), json!(code));
            m.insert("status_text".into(), json!(s));
        }
    }
    if let Some(e) = d.get("epfd").and_then(Value::as_str) {
        if let Some(code) = epfd_code(e) {
            m.insert("epfd".into(), json!(code));
            m.insert("epfd_text".into(), json!(e));
        }
    }
    // Type 5 ETA "MM-DDTHH:MM" → month/day/hour/minute.
    if let Some(eta) = d.get("eta").and_then(Value::as_str) {
        let parts: Vec<u64> = eta
            .split(['-', 'T', ':'])
            .filter_map(|p| p.parse().ok())
            .collect();
        if let [mo, da, h, mi] = parts[..] {
            for (k, v) in [("month", mo), ("day", da), ("hour", h), ("minute", mi)] {
                m.insert(k.into(), json!(v));
            }
        }
    }
    // Type 4 / 11 UTC "YYYY-MM-DDTHH:MM:SSZ" → year..second.
    if let Some(utc) = d.get("utc").and_then(Value::as_str) {
        let parts: Vec<u64> = utc
            .trim_end_matches('Z')
            .split(['-', 'T', ':'])
            .filter_map(|p| p.parse().ok())
            .collect();
        if let [y, mo, da, h, mi, s] = parts[..] {
            for (k, v) in [
                ("year", y),
                ("month", mo),
                ("day", da),
                ("hour", h),
                ("minute", mi),
                ("second", s),
            ] {
                m.insert(k.into(), json!(v));
            }
        }
    }
    // Type 24: part A carries the name, part B the type and dimensions.
    if msg_type == 24 {
        let part = if d.contains_key("ship_type") || d.contains_key("callsign") {
            1
        } else {
            0
        };
        m.insert("partno".into(), json!(part));
    }
    Some(Value::Object(m))
}

/// One AIS intake POST body: AIS-catcher's `jsonaiscatcher` envelope.
pub fn envelope(station: &str, msgs: Vec<Value>) -> Value {
    json!({
        "protocol": "jsonaiscatcher",
        "encodetime": chrono::Utc::now().format("%Y%m%d%H%M%S").to_string(),
        "stationid": station,
        "receiver": {
            "description": "xng",
            "version": 0,
            "engine": format!("xng {}", env!("CARGO_PKG_VERSION")),
            "setting": "",
        },
        "device": { "product": "xng", "vendor": "", "serial": "", "setting": "" },
        "msgs": msgs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use xng_types::*;

    fn msg(msg_type: u8, details: Value) -> Message {
        Message {
            mode: Mode::Ais,
            timestamp: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 5).unwrap(),
            frequency_hz: 161_975_000,
            signal: SignalQuality {
                rssi_db: Some(-12.34),
                ..Default::default()
            },
            decode: DecodeQuality {
                crc_ok: true,
                ..Default::default()
            },
            body: MessageBody::Ais {
                nmea: vec!["!AIVDM,1,1,,B,15M67FC000G?ufbE`FepT@3n00Sa,0*5C".into()],
                msg_type: Some(msg_type),
                mmsi: Some(366_053_209),
                details: Some(details),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("XX-TEST"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn position_report_uses_aiscatcher_names_and_types() {
        let m = msg(
            1,
            json!({
                "nav_status": "moored", "rot_deg_min": 0.0, "sog_kt": 0.0,
                "position_accuracy": false, "lat": 47.582833, "lon": -122.345833,
                "cog_deg": 51.0, "heading_deg": 181, "timestamp_sec": 15, "raim": false,
            }),
        );
        let v = format_aiscatcher(&m).unwrap();
        assert_eq!(v["type"], 1);
        assert_eq!(v["mmsi"], 366_053_209);
        assert_eq!(v["rxtime"], "20261122120005");
        assert_eq!(v["channel"], "B");
        assert_eq!(v["scaled"], true);
        assert_eq!(v["status"], 5);
        assert_eq!(v["status_text"], "moored");
        assert_eq!(v["turn"], 0);
        assert_eq!(v["speed"], 0.0);
        assert_eq!(v["accuracy"], false);
        assert_eq!(v["lat"], 47.582833);
        assert_eq!(v["lon"], -122.345833);
        assert_eq!(v["course"], 51.0);
        assert!(v["heading"].is_u64(), "heading must be an integer");
        assert_eq!(v["heading"], 181);
        assert_eq!(v["second"], 15);
        assert_eq!(v["raim"], false);
        assert_eq!(v["signalpower"], -12.3);
    }

    #[test]
    fn static_voyage_report_maps_names_eta_and_epfd() {
        let m = msg(
            5,
            json!({
                "ais_version": 0, "imo": 6710932, "callsign": "WDA9674", "name": "MT.MITCHELL",
                "ship_type": 99, "to_bow": 90, "to_stern": 90, "to_port": 10, "to_starboard": 10,
                "epfd": "GPS", "draught_m": 6.0, "destination": "SEATTLE", "eta": "01-02T08:00",
                "dte_ready": true,
            }),
        );
        let v = format_aiscatcher(&m).unwrap();
        assert_eq!(v["imo"], 6710932);
        assert_eq!(v["shipname"], "MT.MITCHELL");
        assert_eq!(v["shiptype"], 99);
        assert_eq!(v["epfd"], 1);
        assert_eq!(v["epfd_text"], "GPS");
        assert_eq!(v["draught"], 6.0);
        assert_eq!(
            (
                v["month"].clone(),
                v["day"].clone(),
                v["hour"].clone(),
                v["minute"].clone()
            ),
            (json!(1), json!(2), json!(8), json!(0))
        );
        assert_eq!(v["dte"], true);
    }

    #[test]
    fn aton_keeps_name_and_class_b_static_gets_partno() {
        let v = format_aiscatcher(&msg(
            21,
            json!({ "aton_type": 14, "name": "BUOY 3", "virtual_aid": false }),
        ))
        .unwrap();
        assert_eq!(v["name"], "BUOY 3");
        assert_eq!(v["aid_type"], 14);
        assert_eq!(v["virtual_aid"], false);
        let a = format_aiscatcher(&msg(24, json!({ "name": "SEA LION" }))).unwrap();
        assert_eq!(
            (a["partno"].clone(), a["shipname"].clone()),
            (json!(0), json!("SEA LION"))
        );
        let b =
            format_aiscatcher(&msg(24, json!({ "ship_type": 37, "callsign": "WX1234" }))).unwrap();
        assert_eq!(b["partno"], 1);
    }

    #[test]
    fn envelope_names_the_station() {
        let e = envelope("XX-TEST-AIS", vec![json!({ "type": 1 })]);
        assert_eq!(e["protocol"], "jsonaiscatcher");
        assert_eq!(e["stationid"], "XX-TEST-AIS");
        assert_eq!(e["msgs"].as_array().unwrap().len(), 1);
    }
}
