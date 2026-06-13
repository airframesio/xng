//! Embedded web dashboard: a live map of decoded aircraft (Mode S CPR)
//! and vessels (AIS) plus a message stream and per-mode counters —
//! the in-browser view single-mode decoders get from tar1090 or the
//! AIS-catcher viewer, here for every mode at once.
//!
//! One hand-rolled HTTP endpoint (same pattern as the Prometheus
//! exporter): `GET /` serves the embedded page, `GET /api/state`
//! serves the JSON snapshot the page polls.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

const PAGE: &str = include_str!("assets/dashboard.html");
const RECENT_CAP: usize = 200;
/// Drop map entities not heard from in this many seconds.
const EXPIRE_S: u64 = 300;

#[derive(Default)]
struct Dash {
    aircraft: HashMap<String, Value>,
    vessels: HashMap<u32, Value>,
    recent: VecDeque<Value>,
    totals: HashMap<String, u64>,
    /// Monotonic message id — lets the page keep expansion state
    /// across poll re-renders.
    next_id: u64,
    station: String,
    started: u64,
}

/// Append to the entity's position trail (decimated: only when moved
/// meaningfully; capped length).
fn push_trail(o: &mut serde_json::Map<String, Value>, lat: f64, lon: f64) {
    let trail = o.entry("trail").or_insert_with(|| json!([]));
    let arr = trail.as_array_mut().unwrap();
    if let Some(last) = arr.last().and_then(Value::as_array) {
        let (pl, po) = (last[0].as_f64().unwrap_or(0.0), last[1].as_f64().unwrap_or(0.0));
        if (pl - lat).abs() < 1e-4 && (po - lon).abs() < 1e-4 {
            return;
        }
    }
    arr.push(json!([lat, lon]));
    if arr.len() > 60 {
        arr.remove(0);
    }
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn update(d: &mut Dash, m: &Message) {
    let mode = m.mode.as_str().to_string();
    *d.totals.entry(mode.clone()).or_insert(0) += 1;

    match &m.body {
        MessageBody::ModeS {
            icao: Some(icao),
            callsign,
            altitude_ft,
            lat,
            lon,
            speed_kt,
            track_deg,
            squawk,
            ..
        } => {
            let e = d.aircraft.entry(icao.clone()).or_insert_with(|| json!({}));
            let o = e.as_object_mut().unwrap();
            o.insert("icao".into(), json!(icao));
            o.insert("seen".into(), json!(now_s()));
            if !o.contains_key("country") {
                if let Ok(hex) = u32::from_str_radix(icao, 16) {
                    if let Some(c) = crate::outputs::dbinfo::icao_country(hex) {
                        o.insert("country".into(), json!(c));
                    }
                    if let Some((reg, typ)) = crate::outputs::dbinfo::AircraftDb::lookup(hex) {
                        if !reg.is_empty() {
                            o.insert("reg".into(), json!(reg));
                        }
                        if !typ.is_empty() {
                            o.insert("actype".into(), json!(typ));
                        }
                    }
                }
            }
            let msgs = o.get("msgs").and_then(Value::as_u64).unwrap_or(0);
            o.insert("msgs".into(), json!(msgs + 1));
            if let (Some(la), Some(lo_)) = (lat, lon) {
                push_trail(o, *la, *lo_);
            }
            for (k, v) in [
                ("callsign", callsign.as_ref().map(|c| json!(c.trim()))),
                ("alt", altitude_ft.map(|v| json!(v))),
                ("lat", lat.map(|v| json!(v))),
                ("lon", lon.map(|v| json!(v))),
                ("spd", speed_kt.map(|v| json!(v.round()))),
                ("trk", track_deg.map(|v| json!(v.round()))),
                ("squawk", squawk.as_ref().map(|v| json!(v))),
            ] {
                if let Some(v) = v {
                    o.insert(k.into(), v);
                }
            }
        }
        MessageBody::Ais { mmsi: Some(mmsi), details: Some(det), msg_type, .. } => {
            let e = d.vessels.entry(*mmsi).or_insert_with(|| json!({}));
            let o = e.as_object_mut().unwrap();
            o.insert("mmsi".into(), json!(mmsi));
            o.insert("seen".into(), json!(now_s()));
            if !o.contains_key("country") {
                if let Some(c) = crate::outputs::dbinfo::mid_country(*mmsi) {
                    o.insert("country".into(), json!(c));
                }
            }
            if let (Some(la), Some(lo_)) = (
                det.get("lat").and_then(Value::as_f64),
                det.get("lon").and_then(Value::as_f64),
            ) {
                push_trail(o, la, lo_);
            }
            if let Some(t) = msg_type {
                o.insert("type".into(), json!(t));
            }
            // Ship type (ITU-R M.1371 code, types 5/19/21/24) — sticky:
            // it arrives on static reports, the map marker is keyed to
            // it, and position reports must not clear it.
            if let Some(st) = det.get("ship_type").and_then(Value::as_u64) {
                if st != 0 {
                    o.insert("shiptype".into(), json!(st));
                }
            }
            for (k, src) in
                [("lat", "lat"), ("lon", "lon"), ("sog", "sog_kt"), ("cog", "cog_deg"), ("name", "name")]
            {
                if let Some(v) = det.get(src) {
                    o.insert(k.into(), v.clone());
                }
            }
        }
        _ => {}
    }

    // Message stream entry: a one-line summary, plus the full decoded
    // message for the click-to-expand detail view.
    let line = crate::outputs::console::format_message(m, crate::outputs::console::ConsoleFormat::Pretty);
    d.next_id += 1;
    d.recent.push_back(json!({
        "id": d.next_id,
        "t": m.timestamp.to_rfc3339(),
        "mode": mode,
        "freq": m.frequency_hz,
        "text": line,
        "detail": serde_json::to_value(m).unwrap_or(Value::Null),
    }));
    while d.recent.len() > RECENT_CAP {
        d.recent.pop_front();
    }
}

fn snapshot(d: &mut Dash) -> String {
    let cutoff = now_s().saturating_sub(EXPIRE_S);
    d.aircraft.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    d.vessels.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    json!({
        "station": d.station,
        "started": d.started,
        "aircraft": d.aircraft.values().collect::<Vec<_>>(),
        "vessels": d.vessels.values().collect::<Vec<_>>(),
        "messages": d.recent.iter().rev().take(100).collect::<Vec<_>>(),
        "totals": d.totals,
        "now": now_s(),
    })
    .to_string()
}

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    addr: String,
    station: String,
) -> std::io::Result<()> {
    let state = Arc::new(Mutex::new(Dash {
        station,
        started: now_s(),
        ..Dash::default()
    }));

    // HTTP listener.
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("dashboard on http://{addr}/");
    let http_state = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let state = http_state.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (ctype, body) = if path.starts_with("/api/state") {
                    ("application/json", snapshot(&mut state.lock().unwrap()))
                } else {
                    ("text/html; charset=utf-8", PAGE.to_string())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    // Bus consumer.
    loop {
        match rx.recv().await {
            Ok(msg) => update(&mut state.lock().unwrap(), &msg),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("dashboard lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}
