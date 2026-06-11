//! `xng extern` — second-class wrapped external decoders: spawn a child
//! decoder (or read stdin) and normalize its JSON output onto the bus,
//! so wrapped decoders get every xng output (asf-2.0, feeds, JSONL,
//! console) and the xng-acars application layer.

use crate::bus::MessageBus;
use crate::outputs::console;
use crate::outputs::{acarsdec_json, jsonl};
use crate::runtime::OutputConfig;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use xng_types::{
    AcarsCore, AppInfo, DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality,
    StationIdentity,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExternFormat {
    Dumphfdl,
    Dumpvdl2,
    Acarsdec,
}

impl std::str::FromStr for ExternFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "dumphfdl" | "hfdl" => Ok(Self::Dumphfdl),
            "dumpvdl2" | "vdl2" => Ok(Self::Dumpvdl2),
            "acarsdec" | "acars" => Ok(Self::Acarsdec),
            other => Err(format!("unknown extern format: {other}")),
        }
    }
}

/// Find the first object under `key` anywhere in the JSON tree.
fn find_object<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(found);
            }
            map.values().find_map(|child| find_object(child, key))
        }
        Value::Array(items) => items.iter().find_map(|child| find_object(child, key)),
        _ => None,
    }
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

fn char_of(v: &Value, keys: &[&str]) -> Option<char> {
    str_of(v, keys).and_then(|s| s.chars().next())
}

/// Parse one JSON line from an external decoder into a normalized
/// message. Tolerant: unknown layouts still produce a raw event.
pub fn parse_line(line: &str, format: ExternFormat, station: &StationIdentity) -> Option<Message> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let (mode, envelope) = match format {
        ExternFormat::Dumphfdl => (Mode::Hfdl, v.get("hfdl").unwrap_or(&v)),
        ExternFormat::Dumpvdl2 => (Mode::Vdl2, v.get("vdl2").unwrap_or(&v)),
        ExternFormat::Acarsdec => (Mode::AcarsPoa, &v),
    };

    // Timestamp: {"t":{"sec","usec"}} or float "timestamp".
    let timestamp: DateTime<Utc> = envelope
        .get("t")
        .and_then(|t| {
            let sec = t.get("sec")?.as_i64()?;
            let usec = t.get("usec").and_then(Value::as_i64).unwrap_or(0);
            Utc.timestamp_opt(sec, (usec * 1000) as u32).single()
        })
        .or_else(|| {
            let ts = envelope.get("timestamp")?.as_f64()?;
            Utc.timestamp_opt(ts as i64, ((ts.fract()) * 1e9) as u32).single()
        })
        .unwrap_or_else(Utc::now);

    // Frequency: Hz int (dumphfdl/dumpvdl2) or MHz float (acarsdec).
    let frequency_hz = envelope
        .get("freq")
        .and_then(|f| {
            if let Some(hz) = f.as_u64() {
                Some(if hz < 1_000_000 { (f.as_f64()? * 1e6) as u64 } else { hz })
            } else {
                f.as_f64().map(|mhz| (mhz * 1e6) as u64)
            }
        })
        .unwrap_or(0);

    let signal = SignalQuality {
        rssi_db: envelope
            .get("sig_level")
            .or_else(|| envelope.get("level"))
            .and_then(Value::as_f64)
            .map(|x| x as f32),
        noise_db: envelope.get("noise_level").and_then(Value::as_f64).map(|x| x as f32),
        freq_skew_hz: envelope.get("freq_skew").and_then(Value::as_f64).map(|x| x as f32),
        ..Default::default()
    };

    // ACARS content anywhere in the tree.
    let acars = if format == ExternFormat::Acarsdec {
        Some(envelope)
    } else {
        find_object(envelope, "acars")
    };
    let body = match acars {
        Some(a) if a.get("label").is_some() => {
            let label = str_of(a, &["label"]).unwrap_or("").to_string();
            let text = str_of(a, &["text", "msg_text"]).unwrap_or("").to_string();
            let block_id = char_of(a, &["blk_id", "block_id"]);
            let downlink = block_id.map(|c| c.is_ascii_digit()).unwrap_or(false);
            let appdec = xng_acars::decode(&label, &text, downlink);
            MessageBody::Acars(AcarsCore {
                mode: char_of(a, &["mode"]).unwrap_or('2'),
                tail: str_of(a, &["reg", "tail"]).map(|s| s.trim_start_matches('.').to_string()),
                label,
                sublabel: appdec.sublabel.or(str_of(a, &["sublabel"]).map(String::from)),
                mfi: appdec.mfi.or(str_of(a, &["mfi"]).map(String::from)),
                block_id,
                ack: match a.get("ack") {
                    Some(Value::Bool(false)) => None,
                    Some(Value::String(s)) => s.chars().next(),
                    _ => None,
                },
                flight: str_of(a, &["flight"]).map(String::from),
                msg_num: str_of(a, &["msg_num", "msgno"]).map(String::from),
                text,
                more_to_come: false,
                reassembled: false,
                app: appdec.app.map(|x| serde_json::to_value(&x).unwrap_or_default()),
            })
        }
        _ => MessageBody::Hfdl {
            kind: "extern".into(),
            details: envelope.clone(),
        },
    };
    let crc_ok = acars
        .and_then(|a| a.get("err").map(|e| e != &Value::Bool(true)))
        .unwrap_or(true);

    Some(Message {
        mode,
        timestamp,
        frequency_hz,
        signal,
        decode: DecodeQuality { crc_ok, fec_corrected: None, errors: None },
        body,
        raw: None,
        source: Provenance {
            station: station.clone(),
            app: AppInfo::xng(),
            sdr: None,
            channel: None,
        },
    })
}

