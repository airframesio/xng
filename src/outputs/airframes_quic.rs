//! Airframes feed protocol uplink (ALPN `airframes-feed/1`): one QUIC
//! connection (TLS over TCP when UDP is blocked) to the Airframes feed
//! gateway, with one stream per feed.
//!
//! Every feed the per-mode router resolves (ACARS, VDL2, HFDL, Aero L/C,
//! Iridium, AIS; several of each) becomes its own stream on the one
//! connection, carrying the same native JSON the per-port ingests take. The
//! gateway measures each feed's health and the link's latency in both
//! directions, and can ask read-only diagnostics (`status`, `clock`, `net`).
//!
//! While the uplink is not connected (or a feed is refused) the router falls
//! back to the per-port UDP/TCP/HTTP feeds, so nothing is lost by turning it
//! on. Protocol: aggregation-server `gateway/docs/PROTOCOL.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::airband::atc_auth;
use crate::outputs::asf2_quic::TrustMode;
use xng_types::Mode;

pub const ALPN: &[u8] = b"airframes-feed/1";
pub const DEFAULT_ENDPOINT: &str = "feed.airframes.io:5580";
const VERSION: u64 = 1;
const MAX_PAYLOAD: usize = 0xFFFF;
/// Largest message one DATA frame carries (fits in a TCP STREAM frame too).
pub const MAX_MESSAGE: usize = MAX_PAYLOAD - 8 - 6;

// Frame types.
const HELLO: u8 = 0x01;
const WELCOME: u8 = 0x02;
const OPEN: u8 = 0x03;
const OPENED: u8 = 0x04;
const STATS: u8 = 0x09;
const REAUTH: u8 = 0x0A;
const NOTICE: u8 = 0x0B;
const PING: u8 = 0x0E;
const PONG: u8 = 0x0F;
const DATA: u8 = 0x10;
const COMMAND: u8 = 0x11;
const RESULT: u8 = 0x12;
const LATENCY: u8 = 0x13;
const STREAM: u8 = 0x20;
const ERROR: u8 = 0x7F;

/// Commands this client answers (all read only).
const COMMANDS: &[&str] = &["status", "clock", "net"];

/// Messages queued per feed stream before new ones are dropped.
const FEED_QUEUE: usize = 2048;

/// Uplink settings.
#[derive(Clone, Debug)]
pub struct QuicSettings {
    /// `host:port` of the feed gateway.
    pub endpoint: String,
    /// `auto` (QUIC, then TLS over TCP after 2 QUIC failures), `quic` or `tcp`.
    pub transport: String,
    /// PEM file to trust instead of system roots (self-hosted/dev gateway).
    pub ca: Option<PathBuf>,
    /// Lab only: no certificate verification.
    pub insecure: bool,
    /// Sign-in file (`xng atc login`); its feeder id is the instance id.
    /// Without a saved sign-in the uplink connects anonymously.
    pub credentials: Option<PathBuf>,
    /// Added to the instance id so two xng processes on one host (each
    /// feeding different stations or modes) don't replace each other's
    /// session. Set from the routing table.
    pub instance_tag: String,
}

impl Default for QuicSettings {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            transport: "auto".to_string(),
            ca: None,
            insecure: false,
            credentials: None,
            instance_tag: String::new(),
        }
    }
}

impl QuicSettings {
    /// Settings for an Airframes `transport` value, or `None` for the
    /// per-port feeds (`ports`, the default). `quic`: QUIC, then TLS over
    /// TCP when UDP is blocked; `quic-only`; `tls`: TLS over TCP only.
    pub fn for_transport(
        transport: Option<&str>,
        endpoint: Option<String>,
        ca: Option<PathBuf>,
        insecure: bool,
    ) -> anyhow::Result<Option<Self>> {
        let t = match transport.unwrap_or("ports") {
            "ports" | "" => return Ok(None),
            "quic" => "auto",
            "quic-only" => "quic",
            "tls" => "tcp",
            other => anyhow::bail!(
                "airframes transport must be ports, quic, quic-only or tls (got {other:?})"
            ),
        };
        anyhow::ensure!(
            !(insecure && ca.is_some()),
            "airframes: quic-ca and quic-insecure are mutually exclusive"
        );
        Ok(Some(Self {
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
            transport: t.to_string(),
            ca,
            insecure,
            credentials: None,
            instance_tag: String::new(),
        }))
    }
}

/// xng's own normalized message JSON, for modes without a native Airframes
/// ingest format: `{"v":1,"station_id":..,"message":<Message>}`.
pub const XNG_FORMAT: &str = "xng";

/// Whether a mode may go to the gateway at all. ADS-B and UAT never go to
/// Airframes (they belong on ADS-B networks), airband audio goes to the ATC
/// ingest, sondes to their own intake.
pub fn gateway_eligible(mode: Mode) -> bool {
    !matches!(
        mode,
        Mode::Adsb | Mode::Uat | Mode::Airband | Mode::Sonde | Mode::Extern | Mode::Time
    )
}

/// The format to send a mode in given the gateway's list (see
/// [`Uplink::wire_format`]).
pub fn choose_format(mode: Mode, ingests: Option<&[(String, String)]>) -> Option<&'static str> {
    if !gateway_eligible(mode) {
        return None;
    }
    let native = format_for(mode);
    let Some(list) = ingests else { return native };
    let listed = |f: &str| list.iter().any(|(lf, lm)| lf == f && lm == mode.as_str());
    match native {
        Some(f) if listed(f) => Some(f),
        _ if listed(XNG_FORMAT) => Some(XNG_FORMAT),
        _ => None,
    }
}

