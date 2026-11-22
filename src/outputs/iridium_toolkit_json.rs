//! iridium-toolkit ACARS JSON output: Iridium SBD-carried ACARS in the shape
//! iridium-toolkit's ACARS reassembler emits, which the Airframes Iridium
//! ingest takes as newline-delimited JSON over TCP (`feed.airframes.io:5590`).
//!
//! The ingest credits the feed by `source.station_id`. The link direction
//! comes from the SBD header on the transient `core.link` field (stashed by
//! `xng_mode_iridium::to_message`): a mobile-originated sequence number means
//! the aircraft sent it (downlink), a mobile-terminated one an uplink.

use serde_json::{json, Value};
use xng_types::{Message, MessageBody, Mode};

/// Render an Iridium ACARS message as one iridium-toolkit JSON object.
/// `None` for other bodies or modes and for failed CRCs.
pub fn format_iridium_toolkit(msg: &Message, station: &str) -> Option<Value> {
    if msg.mode != Mode::Iridium || !msg.decode.crc_ok {
        return None;
    }
    let MessageBody::Acars(core) = &msg.body else {
        return None;
    };
    let mut acars = json!({
        "timestamp": msg.timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "errors": msg.decode.errors.unwrap_or(0),
        "mode": core.mode.to_string(),
        "label": core.label,
        // iridium-toolkit prints the technical ack, or "!" for a NAK.
        "ack": core.ack.map(|c| c.to_string()).unwrap_or_else(|| "!".into()),
        "text": core.text,
        "continues": core.more_to_come,
    });
    let a = acars.as_object_mut().unwrap();
    let header = core.link.as_ref();
    let direction = if header.and_then(|h| h.get("momsn")).is_some() {
        Some("downlink")
    } else if header.and_then(|h| h.get("mtmsn")).is_some() {
        Some("uplink")
    } else {
        None
    };
    if let Some(d) = direction {
        a.insert("link_direction".into(), json!(d));
    }
    if let Some(t) = &core.tail {
        a.insert("tail".into(), json!(t));
    }
    if let Some(f) = &core.flight {
        a.insert("flight_no".into(), json!(f));
    }
    if let Some(b) = core.block_id {
        a.insert("block_id".into(), json!(b.to_string()));
    }
    if let Some(n) = &core.msg_num {
        a.insert("sequence_no".into(), json!(n));
    }
    let mut v = json!({
        "app": { "name": "xng", "ver": env!("CARGO_PKG_VERSION") },
        "source": { "transport": "iridium", "protocol": "acars", "station_id": station },
        "acars": acars,
        "freq": msg.frequency_hz,
    });
    if let Some(l) = msg.signal.rssi_db {
        v["level"] = json!((l as f64 * 10.0).round() / 10.0);
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use xng_types::*;

    fn msg(link: Option<Value>) -> Message {
        Message {
            mode: Mode::Iridium,
            timestamp: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
            frequency_hz: 1_626_270_000,
            signal: SignalQuality {
                rssi_db: Some(-41.26),
                ..Default::default()
            },
            decode: DecodeQuality {
                crc_ok: true,
                errors: Some(0),
                ..Default::default()
            },
            body: MessageBody::Acars(AcarsCore {
                mode: '2',
                tail: Some("N767AX".into()),
                label: "H1".into(),
                block_id: Some('7'),
                ack: None,
                flight: Some("AA0100".into()),
                msg_num: Some("D23A".into()),
                text: "POSN12345".into(),
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
    fn renders_toolkit_fields() {
        let v = format_iridium_toolkit(
            &msg(Some(json!({ "momsn": 12, "imei": "300234010000000" }))),
            "XX-TEST-IRDM",
        )
        .unwrap();
        assert_eq!(v["source"]["station_id"], "XX-TEST-IRDM");
        assert_eq!(v["source"]["protocol"], "acars");
        let a = &v["acars"];
        assert_eq!(a["link_direction"], "downlink");
        assert_eq!(a["tail"], "N767AX");
        assert_eq!(a["flight_no"], "AA0100");
        assert_eq!(a["label"], "H1");
        assert_eq!(a["block_id"], "7");
        assert_eq!(a["sequence_no"], "D23A");
        assert_eq!(a["ack"], "!");
        assert_eq!(a["text"], "POSN12345");
        assert_eq!(a["continues"], false);
        assert_eq!(v["level"], -41.3);
    }

    #[test]
    fn direction_from_sbd_header() {
        let up = format_iridium_toolkit(&msg(Some(json!({ "mtmsn": 4 }))), "S").unwrap();
        assert_eq!(up["acars"]["link_direction"], "uplink");
        let none = format_iridium_toolkit(&msg(None), "S").unwrap();
        assert!(none["acars"].get("link_direction").is_none());
    }
}
