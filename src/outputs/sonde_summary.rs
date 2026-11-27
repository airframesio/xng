//! Radiosonde frames as Horus UDP "payload summary" JSON, the packet
//! radiosonde_auto_rx sends per decoded frame (`autorx/ozimux.py`), which the
//! Airframes sonde intake (`feed.airframes.io:5573`, UDP) takes from auto_rx
//! and xng alike. Field meanings follow auto_rx: `speed` km/h, `vel_h` /
//! `vel_v` m/s, `datetime` ISO 8601 UTC, `callsign` the sonde serial.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{json, Value};
use xng_types::{Message, MessageBody};

/// GPS time runs ahead of UTC by the leap seconds since 1980 (18 since 2017).
const GPS_UTC_LEAP_SECONDS: i64 = 18;

/// UTC time of a GPS week + time of week (ms).
pub fn gps_to_utc(week: u64, tow_ms: u64) -> Option<DateTime<Utc>> {
    let epoch = Utc.with_ymd_and_hms(1980, 1, 6, 0, 0, 0).single()?;
    let ms = i64::try_from(week.checked_mul(604_800_000)?.checked_add(tow_ms)?).ok()?;
    Some(epoch + Duration::milliseconds(ms) - Duration::seconds(GPS_UTC_LEAP_SECONDS))
}

/// One payload summary for a sonde message with a GPS fix, else `None`.
pub fn format_payload_summary(msg: &Message, station_id: &str) -> Option<Value> {
    let MessageBody::Sonde { kind, details } = &msg.body else {
        return None;
    };
    let serial = details.get("serial")?.as_str()?.trim();
    if serial.is_empty() {
        return None;
    }
    let pos = details.get("gps_pos")?;
    let lat = pos.get("lat")?.as_f64()?;
    let lon = pos.get("lon")?.as_f64()?;
    if !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
        || (lat == 0.0 && lon == 0.0)
    {
        return None;
    }
    // No navigation solution: an all-zero ECEF block decodes to lat 180.
    if pos.get("num_sv").and_then(Value::as_u64) == Some(0) {
        return None;
    }
    let alt = pos.get("alt_m").and_then(Value::as_f64);
    let speed = pos.get("speed_ms").and_then(Value::as_f64);
    // The frame's own GPS time when it decoded; otherwise when xng heard it.
    let when = details
        .get("gps_time")
        .and_then(|t| Some((t.get("week")?.as_u64()?, t.get("tow_ms")?.as_u64()?)))
        .filter(|(week, _)| *week > 0)
        .and_then(|(week, tow)| gps_to_utc(week, tow))
        .unwrap_or(msg.timestamp);
    let model = kind.to_ascii_uppercase();
    let mhz = msg.frequency_hz as f64 / 1e6;
    let mut v = json!({
        "type": "PAYLOAD_SUMMARY",
        "station": station_id,
        "callsign": serial,
        "latitude": lat,
        "longitude": lon,
        "altitude": alt,
        "speed": speed.map(|s| s * 3.6).unwrap_or(-1.0),
        "heading": pos.get("course_deg").and_then(Value::as_f64).unwrap_or(-1.0),
        "time": when.format("%H:%M:%S").to_string(),
        "datetime": when.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        "comment": "Radiosonde",
        "model": model,
        "freq": format!("{mhz:.3} MHz"),
        "f_centre": msg.frequency_hz as f64,
        "frame": details.get("frame_num").and_then(Value::as_u64),
        "vel_h": speed,
        "vel_v": pos.get("climb_ms").and_then(Value::as_f64),
        "sats": pos.get("num_sv").and_then(Value::as_u64),
        "batt": details.get("battery_v").and_then(Value::as_f64),
        // RS41 temperature/humidity/pressure need the full calibration table
        // (51 frames); single-frame decodes have none, sent as auto_rx does.
        "temp": -273.0,
        "humidity": -1.0,
        "pressure": -1.0,
        "software_name": "xng",
        "software_version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(snr) = msg.signal.snr_db {
        v["snr"] = json!(snr);
    }
    if let Some(rssi) = msg.signal.rssi_db {
        v["rssi"] = json!(rssi);
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fix_or_out_of_range_positions_are_skipped() {
        let msg = |lat: f64, num_sv: u8| Message {
            mode: xng_types::Mode::Sonde,
            timestamp: Utc::now(),
            frequency_hz: 403_000_000,
            signal: Default::default(),
            decode: Default::default(),
            body: MessageBody::Sonde {
                kind: "rs41".into(),
                details: json!({"serial": "S1", "gps_pos": {"lat": lat, "lon": 0.5, "alt_m": 100.0, "num_sv": num_sv}}),
            },
            raw: None,
            source: xng_types::Provenance {
                station: xng_types::StationIdentity::new("XX-TEST"),
                app: xng_types::AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        assert!(format_payload_summary(&msg(47.0, 8), "X").is_some());
        assert!(format_payload_summary(&msg(47.0, 0), "X").is_none());
        assert!(format_payload_summary(&msg(180.0, 8), "X").is_none());
    }

    #[test]
    fn gps_week_to_utc() {
        // GPS week 2394, TOW 0 = Sunday 2025-11-23 00:00:00 GPS = 23:59:42 UTC the day before.
        let t = gps_to_utc(2394, 0).unwrap();
        assert_eq!(t.to_rfc3339(), "2025-11-22T23:59:42+00:00");
    }
}
