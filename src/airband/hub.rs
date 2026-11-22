//! The process-wide airband audio hub: a broadcast channel of audio and
//! squelch events plus the live per-channel status the dashboard and
//! metrics read.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use xng_mode_airband::squelch::SquelchMode;
use xng_mode_airband::Modulation;

/// Audio sample rate on the hub (Hz).
pub const AUDIO_RATE: u32 = xng_mode_airband::CHANNEL_RATE as u32;
/// Broadcast depth. One channel publishes roughly 40 audio frames a second
/// while open, so this rides out a slow consumer for a few seconds.
const CAPACITY: usize = 4_096;
/// Finished transmissions kept for the dashboard.
const RECENT: usize = 300;

/// Which audio stream an event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamId {
    Channel(u64),
    Mix,
    Scan,
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamId::Channel(hz) => write!(f, "{hz}"),
            StreamId::Mix => f.write_str("mix"),
            StreamId::Scan => f.write_str("scan"),
        }
    }
}

impl Serialize for StreamId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Static description of one channel.
#[derive(Clone, Debug, Serialize)]
pub struct ChannelMeta {
    pub freq_hz: u64,
    pub label: Option<String>,
    /// Stable key (ATC feed identity), `<sdr>:<freq>` unless configured.
    pub key: String,
    pub atc_frequency: Option<String>,
    pub modulation: Modulation,
    pub bandwidth_hz: f64,
    pub squelch: SquelchMode,
    /// The SDR selector this channel is received on, if any.
    pub sdr: Option<String>,
}

impl ChannelMeta {
    /// "KSFO Tower (118.300)" or "118.300".
    pub fn display(&self) -> String {
        let mhz = format!("{:.3}", self.freq_hz as f64 / 1e6);
        match &self.label {
            Some(l) => format!("{l} ({mhz})"),
            None => mhz,
        }
    }

    /// Synthetic meta for the mix stream.
    pub fn mix() -> Self {
        Self {
            freq_hz: 0,
            label: Some("Mix".into()),
            key: "mix".into(),
            atc_frequency: None,
            modulation: Modulation::Am,
            bandwidth_hz: 0.0,
            squelch: SquelchMode::Off,
            sdr: None,
        }
    }
}

/// A block of audio. `at` is the capture time of the first sample.
#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub stream: StreamId,
    pub chan: Arc<ChannelMeta>,
    /// Transmission number within `stream`.
    pub tx: u64,
    pub at: DateTime<Utc>,
    pub samples: Arc<[f32]>,
    /// Samples were dropped just before this frame.
    pub discontinuity: bool,
}

/// What the hub carries.
#[derive(Clone, Debug)]
pub enum AudioEvent {
    Open {
        stream: StreamId,
        chan: Arc<ChannelMeta>,
        tx: u64,
        at: DateTime<Utc>,
        level_dbfs: f32,
        noise_dbfs: f32,
    },
    Audio(Arc<AudioFrame>),
    Close {
        stream: StreamId,
        chan: Arc<ChannelMeta>,
        tx: u64,
        at: DateTime<Utc>,
        duration_s: f64,
        peak_dbfs: f32,
        noise_dbfs: f32,
    },
    /// Periodic per-channel levels (about four a second).
    Status(Arc<ChannelStatus>),
}

impl AudioEvent {
    pub fn stream(&self) -> StreamId {
        match self {
            AudioEvent::Open { stream, .. } | AudioEvent::Close { stream, .. } => *stream,
            AudioEvent::Audio(f) => f.stream,
            AudioEvent::Status(s) => StreamId::Channel(s.freq_hz),
        }
    }
}

/// Live state of one channel.
#[derive(Clone, Debug, Serialize)]
pub struct ChannelStatus {
    pub freq_hz: u64,
    pub level_dbfs: f32,
    pub noise_dbfs: f32,
    pub squelch_dbfs: f32,
    pub open: bool,
    pub transmissions: u64,
    pub airtime_s: f64,
    pub afc_hz: f32,
}

