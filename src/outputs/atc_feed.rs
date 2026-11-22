//! Feed airband audio to the Airframes ATC network (aggregator-atc ingest,
//! protocol v0.2, ALPN `airframes-atc/1`).
//!
//! One connection per xng process, QUIC preferred (audio as datagrams),
//! TLS 1.3 over TCP as the fallback. Each fed stream is declared with OPEN;
//! per transmission the feeder sends SQUELCH open, Opus AUDIO packets
//! (TX_START on the first, TX_END on the last), then SQUELCH close. Audio is
//! squelch-gated, so a quiet channel costs nothing. STATS go every 10 s,
//! PING every heartbeat, and the access token is refreshed and replaced with
//! REAUTH before it expires.

use crate::airband::atc_auth::{self, AccessToken};
use crate::airband::atc_proto as p;
use crate::airband::config::{parse_streams, AtcFeedSettings};
use crate::airband::opus_enc::OpusEncoder;
use crate::airband::{hub, AudioEvent, ChannelMeta, StreamId};
use crate::outputs::asf2_quic::TrustMode;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use xng_types::Message;

/// What the feeder says about the station in HELLO.
#[derive(Clone, Debug, Default)]
pub struct StationInfo {
    pub name: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt_m: Option<f64>,
}

fn now_us() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

fn finite(v: f32) -> Option<f64> {
    v.is_finite().then(|| (v as f64 * 10.0).round() / 10.0)
}

/// Make a channel key acceptable to the server (`[A-Za-z0-9._:/@+-]{1,64}`).
fn clean_key(k: &str) -> String {
    let mut s: String =
        k.chars().map(|c| if c.is_ascii_alphanumeric() || "._:/@+-".contains(c) { c } else { '-' }).take(64).collect();
    if s.is_empty() {
        s.push('0');
    }
    s
}

/// Where the access token comes from.
#[derive(Clone)]
enum TokenSource {
    Fixed(String),
    Saved(PathBuf),
}

impl TokenSource {
    async fn get(&self) -> anyhow::Result<AccessToken> {
        match self {
            TokenSource::Fixed(t) => Ok(AccessToken { token: t.clone(), expires_at: atc_auth::jwt_exp(t) }),
            TokenSource::Saved(path) => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || atc_auth::refresh(&path)).await?
            }
        }
    }
}

enum Out {
    /// A framed message for the byte stream.
    Stream(Vec<u8>),
    /// `0x10` + AUDIO payload, as a QUIC datagram.
    Datagram(Vec<u8>),
}

/// The open connection: frames go through a writer task; frames the server
/// sends (except PING, answered by the reader) arrive on `incoming`.
/// Frames waiting for the writer. A server that stops reading fills this
/// (several seconds of audio) and the session reconnects instead of queueing
/// without limit; nothing is dropped silently.
const OUT_QUEUE: usize = 1_024;
/// One socket write; longer means the peer is not reading.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

struct Link {
    out: mpsc::Sender<Out>,
    incoming: mpsc::UnboundedReceiver<(u8, Vec<u8>)>,
    quic: Option<quinn::Connection>,
    transport: &'static str,
    last_rx: Arc<std::sync::Mutex<Instant>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Link {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Some(c) = &self.quic {
            c.close(0u32.into(), b"bye");
        }
    }
}

impl Link {
    fn send(&self, kind: u8, payload: &[u8]) -> anyhow::Result<()> {
        let f = p::frame(kind, payload)?;
        self.push(Out::Stream(f))
    }

