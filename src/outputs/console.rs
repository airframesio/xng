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
        "cpdlc" => {
            // Decoded element text when available, IMI fallback otherwise.
            if let Some(text) = app.get("text").and_then(|t| t.as_str()) {
                let more = app
                    .get("more_elements")
                    .and_then(|m| m.as_bool())
                    .unwrap_or(false);
                Some(format!("CPDLC {text}{}", if more { " (+more)" } else { "" }))
            } else {
                Some(format!("CPDLC {}", app.get("imi")?.as_str()?))
            }
        }
        "media_advisory" => Some("MEDIA-ADV".to_owned()),
        _ => None,
    }
}

/// Optional ground-station name table (hex AVLC address → name),
/// loaded once from --gs-file.
static GS_NAMES: std::sync::OnceLock<std::collections::HashMap<String, String>> =
    std::sync::OnceLock::new();

/// Load a JSON object file mapping hex addresses to names.
pub fn load_gs_names(path: &std::path::Path) -> anyhow::Result<()> {
    let map: std::collections::HashMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let _ = GS_NAMES.set(map);
    Ok(())
}

fn gs_name(addr: &str) -> Option<&'static str> {
    GS_NAMES.get()?.get(addr).map(String::as_str)
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
                MessageBody::Ais { nmea, msg_type, mmsi, details } => {
                    let mut s = format!(
                        "AIS type={} mmsi={}",
                        msg_type.map_or("?".into(), |t| t.to_string()),
                        mmsi.map_or("?".into(), |m| m.to_string()),
                    );
                    if let Some(d) = details {
                        for (key, label) in [
                            ("name", "name"),
                            ("text", "txt"),
                            ("destination", "dest"),
                            ("nav_status", "status"),
                        ] {
                            if let Some(v) = d.get(key).and_then(|v| v.as_str()) {
                                s.push_str(&format!(" {label}={v}"));
                            }
                        }
                        if let (Some(lat), Some(lon)) = (
                            d.get("lat").and_then(|v| v.as_f64()),
                            d.get("lon").and_then(|v| v.as_f64()),
                        ) {
                            s.push_str(&format!(" pos={lat:.5},{lon:.5}"));
                        }
                        if let Some(v) = d.get("sog_kt").and_then(|v| v.as_f64()) {
                            s.push_str(&format!(" sog={v}kt"));
                        }
                    }
                    s.push_str(&format!(" {}", nmea.first().map(String::as_str).unwrap_or("")));
                    s
                }
                MessageBody::ModeS {
                    df,
                    icao,
                    callsign,
                    altitude_ft,
                    squawk,
                    lat,
                    lon,
                    speed_kt,
                    speed_type,
                    track_deg,
                    vertical_rate_fpm,
                    comm_b,
                } => {
                    let mut s = format!("MODE-S df={} icao={}", df, icao.as_deref().unwrap_or("-"));
                    if let Some(c) = callsign {
                        s.push_str(&format!(" ident={c}"));
                    }
                    if let Some(sq) = squawk {
                        s.push_str(&format!(" squawk={sq}"));
                    }
                    if let Some(a) = altitude_ft {
                        s.push_str(&format!(" alt={a}ft"));
                    }
                    if let (Some(la), Some(lo)) = (lat, lon) {
                        s.push_str(&format!(" pos={la:.5},{lo:.5}"));
                    }
                    if let Some(v) = speed_kt {
                        s.push_str(&format!(" {}={v:.0}kt", speed_type.as_deref().unwrap_or("GS").to_lowercase()));
                    }
                    if let Some(t) = track_deg {
                        s.push_str(&format!(" trk={t:.0}"));
                    }
                    if let Some(vr) = vertical_rate_fpm {
                        s.push_str(&format!(" vr={vr:+}fpm"));
                    }
                    if let Some(cb) = comm_b {
                        if let Some(b) = cb.get("bds").and_then(|v| v.as_str()) {
                            s.push_str(&format!(" bds={b}"));
                        }
                        if let Some(c) = cb.get("callsign").and_then(|v| v.as_str()) {
                            s.push_str(&format!(" ident={c}"));
                        }
                        if let Some(a) = cb.get("selected_altitude_mcp") {
                            s.push_str(&format!(" sel_alt={a}ft"));
                        }
                        if let Some(h) = cb.get("magnetic_heading").and_then(|v| v.as_f64()) {
                            s.push_str(&format!(" hdg={h:.0}"));
                        }
                    }
                    s
                }
                MessageBody::Iridium { kind, details } => {
                    let mut s = format!("IRIDIUM {kind}");
                    if let Some(ric) = details.pointer("/body/ric").or_else(|| details.get("ric")) {
                        s.push_str(&format!(" ric={ric}"));
                    }
                    if let Some(t) = details
                        .pointer("/body/text")
                        .or_else(|| details.get("text"))
                        .and_then(|v| v.as_str())
                    {
                        s.push_str(&format!(" | {t}"));
                    }
                    for key in ["sat", "beam"] {
                        if let Some(v) = details.get(key) {
                            s.push_str(&format!(" {key}={v}"));
                        }
                    }
                    if let (Some(lat), Some(lon)) = (
                        details.get("lat").and_then(|v| v.as_f64()),
                        details.get("lon").and_then(|v| v.as_f64()),
                    ) {
                        s.push_str(&format!(" pos={lat:.2},{lon:.2}"));
                    }
                    if let Some(p) = details.get("pages").and_then(|v| v.as_array()) {
                        s.push_str(&format!(" pages={}", p.len()));
                    }
                    s
                }
                MessageBody::Vdl2 { kind, details } => {
                    let addr = |k: &str| {
                        details
                            .get(k)
                            .and_then(|a| a.get("addr"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_owned()
                    };
                    let deco = |a: String| match gs_name(&a) {
                        Some(n) => format!("{a}[{n}]"),
                        None => a,
                    };
                    let mut s = format!(
                        "VDL2 {} {}→{}",
                        kind.to_uppercase(),
                        deco(addr("src")),
                        deco(addr("dst"))
                    );
                    if let Some(nr) = details.pointer("/control/nr").and_then(|v| v.as_u64()) {
                        s.push_str(&format!(" nr={nr}"));
                    }
                    if let Some(p) = details.get("protocol").and_then(|v| v.as_str()) {
                        s.push_str(&format!(" [{p}]"));
                    }
                    if let Some(params) = details.get("params").and_then(|v| v.as_array()) {
                        let named: Vec<String> = params
                            .iter()
                            .filter_map(|p| {
                                let name = p.get("name")?.as_str()?;
                                match p.get("text").and_then(|t| t.as_str()) {
                                    Some(t) => Some(format!("{name}={t}")),
                                    None => Some(name.to_string()),
                                }
                            })
                            .collect();
                        if !named.is_empty() {
                            s.push_str(&format!(" | {}", named.join(", ")));
                        }
                    }
                    s
                }
                MessageBody::Hfdl { kind, details } => {
                    let mut s = format!("HFDL {kind}");
                    for key in ["gs_id", "flight", "icao", "frame_index"] {
                        if let Some(v) = details.get(key) {
                            s.push_str(&format!(" {key}={v}"));
                        }
                    }
                    if let (Some(lat), Some(lon)) =
                        (details.get("lat").and_then(|v| v.as_f64()), details.get("lon").and_then(|v| v.as_f64()))
                    {
                        s.push_str(&format!(" pos={lat:.4},{lon:.4}"));
                    }
                    s
                }
                MessageBody::StdC { name, text, details } => {
                    let svc = details.get("service").and_then(|v| v.as_str()).unwrap_or("");
                    let pri = details.get("priority").and_then(|v| v.as_str()).unwrap_or("");
                    let mut s = format!("STD-C {name}");
                    if !svc.is_empty() {
                        s.push_str(&format!(" {svc}"));
                    }
                    if !pri.is_empty() {
                        s.push_str(&format!(" [{pri}]"));
                    }
                    if let Some(t) = text {
                        s.push_str(&format!(" | {}", t.replace('\n', "·")));
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