/// The payload of an `xng` format DATA frame.
pub fn xng_payload(msg: &xng_types::Message, station_id: &str) -> Option<Vec<u8>> {
    serde_json::to_vec(&json!({ "v": 1, "station_id": station_id, "message": msg })).ok()
}

/// The native wire format name for a mode, or `None` when that mode has no
/// native Airframes ingest format (it can still go as [`XNG_FORMAT`] when
/// the gateway lists it).
pub fn format_for(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::AcarsPoa => Some("acarsdec"),
        Mode::Vdl2 => Some("dumpvdl2"),
        Mode::Hfdl => Some("dumphfdl"),
        Mode::AeroL => Some("jaero-l"),
        Mode::AeroC => Some("jaero-c"),
        Mode::Iridium => Some("iridium-toolkit"),
        Mode::Ais => Some("aiscatcher"),
        _ => None,
    }
}

// ---- framing ----

fn frame(t: u8, payload: &[u8]) -> Vec<u8> {
    let n = payload.len().min(MAX_PAYLOAD);
    let mut b = Vec::with_capacity(4 + n);
    b.extend_from_slice(&[t, (n >> 16) as u8, (n >> 8) as u8, n as u8]);
    b.extend_from_slice(&payload[..n]);
    b
}

fn stream_frame(id: u16, t: u8, payload: &[u8]) -> Vec<u8> {
    let mut inner = id.to_be_bytes().to_vec();
    inner.extend_from_slice(&frame(t, payload));
    frame(STREAM, &inner)
}

fn data_payload(capture_us: u64, msg: &[u8]) -> Vec<u8> {
    let mut p = capture_us.to_be_bytes().to_vec();
    p.extend_from_slice(msg);
    p
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h).await?;
    let n = (h[1] as usize) << 16 | (h[2] as usize) << 8 | h[3] as usize;
    if n > MAX_PAYLOAD {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut p = vec![0u8; n];
    r.read_exact(&mut p).await?;
    Ok((h[0], p))
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// PONG for a PING: t1 echoed, t2 when it arrived, t3 now.
fn pong(ping: &[u8], t2: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(24);
    p.extend_from_slice(ping.get(..8).unwrap_or(&[0; 8]));
    p.extend_from_slice(&t2.to_be_bytes());
    p.extend_from_slice(&now_us().to_be_bytes());
    p
}

/// Whether the system clock is synchronized, for one-way latency. Linux:
/// `timedatectl`; elsewhere unknown. Blocking: run off the async workers.
fn read_clock_state() -> Value {
    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = std::process::Command::new("timedatectl")
            .args(["show", "-p", "NTPSynchronized", "--value"])
            .output()
        {
            if out.status.success() {
                let synced = String::from_utf8_lossy(&out.stdout).trim() == "yes";
                return json!({ "source": "ntp", "synced": synced });
            }
        }
    }
    unknown_clock()
}

fn unknown_clock() -> Value {
    json!({ "source": "unknown", "synced": false })
}

/// Refreshes the cached clock state now and every 5 minutes, each check
/// bounded, so neither HELLO nor STATS waits on a slow `timedatectl`.
fn spawn_clock_watch(shared: Arc<Shared>) {
    tokio::spawn(async move {
        loop {
            let check = tokio::task::spawn_blocking(read_clock_state);
            if let Ok(Ok(v)) = tokio::time::timeout(Duration::from_secs(5), check).await {
                *shared.clock.lock().unwrap() = v;
            }
            // Ends with the last Uplink handle.
            if Arc::strong_count(&shared) <= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });
}

// ---- public handle ----

/// Feed state: waiting for OPENED, accepted, or refused (use the per-port
/// path for it).
const PENDING: u8 = 0;
const OPEN_OK: u8 = 1;
const REFUSED: u8 = 2;

/// Messages a gateway-only feed holds while the gateway decides on it.
const HOLD_MAX: usize = 256;
/// How long opening one feed may take before it is given up (and retried).
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// What one feed carries.
#[derive(Clone, Debug)]
pub struct FeedSpec {
    pub key: String,
    pub mode: Mode,
    /// From [`Uplink::wire_format`].
    pub format: &'static str,
    pub station_ident: String,
    pub station_uuid: Option<String>,
    pub sdr_id: Option<String>,
}

/// A request to open a feed (the session opens it; messages go straight to
/// the feed's writer).
struct Item {
    spec: FeedSpec,
}

/// A feed's frame queue and, on TCP, its stream id.
type FeedWriter = (mpsc::Sender<Vec<u8>>, Option<u16>);

/// One feed, shared by `send` (which writes its messages) and the session
/// (which opens it and reads OPENED).
struct Feed {
    state: AtomicU8,
    /// The feed's frames: its own QUIC stream, or the TCP connection with
    /// the feed's stream id. Set once the OPEN is sent.
    writer: Mutex<Option<FeedWriter>>,
    /// DATA payloads of a gateway-only feed received before OPENED (there
    /// is no per-port path to hand them to meanwhile).
    held: Mutex<Vec<Vec<u8>>>,
    decoded: AtomicU64,
    sent: AtomicU64,
    dropped: AtomicU64,
    last_us: AtomicU64,
}

impl Feed {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(PENDING),
            writer: Mutex::new(None),
            held: Mutex::new(Vec::new()),
            decoded: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            last_us: AtomicU64::new(0),
        }
    }

    /// Hands one DATA payload to the writer. Err(closed): the writer is gone.
    fn write(&self, payload: &[u8]) -> Result<bool, ()> {
        let w = self.writer.lock().unwrap();
        let Some((tx, tcp_id)) = w.as_ref() else { return Ok(false) };
        let fr = match tcp_id {
            Some(id) => stream_frame(*id, DATA, payload),
            None => frame(DATA, payload),
        };
        match tx.try_send(fr) {
            Ok(()) => {
                self.sent.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
        }
    }

    fn json(&self, key: &str) -> Value {
        json!({
            "key": key,
            "decoded": self.decoded.load(Ordering::Relaxed),
            "sent": self.sent.load(Ordering::Relaxed),
            "dropped": self.dropped.load(Ordering::Relaxed),
            "last_decode_us": self.last_us.load(Ordering::Relaxed),
            "open": self.state.load(Ordering::Relaxed) == OPEN_OK,
        })
    }
}

