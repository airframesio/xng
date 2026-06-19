//! acarsdec-compatible flat JSON output (the format the Airframes VHF ACARS
//! ingest accepts on feed.airframes.io:5550/UDP), sent as one datagram per
//! message.

use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

/// Render a normalized message in acarsdec's flat JSON shape. Returns `None`
/// for non-ACARS bodies. Uses the message's own (canonical) station ident.
pub fn format_acarsdec(msg: &Message) -> Option<serde_json::Value> {
    format_acarsdec_with_station(msg, None)
}

/// As [`format_acarsdec`], but stamp `station_id_override` as the `station_id`
/// field when given (the Airframes per-mode/per-session feed id), leaving the
/// message's own provenance ident untouched.
pub fn format_acarsdec_with_station(
    msg: &Message,
    station_id_override: Option<&str>,
) -> Option<serde_json::Value> {
    let MessageBody::Acars(a) = &msg.body else {
        return None;
    };
    // Feed only verified frames (acarsdec likewise emits only frames whose
    // CRC checks out, possibly after correction).
    if !msg.decode.crc_ok {
        return None;
    }
    let station_id = station_id_override.unwrap_or(msg.source.station.ident.as_str());
    let ts = msg.timestamp.timestamp() as f64 + msg.timestamp.timestamp_subsec_micros() as f64 / 1e6;
    let mut v = serde_json::json!({
        "app": { "name": "xng", "ver": env!("CARGO_PKG_VERSION") },
        "timestamp": ts,
        "station_id": station_id,
        "channel": msg.source.channel.map(|c| c.index).unwrap_or(0),
        "freq": (msg.frequency_hz as f64 / 1e6 * 1000.0).round() / 1000.0,
        "level": msg.signal.rssi_db.map(|l| (l as f64 * 10.0).round() / 10.0).unwrap_or(0.0),
        "error": msg.decode.fec_corrected.unwrap_or(0) + msg.decode.errors.unwrap_or(0),
        "mode": a.mode.to_string(),
        "label": a.label,
        // acarsdec emits `false` for NAK, the ack character otherwise.
        "ack": a.ack.map(|c| serde_json::json!(c.to_string())).unwrap_or(serde_json::json!(false)),
    });
    let obj = v.as_object_mut().unwrap();
    // acarsdec's `noise` floor (dBFS) — now that the MSK demod tracks it
    // (ACARS-4.1). Omitted when not measured.
    if let Some(n) = msg.signal.noise_db {
        obj.insert("noise".into(), ((n as f64 * 10.0).round() / 10.0).into());
    }
    if let Some(b) = a.block_id {
        obj.insert("block_id".into(), b.to_string().into());
    }
    if let Some(t) = &a.tail {
        obj.insert("tail".into(), t.clone().into());
    }
    if let Some(f) = &a.flight {
        obj.insert("flight".into(), f.clone().into());
    }
    if let Some(m) = &a.msg_num {
        obj.insert("msgno".into(), m.clone().into());
    }
    if !a.text.is_empty() {
        obj.insert("text".into(), a.text.clone().into());
    }
    if !a.more_to_come {
        obj.insert("end".into(), true.into());
    }
    // Reassembly status, as acarsdec emits it (omitted when the message never
    // passed the reassembler).
    if let Some(s) = &a.assstat {
        obj.insert("assstat".into(), s.clone().into());
    }
    // H1/H2 sublabel + MFI and the decoded application layer (ADS-C, CPDLC,
    // OOOI, position, met...), matching acarsdec's `sublabel`/`mfi` fields and
    // its nested libacars envelope so aggregators get the full decode.
    if let Some(sl) = &a.sublabel {
        obj.insert("sublabel".into(), sl.clone().into());
    }
    if let Some(m) = &a.mfi {
        obj.insert("mfi".into(), m.clone().into());
    }
    if let Some(app) = &a.app {
        obj.insert("libacars".into(), app.clone());
    }
    Some(v)
}

/// Consume the bus, sending each ACARS message as an acarsdec-JSON UDP
/// datagram to `target` (e.g. `feed.airframes.io:5550`).
pub async fn run(mut rx: broadcast::Receiver<Arc<Message>>, target: String) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let mut sent: u64 = 0;
    loop {
        match rx.recv().await {
            Ok(msg) => {
                if let Some(v) = format_acarsdec(&msg) {
                    match socket.send_to(v.to_string().as_bytes(), &target).await {
                        Ok(_) => sent += 1,
                        Err(e) => tracing::warn!("udp send to {target} failed: {e}"),
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("udp output lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    tracing::info!("udp output to {target}: {sent} messages sent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use xng_types::*;

    #[test]
    fn formats_flat_json() {
        let msg = Message {
            mode: Mode::AcarsPoa,
            timestamp: chrono::Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap(),
            frequency_hz: 131_550_000,
            signal: SignalQuality { rssi_db: Some(-18.42), noise_db: Some(-55.0), ..Default::default() },
            decode: DecodeQuality { crc_ok: true, errors: Some(0), ..Default::default() },
            body: MessageBody::Acars(AcarsCore {
                mode: '2',
                tail: Some("N471XG".into()),
                label: "H1".into(),
                block_id: Some('3'),
                ack: None,
                flight: Some("XG0042".into()),
                msg_num: Some("M42A".into()),
                text: "HELLO".into(),
                ..Default::default()
            }),
            raw: None,
            source: Provenance {
                station: StationIdentity::new("XX-TEST-ACARS"),
                app: AppInfo::xng(),
                sdr: None,
                channel: Some(ChannelInfo { index: 2, frequency_hz: 131_550_000, sample_rate: 24000.0 }),
            },
        };
        let v = format_acarsdec(&msg).unwrap();
        assert_eq!(v["freq"], 131.55);
        assert_eq!(v["channel"], 2);
        assert_eq!(v["mode"], "2");
        assert_eq!(v["ack"], false);
        assert_eq!(v["tail"], "N471XG");
        assert_eq!(v["flight"], "XG0042");
        assert_eq!(v["msgno"], "M42A");
        assert_eq!(v["text"], "HELLO");
        assert_eq!(v["end"], true);
        assert_eq!(v["level"], -18.4);
        assert_eq!(v["noise"], -55.0);
        // No reassembler verdict on this fixture → field omitted.
        assert!(v.get("assstat").is_none(), "{v}");
    }

    #[test]
    fn emits_assstat_when_present() {
        let msg = Message {
            mode: Mode::AcarsPoa,
            timestamp: chrono::Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap(),
            frequency_hz: 131_550_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body: MessageBody::Acars(AcarsCore {
                mode: '2',
                label: "H1".into(),
                text: "HELLO WORLD".into(),
                reassembled: true,
                assstat: Some("complete".into()),
                ..Default::default()
            }),
            raw: None,
            source: Provenance {
                station: StationIdentity::new("XX-TEST-ACARS"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let v = format_acarsdec(&msg).unwrap();
        assert_eq!(v["assstat"], "complete");
    }
}