    fn push(&self, o: Out) -> anyhow::Result<()> {
        self.out.try_send(o).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => anyhow::anyhow!("the server is not reading (send queue full)"),
            mpsc::error::TrySendError::Closed(_) => anyhow::anyhow!("connection closed"),
        })
    }

    fn send_json(&self, kind: u8, v: &Value) -> anyhow::Result<()> {
        self.send(kind, &serde_json::to_vec(v)?)
    }

    /// AUDIO as a QUIC datagram when it fits, else on the control stream.
    fn send_audio(&self, payload: Vec<u8>) -> anyhow::Result<()> {
        if let Some(c) = &self.quic {
            if c.max_datagram_size().is_some_and(|m| payload.len() < m) {
                let mut d = Vec::with_capacity(payload.len() + 1);
                d.push(p::AUDIO);
                d.extend_from_slice(&payload);
                // Through the writer, so a datagram never overtakes the
                // SQUELCH frame queued before it.
                return self.push(Out::Datagram(d));
            }
        }
        self.send(p::AUDIO, &payload)
    }

    async fn recv(&mut self, timeout: Duration) -> anyhow::Result<(u8, Vec<u8>)> {
        match tokio::time::timeout(timeout, self.incoming.recv()).await {
            Ok(Some(f)) => Ok(f),
            Ok(None) => anyhow::bail!("connection closed by the server"),
            Err(_) => anyhow::bail!("no answer from the server in {}s", timeout.as_secs()),
        }
    }
}

