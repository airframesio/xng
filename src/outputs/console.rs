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
                    adsb_status,
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
                    if let Some(st) = adsb_status {
                        if let Some(em) = st.get("emergency").and_then(|v| v.as_str()) {
                            if em != "none" {
                                s.push_str(&format!(" EMERGENCY={em}"));
                            }
                        }
                        if let Some(v) = st.get("version").and_then(|v| v.as_u64()) {
                            s.push_str(&format!(" adsbv={v}"));
                        }
                        if let Some(n) = st.get("nac_p").and_then(|v| v.as_u64()) {
                            s.push_str(&format!(" nacp={n}"));
                        }
                    }
                    s
                }
                MessageBody::Iridium { kind, details } => {
                    let mut s = format!("IRIDIUM {kind}");
                    let g = |k: &str| details.get(k);
                    let gs = |k: &str| details.get(k).and_then(|v| v.as_str());

                    // Sub-frame classification — the single most useful label.
                    // IP-channel bursts decode to IIP/IIQ/IIR/IIU and voice
                    // bursts to VOC/VDA/VO6/VOD/VOZ; surface it right after the
                    // kind so "ip-data"/"voice" stop reading as opaque.
                    for key in ["ip_frame", "voice_type"] {
                        if let Some(v) = gs(key) {
                            s.push_str(&format!(" {v}"));
                        }
                    }
                    // IP / VDA ARQ header type (ack-idle / data).
                    if let Some(t) = gs("ip_type") {
                        s.push_str(&format!(" {t}"));
                    }
                    // Satellite / beam (IRA ring alerts, IBC broadcast).
                    for key in ["sat", "beam"] {
                        if let Some(v) = g(key) {
                            s.push_str(&format!(" {key}={v}"));
                        }
                    }
                    // IBC broadcast specifics: timeslot, broadcast clock and
                    // the count of channel assignments it carries.
                    if kind == "broadcast" {
                        if let Some(slot) = g("slot") {
                            s.push_str(&format!(" slot={slot}"));
                        }
                        if let Some(t) = g("iri_time_unix").and_then(|v| v.as_f64()) {
                            if let Some(dt) = chrono::DateTime::from_timestamp(t as i64, 0) {
                                s.push_str(&format!(" time={}", dt.format("%H:%M:%S")));
                            }
                        }
                        if let Some(a) = g("assignments").and_then(|v| v.as_array()) {
                            s.push_str(&format!(" asn={}", a.len()));
                        }
                    }
                    // IMS pager: group, identity, format, sequence.
                    if let Some(grp) = gs("group") {
                        s.push_str(&format!(" grp={grp}"));
                    }
                    if let Some(ric) = details.pointer("/body/ric").or_else(|| g("ric")) {
                        s.push_str(&format!(" ric={ric}"));
                    }
                    if let Some(fmt) = details.pointer("/body/format") {
                        s.push_str(&format!(" fmt={fmt}"));
                    }
                    // Sequence/ack: IP & VDA carry top-level seq/ack; pager
                    // sequence lives under /body. IIQ carries a 13-bit counter.
                    if let Some(seq) = g("seq").or_else(|| details.pointer("/body/seq")) {
                        s.push_str(&format!(" seq={seq}"));
                    }
                    if let Some(ack) = g("ack") {
                        s.push_str(&format!(" ack={ack}"));
                    }
                    if let Some(ctr) = g("counter") {
                        s.push_str(&format!(" ctr={ctr}"));
                    }
                    if let Some(rs) = g("rs_corrected") {
                        s.push_str(&format!(" rs={rs}"));
                    }
                    // Position (IRA / mt-position).
                    if let (Some(lat), Some(lon)) = (
                        g("lat").and_then(|v| v.as_f64()),
                        g("lon").and_then(|v| v.as_f64()),
                    ) {
                        s.push_str(&format!(" pos={lat:.2},{lon:.2}"));
                    }
                    // LCW control word carried by every duplex burst
                    // (maint/acchl/hndof) — show its control type unless silent.
                    if let Some(ty) = details.pointer("/lcw/type").and_then(|v| v.as_str()) {
                        let code = details.pointer("/lcw/code");
                        let code_s = code
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                            .or_else(|| {
                                code.and_then(|v| v.get("code"))
                                    .and_then(|v| v.as_str())
                                    .map(str::to_owned)
                            });
                        match code_s {
                            Some(c) if c != "<silent>" => s.push_str(&format!(" lcw={ty}:{c}")),
                            None => s.push_str(&format!(" lcw={ty}")),
                            _ => {}
                        }
                    }
                    // Decoded text payloads: pager ASCII / reassembled page /
                    // IP-or-VDA ASCII / pager BCD digits.
                    let text = details
                        .pointer("/body/text")
                        .or_else(|| g("text"))
                        .or_else(|| g("data_ascii"))
                        .or_else(|| details.pointer("/body/digits"))
                        .and_then(|v| v.as_str())
                        .filter(|t| !t.is_empty());
                    if let Some(t) = text {
                        s.push_str(&format!(" | {t}"));
                    }
                    // Multi-part page progress (1-based).
                    if let (Some(ctr), Some(ctrm)) = (
                        details.pointer("/body/ctr").and_then(|v| v.as_u64()),
                        details.pointer("/body/ctr_max").and_then(|v| v.as_u64()),
                    ) {
                        if ctrm > 0 {
                            s.push_str(&format!(" [{}/{}]", ctr + 1, ctrm + 1));
                        }
                    }
                    if let Some(p) = g("pages").and_then(|v| v.as_array()) {
                        s.push_str(&format!(" pages={}", p.len()));
                    }
                    s
                }
                MessageBody::Aero { kind, details } => {
                    let mut s = format!("AERO {kind}");
                    if let Some(svc) = details.get("service").and_then(|v| v.as_str()) {
                        s.push_str(&format!(" {svc}"));
                    }
                    if let (Some(aes), Some(ges)) = (
                        details.get("aes_id").and_then(|v| v.as_str()),
                        details.get("ges_id").and_then(|v| v.as_u64()),
                    ) {
                        s.push_str(&format!(" AES:{aes} GES:{ges}"));
                    }
                    if let (Some(rx), Some(tx)) = (
                        details.get("receive_mhz").and_then(|v| v.as_f64()),
                        details.get("transmit_mhz").and_then(|v| v.as_f64()),
                    ) {
                        s.push_str(&format!(" rx:{rx:.4} tx:{tx:.4} MHz"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use xng_types::{AppInfo, Mode, Provenance, StationIdentity};

    fn line(kind: &'static str, details: serde_json::Value) -> String {
        let m = Message {
            mode: Mode::Iridium,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_626_000_000,
            signal: Default::default(),
            decode: Default::default(),
            body: MessageBody::Iridium { kind: kind.into(), details },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        format_message(&m, ConsoleFormat::Pretty)
    }

    #[test]
    fn broadcast_shows_sat_beam_slot_time_assignments() {
        let s = line(
            "broadcast",
            json!({
                "bc_type": 0, "sat": 12, "beam": 34, "slot": 1,
                "info_type": 1, "iri_time_unix": 1_700_000_000.0,
                "assignments": [{"access":1},{"access":2},{"access":3}],
            }),
        );
        assert!(s.contains("IRIDIUM broadcast"), "{s}");
        assert!(s.contains("sat=12") && s.contains("beam=34"), "{s}");
        assert!(s.contains("slot=1"), "{s}");
        assert!(s.contains("time="), "{s}");
        assert!(s.contains("asn=3"), "{s}");
    }

    #[test]
    fn ip_data_iip_shows_frame_type_seq_ack_ascii_and_lcw() {
        let s = line(
            "ip-data",
            json!({
                "ip_frame": "IIP", "ip_type": "data", "seq": 7, "ack": 99,
                "data_ascii": "HELLO",
                "lcw": {"type": "maint", "code": "geoloc"},
            }),
        );
        assert!(s.contains("IRIDIUM ip-data IIP data"), "{s}");
        assert!(s.contains("seq=7") && s.contains("ack=99"), "{s}");
        assert!(s.contains("| HELLO"), "{s}");
        assert!(s.contains("lcw=maint:geoloc"), "{s}");
    }

    #[test]
    fn ip_data_iiq_shows_counter() {
        let s = line("ip-data", json!({ "ip_frame": "IIQ", "flags": 5, "counter": 291 }));
        assert!(s.contains("IRIDIUM ip-data IIQ"), "{s}");
        assert!(s.contains("ctr=291"), "{s}");
    }

    #[test]
    fn msg_ascii_shows_group_ric_seq_text_and_multipart() {
        let s = line(
            "msg",
            json!({
                "block": 3, "frame": 9, "group": "1",
                "body": { "ric": 1234567, "format": 5, "seq": 7, "content": "ascii",
                          "text": "CALL OPS", "ctr": 0, "ctr_max": 1, "csum_ok": true },
            }),
        );
        assert!(s.contains("IRIDIUM msg"), "{s}");
        assert!(s.contains("grp=1"), "{s}");
        assert!(s.contains("ric=1234567"), "{s}");
        assert!(s.contains("seq=7"), "{s}");
        assert!(s.contains("| CALL OPS"), "{s}");
        assert!(s.contains("[1/2]"), "{s}");
    }

    #[test]
    fn msg_bcd_shows_digits() {
        let s = line(
            "msg",
            json!({
                "block": 1, "frame": 2, "group": "0",
                "body": { "ric": 42, "format": 3, "seq": 1, "content": "bcd", "digits": "4155550100" },
            }),
        );
        assert!(s.contains("| 4155550100"), "{s}");
    }

    #[test]
    fn voice_voc_shows_type_and_lcw_nested_code() {
        let s = line(
            "voice",
            json!({
                "voice_type": "VOC", "ambe_hex": "deadbeef",
                "lcw": {"type": "hndof", "code": {"code": "handoff_resp"}},
            }),
        );
        assert!(s.contains("IRIDIUM voice VOC"), "{s}");
        assert!(s.contains("lcw=hndof:handoff_resp"), "{s}");
    }

    #[test]
    fn voice_silent_lcw_is_omitted() {
        let s = line(
            "voice",
            json!({ "voice_type": "VOC", "lcw": {"type": "maint", "code": "<silent>"} }),
        );
        assert!(s.contains("IRIDIUM voice VOC"), "{s}");
        assert!(!s.contains("lcw="), "silent lcw should be omitted: {s}");
    }
}
