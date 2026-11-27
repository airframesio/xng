//! Per-mode Airframes feeding.
//!
//! Routes each decoded message to the Airframes ingest for *its* mode, in
//! that ingest's native wire format, under a per-mode / per-session station
//! id. This is the legacy per-port push path; asf-2.0 (the multiplexed
//! gRPC/QUIC feed) is a separate, independent path under the canonical
//! station ident and does NOT go through this router.
//!
//! Airframes ingests (aggregation-server, `feed.airframes.io`):
//! - ACARS   UDP  `:5550`  acarsdec flat JSON
//! - VDL2    UDP  `:5552`  dumpvdl2 `decoded:json` (also TCP `:5553`)
//! - HFDL    TCP  `:5556`  dumphfdl `decoded:json`, one object per line
//! - AIS     HTTP `:5599`  AIS-catcher `jsonaiscatcher` batches
//! - Aero-L  UDP  `:5571`  JAERO JDUMP JSON
//! - Aero-C  UDP  `:5561`  JAERO JDUMP JSON
//! - Iridium TCP  `:5590`  iridium-toolkit ACARS JSON, one object per line
//!
//! STD-C / ADS-B / UAT / … have no per-port ingest — those modes reach
//! Airframes via asf-2.0 only.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc};
use xng_types::{Message, Mode};

/// Public Airframes ingest host.
pub const AIRFRAMES_HOST: &str = "feed.airframes.io";

/// Transport for an Airframes ingest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Udp,
    /// Newline-delimited JSON over one long-lived TCP connection.
    Tcp,
    /// HTTP POST of batched JSON (the AIS intake).
    Http,
}

/// A resolved Airframes ingest endpoint plus the station id to stamp.
#[derive(Clone, Debug)]
pub struct AirframesTarget {
    pub host: String,
    pub port: u16,
    pub proto: Proto,
    pub station_id: String,
}

impl AirframesTarget {
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// One routing rule: messages of `mode` from `sdr_id` (None = any SDR) feed
/// to `target`.
#[derive(Clone, Debug)]
pub struct Route {
    pub sdr_id: Option<String>,
    pub mode: Mode,
    pub target: AirframesTarget,
}

/// The per-session Airframes feed routing table for a process.
#[derive(Clone, Debug, Default)]
pub struct AirframesRouter {
    pub routes: Vec<Route>,
}

impl AirframesRouter {
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Resolve the feed target for a message: prefer an exact `(sdr_id, mode)`
    /// match (per-session routes from a station config), then a wildcard
    /// (`sdr_id = None`) match on mode (single-session CLI routes).
    pub fn resolve(&self, mode: Mode, sdr_id: Option<&str>) -> Option<&AirframesTarget> {
        if let Some(id) = sdr_id {
            if let Some(r) = self
                .routes
                .iter()
                .find(|r| r.mode == mode && r.sdr_id.as_deref() == Some(id))
            {
                return Some(&r.target);
            }
        }
        self.routes
            .iter()
            .find(|r| r.mode == mode && r.sdr_id.is_none())
            .map(|r| &r.target)
    }
}

/// The default public Airframes ingest for a mode, or `None` if that mode has
/// no per-port ingest (feed it via asf-2.0 instead).
pub fn default_endpoint(mode: Mode) -> Option<(&'static str, u16, Proto)> {
    match mode {
        Mode::AcarsPoa => Some((AIRFRAMES_HOST, 5550, Proto::Udp)),
        Mode::Vdl2 => Some((AIRFRAMES_HOST, 5552, Proto::Udp)),
        Mode::Hfdl => Some((AIRFRAMES_HOST, 5556, Proto::Tcp)),
        Mode::Ais => Some((AIRFRAMES_HOST, 5599, Proto::Http)),
        Mode::AeroL => Some((AIRFRAMES_HOST, 5571, Proto::Udp)),
        Mode::AeroC => Some((AIRFRAMES_HOST, 5561, Proto::Udp)),
        Mode::Iridium => Some((AIRFRAMES_HOST, 5590, Proto::Tcp)),
        _ => None,
    }
}

/// Whether xng can currently serialize this mode into its Airframes native
/// format. Modes without a serializer route to asf-2.0 only (for now).
pub fn has_serializer(mode: Mode) -> bool {
    default_endpoint(mode).is_some()
}

/// The station-id suffix Airframes expects per mode (for `auto-suffix`).
pub fn mode_suffix(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::AcarsPoa => Some("ACARS"),
        Mode::Vdl2 => Some("VDL2"),
        Mode::Hfdl => Some("HFDL"),
        Mode::Ais => Some("AIS"),
        Mode::AeroL | Mode::AeroC => Some("IMSL"),
        Mode::Iridium => Some("IRDM"),
        Mode::StdC => Some("STDC"),
        Mode::Adsb => Some("ADSB"),
        Mode::Uat => Some("UAT"),
        Mode::Sarsat => Some("SARSAT"),
        Mode::Dsc => Some("DSC"),
        Mode::Navtex => Some("NAVTEX"),
        Mode::Sonde => Some("SONDE"),
        Mode::AdsL => Some("ADSL"),
        Mode::Atcs => Some("ATCS"),
        // No public per-port Airframes ingest — fed via asf-2.0, not the
        // per-mode router (so no station-id suffix).
        Mode::Aprs | Mode::Pocsag | Mode::Eot | Mode::Flex | Mode::Vdes | Mode::Time => None,
        Mode::Meshtastic | Mode::MeshCore => None,
        // Airband audio feeds the ATC aggregator (`--atc-feed`), not the
        // per-port message ingests.
        Mode::Airband => None,
        Mode::Extern => None,
    }
}

