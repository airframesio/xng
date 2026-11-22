//! JAERO JDUMP output: Inmarsat Aero ACARS in the nested JSON JAERO's
//! JDUMP feeder format, which the Airframes JAERO ingests take over UDP
//! (`feed.airframes.io:5571` for L-band, `:5561` for C-band).
//!
//! The AES/GES ids and the channel come from the transient `core.link` field
//! stashed by `xng_mode_aero::to_message`. P-channel bursts are sent by the
//! ground station (uplink); R- and T-channel bursts by the aircraft
//! (downlink). The ingest credits the feed by `station`, reads the receiver
//! time from `t` (Unix seconds plus microseconds) and the direction from
//! `isu.src.type`.

use serde_json::{json, Value};
use xng_types::{Message, MessageBody, Mode};

const AES: &str = "Aircraft Earth Station";
const GES: &str = "Ground Earth Station";

/// Render an Aero ACARS message as one JDUMP JSON object. `None` for other
/// bodies, other modes, failed CRCs and messages without the AES id.
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
    let aes = json!({ "type": AES, "addr": link.get("aes_id")?.as_str()?.to_uppercase() });
    let ges = json!({ "type": GES, "addr": hex2("ges_id").unwrap_or_else(|| "00".into()) });
    let uplink = link.get("channel").and_then(Value::as_str) == Some("p-channel");
    let (src, dst) = if uplink { (ges, aes) } else { (aes, ges) };

    let mut acars = json!({
        "mode": core.mode.to_string(),
        "label": core.label,
        // JAERO prints the technical ack, or "!" for a NAK.
        "ack": core.ack.map(|c| c.to_string()).unwrap_or_else(|| "!".into()),
        "msg_text": core.text,
    });
    let a = acars.as_object_mut().unwrap();
    if let Some(t) = &core.tail {
        a.insert("reg".into(), json!(t));
    }
    if let Some(b) = core.block_id {
        a.insert("blk_id".into(), json!(b.to_string()));
    }
    if let Some(f) = &core.flight {
        a.insert("flight".into(), json!(f));
    }

    let mut isu = json!({ "src": src, "dst": dst, "acars": acars });
    for key in ["qno", "refno"] {
        if let Some(h) = hex2(key) {
            isu[key] = json!(h);
        }
    }
    let micros = msg.timestamp.timestamp_micros();
    let mut v = json!({
        "app": { "name": "xng", "ver": env!("CARGO_PKG_VERSION") },
        "t": { "sec": micros.div_euclid(1_000_000), "usec": micros.rem_euclid(1_000_000) },
        "isu": isu,
    });
    if let Some(st) = station {
        v["station"] = json!(st);
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
    fn renders_jdump_fields() {
        let link = json!({ "aes_id": "76cca9", "ges_id": 144, "qno": 3, "refno": 10,
                           "bit_rate": 1200, "channel": "r-channel" });
        let v = format_jaero(&msg(Mode::AeroL, Some(link)), Some("XX-TEST-IMSL")).unwrap();
        assert_eq!(v["station"], "XX-TEST-IMSL");
        assert_eq!(v["app"]["name"], "xng");
        assert_eq!(v["t"]["sec"], 1_795_348_800i64);
        assert_eq!(v["t"]["usec"], 0);
        let isu = &v["isu"];
        assert_eq!(isu["src"]["type"], "Aircraft Earth Station");
        assert_eq!(isu["src"]["addr"], "76CCA9");
        assert_eq!(isu["dst"]["type"], "Ground Earth Station");
        assert_eq!(isu["dst"]["addr"], "90");
        assert_eq!(isu["qno"], "03");
        assert_eq!(isu["refno"], "0A");
        let a = &isu["acars"];
        assert_eq!(a["reg"], "9V-SMF");
        assert_eq!(a["label"], "H1");
        assert_eq!(a["blk_id"], "3");
        assert_eq!(a["ack"], "A");
        assert_eq!(a["flight"], "SQ0321");
        assert_eq!(a["msg_text"], "/AKLCDYA.ADS");
    }

    #[test]
    fn p_channel_is_uplink() {
        let link = json!({ "aes_id": "76CCA9", "ges_id": 144, "channel": "p-channel" });
        let v = format_jaero(&msg(Mode::AeroL, Some(link)), None).unwrap();
        assert_eq!(v["isu"]["src"]["type"], "Ground Earth Station");
        assert_eq!(v["isu"]["dst"]["addr"], "76CCA9");
        assert!(v.get("station").is_none());
    }

    #[test]
    fn needs_aero_mode_and_link() {
        let link = json!({ "aes_id": "76CCA9" });
        assert!(format_jaero(&msg(Mode::AeroC, Some(link.clone())), None).is_some());
        assert!(format_jaero(&msg(Mode::Iridium, Some(link)), None).is_none());
        assert!(format_jaero(&msg(Mode::AeroL, None), None).is_none());
    }
}
