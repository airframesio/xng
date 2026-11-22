//! The dashboard's airband pages: `/airband` (live channels, scanner,
//! history, listening in the browser), `/api/airband` (state as JSON),
//! `POST /api/airband/scan` (pin / lock out), and `/airband/stream?s=` (live
//! 16 kHz s16le PCM of one stream, played by the page with Web Audio).

use crate::airband::config::parse_stream;
use crate::airband::{hub, AudioEvent};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;

pub const PAGE: &str = include_str!("assets/airband.html");

fn query<'a>(path: &'a str, key: &str) -> Option<&'a str> {
    path.split_once('?')?.1.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then_some(v)
    })
}

/// Everything the page draws.
pub fn state_json() -> String {
    let h = hub();
    let status = h.status();
    let (scan, locked, selected, pinned) = match h.scan.lock().unwrap().as_ref() {
        Some(s) => (true, s.locked_out.clone(), s.selected, s.ctl.pinned()),
        None => (false, Vec::new(), None, None),
    };
    let channels: Vec<Value> = h
        .channels()
        .iter()
        .map(|c| {
            let st = status.iter().find(|s| s.freq_hz == c.freq_hz);
            json!({
                "freq_hz": c.freq_hz,
                "label": c.label,
                "key": c.key,
                "atc_frequency": c.atc_frequency,
                "modulation": c.modulation.to_string(),
                "bandwidth_hz": c.bandwidth_hz,
                "sdr": c.sdr,
                "status": st,
                "locked_out": locked.contains(&c.freq_hz),
            })
        })
        .collect();
    let mut recent = h.recent();
    recent.reverse();
    recent.truncate(100);
    json!({
        "channels": channels,
        "mix": h.has_mix(),
        "scan": scan.then(|| json!({"selected": selected, "pinned": pinned, "locked_out": locked})),
        "recent": recent,
    })
    .to_string()
}

/// `POST /api/airband/scan?pin=<freq|off>` or `?lockout=<freq>&on=<0|1>`.
pub fn scan_control(path: &str) -> (u16, String) {
    let parse = |v: &str| crate::freq::parse_hz(v).map_err(|e| e.to_string());
    let r: Result<bool, String> = if let Some(v) = query(path, "pin") {
        if v == "off" || v.is_empty() {
            Ok(crate::airband::scan_stream::pin(None))
        } else {
            parse(v).map(|f| crate::airband::scan_stream::pin(Some(f)))
        }
    } else if let Some(v) = query(path, "lockout") {
        let on = query(path, "on") != Some("0");
        parse(v).map(|f| crate::airband::scan_stream::lockout(f, on))
    } else {
        Err("use pin=<freq|off> or lockout=<freq>&on=<0|1>".into())
    };
    match r {
        Ok(true) => (200, json!({"ok": true}).to_string()),
        Ok(false) => (409, json!({"ok": false, "error": "the scanner is not enabled (--scan)"}).to_string()),
        Err(e) => (400, json!({"ok": false, "error": e}).to_string()),
    }
}

/// Serve `/airband/stream?s=<freq|mix|scan>` on `sock` until the client
/// goes away. Audio is sent only while the stream carries a transmission.
pub async fn stream(mut sock: tokio::net::TcpStream, path: &str) {
    let stream = match query(path, "s").map(parse_stream) {
        Some(Ok(s)) => s,
        _ => {
            let body = "stream needs ?s=<freq|mix|scan>";
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            return;
        }
    };
    let mut rx = hub().subscribe();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: audio/L16;rate={};channels=1\r\nX-Sample-Format: s16le\r\n\
         Cache-Control: no-cache, no-store\r\nConnection: close\r\n\r\n",
        crate::airband::hub::AUDIO_RATE
    );
    if sock.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let _ = sock.set_nodelay(true);
    tracing::debug!("dashboard listener on airband stream {stream}");
    let (mut rd, mut wr) = sock.into_split();
    let mut scratch = [0u8; 256];
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(AudioEvent::Audio(f)) if f.stream == stream => {
                    let pcm = xng_mode_airband::codec::i16_le_bytes(&xng_mode_airband::codec::to_i16(&f.samples));
                    if wr.write_all(&pcm).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            // The listener sends nothing after its request, so a read
            // returning means it hung up; this frees the slot even while the
            // channel is quiet and nothing is being written.
            r = rd.read(&mut scratch) => if matches!(r, Ok(0) | Err(_)) { break },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values() {
        assert_eq!(query("/airband/stream?s=118.3", "s"), Some("118.3"));
        assert_eq!(query("/api/airband/scan?lockout=121.5&on=0", "on"), Some("0"));
        assert_eq!(query("/airband", "s"), None);
        assert!(serde_json::from_str::<Value>(&state_json()).is_ok());
        assert_eq!(scan_control("/api/airband/scan").0, 400);
    }
}