/// Latest link measurement from the gateway.
#[derive(Clone, Debug, Default)]
pub struct LinkStatus {
    pub connected: bool,
    pub transport: &'static str,
    pub session_id: String,
    pub rtt_ms: Option<f64>,
    pub up_ms: Option<f64>,
    pub down_ms: Option<f64>,
    pub trusted: bool,
    pub reconnects: u64,
}

/// Handle to the uplink task.
pub struct Uplink {
    tx: mpsc::Sender<Item>,
    shared: Arc<Shared>,
}

struct Shared {
    connected: AtomicBool,
    /// Feed key -> feed, so `send` writes to it or falls back.
    feeds: Mutex<HashMap<String, Arc<Feed>>>,
    link: Mutex<LinkStatus>,
    /// (format, mode) pairs the gateway listed in WELCOME; `None` before the
    /// first WELCOME or from a gateway that does not list them.
    ingests: Mutex<Option<Vec<(String, String)>>>,
    /// Latest clock check (see [`spawn_clock_watch`]).
    clock: Mutex<Value>,
    started: Instant,
}

impl Shared {
    /// Forgets `feed` if it is still the current one for `key` (a later
    /// replacement stays).
    fn forget(&self, key: &str, feed: &Arc<Feed>) {
        let mut feeds = self.feeds.lock().unwrap();
        if feeds.get(key).is_some_and(|f| Arc::ptr_eq(f, feed)) {
            feeds.remove(key);
        }
    }

    fn clock(&self) -> Value {
        self.clock.lock().unwrap().clone()
    }
}

impl Uplink {
    /// Start the uplink task.
    pub fn spawn(settings: QuicSettings) -> Self {
        let (tx, rx) = mpsc::channel(1024);
        let shared = Arc::new(Shared {
            connected: AtomicBool::new(false),
            feeds: Mutex::new(HashMap::new()),
            link: Mutex::new(LinkStatus::default()),
            ingests: Mutex::new(None),
            clock: Mutex::new(unknown_clock()),
            started: Instant::now(),
        });
        spawn_clock_watch(shared.clone());
        tokio::spawn(run(settings, rx, shared.clone()));
        Self { tx, shared }
    }

    /// Writes one message to its feed. Returns true once the feed's writer
    /// took it (or, with `hold`, the feed holds it until the gateway opens
    /// it). False: send it the per-port way instead (not connected, feed not
    /// open yet, refused, closed, or its queue full).
    ///
    /// `hold` is for gateway-only modes, which have no per-port path: their
    /// first messages wait (up to [`HOLD_MAX`]) for the gateway to accept
    /// the feed instead of being dropped.
    pub fn send(&self, spec: &FeedSpec, capture_us: u64, msg: Vec<u8>, hold: bool) -> bool {
        if !self.shared.connected.load(Ordering::Relaxed) || msg.len() > MAX_MESSAGE {
            return false;
        }
        let feed = {
            let mut feeds = self.shared.feeds.lock().unwrap();
            match feeds.get(&spec.key) {
                Some(f) => f.clone(),
                None => {
                    let f = Arc::new(Feed::new());
                    if self.tx.try_send(Item { spec: spec.clone() }).is_err() {
                        return false;
                    }
                    feeds.insert(spec.key.clone(), f.clone());
                    f
                }
            }
        };
        feed.decoded.fetch_add(1, Ordering::Relaxed);
        feed.last_us.store(capture_us, Ordering::Relaxed);
        let payload = data_payload(capture_us, &msg);
        match feed.state.load(Ordering::Relaxed) {
            OPEN_OK => self.write_open(spec, &feed, &payload),
            PENDING if hold => {
                let mut held = feed.held.lock().unwrap();
                // OPENED may have arrived meanwhile (it flushes under this lock).
                if feed.state.load(Ordering::Relaxed) == OPEN_OK {
                    drop(held);
                    return self.write_open(spec, &feed, &payload);
                }
                if held.len() >= HOLD_MAX {
                    feed.dropped.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                held.push(payload);
                true
            }
            _ => false,
        }
    }

    fn write_open(&self, spec: &FeedSpec, feed: &Arc<Feed>, payload: &[u8]) -> bool {
        match feed.write(payload) {
            Ok(taken) => taken,
            Err(()) => {
                // Its stream ended: forget it, so the next message reopens it.
                self.shared.forget(&spec.key, feed);
                false
            }
        }
    }

    /// The format to send a mode in, from what the gateway lists: its native
    /// format, else the xng format, else `None` (the gateway does not take
    /// it; use the other outputs). A gateway that lists nothing takes the
    /// native formats.
    pub fn wire_format(&self, mode: Mode) -> Option<&'static str> {
        choose_format(mode, self.shared.ingests.lock().unwrap().as_deref())
    }

    /// The (format, mode) pairs the gateway last listed.
    pub fn ingests(&self) -> Option<Vec<(String, String)>> {
        self.shared.ingests.lock().unwrap().clone()
    }

    /// The latest link status.
    pub fn status(&self) -> LinkStatus {
        self.shared.link.lock().unwrap().clone()
    }
}

// ---- connection ----