const KNOWN_SUFFIXES: &[&str] = &[
    "ACARS", "VDL2", "HFDL", "AIS", "IMSL", "IRDM", "STDC", "ADSB", "UAT", "SARSAT", "DSC",
    "NAVTEX", "SONDE", "ADSL", "ATCS",
];

/// Derive a per-mode station id from a base by stripping a trailing known
/// mode suffix (if any) and appending this mode's suffix:
/// `KE-KSEA` + acars → `KE-KSEA-ACARS`; `KE-KSEA-ACARS` + vdl2 → `KE-KSEA-VDL2`.
/// Bases that don't end in a known suffix are left intact and appended to.
pub fn auto_suffix(base: &str, mode: Mode) -> String {
    let Some(suffix) = mode_suffix(mode) else {
        return base.to_string();
    };
    let stem = strip_known_suffix(base);
    format!("{stem}-{suffix}")
}

fn strip_known_suffix(s: &str) -> &str {
    if let Some((stem, last)) = s.rsplit_once('-') {
        // Tolerate a trailing instance number, e.g. `-ACARS1`.
        let alpha = last.trim_end_matches(|c: char| c.is_ascii_digit());
        if KNOWN_SUFFIXES.contains(&alpha) {
            return stem;
        }
    }
    s
}

/// Build a router for a single CLI session feeding `--feed-airframes`: the
/// station id is used verbatim (no auto-suffix), with one wildcard route per
/// mode that has a serializer.
pub fn cli_router(feed: bool, station_id: &str, modes: &[Mode]) -> AirframesRouter {
    if !feed {
        return AirframesRouter::default();
    }
    let routes = modes
        .iter()
        .filter_map(|&mode| {
            if !has_serializer(mode) {
                return None;
            }
            let (host, port, proto) = default_endpoint(mode)?;
            Some(Route {
                sdr_id: None,
                mode,
                target: AirframesTarget {
                    host: host.to_string(),
                    port,
                    proto,
                    station_id: station_id.to_string(),
                },
            })
        })
        .collect();
    AirframesRouter { routes }
}

/// Serialize a message into its mode's native Airframes line/datagram
/// payload (no trailing newline), stamping `station_id`. Returns `None` when
/// the body doesn't match the mode or isn't something that ingest takes. AIS
/// is batched separately (see [`AisBatcher`]).
fn serialize(msg: &Message, mode: Mode, station_id: &str) -> Option<Vec<u8>> {
    use crate::outputs::{acarsdec_json, dumphfdl_json, dumpvdl2_json, iridium_toolkit_json, jaero_json};
    let v = match mode {
        Mode::AcarsPoa => acarsdec_json::format_acarsdec_with_station(msg, Some(station_id)),
        Mode::Vdl2 => dumpvdl2_json::format_dumpvdl2(msg, Some(station_id)),
        Mode::Hfdl => dumphfdl_json::format_dumphfdl(msg, Some(station_id)),
        Mode::AeroL | Mode::AeroC => jaero_json::format_jaero(msg, Some(station_id)),
        Mode::Iridium => iridium_toolkit_json::format_iridium_toolkit(msg, station_id),
        _ => None,
    }?;
    Some(v.to_string().into_bytes())
}