pub fn run(
    format: ExternFormat,
    command: &[String],
    station_ident: String,
    outputs: OutputConfig,
) -> anyhow::Result<()> {
    let station = StationIdentity::new(station_ident);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn({
            let rx = bus.subscribe();
            let fmt = outputs.console;
            async move {
                console::run(rx, fmt).await;
                Ok::<(), std::io::Error>(())
            }
        }));
        if let Some(path) = outputs.jsonl.clone() {
            let rx = bus.subscribe();
            tasks.push(tokio::spawn(async move { jsonl::run(rx, &path).await }));
        }
        for target in outputs.udp.clone() {
            tasks.push(tokio::spawn(acarsdec_json::run(bus.subscribe(), target)));
        }
        if let Some(url) = outputs.asf2_grpc.clone() {
            let (id, ident) = (station.id.to_string(), station.ident.clone());
            tasks.push(tokio::spawn(crate::outputs::asf2_grpc::run(
                bus.subscribe(),
                url,
                id,
                ident,
            )));
        }
        if let Some(target) = outputs.asf2_quic.clone() {
            let (id, ident) = (station.id.to_string(), station.ident.clone());
            tasks.push(tokio::spawn(crate::outputs::asf2_quic::run(
                bus.subscribe(),
                target,
                outputs.asf2_quic_trust.clone(),
                id,
                ident,
            )));
        }

        let mut count: u64 = 0;
        if command.is_empty() {
            tracing::info!("reading external decoder JSON from stdin");
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Some(line) = lines.next_line().await? {
                if let Some(msg) = parse_line(&line, format, &station) {
                    bus.publish(msg);
                    count += 1;
                }
            }
        } else {
            tracing::info!("spawning: {}", command.join(" "));
            let mut child = tokio::process::Command::new(&command[0])
                .args(&command[1..])
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let stdout = child.stdout.take().expect("piped stdout");
            let mut lines = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    line = lines.next_line() => match line? {
                        Some(line) => {
                            if let Some(msg) = parse_line(&line, format, &station) {
                                bus.publish(msg);
                                count += 1;
                            }
                        }
                        None => break,
                    },
                    _ = tokio::signal::ctrl_c() => {
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            let _ = child.wait().await;
        }
        drop(bus);
        for t in tasks {
            if let Err(e) = t.await? {
                tracing::warn!("output error: {e}");
            }
        }
        tracing::info!("extern session: {count} message(s) normalized");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station() -> StationIdentity {
        StationIdentity::new("XX-TEST")
    }

    #[test]
    fn parses_dumphfdl_acars() {
        let line = r#"{"hfdl":{"app":{"name":"dumphfdl","ver":"1.6.0"},"station":"XX","t":{"sec":1717000000,"usec":250000},"freq":10063000,"bit_rate":1800,"sig_level":-18.5,"noise_level":-39.1,"lpdu":{"src":{"type":"Aircraft"},"hfnpdu":{"type":"ACARS","acars":{"err":false,"mode":"2","reg":".N471XG","ack":"!","label":"H1","blk_id":"3","msg_num":"M42A","flight":"UA0042","text":"POSN 4737.2N"}}}}}"#;
        let m = parse_line(line, ExternFormat::Dumphfdl, &station()).unwrap();
        assert_eq!(m.mode, Mode::Hfdl);
        assert_eq!(m.frequency_hz, 10_063_000);
        assert_eq!(m.signal.rssi_db, Some(-18.5));
        let MessageBody::Acars(a) = &m.body else { panic!("expected acars") };
        assert_eq!(a.tail.as_deref(), Some("N471XG"));
        assert_eq!(a.label, "H1");
        assert_eq!(a.flight.as_deref(), Some("UA0042"));
    }

    #[test]
    fn parses_acarsdec_flat() {
        let line = r#"{"timestamp":1717000123.456,"station_id":"XX","channel":2,"freq":131.550,"level":-21.0,"error":0,"mode":"2","label":"B6","block_id":"4","ack":false,"tail":"VT-ANB","flight":"AI0142","msgno":"M11A","text":"/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5","end":true}"#;
        let m = parse_line(line, ExternFormat::Acarsdec, &station()).unwrap();
        assert_eq!(m.mode, Mode::AcarsPoa);
        assert_eq!(m.frequency_hz, 131_550_000);
        let MessageBody::Acars(a) = &m.body else { panic!() };
        assert_eq!(a.label, "B6");
        // The xng application layer runs on wrapped output too.
        assert_eq!(a.app.as_ref().unwrap()["app"], "adsc");
    }

    #[test]
    fn parses_dumpvdl2_avlc_acars() {
        let line = r#"{"vdl2":{"app":{"name":"dumpvdl2"},"t":{"sec":1717000200,"usec":1000},"freq":136975000,"sig_level":-25.2,"freq_skew":1.2,"avlc":{"src":{"addr":"A1B2C3"},"dst":{"addr":"10A234"},"acars":{"err":false,"mode":"2","reg":".N818WX","label":"Q0","blk_id":"A","text":""}}}}"#;
        let m = parse_line(line, ExternFormat::Dumpvdl2, &station()).unwrap();
        assert_eq!(m.mode, Mode::Vdl2);
        assert_eq!(m.frequency_hz, 136_975_000);
        let MessageBody::Acars(a) = &m.body else { panic!() };
        assert_eq!(a.tail.as_deref(), Some("N818WX"));
    }

    #[test]
    fn non_acars_becomes_raw_event() {
        let line = r#"{"hfdl":{"t":{"sec":1717000300},"freq":10063000,"spdu":{"gs":{"id":7}}}}"#;
        let m = parse_line(line, ExternFormat::Dumphfdl, &station()).unwrap();
        assert!(matches!(m.body, MessageBody::Hfdl { .. }));
    }
}