/// Where control and feed frames go on a connection.
enum Link {
    Quic {
        conn: quinn::Connection,
        ctrl: mpsc::Sender<Vec<u8>>,
    },
    Tcp {
        out: mpsc::Sender<Vec<u8>>,
        next_id: u16,
        /// The connection's reader, stopped on close (a peer that never
        /// closes would otherwise keep it, and the socket, forever).
        reader: tokio::task::AbortHandle,
    },
}

struct Dialer {
    host: String,
    addr: String,
    tls: Arc<rustls::ClientConfig>,
    quic: quinn::ClientConfig,
}

impl Dialer {
    fn new(s: &QuicSettings) -> anyhow::Result<Self> {
        let trust = if s.insecure {
            TrustMode::Insecure
        } else if let Some(ca) = &s.ca {
            TrustMode::CaFile(ca.clone())
        } else {
            TrustMode::SystemRoots
        };
        let crypto = crate::outputs::asf2_quic::client_crypto(
            &trust,
            ALPN,
            "airframes quic (--airframes-quic-ca / --airframes-quic-insecure)",
        )?;
        let host = s
            .endpoint
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(&s.endpoint);
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let mut qc = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto.clone())?,
        ));
        let mut tc = quinn::TransportConfig::default();
        // Keepalives hold NAT bindings open on quiet feeds.
        tc.keep_alive_interval(Some(Duration::from_secs(10)));
        tc.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
        qc.transport_config(Arc::new(tc));
        Ok(Self {
            host,
            addr: s.endpoint.clone(),
            tls: Arc::new(crypto),
            quic: qc,
        })
    }
}

/// Frames read from the control stream (or the TCP connection).
type Incoming = mpsc::Receiver<(u8, Vec<u8>)>;

fn spawn_reader<R: AsyncRead + Unpin + Send + 'static>(
    mut r: R,
) -> (Incoming, tokio::task::AbortHandle) {
    let (tx, rx) = mpsc::channel(256);
    let task = tokio::spawn(async move {
        while let Ok(f) = read_frame(&mut r).await {
            if tx.send(f).await.is_err() {
                break;
            }
        }
    });
    (rx, task.abort_handle())
}

fn spawn_writer<W: AsyncWrite + Unpin + Send + 'static>(
    mut w: W,
    cap: usize,
) -> mpsc::Sender<Vec<u8>> {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(cap);
    tokio::spawn(async move {
        while let Some(b) = rx.recv().await {
            if w.write_all(&b).await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    tx
}

/// Connects to the gateway, trying each address the name resolves to (an
/// unreachable IPv6 address must not hide a working IPv4 one) within one
/// overall deadline. Certificates are checked against the host name.
async fn dial(d: &Dialer, quic: bool) -> anyhow::Result<(Link, Incoming)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(&d.addr).await?.collect();
    anyhow::ensure!(!addrs.is_empty(), "cannot resolve {}", d.addr);
    let mut last = None;
    for addr in addrs {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, dial_addr(d, quic, addr)).await {
            Ok(Ok(l)) => return Ok(l),
            Ok(Err(e)) => last = Some(e.context(addr.to_string())),
            Err(_) => last = Some(anyhow::anyhow!("{addr}: timed out")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("connect to {} timed out", d.addr)))
}

async fn dial_addr(
    d: &Dialer,
    quic: bool,
    addr: std::net::SocketAddr,
) -> anyhow::Result<(Link, Incoming)> {
    if quic {
        let bind: std::net::SocketAddr = if addr.is_ipv6() {
            "[::]:0".parse()?
        } else {
            "0.0.0.0:0".parse()?
        };
        let mut ep = quinn::Endpoint::client(bind)?;
        ep.set_default_client_config(d.quic.clone());
        let conn = tokio::time::timeout(Duration::from_secs(8), ep.connect(addr, &d.host)?)
            .await
            .map_err(|_| anyhow::anyhow!("QUIC handshake timed out (UDP blocked?)"))??;
        let (send, recv) = conn.open_bi().await?;
        let (rx, _) = spawn_reader(recv);
        let ctrl = spawn_writer(send, 256);
        Ok((Link::Quic { conn, ctrl }, rx))
    } else {
        let tcp = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        .map_err(|_| anyhow::anyhow!("TCP connect timed out"))??;
        tcp.set_nodelay(true)?;
        let name = rustls::pki_types::ServerName::try_from(d.host.clone())?;
        let tls = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_rustls::TlsConnector::from(d.tls.clone()).connect(name, tcp),
        )
        .await
        .map_err(|_| anyhow::anyhow!("TLS handshake timed out"))??;
        let (r, w) = tokio::io::split(tls);
        let (rx, reader) = spawn_reader(r);
        let out = spawn_writer(w, FEED_QUEUE * 4);
        Ok((Link::Tcp { out, next_id: 0, reader }, rx))
    }
}

impl Link {
    fn ctrl(&self) -> &mpsc::Sender<Vec<u8>> {
        match self {
            Link::Quic { ctrl, .. } => ctrl,
            Link::Tcp { out, .. } => out,
        }
    }

    fn transport(&self) -> &'static str {
        match self {
            Link::Quic { .. } => "quic",
            Link::Tcp { .. } => "tcp",
        }
    }

    /// Ends the connection: QUIC closes (its streams end with it); TCP stops
    /// the reader, and the writer ends once the last sender (the session's
    /// and the forgotten feeds') is dropped.
    fn close(&self) {
        match self {
            Link::Quic { conn, .. } => conn.close(0u32.into(), b"bye"),
            Link::Tcp { reader, .. } => reader.abort(),
        }
    }
}