/// Lines queued per TCP ingest while it is unreachable; the oldest are kept
/// in flight and new ones dropped once full (counted in the log).
const TCP_QUEUE: usize = 4096;

/// One long-lived TCP connection to a line-JSON ingest (HFDL, Iridium):
/// connects lazily, reconnects with backoff, and never blocks the bus.
struct TcpFeed {
    tx: mpsc::Sender<Vec<u8>>,
    dropped: u64,
}

impl TcpFeed {
    fn spawn(endpoint: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(TCP_QUEUE);
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            // A line that failed to write is retried on the next connection.
            let mut pending: Option<Vec<u8>> = None;
            loop {
                let mut stream = match TcpStream::connect(&endpoint).await {
                    Ok(s) => {
                        tracing::info!("airframes tcp: connected to {endpoint}");
                        let _ = s.set_nodelay(true);
                        backoff = Duration::from_secs(1);
                        s
                    }
                    Err(e) => {
                        tracing::warn!("airframes tcp: connect to {endpoint} failed: {e}; retrying in {backoff:?}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                        continue;
                    }
                };
                loop {
                    let line = match pending.take() {
                        Some(l) => l,
                        None => match rx.recv().await {
                            Some(l) => l,
                            None => return, // bus closed: shutting down
                        },
                    };
                    if let Err(e) = stream.write_all(&line).await {
                        tracing::warn!("airframes tcp: write to {endpoint} failed: {e}; reconnecting");
                        pending = Some(line);
                        break;
                    }
                }
            }
        });
        Self { tx, dropped: 0 }
    }

    fn send(&mut self, mut line: Vec<u8>, endpoint: &str) -> bool {
        line.push(b'\n');
        match self.tx.try_send(line) {
            Ok(()) => true,
            Err(_) => {
                self.dropped += 1;
                if self.dropped.is_power_of_two() {
                    tracing::warn!("airframes tcp: {endpoint} queue full, {} message(s) dropped", self.dropped);
                }
                false
            }
        }
    }
}

/// AIS messages queued per intake while it is unreachable (newest kept).
const AIS_QUEUE: usize = 20_000;
/// Messages per AIS intake POST.
const AIS_BATCH: usize = 500;
/// How often queued AIS messages are posted.
const AIS_INTERVAL: Duration = Duration::from_secs(5);

/// Batches AIS messages and POSTs them, gzip-compressed, to the AIS intake
/// in AIS-catcher's `jsonaiscatcher` envelope under the feed's station id.
struct AisBatcher {
    tx: mpsc::Sender<serde_json::Value>,
}

