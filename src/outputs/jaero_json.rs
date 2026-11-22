//! JAERO JSON output: Inmarsat Aero ACARS in the flat upper-case JSON JAERO
//! sends to feeders, which the Airframes JAERO ingests take over UDP
//! (`feed.airframes.io:5571` for L-band, `:5561` for C-band).
//!
//! The AES/GES ids come from the transient `core.link` field stashed by
//! `xng_mode_aero::to_message`. `STATION_ID` is extra to JAERO's own output:
//! the ingest credits JAERO feeds by source address today, and the field lets
//! it use the feeder's ident once it reads it.

use serde_json::{json, Value};
use xng_types::{Message, MessageBody, Mode};

/// Render an Aero ACARS message as one JAERO JSON object. `None` for other
/// bodies, other modes, failed CRCs and messages without the AES ids.
pub fn format_jaero(msg: &Message, station: Option<&str>) -> Option<Value> {
    if !matches!(msg.mode, Mode::AeroL | Mode::AeroC) || !msg.decode.crc_ok {
        return None;
    }
    let MessageBody::Acars(core) = &msg.body else {
        return None;
    };
    let link = core.link.as_ref()?;
    let hex2 = |k: &str| {
        link.get(k)
            .and_then(Value::as_u64)
            .map(|v| format!("{v:02X}"))
    };
    let time = msg
        .timestamp
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut v = json!({
        "NAME": "xng",
        "VERSION": env!("CARGO_PKG_VERSION"),
        "TIME": time,
        "TIME_UTC": msg.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
        "NONACARS": false,
        "AESID": link.get("aes_id")?.as_str()?,
        "MODE": core.mode.to_string(),
        "LABEL": core.label,
        // JAERO prints the technical ack, or "!" for a NAK.
        "TAK": core.ack.map(|c| c.to_string()).unwrap_or_else(|| "!".into()),
        "MESSAGE": core.text,
    });
    let o = v.as_object_mut().unwrap();
    for (key, field) in [("GESID", "ges_id"), ("QNO", "qno"), ("REFNO", "refno")] {
        if let Some(h) = hex2(field) {
            o.insert(key.into(), json!(h));
        }
    }
    if let Some(t) = &core.tail {
        o.insert("REG".into(), json!(t));
    }
    if let Some(b) = core.block_id {
        o.insert("BI".into(), json!(b.to_string()));
    }
    if let Some(f) = &core.flight {
        o.insert("FLIGHT".into(), json!(f));
    }
    if let Some(st) = station {
        o.insert("STATION_ID".into(), json!(st));
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use xng_types::*;

    fn msg(mode: Mode, link: Option<Value>) -> Message {
        Message {
            mode,
            timestamp: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
            frequency_hz: 1_545_000_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality {
                crc_ok: true,
                ..Default::default()
            },
            body: MessageBody::Acars(AcarsCore {
                mode: '2',
                tail: Some("9V-SMF".into()),
                label: "H1".into(),
                block_id: Some('3'),
                ack: Some('A'),
                flight: Some("SQ0321".into()),
                text: "/AKLCDYA.ADS".into(),
                link,
                ..Default::default()
            }),
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
    fn renders_jaero_fields() {
        let link =
            json!({ "aes_id": "76CCA9", "ges_id": 144, "qno": 3, "refno": 10, "bit_rate": 1200 });
        let v = format_jaero(&msg(Mode::AeroL, Some(link)), Some("XX-TEST-IMSL")).unwrap();
        assert_eq!(v["TIME"], "2026-11-22T12:00:00.000Z");
        assert_eq!(v["AESID"], "76CCA9");
        assert_eq!(v["GESID"], "90");
        assert_eq!(v["QNO"], "03");
        assert_eq!(v["REFNO"], "0A");
        assert_eq!(v["REG"], "9V-SMF");
        assert_eq!(v["LABEL"], "H1");
        assert_eq!(v["BI"], "3");
        assert_eq!(v["TAK"], "A");
        assert_eq!(v["FLIGHT"], "SQ0321");
        assert_eq!(v["MESSAGE"], "/AKLCDYA.ADS");
        assert_eq!(v["NONACARS"], false);
        assert_eq!(v["STATION_ID"], "XX-TEST-IMSL");
    }

    #[test]
    fn needs_aero_mode_and_link() {
        let link = json!({ "aes_id": "76CCA9" });
        assert!(format_jaero(&msg(Mode::AeroC, Some(link.clone())), None).is_some());
        assert!(format_jaero(&msg(Mode::Iridium, Some(link)), None).is_none());
        assert!(format_jaero(&msg(Mode::AeroL, None), None).is_none());
    }
}