/// Token for HELLO/REAUTH from the saved sign-in, or `None` (anonymous).
/// Never over a connection whose certificate was not checked
/// (`--airframes-quic-insecure`): anyone on the path could take it.
async fn token(s: &QuicSettings) -> Option<atc_auth::AccessToken> {
    if s.insecure {
        return None;
    }
    let path = s.credentials.clone().unwrap_or_else(atc_auth::default_path);
    let c = atc_auth::load(&path).ok()?;
    c.refresh_token.as_ref()?;
    match tokio::task::spawn_blocking(move || atc_auth::refresh(&path)).await {
        Ok(Ok(t)) => Some(t),
        Ok(Err(e)) => {
            tracing::warn!("airframes quic: sign-in refresh failed ({e:#}); connecting without it");
            None
        }
        Err(_) => None,
    }
}

/// What came back for HELLO.
enum Reply {
    Welcome(Value),
    Refused(Value),
    /// Nothing (in time), the connection ended, or an unreadable WELCOME.
    Failed(&'static str),
}

async fn await_welcome(incoming: &mut Incoming) -> Reply {
    match tokio::time::timeout(Duration::from_secs(10), incoming.recv()).await {
        Ok(Some((WELCOME, p))) => match serde_json::from_slice::<Value>(&p) {
            Ok(v) if v.is_object() => Reply::Welcome(v),
            _ => Reply::Failed("unreadable WELCOME"),
        },
        Ok(Some((ERROR, p))) => Reply::Refused(serde_json::from_slice(&p).unwrap_or_default()),
        Ok(_) => Reply::Failed("no WELCOME"),
        Err(_) => Reply::Failed("no WELCOME (timed out)"),
    }
}

async fn run(s: QuicSettings, mut rx: mpsc::Receiver<Item>, shared: Arc<Shared>) {
    let dialer = match Dialer::new(&s) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("airframes quic: {e:#}; per-port feeding continues");
            return;
        }
    };
    if s.insecure {
        tracing::warn!("airframes quic: certificate checks are off; connecting anonymously");
    }
    let creds_path = s.credentials.clone().unwrap_or_else(atc_auth::default_path);
    let instance_id = match atc_auth::feeder_id(&creds_path) {
        Ok(id) if s.instance_tag.is_empty() => id,
        Ok(id) => format!("{id}/{}", s.instance_tag),
        Err(e) => {
            tracing::warn!(
                "airframes quic: cannot keep an instance id in {} ({e}); using a temporary one",
                creds_path.display()
            );
            uuid::Uuid::new_v4().to_string()
        }
    };
    let mut backoff = Duration::from_secs(1);
    let mut quic_failures = 0u32;
    let mut reconnects = 0u64;
    let mut last_disconnect = String::new();
    loop {
        let use_quic = match s.transport.as_str() {
            "tcp" => false,
            "quic" => true,
            _ => quic_failures < 2,
        };
        let (mut link, mut incoming) = match dial(&dialer, use_quic).await {
            Ok(l) => l,
            Err(e) => {
                if use_quic {
                    quic_failures += 1;
                }
                tracing::warn!(
                    "airframes quic: connect to {} ({}) failed: {e:#}; retrying in {backoff:?}",
                    s.endpoint,
                    if use_quic { "quic" } else { "tcp" }
                );
                last_disconnect = format!("{e:#}");
                if !sleep_draining(&mut rx, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        let tok = token(&s).await;
        let hello = json!({
            "v": VERSION,
            "token": tok.as_ref().map(|t| t.token.clone()),
            "client": { "name": "xng", "version": env!("CARGO_PKG_VERSION"), "platform": format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH) },
            "instance_id": instance_id,
            "clock": shared.clock(),
            "capabilities": { "commands": COMMANDS },
            "reconnects": reconnects,
            "last_disconnect": last_disconnect,
        });
        let _ = link
            .ctrl()
            .send(frame(HELLO, hello.to_string().as_bytes()))
            .await;
        let welcome = match await_welcome(&mut incoming).await {
            Reply::Welcome(w) => w,
            Reply::Refused(e) => {
                tracing::warn!(
                    "airframes quic: gateway refused the connection: {}",
                    e["message"].as_str().unwrap_or("?")
                );
                link.close();
                last_disconnect = e["code"].as_str().unwrap_or("refused").to_string();
                if !sleep_draining(&mut rx, backoff.max(Duration::from_secs(30))).await {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
            Reply::Failed(why) => {
                if use_quic {
                    quic_failures += 1;
                }
                link.close();
                last_disconnect = why.into();
                if !sleep_draining(&mut rx, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        if use_quic {
            quic_failures = 0;
        }
        shared.feeds.lock().unwrap().clear();
        backoff = Duration::from_secs(1);
        let session_id = welcome["session_id"].as_str().unwrap_or("").to_string();
        let ingests = parse_ingests(&welcome);
        if let Some(list) = &ingests {
            tracing::info!(
                "airframes quic: gateway takes {}",
                list.iter().map(|(f, m)| format!("{m} ({f})")).collect::<Vec<_>>().join(", ")
            );
        }
        *shared.ingests.lock().unwrap() = ingests;
        let stats_every =
            Duration::from_secs(welcome["stats_s"].as_u64().unwrap_or(30).clamp(5, 600));
        tracing::info!(
            "airframes quic: connected to {} over {} (session {session_id})",
            s.endpoint,
            link.transport()
        );
        {
            let mut l = shared.link.lock().unwrap();
            *l = LinkStatus {
                connected: true,
                transport: link.transport(),
                session_id: session_id.clone(),
                reconnects,
                ..Default::default()
            };
        }
        shared.connected.store(true, Ordering::Relaxed);

        let reason = session(&mut link, &mut incoming, &mut rx, &shared, stats_every, &s).await;

        shared.connected.store(false, Ordering::Relaxed);
        shared.feeds.lock().unwrap().clear();
        shared.link.lock().unwrap().connected = false;
        link.close();
        if reason == SHUTDOWN {
            return;
        }
        tracing::warn!(
            "airframes quic: disconnected ({reason}); per-port feeding until reconnected"
        );
        last_disconnect = reason;
        reconnects += 1;
        if !sleep_draining(&mut rx, Duration::from_secs(1)).await {
            return;
        }
    }
}

/// Session end reason when the last [`Uplink`] handle is gone.
const SHUTDOWN: &str = "shutting down";

/// Connects once, says HELLO and returns the gateway's WELCOME (what it
/// takes, limits, the account it sees), then closes. For `xng airframes
/// ingests`. With `quic` (auto), a QUIC connection that gives no WELCOME
/// is retried over TLS, as the uplink would.
pub async fn probe(s: &QuicSettings) -> anyhow::Result<(Value, &'static str)> {
    let dialer = Dialer::new(s)?;
    let mut last = None;
    let tries: &[bool] = match s.transport.as_str() {
        "tcp" => &[false],
        "quic" => &[true],
        _ => &[true, false],
    };
    for &quic in tries {
        match dial(&dialer, quic).await {
            Ok((link, mut incoming)) => {
                let tok = token(s).await;
                let hello = json!({
                    "v": VERSION,
                    "token": tok.as_ref().map(|t| t.token.clone()),
                    "client": { "name": "xng", "version": env!("CARGO_PKG_VERSION"), "platform": format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH) },
                    "instance_id": format!("probe-{}", uuid::Uuid::new_v4()),
                });
                link.ctrl().send(frame(HELLO, hello.to_string().as_bytes())).await?;
                let r = await_welcome(&mut incoming).await;
                let transport = link.transport();
                link.close();
                match r {
                    Reply::Welcome(w) => return Ok((w, transport)),
                    Reply::Refused(e) => {
                        anyhow::bail!("gateway refused: {}", e["message"].as_str().unwrap_or("?"))
                    }
                    Reply::Failed(why) => {
                        last = Some(anyhow::anyhow!("{why} from {} over {transport}", s.endpoint))
                    }
                }
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no transport")))
}

/// WELCOME `ingests` as (format, mode) pairs, or `None` when absent.
pub fn parse_ingests(welcome: &Value) -> Option<Vec<(String, String)>> {
    let list = welcome.get("ingests")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|i| Some((i["format"].as_str()?.to_string(), i["mode"].as_str()?.to_string())))
            .collect(),
    )
}

/// Waits, discarding open requests (they are made again by the next
/// message). False when the last [`Uplink`] handle is gone: stop.
async fn sleep_draining(rx: &mut mpsc::Receiver<Item>, d: Duration) -> bool {
    let until = tokio::time::Instant::now() + d;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(until) => return true,
            m = rx.recv() => if m.is_none() { return false },
        }
    }
}

async fn session(
    link: &mut Link,
    incoming: &mut Incoming,
    rx: &mut mpsc::Receiver<Item>,
    shared: &Arc<Shared>,
    stats_every: Duration,
    s: &QuicSettings,
) -> String {
    // The feeds this session opened (TCP: by stream id for OPENED).
    let mut feeds: HashMap<String, Arc<Feed>> = HashMap::new();
    let mut by_tcp_id: HashMap<u16, (String, Arc<Feed>)> = HashMap::new();
    // QUIC feed streams that ended, from their reader tasks.
    let (ended_tx, mut ended) = mpsc::unbounded_channel::<(String, Arc<Feed>)>();
    let mut stats = tokio::time::interval(stats_every);
    stats.tick().await;
    let mut last_rx = Instant::now();
    let mut watchdog = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            item = rx.recv() => {
                let Some(item) = item else { return SHUTDOWN.into() };
                let key = item.spec.key.clone();
                let Some(feed) = shared.feeds.lock().unwrap().get(&key).cloned() else { continue };
                if feeds.get(&key).is_some_and(|f| Arc::ptr_eq(f, &feed)) {
                    continue;
                }
                match tokio::time::timeout(OPEN_TIMEOUT, open_feed(link, &item.spec, &feed, &ended_tx)).await {
                    Ok(Ok(tcp_id)) => {
                        if let Some(id) = tcp_id {
                            by_tcp_id.insert(id, (key.clone(), feed.clone()));
                        }
                        feeds.insert(key, feed);
                    }
                    Ok(Err(e)) => {
                        if let Link::Quic { conn, .. } = link {
                            if conn.close_reason().is_some() {
                                return format!("open feed: {e:#}");
                            }
                        }
                        tracing::warn!("airframes quic: feed {key} not opened ({e:#}); retried with its next message");
                        shared.forget(&key, &feed);
                    }
                    Err(_) => {
                        tracing::warn!("airframes quic: feed {key} not opened in {OPEN_TIMEOUT:?}; retried with its next message");
                        shared.forget(&key, &feed);
                    }
                }
            }
            Some((key, feed)) = ended.recv() => {
                // Its stream ended with the connection still up: the next
                // message opens it again (or goes per port meanwhile).
                if feeds.get(&key).is_some_and(|f| Arc::ptr_eq(f, &feed)) {
                    feeds.remove(&key);
                }
                shared.forget(&key, &feed);
            }
            fr = incoming.recv() => {
                let Some((t, p)) = fr else { return "connection lost".into() };
                last_rx = Instant::now();
                match t {
                    PING => { let _ = link.ctrl().try_send(frame(PONG, &pong(&p, now_us()))); }
                    LATENCY => {
                        if let Ok(v) = serde_json::from_slice::<Value>(&p) {
                            let mut l = shared.link.lock().unwrap();
                            l.rtt_ms = v["rtt_ms"].as_f64();
                            l.trusted = v["trusted"].as_bool().unwrap_or(false);
                            l.up_ms = v["up_ms"].as_f64().filter(|_| l.trusted);
                            l.down_ms = v["down_ms"].as_f64().filter(|_| l.trusted);
                            tracing::debug!("airframes quic: rtt {:?} ms, up {:?} ms, down {:?} ms", l.rtt_ms, l.up_ms, l.down_ms);
                        }
                    }
                    NOTICE => {
                        let v: Value = serde_json::from_slice(&p).unwrap_or_default();
                        if v["code"] == "token_expiring" {
                            if let Some(t) = token(s).await {
                                let _ = link.ctrl().send(frame(REAUTH, json!({"token": t.token}).to_string().as_bytes())).await;
                            }
                        } else {
                            tracing::info!("airframes quic: gateway notice {}: {}", v["code"], v["message"].as_str().unwrap_or(""));
                        }
                    }
                    COMMAND => {
                        let cmd: Value = serde_json::from_slice(&p).unwrap_or_default();
                        let res = command(&cmd, link, &feeds, shared);
                        let _ = link.ctrl().try_send(frame(RESULT, res.to_string().as_bytes()));
                    }
                    STREAM => {
                        // TCP: OPENED for a feed.
                        if p.len() >= 6 && p[2] == OPENED {
                            let id = u16::from_be_bytes([p[0], p[1]]);
                            if let Some((key, f)) = by_tcp_id.get(&id) {
                                opened(f, &p[6..], key);
                            }
                        }
                    }
                    ERROR => {
                        let e: Value = serde_json::from_slice(&p).unwrap_or_default();
                        if e["fatal"].as_bool().unwrap_or(false) {
                            return format!("gateway error {}: {}", e["code"].as_str().unwrap_or("?"), e["message"].as_str().unwrap_or(""));
                        }
                    }
                    _ => {}
                }
            }
            _ = stats.tick() => {
                let list: Vec<Value> = feeds.iter().map(|(k, f)| {
                    let mut v = f.json(k);
                    v["sdr"] = json!({ "state": "ok" });
                    v
                }).collect();
                let st = json!({ "feeds": list, "uptime_s": shared.started.elapsed().as_secs(), "clock": shared.clock() });
                let _ = link.ctrl().try_send(frame(STATS, st.to_string().as_bytes()));
            }
            _ = watchdog.tick() => {
                // The gateway pings every 15 s: silence for 3 heartbeats is a dead link.
                if last_rx.elapsed() > Duration::from_secs(50) {
                    return "heartbeat timeout".into();
                }
                if let Link::Quic { conn, .. } = link {
                    if let Some(e) = conn.close_reason() {
                        return format!("{e}");
                    }
                }
            }
        }
    }
}

/// OPENED for a feed: accepted (send what it held, then take messages) or
/// refused (drop what it held; per port from now on).
fn opened(feed: &Feed, payload: &[u8], key: &str) {
    let v: Value = serde_json::from_slice(payload).unwrap_or_default();
    let mut held = feed.held.lock().unwrap();
    if v["ok"].as_bool() == Some(true) {
        for p in held.drain(..) {
            let _ = feed.write(&p);
        }
        feed.state.store(OPEN_OK, Ordering::Relaxed);
    } else {
        feed.dropped.fetch_add(held.len() as u64, Ordering::Relaxed);
        held.clear();
        feed.state.store(REFUSED, Ordering::Relaxed);
        tracing::warn!(
            "airframes quic: feed {key} refused ({}: {}); it stays on the per-port feed",
            v["code"].as_str().unwrap_or("?"),
            v["message"].as_str().unwrap_or("")
        );
    }
}

/// Sends OPEN for a feed and sets its writer. Returns its TCP stream id.
async fn open_feed(
    link: &mut Link,
    spec: &FeedSpec,
    feed: &Arc<Feed>,
    ended: &mpsc::UnboundedSender<(String, Arc<Feed>)>,
) -> anyhow::Result<Option<u16>> {
    let open = json!({
        "key": spec.key,
        "mode": spec.mode.as_str(),
        "format": spec.format,
        "station_ident": spec.station_ident,
        "station_uuid": spec.station_uuid,
        "sdr": spec.sdr_id.as_ref().map(|id| json!({ "id": id })),
        "decoder": { "name": "xng", "version": env!("CARGO_PKG_VERSION") },
    });
    match link {
        Link::Quic { conn, .. } => {
            let (mut send, mut recv) = conn.open_bi().await?;
            send.write_all(&frame(OPEN, open.to_string().as_bytes()))
                .await?;
            *feed.writer.lock().unwrap() = Some((spawn_writer(send, FEED_QUEUE), None));
            let (f, key, ended) = (feed.clone(), spec.key.clone(), ended.clone());
            tokio::spawn(async move {
                while let Ok((t, p)) = read_frame(&mut recv).await {
                    if t == OPENED {
                        opened(&f, &p, &key);
                    }
                }
                let _ = ended.send((key, f));
            });
            Ok(None)
        }
        Link::Tcp { out, next_id, .. } => {
            *next_id = next_id.wrapping_add(1).max(1);
            let id = *next_id;
            out.send(stream_frame(id, OPEN, open.to_string().as_bytes()))
                .await?;
            *feed.writer.lock().unwrap() = Some((out.clone(), Some(id)));
            Ok(Some(id))
        }
    }
}

fn command(
    cmd: &Value,
    link: &Link,
    feeds: &HashMap<String, Arc<Feed>>,
    shared: &Arc<Shared>,
) -> Value {
    let id = cmd["id"].clone();
    let name = cmd["name"].as_str().unwrap_or("");
    let data = match name {
        "status" => json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_s": shared.started.elapsed().as_secs(),
            "feeds": feeds.iter().map(|(k, f)| f.json(k)).collect::<Vec<_>>(),
        }),
        "clock" => shared.clock(),
        "net" => {
            let l = shared.link.lock().unwrap().clone();
            let mut v = json!({ "transport": link.transport(), "rtt_ms": l.rtt_ms, "reconnects": l.reconnects });
            if let Link::Quic { conn, .. } = link {
                let st = conn.stats();
                v["quic"] = json!({
                    "rtt_ms": conn.rtt().as_secs_f64() * 1000.0,
                    "lost_packets": st.path.lost_packets,
                    "sent_packets": st.path.sent_packets,
                    "congestion_events": st.path.congestion_events,
                });
            }
            v
        }
        _ => return json!({ "id": id, "ok": false, "error": "unsupported" }),
    };
    json!({ "id": id, "ok": true, "data": data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut b = frame(DATA, &data_payload(7, b"{\"a\":1}"));
            b.extend(stream_frame(3, OPEN, b"{}"));
            let mut r = &b[..];
            let (t, p) = read_frame(&mut r).await.unwrap();
            assert_eq!(t, DATA);
            assert_eq!(u64::from_be_bytes(p[..8].try_into().unwrap()), 7);
            assert_eq!(&p[8..], b"{\"a\":1}");
            let (t, p) = read_frame(&mut r).await.unwrap();
            assert_eq!(t, STREAM);
            assert_eq!(u16::from_be_bytes([p[0], p[1]]), 3);
            assert_eq!(p[2], OPEN);
        });
    }

    #[test]
    fn held_messages_go_out_on_opened_and_are_dropped_on_refusal() {
        let (tx, mut rx) = mpsc::channel(8);
        let f = Feed::new();
        *f.writer.lock().unwrap() = Some((tx, Some(4)));
        f.held.lock().unwrap().push(data_payload(1, b"a"));
        f.held.lock().unwrap().push(data_payload(2, b"b"));
        opened(&f, br#"{"ok":true}"#, "k");
        assert_eq!(f.state.load(Ordering::Relaxed), OPEN_OK);
        assert_eq!(f.sent.load(Ordering::Relaxed), 2);
        assert!(rx.try_recv().is_ok() && rx.try_recv().is_ok());

        let g = Feed::new();
        g.held.lock().unwrap().push(data_payload(1, b"a"));
        opened(&g, br#"{"ok":false,"code":"format"}"#, "k");
        assert_eq!(g.state.load(Ordering::Relaxed), REFUSED);
        assert_eq!(g.dropped.load(Ordering::Relaxed), 1);
        assert!(g.held.lock().unwrap().is_empty());
    }

    #[test]
    fn a_full_writer_is_not_taken_and_a_closed_one_is_reported() {
        let (tx, rx) = mpsc::channel(1);
        let f = Feed::new();
        *f.writer.lock().unwrap() = Some((tx, None));
        assert_eq!(f.write(b"x"), Ok(true));
        assert_eq!(f.write(b"y"), Ok(false), "full: per port instead");
        drop(rx);
        assert_eq!(f.write(b"z"), Err(()));
    }

    #[test]
    fn pong_echoes_and_stamps() {
        let p = pong(&42u64.to_be_bytes(), 100);
        assert_eq!(p.len(), 24);
        assert_eq!(u64::from_be_bytes(p[..8].try_into().unwrap()), 42);
        assert_eq!(u64::from_be_bytes(p[8..16].try_into().unwrap()), 100);
        assert!(u64::from_be_bytes(p[16..24].try_into().unwrap()) >= 100);
    }

    #[test]
    fn formats_follow_the_gateway_list() {
        let w = json!({ "ingests": [
            { "format": "acarsdec", "mode": "acars" },
            { "format": "xng", "mode": "std-c" },
            { "format": "xng", "mode": "adsb" },
        ]});
        let list = parse_ingests(&w).unwrap();
        assert_eq!(choose_format(Mode::AcarsPoa, Some(&list)), Some("acarsdec"));
        assert_eq!(choose_format(Mode::StdC, Some(&list)), Some(XNG_FORMAT));
        assert_eq!(choose_format(Mode::Vdl2, Some(&list)), None, "not listed: per port");
        assert_eq!(choose_format(Mode::Adsb, Some(&list)), None, "ADS-B never");
        // A gateway that lists nothing takes the native formats only.
        assert_eq!(choose_format(Mode::Vdl2, None), Some("dumpvdl2"));
        assert_eq!(choose_format(Mode::StdC, None), None);
        assert!(parse_ingests(&json!({})).is_none());
    }

    #[test]
    fn transport_values() {
        assert!(QuicSettings::for_transport(None, None, None, false)
            .unwrap()
            .is_none());
        assert!(
            QuicSettings::for_transport(Some("ports"), None, None, false)
                .unwrap()
                .is_none()
        );
        let q = QuicSettings::for_transport(Some("quic"), None, None, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            (q.transport.as_str(), q.endpoint.as_str()),
            ("auto", DEFAULT_ENDPOINT)
        );
        assert_eq!(
            QuicSettings::for_transport(Some("tls"), None, None, false)
                .unwrap()
                .unwrap()
                .transport,
            "tcp"
        );
        assert!(QuicSettings::for_transport(Some("udp"), None, None, false).is_err());
        assert!(
            QuicSettings::for_transport(Some("quic"), None, Some("ca.pem".into()), true).is_err()
        );
    }

    #[test]
    fn formats_cover_gateway_modes_only() {
        assert_eq!(format_for(Mode::Vdl2), Some("dumpvdl2"));
        assert_eq!(format_for(Mode::AeroC), Some("jaero-c"));
        assert_eq!(format_for(Mode::Ais), Some("aiscatcher"));
        assert_eq!(format_for(Mode::Sonde), None);
        assert_eq!(format_for(Mode::Adsb), None);
    }
}