impl AisBatcher {
    fn spawn(url: String, station: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<serde_json::Value>(AIS_BATCH * 4);
        tokio::spawn(async move {
            let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
            let mut queue: VecDeque<serde_json::Value> = VecDeque::new();
            let mut dropped: u64 = 0;
            let mut tick = tokio::time::interval(AIS_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    m = rx.recv() => match m {
                        Some(m) => {
                            queue.push_back(m);
                            while queue.len() > AIS_QUEUE {
                                queue.pop_front();
                                dropped += 1;
                                if dropped.is_power_of_two() {
                                    tracing::warn!("airframes ais: intake unreachable, {dropped} message(s) dropped");
                                }
                            }
                        }
                        None => break,
                    },
                    _ = tick.tick() => {
                        while !queue.is_empty() {
                            let n = queue.len().min(AIS_BATCH);
                            let batch: Vec<_> = queue.iter().take(n).cloned().collect();
                            let body = crate::outputs::aiscatcher_json::envelope(&station, batch);
                            let (agent2, url2) = (agent.clone(), url.clone());
                            let res = tokio::task::spawn_blocking(move || post_gzip_json(&agent2, &url2, &body)).await;
                            match res {
                                Ok(Ok(())) => {
                                    queue.drain(..n);
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!("airframes ais: post to {url} failed ({} queued): {e}", queue.len());
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!("airframes ais: post task failed: {e}");
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
        Self { tx }
    }
}

fn post_gzip_json(agent: &ureq::Agent, url: &str, body: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let bytes = gz.finish().map_err(|e| e.to_string())?;
    agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("Content-Encoding", "gzip")
        .send_bytes(&bytes)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Consume the bus and feed each message to its mode's Airframes ingest.
pub async fn run(mut rx: broadcast::Receiver<Arc<Message>>, router: AirframesRouter) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let mut sent: u64 = 0;
    let mut tcp: HashMap<String, TcpFeed> = HashMap::new();
    let mut ais: HashMap<(String, String), AisBatcher> = HashMap::new();
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let sdr_id = msg.source.sdr.as_ref().map(|s| s.id.as_str());
                let Some(target) = router.resolve(msg.mode, sdr_id) else {
                    continue;
                };
                let endpoint = target.endpoint();
                match target.proto {
                    Proto::Udp => {
                        if let Some(bytes) = serialize(&msg, msg.mode, &target.station_id) {
                            match socket.send_to(&bytes, &endpoint).await {
                                Ok(_) => sent += 1,
                                Err(e) => tracing::warn!("airframes udp send to {endpoint} failed: {e}"),
                            }
                        }
                    }
                    Proto::Tcp => {
                        if let Some(bytes) = serialize(&msg, msg.mode, &target.station_id) {
                            let feed = tcp.entry(endpoint.clone()).or_insert_with(|| TcpFeed::spawn(endpoint.clone()));
                            if feed.send(bytes, &endpoint) {
                                sent += 1;
                            }
                        }
                    }
                    Proto::Http => {
                        if let Some(v) = crate::outputs::aiscatcher_json::format_aiscatcher(&msg) {
                            let key = (endpoint.clone(), target.station_id.clone());
                            let batcher = ais.entry(key).or_insert_with(|| {
                                AisBatcher::spawn(format!("http://{endpoint}/"), target.station_id.clone())
                            });
                            if batcher.tx.try_send(v).is_ok() {
                                sent += 1;
                            }
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("airframes output lagged, dropped {n} messages")
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    tracing::info!("airframes output: {sent} message(s) sent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_suffix_appends_and_swaps() {
        assert_eq!(auto_suffix("KE-KSEA", Mode::AcarsPoa), "KE-KSEA-ACARS");
        assert_eq!(auto_suffix("KE-KSEA-ACARS", Mode::Vdl2), "KE-KSEA-VDL2");
        assert_eq!(auto_suffix("KE-KSEA-ACARS1", Mode::Hfdl), "KE-KSEA-HFDL");
        // unknown trailing segment is preserved (operator's base kept intact)
        assert_eq!(auto_suffix("KE-KSEA-TEST", Mode::AcarsPoa), "KE-KSEA-TEST-ACARS");
    }

    #[test]
    fn resolve_prefers_exact_session_then_wildcard() {
        let r = AirframesRouter {
            routes: vec![
                Route {
                    sdr_id: Some("rtl0".into()),
                    mode: Mode::AcarsPoa,
                    target: AirframesTarget {
                        host: "h".into(),
                        port: 5550,
                        proto: Proto::Udp,
                        station_id: "EXACT".into(),
                    },
                },
                Route {
                    sdr_id: None,
                    mode: Mode::AcarsPoa,
                    target: AirframesTarget {
                        host: "h".into(),
                        port: 5550,
                        proto: Proto::Udp,
                        station_id: "WILD".into(),
                    },
                },
            ],
        };
        assert_eq!(r.resolve(Mode::AcarsPoa, Some("rtl0")).unwrap().station_id, "EXACT");
        assert_eq!(r.resolve(Mode::AcarsPoa, Some("other")).unwrap().station_id, "WILD");
        assert_eq!(r.resolve(Mode::AcarsPoa, None).unwrap().station_id, "WILD");
        assert!(r.resolve(Mode::Vdl2, Some("rtl0")).is_none());
    }

    #[test]
    fn cli_router_only_routes_serializable_modes() {
        let r = cli_router(true, "XX-TEST", &[Mode::AcarsPoa, Mode::Vdl2, Mode::Hfdl, Mode::StdC]);
        // STD-C has no per-port ingest, so it is not routed.
        assert_eq!(r.routes.len(), 3);
        assert_eq!(r.routes[0].mode, Mode::AcarsPoa);
        assert_eq!(r.routes[0].target.port, 5550);
        assert_eq!(r.routes[1].mode, Mode::Vdl2);
        assert_eq!(r.routes[1].target.port, 5552);
        assert_eq!(r.routes[2].mode, Mode::Hfdl);
        assert_eq!(r.routes[2].target.proto, Proto::Tcp);
        assert!(cli_router(false, "XX-TEST", &[Mode::AcarsPoa]).is_empty());
    }

    #[test]
    fn endpoints_match_verified_ports() {
        assert_eq!(default_endpoint(Mode::AcarsPoa), Some((AIRFRAMES_HOST, 5550, Proto::Udp)));
        assert_eq!(default_endpoint(Mode::Vdl2), Some((AIRFRAMES_HOST, 5552, Proto::Udp)));
        // The aggregator's dumphfdl ingest is TCP only.
        assert_eq!(default_endpoint(Mode::Hfdl), Some((AIRFRAMES_HOST, 5556, Proto::Tcp)));
        assert_eq!(default_endpoint(Mode::Ais), Some((AIRFRAMES_HOST, 5599, Proto::Http)));
        assert_eq!(default_endpoint(Mode::AeroL), Some((AIRFRAMES_HOST, 5571, Proto::Udp)));
        assert_eq!(default_endpoint(Mode::AeroC), Some((AIRFRAMES_HOST, 5561, Proto::Udp)));
        assert_eq!(default_endpoint(Mode::Iridium), Some((AIRFRAMES_HOST, 5590, Proto::Tcp)));
        // No public per-port ingest → asf-2.0 only.
        assert_eq!(default_endpoint(Mode::StdC), None);
        assert_eq!(default_endpoint(Mode::Adsb), None);
    }

    // ---- Wire samples: real decoder output through each feed serializer ----

    fn prov() -> xng_types::Provenance {
        xng_types::Provenance {
            station: xng_types::StationIdentity::new("XX-TEST"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        }
    }

    fn iq_i16(path: &str, stereo: bool) -> Vec<num_complex::Complex<f32>> {
        let raw = std::fs::read(path).expect("fixture present");
        let s = |b: &[u8]| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0;
        if stereo {
            raw.chunks_exact(4).map(|b| num_complex::Complex::new(s(&b[..2]), s(&b[2..]))).collect()
        } else {
            raw.chunks_exact(2).map(|b| num_complex::Complex::new(s(b), 0.0)).collect()
        }
    }

    /// One line per feed, from real decoder output: the VDL2 and Aero
    /// off-air fixtures, HFDL and Iridium ACARS built at the PDU layer and
    /// AIS from real AIVDM payloads. With `XNG_WIRE_SAMPLES_DIR` set, the
    /// lines are written there (`<feed>.jsonl`) so they can be checked
    /// against the aggregation-server's own parsers.
    fn wire_samples() -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let root = env!("CARGO_MANIFEST_DIR");

        // VDL2: off-air HB-IJW downlink.
        let mut dec = xng_mode_vdl2::Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
        let iq = iq_i16(&format!("{root}/crates/xng-mode-vdl2/tests/data/vdl2_offair_6s.i16"), true);
        'vdl2: for chunk in iq.chunks(65_536) {
            for f in dec.process(chunk) {
                if f.acars.is_some() {
                    let m = xng_mode_vdl2::to_message(&f, 136_975_000, -10.77, prov());
                    if let Some(b) = serialize(&m, Mode::Vdl2, "XX-TEST-VDL2") {
                        out.push(("dumpvdl2", String::from_utf8(b).unwrap()));
                        break 'vdl2;
                    }
                }
            }
        }

        // Aero-L: off-air JAERO 600 bps sample.
        let mut dec = xng_mode_aero::AeroChannelDecoder::new(48_000.0, 1_066.0).unwrap();
        let iq = iq_i16(&format!("{root}/crates/xng-mode-aero/tests/data/600bps_offair_12s.i16"), false);
        'aero: for chunk in iq.chunks(65_536) {
            for e in dec.process(chunk) {
                if e.acars.as_ref().is_some_and(|b| b.crc_ok) {
                    let m = xng_mode_aero::to_message(&e, 1_545_000_000, -20.0, prov());
                    if let Some(b) = serialize(&m, Mode::AeroL, "XX-TEST-IMSL") {
                        out.push(("jaero", String::from_utf8(b).unwrap()));
                        break 'aero;
                    }
                }
            }
        }

        // HFDL: logon confirm (fills the ICAO cache), then a downlink ACARS
        // and a performance-data report from that aircraft.
        use xng_mode_hfdl::pdu;
        let mut parser = pdu::PduParser::new();
        let logon = pdu::build_mpdu_uplink(7, 0xC7, &[pdu::build_lpdu_logon_confirm(0xA1B2C3, 0xC7)]);
        let block = xng_acars::block::build(
            '2', "N471XG", None, "H1", '4', Some("D11A"), Some("UA0042"), "#M1BPOSN37.5W122.1", false,
        );
        let acars = pdu::build_mpdu_downlink(7, 0xC7, &[pdu::build_lpdu_acars(&block)]);
        let mut perf = vec![0xFF, 0xD1];
        perf.extend_from_slice(b"UA0042");
        perf.resize(47, 0);
        perf[15] = 1; // version
        perf[16] = 2; // flight leg
        perf[17] = 7; // ground station
        let perf = pdu::build_mpdu_downlink(7, 0xC7, &[pdu::build_lpdu_hfnpdu(&perf)]);
        let mut events = parser.parse(&logon, 1800);
        events.extend(parser.parse(&acars, 1800));
        events.extend(parser.parse(&perf, 1800));
        for e in &events {
            let m = xng_mode_hfdl::to_message(e, 8_927_000, -23.0, prov());
            if let Some(b) = serialize(&m, Mode::Hfdl, "XX-TEST-HFDL") {
                out.push(("dumphfdl", String::from_utf8(b).unwrap()));
            }
        }

        // Iridium: SBD-carried ACARS (mobile originated).
        let block = xng_acars::block::build(
            '2', "N767AX", None, "H1", '7', Some("D23A"), Some("AA0100"), "POSN12345", false,
        );
        let b = xng_acars::block::parse(&block).unwrap();
        let f = xng_mode_iridium::ira::IridiumFrame {
            kind: "sbd",
            details: serde_json::json!({ "momsn": 12, "type": "0076" }),
            acars: Some(b),
            raw_bits: Vec::new(),
        };
        let m = xng_mode_iridium::to_message(&f, 1_626_270_000, -41.0, prov());
        out.push(("iridium-toolkit", String::from_utf8(serialize(&m, Mode::Iridium, "XX-TEST-IRDM").unwrap()).unwrap()));

        // AIS: a class A position report and a static/voyage report.
        let mut msgs = Vec::new();
        for (sentence, payload, fill) in [
            ("!AIVDM,1,1,,B,15M67FC000G?ufbE`FepT@3n00Sa,0*5C", "15M67FC000G?ufbE`FepT@3n00Sa", 0),
            (
                "!AIVDM,1,1,,A,55P5TL01VIaAL@7WKO@mBplU@<PDhh000000001S;AJ::4A80?4i@E531@0000000000000,2*00",
                "55P5TL01VIaAL@7WKO@mBplU@<PDhh000000001S;AJ::4A80?4i@E531@0000000000000",
                2,
            ),
        ] {
            let mut bits = xng_mode_ais::nmea::payload_to_bits(payload);
            bits.truncate(bits.len() - fill);
            let t = bits[..6].iter().fold(0u8, |v, &b| (v << 1) | b);
            let mmsi = bits[8..38].iter().fold(0u32, |v, &b| (v << 1) | b as u32);
            let m = Message {
                mode: Mode::Ais,
                timestamp: chrono::Utc::now(),
                frequency_hz: 161_975_000,
                signal: xng_types::SignalQuality { rssi_db: Some(-12.0), ..Default::default() },
                decode: xng_types::DecodeQuality { crc_ok: true, ..Default::default() },
                body: xng_types::MessageBody::Ais {
                    nmea: vec![sentence.into()],
                    msg_type: Some(t),
                    mmsi: Some(mmsi),
                    details: xng_mode_ais::fields::decode(t, &bits),
                },
                raw: None,
                source: prov(),
            };
            msgs.push(crate::outputs::aiscatcher_json::format_aiscatcher(&m).unwrap());
        }
        let env = crate::outputs::aiscatcher_json::envelope("XX-TEST-AIS", msgs);
        out.push(("aiscatcher", env.to_string()));

        if let Ok(dir) = std::env::var("XNG_WIRE_SAMPLES_DIR") {
            let mut files: HashMap<&str, String> = HashMap::new();
            for (feed, line) in &out {
                let f = files.entry(feed).or_default();
                f.push_str(line);
                f.push('\n');
            }
            for (feed, body) in files {
                std::fs::write(format!("{dir}/{feed}.jsonl"), body).unwrap();
            }
        }
        out
    }

    #[test]
    fn every_feed_serializes_real_decodes() {
        let samples = wire_samples();
        let count = |feed: &str| samples.iter().filter(|(f, _)| *f == feed).count();
        assert_eq!(count("dumpvdl2"), 1, "{samples:#?}");
        assert_eq!(count("jaero"), 1, "{samples:#?}");
        assert_eq!(count("dumphfdl"), 2, "ACARS + performance data: {samples:#?}");
        assert_eq!(count("iridium-toolkit"), 1);
        assert_eq!(count("aiscatcher"), 1);
        for (feed, line) in &samples {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            match *feed {
                "dumpvdl2" => assert_eq!(v["vdl2"]["station"], "XX-TEST-VDL2"),
                "dumphfdl" => {
                    assert_eq!(v["hfdl"]["station"], "XX-TEST-HFDL");
                    assert_eq!(v["hfdl"]["lpdu"]["ac_info"]["icao"], "A1B2C3", "{v}");
                }
                "jaero" => {
                    assert_eq!(v["station"], "XX-TEST-IMSL");
                    let isu = &v["isu"];
                    let aes = if isu["src"]["type"] == "Aircraft Earth Station" { &isu["src"] } else { &isu["dst"] };
                    assert_eq!(aes["addr"].as_str().map(str::len), Some(6), "{v}");
                    assert!(v["isu"]["acars"]["reg"].is_string(), "{v}");
                    assert!(v["t"]["sec"].is_i64(), "{v}");
                }
                "iridium-toolkit" => assert_eq!(v["source"]["station_id"], "XX-TEST-IRDM"),
                "aiscatcher" => assert_eq!(v["msgs"].as_array().unwrap().len(), 2),
                _ => unreachable!(),
            }
        }
    }

    // ---- Transports ----

    #[tokio::test]
    async fn tcp_feed_writes_lines_and_reconnects() {
        use tokio::io::AsyncBufReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let mut feed = TcpFeed::spawn(addr.clone());
        assert!(feed.send(b"{\"a\":1}".to_vec(), &addr));
        let (sock, _) = listener.accept().await.unwrap();
        let mut lines = tokio::io::BufReader::new(sock).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("{\"a\":1}"));
        // The ingest drops the connection; later lines arrive on a new one.
        drop(lines);
        let mut got = None;
        for i in 0..50 {
            feed.send(format!("{{\"b\":{i}}}").into_bytes(), &addr);
            if let Ok(Ok((sock, _))) =
                tokio::time::timeout(Duration::from_millis(200), listener.accept()).await
            {
                let mut lines = tokio::io::BufReader::new(sock).lines();
                got = tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await.ok();
                break;
            }
        }
        let line = got.expect("reconnected").unwrap().unwrap();
        assert!(line.starts_with("{\"b\":"), "{line}");
    }

    #[tokio::test]
    async fn ais_batcher_posts_gzip_envelope() {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let (mut len, mut gzip, mut line) = (0usize, false, String::new());
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let l = line.trim().to_ascii_lowercase();
                if l.is_empty() {
                    break;
                }
                if let Some(v) = l.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                gzip |= l == "content-encoding: gzip";
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK").unwrap();
            assert!(gzip);
            let mut json = String::new();
            flate2::read::GzDecoder::new(&body[..]).read_to_string(&mut json).unwrap();
            json
        });
        let b = AisBatcher::spawn(format!("http://{addr}/"), "XX-TEST-AIS".into());
        b.tx.send(serde_json::json!({ "type": 1, "mmsi": 366053209 })).await.unwrap();
        let json = tokio::task::spawn_blocking(move || server.join().unwrap()).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["stationid"], "XX-TEST-AIS");
        assert_eq!(v["msgs"][0]["mmsi"], 366053209);
    }
}