/// One finished transmission, for the dashboard's history.
#[derive(Clone, Debug, Serialize)]
pub struct TxRecord {
    pub stream: StreamId,
    pub freq_hz: u64,
    pub label: Option<String>,
    pub tx: u64,
    pub start: DateTime<Utc>,
    pub duration_s: f64,
    pub peak_dbfs: f32,
    pub noise_dbfs: f32,
}

pub struct AudioHub {
    tx: broadcast::Sender<AudioEvent>,
    channels: Mutex<BTreeMap<u64, Arc<ChannelMeta>>>,
    status: Mutex<BTreeMap<u64, ChannelStatus>>,
    recent: Mutex<VecDeque<TxRecord>>,
    /// The scanner behind the `scan` stream, when enabled (for pin/lockout).
    pub(crate) scan: Mutex<Option<ScanState>>,
    mix: std::sync::atomic::AtomicBool,
}

/// Scanner state shared with the dashboard controls.
pub struct ScanState {
    pub ctl: xng_mode_airband::scan::ScanController,
    pub selected: Option<u64>,
    pub locked_out: Vec<u64>,
}

static HUB: OnceLock<AudioHub> = OnceLock::new();

/// The process-wide hub.
pub fn hub() -> &'static AudioHub {
    HUB.get_or_init(|| AudioHub {
        tx: broadcast::channel(CAPACITY).0,
        channels: Mutex::new(BTreeMap::new()),
        status: Mutex::new(BTreeMap::new()),
        recent: Mutex::new(VecDeque::new()),
        scan: Mutex::new(None),
        mix: std::sync::atomic::AtomicBool::new(false),
    })
}

impl AudioHub {
    pub fn subscribe(&self) -> broadcast::Receiver<AudioEvent> {
        self.tx.subscribe()
    }

    /// Register a channel (done when a session builds its decoders).
    pub fn register(&self, meta: Arc<ChannelMeta>) {
        self.channels.lock().unwrap().insert(meta.freq_hz, meta);
    }

    /// Note that some session publishes the `mix` stream.
    pub fn enable_mix(&self) {
        self.mix.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn has_mix(&self) -> bool {
        self.mix.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn channels(&self) -> Vec<Arc<ChannelMeta>> {
        self.channels.lock().unwrap().values().cloned().collect()
    }

    pub fn channel(&self, freq_hz: u64) -> Option<Arc<ChannelMeta>> {
        self.channels.lock().unwrap().get(&freq_hz).cloned()
    }

    pub fn status(&self) -> Vec<ChannelStatus> {
        self.status.lock().unwrap().values().cloned().collect()
    }

    pub fn recent(&self) -> Vec<TxRecord> {
        self.recent.lock().unwrap().iter().cloned().collect()
    }

    /// True once any airband channel exists (outputs use this to decide
    /// whether to show airband views).
    pub fn active(&self) -> bool {
        !self.channels.lock().unwrap().is_empty()
    }

    /// Publish an event, keeping the status/history views current. A send
    /// with no subscribers is fine (nobody is listening).
    pub fn publish(&self, ev: AudioEvent) {
        match &ev {
            AudioEvent::Status(s) => {
                self.status.lock().unwrap().insert(s.freq_hz, (**s).clone());
            }
            AudioEvent::Close { stream, chan, tx, at, duration_s, peak_dbfs, noise_dbfs } => {
                if matches!(stream, StreamId::Channel(_)) {
                    let mut r = self.recent.lock().unwrap();
                    let start = *at - chrono::Duration::milliseconds((duration_s * 1_000.0) as i64);
                    r.push_back(TxRecord {
                        stream: *stream,
                        freq_hz: chan.freq_hz,
                        label: chan.label.clone(),
                        tx: *tx,
                        start,
                        duration_s: *duration_s,
                        peak_dbfs: *peak_dbfs,
                        noise_dbfs: *noise_dbfs,
                    });
                    while r.len() > RECENT {
                        r.pop_front();
                    }
                }
            }
            _ => {}
        }
        let _ = self.tx.send(ev);
    }
}
