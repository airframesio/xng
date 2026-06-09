//! Console output: human-readable one-liners (or raw JSON) per message.

use std::sync::Arc;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConsoleFormat {
    Pretty,
    Json,
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
                    format!("ACARS {} {} lbl={} {}{}", tail, flight, a.label, quality, text)
                }
                MessageBody::Ais { nmea } => format!("AIS {} sentence(s)", nmea.len()),
                MessageBody::ModeS { df, icao } => {
                    format!("MODE-S df={} icao={}", df, icao.as_deref().unwrap_or("-"))
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