fn spawn_io<R, W>(mut r: R, mut w: W, quic: Option<quinn::Connection>, transport: &'static str) -> Link
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::channel::<Out>(OUT_QUEUE);
    let dgram_conn = quic.clone();
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let last_rx = Arc::new(std::sync::Mutex::new(Instant::now()));
    let writer = tokio::spawn(async move {
        while let Some(first) = out_rx.recv().await {
            let mut next = Some(first);
            // Write what queued up, then flush once.
            while let Some(o) = next.take().or_else(|| out_rx.try_recv().ok()) {
                match o {
                    Out::Stream(f) => {
                        if !matches!(tokio::time::timeout(WRITE_TIMEOUT, w.write_all(&f)).await, Ok(Ok(()))) {
                            return;
                        }
                    }
                    Out::Datagram(d) => {
                        // Stream bytes written so far must leave first.
                        if !matches!(tokio::time::timeout(WRITE_TIMEOUT, w.flush()).await, Ok(Ok(()))) {
                            return;
                        }
                        let Some(c) = &dgram_conn else { continue };
                        if let Err(e) = c.send_datagram(bytes::Bytes::from(d.clone())) {
                            if matches!(e, quinn::SendDatagramError::ConnectionLost(_)) {
                                return;
                            }
                            // Too big or unsupported after all: use the stream.
                            if let Ok(f) = p::frame(p::AUDIO, &d[1..]) {
                                if !matches!(tokio::time::timeout(WRITE_TIMEOUT, w.write_all(&f)).await, Ok(Ok(()))) {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            if !matches!(tokio::time::timeout(WRITE_TIMEOUT, w.flush()).await, Ok(Ok(()))) {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), w.shutdown()).await;
    });
    let pong = out_tx.clone();
    let lr = last_rx.clone();
    let reader = tokio::spawn(async move {
        loop {
            match p::read_frame(&mut r).await {
                Ok(Some((kind, payload))) => {
                    *lr.lock().unwrap() = Instant::now();
                    match kind {
                        p::PING => {
                            if let Ok(f) = p::frame(p::PONG, &p::pong_payload(&payload, now_us())) {
                                let _ = pong.try_send(Out::Stream(f));
                            }
                        }
                        k if k >= 0x80 => {} // extensions: ignore
                        _ => {
                            if in_tx.send((kind, payload)).is_err() {
                                break;
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!("atc feed: read: {e}");
                    break;
                }
            }
        }
    });
    Link { out: out_tx, incoming: in_rx, quic, transport, last_rx, tasks: vec![writer, reader] }
}

struct Dialer {
    host: String,
    addr: String,
    tls: Arc<rustls::ClientConfig>,
    quic: Option<quinn::ClientConfig>,
}

impl Dialer {
    fn new(s: &AtcFeedSettings) -> anyhow::Result<Self> {
        let trust = if s.insecure {
            TrustMode::Insecure
        } else if let Some(ca) = &s.ca {
            TrustMode::CaFile(ca.clone())
        } else {
            TrustMode::SystemRoots
        };
        let crypto = crate::outputs::asf2_quic::client_crypto(&trust, p::ALPN, "atc feed (--atc-ca / --atc-insecure)")?;
        let host = s.server.rsplit_once(':').map(|(h, _)| h).unwrap_or(&s.server);
        let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let quic = if s.transport == "tcp" {
            None
        } else {
            let mut qc =
                quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto.clone())?));
            let mut tc = quinn::TransportConfig::default();
            tc.keep_alive_interval(Some(Duration::from_secs(10)));
            tc.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
            qc.transport_config(Arc::new(tc));
            Some(qc)
        };
        Ok(Self { host, addr: s.server.clone(), tls: Arc::new(crypto), quic })
    }

    async fn dial(&self, quic: bool) -> anyhow::Result<Link> {
        let addr = tokio::net::lookup_host(&self.addr)
            .await?
            .next()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve {}", self.addr))?;
        if quic {
            let qc = self.quic.clone().ok_or_else(|| anyhow::anyhow!("QUIC disabled"))?;
            let bind: std::net::SocketAddr = if addr.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
            let mut ep = quinn::Endpoint::client(bind)?;
            ep.set_default_client_config(qc);
            let conn = tokio::time::timeout(Duration::from_secs(8), ep.connect(addr, &self.host)?)
                .await
                .map_err(|_| anyhow::anyhow!("QUIC handshake timed out (UDP blocked?)"))??;
            let (send, recv) = tokio::time::timeout(Duration::from_secs(8), conn.open_bi())
                .await
                .map_err(|_| anyhow::anyhow!("QUIC stream open timed out"))??;
            Ok(spawn_io(recv, send, Some(conn), "quic"))
        } else {
            let tcp = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(addr))
                .await
                .map_err(|_| anyhow::anyhow!("TCP connect timed out"))??;
            tcp.set_nodelay(true)?;
            let name = rustls::pki_types::ServerName::try_from(self.host.clone())?;
            let tls =
                tokio::time::timeout(Duration::from_secs(10), tokio_rustls::TlsConnector::from(self.tls.clone()).connect(name, tcp))
                    .await
                    .map_err(|_| anyhow::anyhow!("TLS handshake timed out"))??;
            let (r, w) = tokio::io::split(tls);
            Ok(spawn_io(r, w, None, "tcp"))
        }
    }
}

/// One transmission in progress on a fed stream.
struct Tx {
    tx: u64,
    start_us: u64,
    next_us: u64,
    sent_any: bool,
    disc: bool,
    /// The newest packet, held back so the last one can carry TX_END.
    held: Option<(Vec<u8>, u64, u8)>,
}

struct Feed {
    ch: u16,
    stream: StreamId,
    label: String,
    enc: OpusEncoder,
    seq: u32,
    tx: Option<Tx>,
    listeners: u64,
}

impl Feed {
    fn emit(&mut self, link: &Link, pkt: Vec<u8>, us: u64, flags: u8) -> anyhow::Result<()> {
        let t = self.tx.as_mut().expect("emit inside a transmission");
        if let Some((hp, hus, hflags)) = t.held.replace((pkt, us, flags)) {
            link.send_audio(p::audio_payload(self.ch, self.seq, hus, hflags, &hp))?;
            self.seq = self.seq.wrapping_add(1);
        }
        Ok(())
    }

    fn open(
        &mut self,
        link: &Link,
        chan: &ChannelMeta,
        tx: u64,
        at_us: u64,
        level: f32,
        noise: f32,
    ) -> anyhow::Result<()> {
        if self.tx.is_some() {
            self.close(link, at_us, None)?;
        }
        let _ = self.enc.flush();
        let mut sq = json!({"ch": self.ch, "tx": tx, "open": true, "capture_us": at_us, "freq_hz": chan.freq_hz});
        if let Some(k) = &chan.atc_frequency {
            sq["frequency"] = json!(k);
        }
        if let Some(v) = finite(level) {
            sq["signal_dbfs"] = json!(v);
        }
        if let Some(v) = finite(noise) {
            sq["noise_dbfs"] = json!(v);
        }
        link.send_json(p::SQUELCH, &sq)?;
        self.tx = Some(Tx { tx, start_us: at_us, next_us: at_us, sent_any: false, disc: false, held: None });
        Ok(())
    }

    fn audio(&mut self, link: &Link, samples: &[f32], at_us: u64, discontinuity: bool) -> anyhow::Result<()> {
        let Some(t) = self.tx.as_mut() else { return Ok(()) };
        if discontinuity {
            t.disc = true;
        }
        let base = if self.enc.pending() == 0 { at_us } else { t.next_us };
        let packets = self.enc.push(samples);
        let n = packets.len() as u64;
        for (i, pkt) in packets.into_iter().enumerate() {
            let t = self.tx.as_mut().unwrap();
            let mut flags = 0;
            if !t.sent_any {
                flags |= p::FLAG_TX_START;
                t.sent_any = true;
            }
            if std::mem::take(&mut t.disc) {
                flags |= p::FLAG_DISCONTINUITY;
            }
            self.emit(link, pkt, base + i as u64 * 20_000, flags)?;
        }
        if let Some(t) = self.tx.as_mut() {
            t.next_us = base + n * 20_000;
        }
        Ok(())
    }

    fn close(&mut self, link: &Link, at_us: u64, duration_s: Option<f64>) -> anyhow::Result<()> {
        let Some(t) = self.tx.as_ref() else { return Ok(()) };
        let next_us = t.next_us;
        if let Some(pkt) = self.enc.flush() {
            let t = self.tx.as_mut().unwrap();
            let mut flags = if t.sent_any { 0 } else { p::FLAG_TX_START };
            if std::mem::take(&mut t.disc) {
                flags |= p::FLAG_DISCONTINUITY;
            }
            t.sent_any = true;
            self.emit(link, pkt, next_us, flags)?;
        }
        let t = self.tx.take().unwrap();
        if let Some((pkt, us, flags)) = t.held {
            link.send_audio(p::audio_payload(self.ch, self.seq, us, flags | p::FLAG_TX_END, &pkt))?;
            self.seq = self.seq.wrapping_add(1);
        }
        let duration_ms =
            duration_s.map(|d| (d * 1000.0).round() as u64).unwrap_or_else(|| at_us.saturating_sub(t.start_us) / 1000);
        link.send_json(
            p::SQUELCH,
            &json!({"ch": self.ch, "tx": t.tx, "open": false, "capture_us": at_us, "duration_ms": duration_ms}),
        )?;
        Ok(())
    }
}

/// OPEN payloads for the streams to feed, with their channel numbers.
fn plan_feeds(s: &AtcFeedSettings, chans: &[Arc<ChannelMeta>], bitrate: u32) -> Vec<(StreamId, Value)> {
    let streams = parse_streams(&s.streams).unwrap_or_default();
    let wanted: Vec<StreamId> =
        if streams.is_empty() { chans.iter().map(|c| StreamId::Channel(c.freq_hz)).collect() } else { streams };
    let receiver = |c: Option<&ChannelMeta>| {
        let mut r = s.receiver.clone().filter(|v| v.is_object()).unwrap_or_else(|| json!({}));
        if let Some(c) = c {
            r["modulation"] = json!(c.modulation.to_string());
            r["bandwidth_hz"] = json!(c.bandwidth_hz.round() as u64);
            r["squelch"] = json!(match c.squelch {
                xng_mode_airband::squelch::SquelchMode::Auto { .. } => "auto",
                xng_mode_airband::squelch::SquelchMode::Manual { .. } => "manual",
                xng_mode_airband::squelch::SquelchMode::Off => "off",
            });
            if r.get("type").is_none() {
                if let Some(sdr) = &c.sdr {
                    r["type"] = json!(sdr.split([':', ',', '=']).next().unwrap_or(sdr));
                }
            }
        }
        r
    };
    let codec = json!({"name": "opus", "sample_rate": crate::airband::hub::AUDIO_RATE, "channels": 1, "frame_ms": 20, "bitrate": bitrate});
    let freq_ref = |c: &ChannelMeta| {
        let mut f = json!({"freq_hz": c.freq_hz});
        if let Some(k) = &c.atc_frequency {
            f["id"] = json!(k);
        }
        f
    };
    let mut out = Vec::new();
    for (i, st) in wanted.iter().enumerate() {
        let ch = (i + 1) as u16;
        let open = match st {
            StreamId::Channel(f) => {
                let Some(c) = chans.iter().find(|c| c.freq_hz == *f) else {
                    tracing::warn!("atc feed: no airband channel at {:.3} MHz to feed", *f as f64 / 1e6);
                    continue;
                };
                json!({"ch": ch, "key": clean_key(&c.key), "mode": "single", "frequencies": [freq_ref(c)],
                       "label": c.display(), "codec": codec, "receiver": receiver(Some(c))})
            }
            StreamId::Scan => {
                let locked: Vec<u64> =
                    hub().scan.lock().unwrap().as_ref().map(|s| s.locked_out.clone()).unwrap_or_default();
                let fl: Vec<Value> =
                    chans.iter().filter(|c| !locked.contains(&c.freq_hz)).take(16).map(|c| freq_ref(c)).collect();
                if chans.len() > 16 {
                    tracing::warn!("atc feed: a scan feed carries at most 16 frequencies; the first 16 are declared");
                }
                let sdr = chans.first().and_then(|c| c.sdr.clone()).unwrap_or_else(|| "xng".into());
                json!({"ch": ch, "key": clean_key(&format!("{sdr}:scan")), "mode": "scan", "frequencies": fl,
                       "label": s.station_name.clone().map(|n| format!("{n} scan")).unwrap_or_else(|| "Scanner".into()),
                       "codec": codec, "receiver": receiver(chans.first().map(|c| c.as_ref()))})
            }
            StreamId::Mix => {
                tracing::warn!(
                    "atc feed: the mix stream cannot be fed (listeners need the active frequency); use scan"
                );
                continue;
            }
        };
        out.push((*st, open));
    }
    out
}

pub async fn run(
    bus_rx: broadcast::Receiver<Arc<Message>>,
    s: AtcFeedSettings,
    station: StationInfo,
) -> std::io::Result<()> {
    let closed = crate::airband::bus_closed(bus_rx);
    tokio::pin!(closed);
    let creds = s.credentials.clone().unwrap_or_else(atc_auth::default_path);
    let tokens = match &s.token {
        Some(t) => TokenSource::Fixed(t.clone()),
        None => TokenSource::Saved(creds.clone()),
    };
    let feeder_id = match atc_auth::feeder_id(&creds) {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("atc feed: cannot keep a feeder id in {}: {e}", creds.display());
            return Ok(());
        }
    };
    let dialer = match Dialer::new(&s) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("atc feed: {e}");
            return Ok(());
        }
    };
    // Outputs start before the decoders register their channels.
    let chans = loop {
        let c = hub().channels();
        if !c.is_empty() {
            break c;
        }
        tokio::select! {
            _ = &mut closed => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    };
    let mut backoff = Duration::from_secs(1);
    let mut quic_failures = 0u32;
    loop {
        let use_quic = match s.transport.as_str() {
            "quic" => true,
            "tcp" => false,
            _ => quic_failures < 2,
        };
        let started = Instant::now();
        let mut opened = false;
        let r = session(&s, &dialer, use_quic, &tokens, &feeder_id, &station, &chans, &mut closed, &mut opened).await;
        match r {
            Ok(true) => break, // bus closed
            Ok(false) => {}
            Err(e) => {
                // A QUIC attempt that never got its feeds opened counts
                // toward falling back to TCP, however long it took to fail.
                if use_quic && !opened {
                    quic_failures += 1;
                }
                let fatal = e.downcast_ref::<Fatal>().is_some();
                tracing::warn!(
                    "atc feed {}: {e:#}; {}",
                    s.server,
                    if fatal { "not retrying".to_string() } else { format!("retrying in {}s", backoff.as_secs()) }
                );
                if fatal {
                    return Ok(());
                }
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        let jitter = Duration::from_millis((now_us() / 1000) % 1000);
        tokio::select! {
            _ = &mut closed => break,
            _ = tokio::time::sleep(backoff + jitter) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
    Ok(())
}

/// An error retrying will not fix (rejected sign-in, banned feeder).
#[derive(Debug)]
struct Fatal(String);
impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Fatal {}

fn server_error(payload: &[u8]) -> anyhow::Error {
    let v: Value = serde_json::from_slice(payload).unwrap_or_default();
    let code = v.get("code").and_then(|c| c.as_str()).unwrap_or("error").to_string();
    let msg = v.get("message").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let text = format!("server error {code}: {msg}");
    match code.as_str() {
        "unauthorized" | "invalid_token" | "forbidden" | "feeder_id_taken" | "banned" => Fatal(text).into(),
        _ => anyhow::anyhow!(text),
    }
}

/// One connection's life. `Ok(true)` when the bus closed (stop),
/// `Ok(false)` when the server ended it cleanly (reconnect).
#[allow(clippy::too_many_arguments)]
async fn session(
    s: &AtcFeedSettings,
    dialer: &Dialer,
    quic: bool,
    tokens: &TokenSource,
    feeder_id: &str,
    station: &StationInfo,
    chans: &[Arc<ChannelMeta>],
    closed: &mut std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
    opened: &mut bool,
) -> anyhow::Result<bool> {
    let token = tokio::select! {
        _ = closed.as_mut() => return Ok(true),
        t = tokens.get() => t,
    };
    let mut token = token.map_err(|e| {
        if matches!(tokens, TokenSource::Saved(_)) && e.to_string().contains("xng atc login") {
            anyhow::Error::from(Fatal(e.to_string()))
        } else {
            e
        }
    })?;
    let link = tokio::select! {
        _ = closed.as_mut() => return Ok(true),
        l = dialer.dial(quic) => l,
    };
    let mut link = link.map_err(|e| e.context(if quic { "QUIC" } else { "TLS/TCP" }))?;

    let mut st = json!({"name": station.name, "location_precision_m": s.location_precision_m});
    if let (Some(lat), Some(lon)) = (station.lat, station.lon) {
        st["lat"] = json!(p::round_coord(lat, s.location_precision_m).clamp(-90.0, 90.0));
        st["lon"] = json!(p::round_coord(lon, s.location_precision_m).clamp(-180.0, 180.0));
    }
    if let Some(a) = station.alt_m {
        st["alt_m"] = json!(a.round());
    }
    let platform = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
    let mut hello = json!({"v": p::VERSION, "token": token.token, "feeder_id": feeder_id, "station": st,
                "client": {"name": "xng", "version": env!("CARGO_PKG_VERSION"), "platform": platform}});
    if s.hide_username {
        hello["hide_username"] = json!(true);
    }
    link.send_json(p::HELLO, &hello)?;
    let (kind, payload) = link.recv(Duration::from_secs(10)).await?;
    let welcome: Value = match kind {
        p::WELCOME => serde_json::from_slice(&payload)?,
        p::ERROR => return Err(server_error(&payload)),
        k => anyhow::bail!("expected WELCOME, got {}", p::type_name(k)),
    };
    let heartbeat =
        Duration::from_secs(welcome.get("heartbeat_s").and_then(|v| v.as_u64()).unwrap_or(15).clamp(5, 120));
    let max_bitrate = welcome.pointer("/limits/max_bitrate").and_then(|v| v.as_u64()).unwrap_or(64_000) as u32;
    let max_channels = welcome.pointer("/limits/max_channels").and_then(|v| v.as_u64()).unwrap_or(32) as usize;
    let bitrate = s.bitrate.min(max_bitrate).max(6_000);
    let user = welcome.pointer("/user/username").and_then(|v| v.as_str()).unwrap_or("?").to_string();

    // Declare the feeds.
    let mut plan = plan_feeds(s, chans, bitrate);
    if plan.len() > max_channels {
        tracing::warn!("atc feed: server allows {max_channels} channels; feeding the first {max_channels}");
        plan.truncate(max_channels);
    }
    for (_, open) in &plan {
        link.send_json(p::OPEN, open)?;
    }
    let mut feeds: HashMap<StreamId, Feed> = HashMap::new();
    let mut waiting: HashMap<u16, (StreamId, String)> = plan
        .iter()
        .map(|(st, o)| (o["ch"].as_u64().unwrap_or(0) as u16, (*st, o["label"].as_str().unwrap_or("").to_string())))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !waiting.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        let (kind, payload) = link.recv(left.max(Duration::from_millis(1))).await?;
        match kind {
            p::OPENED => {
                let v: Value = serde_json::from_slice(&payload)?;
                let ch = v["ch"].as_u64().unwrap_or(0) as u16;
                let Some((stream, label)) = waiting.remove(&ch) else { continue };
                if v["ok"].as_bool() == Some(true) {
                    let enc = OpusEncoder::new(bitrate)?;
                    tracing::info!(
                        "atc feed: {label} accepted as {} ({})",
                        v["feed_id"].as_str().unwrap_or("?"),
                        v["frequencies"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|f| f.as_str()).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default()
                    );
                    feeds.insert(stream, Feed { ch, stream, label, enc, seq: 0, tx: None, listeners: 0 });
                } else {
                    let cands = v["candidates"]
                        .as_array()
                        .map(|a| {
                            a.iter().filter_map(|c| c["id"].as_str().map(str::to_string)).collect::<Vec<_>>().join(", ")
                        })
                        .unwrap_or_default();
                    tracing::warn!(
                        "atc feed: {label} rejected: {} {}{}",
                        v["code"].as_str().unwrap_or("?"),
                        v["message"].as_str().unwrap_or(""),
                        if cands.is_empty() {
                            String::new()
                        } else {
                            format!(" (candidates: {cands}; set --atc-frequency)")
                        }
                    );
                }
            }
            p::ERROR => return Err(server_error(&payload)),
            _ => {}
        }
    }
    anyhow::ensure!(!feeds.is_empty(), "the server accepted none of the declared feeds");
    *opened = true;
    tracing::info!(
        "atc feed: connected to {} over {} as {user}, feeding {} stream(s) at {} kbit/s",
        s.server,
        link.transport,
        feeds.len(),
        bitrate / 1000
    );

    let mut rx = hub().subscribe();
    let mut stats_tick = tokio::time::interval(Duration::from_secs(10));
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_tick = tokio::time::interval(heartbeat);
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reauth_pending = false;
    // A token refresh in flight; audio keeps flowing while it runs.
    let mut refresh: Option<tokio::task::JoinHandle<anyhow::Result<AccessToken>>> = None;

    loop {
        tokio::select! {
            _ = closed.as_mut() => {
                let now = now_us();
                for f in feeds.values_mut() {
                    let _ = f.close(&link, now, None);
                }
                for f in feeds.values() {
                    let _ = link.send_json(p::CLOSE, &json!({"ch": f.ch, "reason": "shutdown"}));
                }
                // Let the writer drain before the link drops.
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Ok(true);
            }
            fr = link.incoming.recv() => {
                let Some((kind, payload)) = fr else { anyhow::bail!("connection closed by the server") };
                match kind {
                    p::NOTICE => {
                        let v: Value = serde_json::from_slice(&payload).unwrap_or_default();
                        match v["code"].as_str().unwrap_or("") {
                            "token_expiring" => reauth_pending = true,
                            "listeners" => {
                                let ch = v["ch"].as_u64().unwrap_or(0) as u16;
                                let n = v["count"].as_u64().unwrap_or(0);
                                if let Some(f) = feeds.values_mut().find(|f| f.ch == ch) {
                                    if f.listeners != n {
                                        tracing::info!("atc feed: {} has {n} listener(s)", f.label);
                                    }
                                    f.listeners = n;
                                }
                            }
                            "slow_down" => {
                                tracing::warn!("atc feed: server asked to slow down: {v}");
                            }
                            "maintenance" => {
                                let after = v["reconnect_after_s"].as_u64().unwrap_or(30);
                                tracing::info!("atc feed: server maintenance, reconnecting in {after}s");
                                drop(link);
                                tokio::select! {
                                    _ = closed.as_mut() => return Ok(true),
                                    _ = tokio::time::sleep(Duration::from_secs(after.min(600))) => return Ok(false),
                                }
                            }
                            other => tracing::info!("atc feed: notice {other}: {}", v["message"].as_str().unwrap_or("")),
                        }
                    }
                    p::CLOSE => {
                        let v: Value = serde_json::from_slice(&payload).unwrap_or_default();
                        let ch = v["ch"].as_u64().unwrap_or(0) as u16;
                        if let Some(k) = feeds.iter().find(|(_, f)| f.ch == ch).map(|(k, _)| *k) {
                            let f = feeds.remove(&k).unwrap();
                            tracing::warn!("atc feed: server closed {}: {}", f.label, v["reason"].as_str().unwrap_or(""));
                        }
                        if feeds.is_empty() {
                            anyhow::bail!("the server closed every feed");
                        }
                    }
                    p::ERROR => {
                        let v: Value = serde_json::from_slice(&payload).unwrap_or_default();
                        if v["fatal"].as_bool() == Some(true) {
                            return Err(server_error(&payload));
                        }
                        tracing::warn!("atc feed: server: {} {}", v["code"].as_str().unwrap_or(""), v["message"].as_str().unwrap_or(""));
                    }
                    _ => {}
                }
            }
            ev = rx.recv() => {
                let ev = match ev {
                    Ok(ev) => ev,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("atc feed lagged, dropped {n} audio events");
                        for f in feeds.values_mut() {
                            if let Some(t) = f.tx.as_mut() { t.disc = true; }
                        }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(true),
                };
                let Some(f) = feeds.get_mut(&ev.stream()) else { continue };
                match &ev {
                    AudioEvent::Open { chan, tx, at, level_dbfs, noise_dbfs, .. } => {
                        // Scan opens carry no levels; use the channel's.
                        let (lvl, nf) = if level_dbfs.is_finite() {
                            (*level_dbfs, *noise_dbfs)
                        } else {
                            hub().status().iter().find(|c| c.freq_hz == chan.freq_hz).map(|c| (c.level_dbfs, c.noise_dbfs)).unwrap_or((*level_dbfs, *noise_dbfs))
                        };
                        f.open(&link, chan, *tx, at.timestamp_micros().max(0) as u64, lvl, nf)?;
                    }
                    AudioEvent::Audio(a) => f.audio(&link, &a.samples, a.at.timestamp_micros().max(0) as u64, a.discontinuity)?,
                    AudioEvent::Close { at, duration_s, .. } => {
                        let d = (*duration_s > 0.0).then_some(*duration_s);
                        f.close(&link, at.timestamp_micros().max(0) as u64, d)?;
                    }
                    AudioEvent::Status(_) => {}
                }
                let _ = f.stream;
            }
            _ = stats_tick.tick() => {
                let status = hub().status();
                for f in feeds.values() {
                    let StreamId::Channel(freq) = f.stream else { continue };
                    let Some(c) = status.iter().find(|c| c.freq_hz == freq) else { continue };
                    let mut v = json!({"ch": f.ch, "tx_count": c.transmissions, "airtime_ms": (c.airtime_s * 1000.0).round() as u64});
                    if let Some(n) = finite(c.noise_dbfs) { v["noise_dbfs"] = json!(n); }
                    if let Some(q) = finite(c.squelch_dbfs) { v["squelch_dbfs"] = json!(q); }
                    link.send_json(p::STATS, &v)?;
                }
                if token.remaining_s().is_some_and(|r| r < 600) {
                    reauth_pending = true;
                }
                if reauth_pending && refresh.is_none() {
                    if let TokenSource::Saved(path) = tokens {
                        let path = path.clone();
                        refresh = Some(tokio::task::spawn_blocking(move || atc_auth::refresh(&path)));
                    }
                }
            }
            r = async { refresh.as_mut().expect("guarded").await }, if refresh.is_some() => {
                refresh = None;
                match r.map_err(anyhow::Error::from).and_then(|r| r) {
                    Ok(t) => {
                        token = t;
                        link.send_json(p::REAUTH, &json!({"token": token.token}))?;
                        reauth_pending = false;
                        tracing::debug!("atc feed: access token refreshed");
                    }
                    Err(e) => tracing::warn!("atc feed: token refresh failed: {e:#}"),
                }
            }
            _ = ping_tick.tick() => {
                let idle = link.last_rx.lock().unwrap().elapsed();
                anyhow::ensure!(idle < heartbeat * 3, "no word from the server for {}s", idle.as_secs());
                link.send(p::PING, &now_us().to_be_bytes())?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_cleaned() {
        assert_eq!(clean_key("driver=rtlsdr,serial=00000001:118300000"), "driver-rtlsdr-serial-00000001:118300000");
        assert!(p::valid_key(&clean_key(&"x".repeat(100))));
        assert_eq!(clean_key(""), "0");
    }
}
