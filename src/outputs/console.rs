//! Console output: human-readable one-liners (or raw JSON) per message.

use std::sync::Arc;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConsoleFormat {
    Pretty,
    Json,
}

/// Best-effort one-liner from the decoded application layer JSON.
fn app_summary(app: &serde_json::Value) -> Option<String> {
    match app.get("app")?.as_str()? {
        "adsc" => {
            let tags = app.get("tags")?.as_array()?;
            for t in tags {
                if t.get("tag")?.as_str()? == "report" {
                    return Some(format!(
                        "ADS-C {:.4} {:.4} {} ft",
                        t.get("lat")?.as_f64()?,
                        t.get("lon")?.as_f64()?,
                        t.get("alt_ft")?.as_i64()?
                    ));
                }
            }
            Some(format!("ADS-C {}", tags.first()?.get("tag")?.as_str()?))
        }
        "cpdlc" => Some(format!("CPDLC {}", app.get("imi")?.as_str()?)),
        "media_advisory" => Some("MEDIA-ADV".to_owned()),
        _ => None,
    }
}

pub fn format_message(msg: &Message, fmt: ConsoleFormat) -> String {
    match fmt {
        ConsoleFormat::Json => serde_json::to_string(msg).unwrap_or_else(|e| format!("<serialize error: {e}>")),
        ConsoleFormat::Pretty => {
            let freq_mhz = msg.frequency_hz as f64 / 1e6;
            let quality = if msg.decode.crc_ok { "ok" } else { "BAD" };
            let body = match &msg.body {
                MessageBody::Acars(a) => {
                    let tail = a.tail.as_deref().unwrap_or("-");
                    let flight = a.flight.as_deref().unwrap_or("-");
                    let text = if a.text.is_empty() { String::new() } else { format!(" | {}", a.text.replace('\n', "·")) };
                    let app = a.app.as_ref().and_then(app_summary).map(|s| format!(" [{s}]")).unwrap_or_default();
                    format!("ACARS {} {} lbl={} {}{}{}", tail, flight, a.label, quality, text, app)
                }
                MessageBody::Ais { nmea, msg_type, mmsi } => format!(
                    "AIS type={} mmsi={} {}",
                    msg_type.map_or("?".into(), |t| t.to_string()),
                    mmsi.map_or("?".into(), |m| m.to_string()),
                    nmea.first().map(String::as_str).unwrap_or("")
                ),
                MessageBody::ModeS { df, icao, callsign, altitude_ft } => {
                    let mut s = format!("MODE-S df={} icao={}", df, icao.as_deref().unwrap_or("-"));
                    if let Some(c) = callsign {
                        s.push_str(&format!(" ident={c}"));
                    }
                    if let Some(a) = altitude_ft {
                        s.push_str(&format!(" alt={a}ft"));
                    }
                    s
                }
                MessageBody::Undecoded => format!("FRAME ({} raw bytes)", msg.raw.as_ref().map_or(0, |r| r.len())),
            };
            format!(
                "{} [{}] {:.3} MHz {}",
                msg.timestamp.format("%H:%M:%S%.3f"),
                msg.mode,
                freq_mhz,
                body
            )
        }
    }
}

/// Consume the bus until it closes, printing each message to stdout.
pub async fn run(mut rx: broadcast::Receiver<Arc<Message>>, fmt: ConsoleFormat) {
    loop {
        match rx.recv().await {
            Ok(msg) => println!("{}", format_message(&msg, fmt)),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("console output lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
