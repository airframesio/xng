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

/// Iridium ring-alert positions are split by altitude (cf. iridium-toolkit
/// live-map) via `crate::beam::classify_altitude`: a frame's geocentric
/// position is either the broadcasting satellite (~780 km) or a ground beam
/// footprint (~0 km).
/// Satellites move continuously (keep longer); ground footprints are
/// transient.
const SAT_EXPIRE_S: u64 = 600;
const RING_EXPIRE_S: u64 = 300;

#[derive(Default)]
struct Dash {
    aircraft: HashMap<String, Value>,
    vessels: HashMap<u32, Value>,
    /// Iridium satellite positions, keyed by satellite id.
    iridium_sats: HashMap<u64, Value>,
    /// Iridium ring/beam ground footprints, keyed by "sat-beam".
    iridium_rings: HashMap<String, Value>,
    /// Iridium mobile-terminal positions (vessels/aircraft/handhelds that
    /// report their own ECEF), keyed by quantized lat/lon so a stationary
    /// terminal coalesces and a moving one leaves recent fixes.
    iridium_devices: HashMap<String, Value>,
    /// Reconstructed 48-beam pattern, projected under tracked satellites.
    beams: crate::beam::BeamReconstructor,
    /// Last unix-secs the beam pattern was persisted.
    beams_saved: u64,
    recent: VecDeque<Value>,
    totals: HashMap<String, u64>,
    /// Last time (unix secs) a message of each mode was seen — drives
    /// the `xng status` per-session liveness column.
    last_seen: HashMap<String, u64>,
    /// Monotonic message id — lets the page keep expansion state
    /// across poll re-renders.
    next_id: u64,
    station: String,
    started: u64,
    /// Static per-session descriptors (SDR, mode, tuning) for status.
    sessions: Vec<Value>,
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
    d.last_seen.insert(mode.clone(), now_s());

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
        // Iridium ring alerts carry the broadcasting satellite's geocentric
        // position; split into satellite positions (high altitude) and
        // targeted ground beam footprints (low altitude) for the map.
        MessageBody::Iridium { kind, details } if kind == "ring-alert" => {
            let (lat, lon, alt) = (
                details.get("lat").and_then(Value::as_f64),
                details.get("lon").and_then(Value::as_f64),
                details.get("alt_km").and_then(Value::as_f64),
            );
            let sat = details.get("sat").and_then(Value::as_u64);
            if let (Some(lat), Some(lon), Some(alt), Some(sat)) = (lat, lon, alt, sat) {
                let beam = details.get("beam").and_then(Value::as_u64).unwrap_or(0);
                // Feed the 48-beam reconstructor with the raw ECEF position
                // (details carry x/y/z in units of 4 km).
                if let (Some(x), Some(y), Some(z)) = (
                    details.get("x").and_then(Value::as_i64),
                    details.get("y").and_then(Value::as_i64),
                    details.get("z").and_then(Value::as_i64),
                ) {
                    let ecef = [x as f64 * 4.0, y as f64 * 4.0, z as f64 * 4.0];
                    d.beams.observe(sat, alt, ecef, beam as u8, m.timestamp.timestamp() as f64);
                }
                // Same altitude classifier the reconstructor uses, so a
                // garbage decode (implausible altitude) never plants a
                // phantom satellite marker or ground footprint.
                match crate::beam::classify_altitude(alt) {
                    crate::beam::AltClass::Satellite => {
                        let e = d.iridium_sats.entry(sat).or_insert_with(|| json!({}));
                        let o = e.as_object_mut().unwrap();
                        o.insert("sat".into(), json!(sat));
                        o.insert("beam".into(), json!(beam));
                        o.insert("lat".into(), json!(lat));
                        o.insert("lon".into(), json!(lon));
                        o.insert("alt".into(), json!(alt.round()));
                        o.insert("seen".into(), json!(now_s()));
                        if let Some(name) = details.get("satellite") {
                            o.insert("name".into(), name.clone());
                        }
                        push_trail(o, lat, lon); // satellite ground track
                    }
                    crate::beam::AltClass::Footprint => {
                        let e =
                            d.iridium_rings.entry(format!("{sat}-{beam}")).or_insert_with(|| json!({}));
                        let o = e.as_object_mut().unwrap();
                        o.insert("sat".into(), json!(sat));
                        o.insert("beam".into(), json!(beam));
                        o.insert("lat".into(), json!(lat));
                        o.insert("lon".into(), json!(lon));
                        o.insert("seen".into(), json!(now_s()));
                    }
                    crate::beam::AltClass::Implausible => {}
                }
            }
        }
        // Mobile-terminal self-reported positions: the actual Iridium
        // customer terminals (vessels/aircraft/handhelds), distinct from the
        // ring-alert beam footprints above.
        MessageBody::Iridium { kind, details } if kind == "mt-position" => {
            if let (Some(lat), Some(lon)) = (
                details.get("lat").and_then(Value::as_f64),
                details.get("lon").and_then(Value::as_f64),
            ) {
                let key = format!("{lat:.2},{lon:.2}");
                let e = d.iridium_devices.entry(key).or_insert_with(|| json!({}));
                let o = e.as_object_mut().unwrap();
                o.insert("lat".into(), json!(lat));
                o.insert("lon".into(), json!(lon));
                o.insert("alt_km".into(), json!(details.get("alt_km").and_then(Value::as_i64).unwrap_or(0)));
                if let Some(mt) = details.get("msg_type") {
                    o.insert("msg_type".into(), mt.clone());
                }
                o.insert("seen".into(), json!(now_s()));
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
    let sat_cut = now_s().saturating_sub(SAT_EXPIRE_S);
    let ring_cut = now_s().saturating_sub(RING_EXPIRE_S);
    d.iridium_sats.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= sat_cut);
    d.iridium_rings.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= ring_cut);
    d.iridium_devices.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= ring_cut);
    // Persist the accumulated beam pattern occasionally so it survives
    // restarts and keeps refining across sessions.
    if now_s().saturating_sub(d.beams_saved) > 120 {
        d.beams.save(&crate::beam::BeamReconstructor::default_path());
        d.beams_saved = now_s();
    }
    json!({
        "station": d.station,
        "started": d.started,
        "sessions": d.sessions,
        "aircraft": d.aircraft.values().collect::<Vec<_>>(),
        "vessels": d.vessels.values().collect::<Vec<_>>(),
        "iridium_sats": d.iridium_sats.values().collect::<Vec<_>>(),
        "iridium_rings": d.iridium_rings.values().collect::<Vec<_>>(),
        "iridium_devices": d.iridium_devices.values().collect::<Vec<_>>(),
        "iridium_beam_cells": d.beams.project(now_s() as f64, SAT_EXPIRE_S as f64),
        "messages": d.recent.iter().rev().take(100).collect::<Vec<_>>(),
        "totals": d.totals,
        "last_seen": d.last_seen,
        "now": now_s(),
    })
    .to_string()
}

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    addr: String,
    station: String,
    sessions: Vec<Value>,
) -> std::io::Result<()> {
    let mut dash = Dash { station, started: now_s(), sessions, ..Dash::default() };
    // Resume the accumulated 48-beam pattern across restarts.
    dash.beams = crate::beam::BeamReconstructor::load(&crate::beam::BeamReconstructor::default_path());
    let state = Arc::new(Mutex::new(dash));

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

#[cfg(test)]
mod tests {
    use super::*;
    use xng_types::{AppInfo, Mode, Provenance, StationIdentity};

    fn ring_alert(sat: u64, alt_km: f64) -> Message {
        Message {
            mode: Mode::Iridium,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_626_270_000,
            signal: Default::default(),
            decode: Default::default(),
            body: MessageBody::Iridium {
                kind: "ring-alert".into(),
                details: json!({
                    "sat": sat, "beam": 5, "lat": 40.0, "lon": -120.0,
                    "alt_km": alt_km, "satellite": "IRIDIUM 106"
                }),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn buckets_iridium_satellites_and_rings_by_altitude() {
        let mut d = Dash::default();
        update(&mut d, &ring_alert(44, 797.0)); // satellite altitude → sat
        update(&mut d, &ring_alert(77, 16.0)); // ground footprint → ring
        assert_eq!(d.iridium_sats.len(), 1);
        assert_eq!(d.iridium_rings.len(), 1);
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        assert_eq!(snap["iridium_sats"][0]["sat"], 44);
        assert_eq!(snap["iridium_sats"][0]["name"], "IRIDIUM 106");
        assert_eq!(snap["iridium_rings"][0]["sat"], 77);
        assert_eq!(snap["iridium_rings"][0]["beam"], 5);
    }
}
