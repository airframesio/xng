//! Speech-to-text for airband transmissions with Whisper.
//!
//! A collector task gathers each channel transmission's audio from the hub
//! (Open .. Close, cut into pieces of at most `max-s`) and queues it. One
//! worker thread runs Whisper on the queue, oldest first; when Whisper
//! falls behind, the oldest waiting transmissions are dropped so transcripts
//! stay close to live. Each result goes back on the hub as
//! [`AudioEvent::Transcript`] (dashboard, recordings, the ATC feed) and on
//! the message bus as an airband `transcript` message (console, JSONL, MQTT,
//! ...).
//!
//! Engines: a local whisper.cpp model (`whisper` build feature), a
//! sherpa-onnx model such as Moonshine or Parakeet (`sherpa` build feature,
//! much less CPU than Whisper; see [`super::sherpa`]), or any
//! Whisper HTTP server that takes a WAV upload: the OpenAI-compatible
//! `/v1/audio/transcriptions` (OpenAI, faster-whisper servers, LocalAI,
//! whisper.cpp `server --inference-path`), or whisper.cpp's own `/inference`.

use super::config::{parse_streams, selected, TranscribeSettings};
use super::sherpa as sherpa_models;
use super::hub::{hub, AudioEvent, ChannelMeta, StreamId, Transcript, AUDIO_RATE};
use crate::bus::WeakBus;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Primes Whisper for radiotelephony: callsigns, numbers as digits, the
/// usual clearances.
pub const DEFAULT_PROMPT: &str = "Air traffic control radio. United 1234, cleared to land runway 28 right, \
wind 290 at 12. Climb and maintain flight level 350. Contact NorCal Approach 135.65. Squawk 4521. Roger, wilco.";

/// Longest text kept from one transmission (the ATC protocol's limit).
const MAX_TEXT: usize = 2_000;

/// What this run's transcription announces (engine, model, language), for
/// the ATC feed's OPEN. Set when transcription starts.
static ANNOUNCE: OnceLock<Mutex<Option<Announce>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct Announce {
    pub engine: EngineInfo,
    pub language: Option<String>,
    streams: Vec<StreamId>,
}

impl Announce {
    /// True when transmissions on this channel are transcribed.
    pub fn covers(&self, freq_hz: u64) -> bool {
        selected(&self.streams, StreamId::Channel(freq_hz))
    }
}

/// The active transcription, if any.
pub fn announced() -> Option<Announce> {
    ANNOUNCE.get_or_init(Default::default).lock().unwrap().clone()
}

fn set_announce(a: Option<Announce>) {
    *ANNOUNCE.get_or_init(Default::default).lock().unwrap() = a;
}

/// Audio for Whisper: one transmission, or one piece of a long one.
#[derive(Debug)]
pub struct Clip {
    pub stream: StreamId,
    pub chan: Arc<ChannelMeta>,
    pub tx: u64,
    pub start: DateTime<Utc>,
    pub samples: Vec<f32>,
}

impl Clip {
    fn end(&self) -> DateTime<Utc> {
        self.start + chrono::Duration::microseconds((self.samples.len() as u64 * 1_000_000 / AUDIO_RATE as u64) as i64)
    }
}

/// A piece of recognised text with Whisper's own scores.
#[derive(Clone, Debug, Default)]
pub struct Segment {
    pub text: String,
    /// Whisper's probability that the window held no speech.
    pub no_speech: Option<f32>,
    /// Mean token probability (0..1).
    pub prob: Option<f32>,
}

/// Which implementation and model make the transcripts. Sent to Airframes
/// ATC with each feed (OPEN) and kept on every transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInfo {
    /// Short lowercase id of the implementation: `whisper.cpp`,
    /// `sherpa-onnx`, or for a server whatever it runs (`openai-api` unless
    /// configured, e.g. `faster-whisper`).
    pub engine: String,
    pub version: Option<String>,
    /// The model as the engine knows it: `base.en`, `moonshine-base`,
    /// `parakeet-tdt-0.6b-v2`, a file name, or the server's model name.
    pub model: String,
}

/// A speech-to-text engine. Runs on the worker thread, one clip at a time.
pub trait Engine: Send {
    /// Engine and model reported with each transcript.
    fn info(&self) -> EngineInfo;
    fn transcribe(&mut self, samples: &[f32], prompt: &str) -> anyhow::Result<Vec<Segment>>;
}

// ---- the queue between the collector and the worker ----

struct Queue {
    items: Mutex<(VecDeque<Clip>, bool)>,
    cv: Condvar,
    cap: usize,
    dropped: AtomicU64,
}

impl Queue {
    fn new(cap: usize) -> Self {
        Self { items: Mutex::new((VecDeque::new(), false)), cv: Condvar::new(), cap, dropped: AtomicU64::new(0) }
    }

    /// Queue a clip, dropping the oldest when full. Returns how many were
    /// dropped to make room.
    fn push(&self, c: Clip) -> usize {
        let mut g = self.items.lock().unwrap();
        let mut n = 0;
        while g.0.len() >= self.cap {
            g.0.pop_front();
            n += 1;
        }
        g.0.push_back(c);
        drop(g);
        self.cv.notify_one();
        self.dropped.fetch_add(n as u64, Ordering::Relaxed);
        n
    }

    /// Next clip; `None` once closed and empty.
    fn pop(&self) -> Option<Clip> {
        let mut g = self.items.lock().unwrap();
        loop {
            if let Some(c) = g.0.pop_front() {
                return Some(c);
            }
            if g.1 {
                return None;
            }
            g = self.cv.wait(g).unwrap();
        }
    }

    fn close(&self) {
        self.items.lock().unwrap().1 = true;
        self.cv.notify_all();
    }

    fn clear(&self) -> usize {
        let mut g = self.items.lock().unwrap();
        let n = g.0.len();
        g.0.clear();
        n
    }
}

// ---- collecting transmissions ----

/// Builds clips from hub events.
struct Collector {
    streams: Vec<StreamId>,
    min_samples: usize,
    max_samples: usize,
    open: HashMap<StreamId, Clip>,
}

impl Collector {
    fn new(s: &TranscribeSettings) -> anyhow::Result<Self> {
        Ok(Self {
            streams: parse_streams(&s.streams)?,
            min_samples: s.min_ms as usize * AUDIO_RATE as usize / 1_000,
            max_samples: s.max_s as usize * AUDIO_RATE as usize,
            open: HashMap::new(),
        })
    }

    /// Clips this event completes.
    fn step(&mut self, ev: &AudioEvent) -> Vec<Clip> {
        let stream = ev.stream();
        if !matches!(stream, StreamId::Channel(_)) || !selected(&self.streams, stream) {
            return Vec::new();
        }
        let mut out = Vec::new();
        match ev {
            AudioEvent::Open { chan, tx, at, .. } => {
                if let Some(old) = self.open.remove(&stream) {
                    self.finish(old, &mut out);
                }
                self.open.insert(
                    stream,
                    Clip { stream, chan: chan.clone(), tx: *tx, start: *at, samples: Vec::new() },
                );
            }
            AudioEvent::Audio(f) => {
                // A different transmission (its Open was lost): start over.
                if self.open.get(&stream).is_some_and(|c| c.tx != f.tx) {
                    let old = self.open.remove(&stream).expect("present");
                    self.finish(old, &mut out);
                }
                let max = self.max_samples;
                let clip = self.open.entry(stream).or_insert_with(|| Clip {
                    stream,
                    chan: f.chan.clone(),
                    tx: f.tx,
                    start: f.at,
                    samples: Vec::new(),
                });
                if clip.samples.is_empty() {
                    clip.start = f.at;
                }
                let mut rest: &[f32] = &f.samples;
                while !rest.is_empty() {
                    let room = max - clip.samples.len();
                    let take = room.min(rest.len());
                    clip.samples.extend_from_slice(&rest[..take]);
                    rest = &rest[take..];
                    if clip.samples.len() >= max {
                        // Full window: send this piece, keep collecting.
                        let next_start = clip.end();
                        let piece = std::mem::replace(
                            clip,
                            Clip {
                                stream,
                                chan: clip.chan.clone(),
                                tx: clip.tx,
                                start: next_start,
                                samples: Vec::new(),
                            },
                        );
                        out.push(piece);
                    }
                }
            }
            AudioEvent::Close { .. } => {
                if let Some(c) = self.open.remove(&stream) {
                    self.finish(c, &mut out);
                }
            }
            AudioEvent::Status(_) | AudioEvent::Transcript(_) => {}
        }
        out
    }

    fn finish(&self, c: Clip, out: &mut Vec<Clip>) {
        // A tail after a full piece is kept if it has anything to say; a
        // whole transmission needs min-ms.
        if c.samples.len() >= self.min_samples.max(AUDIO_RATE as usize / 4) {
            out.push(c);
        }
    }
}

// ---- cleaning Whisper's output ----

/// What Whisper commonly invents on noise, static or silence.
const HALLUCINATIONS: &[&str] = &[
    "you",
    "thank you",
    "thanks",
    "thank you very much",
    "thanks for watching",
    "thank you for watching",
    "thanks for watching and see you next time",
    "please subscribe",
    "subscribe",
    "bye",
    "bye bye",
    "goodbye",
    "so",
    "uh",
    "um",
    "hmm",
    "oh",
    "the end",
    "subtitles by the amara org community",
    "transcription by castingwords",
    "i'm sorry",
];

/// Join Whisper segments into one clean line, or `None` when nothing
/// believable was said.
pub fn clean(segs: &[Segment], no_speech_threshold: f32) -> Option<(String, Option<f32>)> {
    let mut words: Vec<String> = Vec::new();
    let (mut psum, mut pn) = (0.0f32, 0usize);
    for s in segs {
        if s.no_speech.is_some_and(|p| p > no_speech_threshold) && s.prob.is_none_or(|p| p < 0.5) {
            continue;
        }
        let text = strip_noise_marks(&s.text);
        if text.trim().is_empty() {
            continue;
        }
        if let Some(p) = s.prob {
            psum += p;
            pn += 1;
        }
        words.extend(text.split_whitespace().map(str::to_string));
    }
    let words = collapse_repeats(words);
    let mut text = words.join(" ");
    let key: String =
        text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '\'').collect::<String>();
    let key = key.split_whitespace().collect::<Vec<_>>().join(" ");
    if key.is_empty() || HALLUCINATIONS.contains(&key.as_str()) {
        return None;
    }
    if text.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Some((text, (pn > 0).then(|| psum / pn as f32)))
}

/// Drop `[BLANK_AUDIO]`, `(static)`, `*crackle*` and similar marks.
fn strip_noise_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth: Option<char> = None;
    for c in s.chars() {
        match (depth, c) {
            (None, '[') => depth = Some(']'),
            (None, '(') => depth = Some(')'),
            (None, '*') => depth = Some('*'),
            (None, '♪') => {}
            (None, _) => out.push(c),
            (Some(close), _) if c == close => depth = None,
            (Some(_), _) => {}
        }
    }
    // An unclosed mark is kept as text.
    if depth.is_some() {
        return s.replace(['♪'], "");
    }
    out
}

/// Whisper sometimes loops ("roger roger roger roger ..."): keep at most
/// three back-to-back copies of the same word or phrase (up to four words).
fn collapse_repeats(mut words: Vec<String>) -> Vec<String> {
    let norm = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    for n in 1..=4 {
        let keys: Vec<String> = words.iter().map(|w| norm(w)).collect();
        let mut keep = vec![true; words.len()];
        let mut i = 0;
        while i + n <= words.len() {
            // How many times the n-gram at i repeats right after itself.
            let mut reps = 1;
            while i + (reps + 1) * n <= words.len()
                && keys[i + reps * n..i + (reps + 1) * n] == keys[i..i + n]
            {
                reps += 1;
            }
            if reps > 3 {
                for k in keep.iter_mut().take(i + reps * n).skip(i + 3 * n) {
                    *k = false;
                }
                i += reps * n;
            } else {
                i += 1;
            }
        }
        let mut it = keep.into_iter();
        words.retain(|_| it.next().unwrap_or(true));
    }
    words
}

// ---- running ----

/// Start transcription: the engine loads on its worker thread (a model
/// download can take a while), the collector follows the hub until the bus
/// closes. Transcripts still queued at the end are finished, up to a minute.
pub async fn run(
    mut rx: broadcast::Receiver<AudioEvent>,
    mut bus_rx: broadcast::Receiver<Arc<Message>>,
    bus: WeakBus,
    s: TranscribeSettings,
) -> std::io::Result<()> {
    let mut collector = match Collector::new(&s) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("airband transcription: {e}");
            return Ok(());
        }
    };
    let streams = collector.streams.clone();
    let queue = Arc::new(Queue::new(s.queue));
    // Provenance of each channel's latest transmission message, for the
    // transcript messages (same station/SDR/channel).
    let prov: Arc<Mutex<HashMap<u64, Provenance>>> = Default::default();
    set_announce(Some(Announce {
        engine: planned(&s),
        language: (s.language != "auto").then(|| s.language.clone()),
        streams,
    }));
    let worker = {
        let (queue, prov, s) = (queue.clone(), prov.clone(), s.clone());
        std::thread::Builder::new()
            .name("xng-whisper".into())
            .spawn(move || worker(queue, prov, bus, s))
            .map_err(std::io::Error::other)?
    };
    let mut warned_at = std::time::Instant::now() - Duration::from_secs(3600);
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    for clip in collector.step(&ev) {
                        let n = queue.push(clip);
                        if n > 0 && warned_at.elapsed() > Duration::from_secs(60) {
                            warned_at = std::time::Instant::now();
                            tracing::warn!(
                                "airband transcription is falling behind ({} transmissions skipped so far); \
                                 try a smaller model, more --whisper-threads or fewer channels",
                                queue.dropped.load(Ordering::Relaxed)
                            );
                        }
                    }
                }
                Err(RecvError::Lagged(n)) => tracing::warn!("airband transcription lagged, dropped {n} events"),
                Err(RecvError::Closed) => break,
            },
            m = bus_rx.recv() => match m {
                Ok(m) => {
                    if let MessageBody::Airband { kind, .. } = &m.body {
                        if kind == "tx_end" || kind == "tx_start" {
                            prov.lock().unwrap().insert(m.frequency_hz, m.source.clone());
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
        }
    }
    // Transmissions still open at the end are transcribed as they are.
    for (_, c) in collector.open.drain() {
        if c.samples.len() >= collector.min_samples {
            queue.push(c);
        }
    }
    queue.close();
    let done = tokio::task::spawn_blocking(move || worker.join());
    if tokio::time::timeout(Duration::from_secs(60), done).await.is_err() {
        let n = queue.clear();
        tracing::warn!("airband transcription: gave up on {n} queued transmissions at shutdown");
    }
    set_announce(None);
    Ok(())
}

/// Which engine and model a configuration will use, before it loads (for
/// the ATC OPEN, which goes out before a model download finishes).
pub fn planned(s: &TranscribeSettings) -> EngineInfo {
    match kind(s) {
        Kind::Http => EngineInfo {
            engine: s.engine.clone().unwrap_or_else(|| HTTP_ENGINE.into()),
            version: None,
            model: s.model.clone().unwrap_or_else(|| HTTP_MODEL.into()),
        },
        Kind::Whisper => EngineInfo {
            engine: WHISPER_ENGINE.into(),
            version: whisper_version(),
            model: model_label(s.model.as_deref().unwrap_or(DEFAULT_LOCAL_MODEL), "ggml-"),
        },
        Kind::Sherpa => EngineInfo {
            engine: SHERPA_ENGINE.into(),
            version: sherpa_version(),
            model: model_label(s.model.as_deref().unwrap_or(sherpa_models::DEFAULT), "sherpa-onnx-"),
        },
    }
}

/// A model's name for reports: `base.en` as given, or a file or directory
/// such as `/models/ggml-small.en-q5_1.bin` as `small.en-q5_1`.
fn model_label(m: &str, prefix: &str) -> String {
    let file = m.trim_end_matches(['/', '\\']);
    let file = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let file = file.strip_suffix(".bin").unwrap_or(file);
    file.strip_prefix(prefix).unwrap_or(file).to_string()
}

pub const WHISPER_ENGINE: &str = "whisper.cpp";
pub const SHERPA_ENGINE: &str = "sherpa-onnx";
/// What a Whisper server reports as unless `engine` names it.
pub const HTTP_ENGINE: &str = "openai-api";
const HTTP_MODEL: &str = "whisper-1";

fn whisper_version() -> Option<String> {
    #[cfg(feature = "whisper")]
    {
        Some(whisper_rs::get_whisper_version().to_string())
    }
    #[cfg(not(feature = "whisper"))]
    None
}

fn sherpa_version() -> Option<String> {
    #[cfg(feature = "sherpa")]
    {
        Some(sherpa_onnx::version().to_string())
    }
    #[cfg(not(feature = "sherpa"))]
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Http,
    Whisper,
    Sherpa,
}

/// Which engine runs: a server when `url` is set; else `engine` if given;
/// else a sherpa-onnx model name or directory picks sherpa-onnx, anything
/// else is whisper.cpp.
pub fn kind(s: &TranscribeSettings) -> Kind {
    if s.url.is_some() {
        return Kind::Http;
    }
    match s.engine.as_deref() {
        Some(WHISPER_ENGINE | "whisper") => Kind::Whisper,
        Some(SHERPA_ENGINE | "sherpa") => Kind::Sherpa,
        _ => match s.model.as_deref() {
            Some(m) if sherpa_models::find(m).is_some() || std::path::Path::new(m).is_dir() => Kind::Sherpa,
            _ => Kind::Whisper,
        },
    }
}

/// Model used when transcription is on and none is named.
pub const DEFAULT_LOCAL_MODEL: &str = "base.en";

fn worker(queue: Arc<Queue>, prov: Arc<Mutex<HashMap<u64, Provenance>>>, bus: WeakBus, s: TranscribeSettings) {
    let mut engine = match make_engine(&s) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("airband transcription disabled: {e:#}");
            set_announce(None);
            // Keep draining so the collector never blocks.
            while queue.pop().is_some() {}
            return;
        }
    };
    let info = engine.info();
    tracing::info!(
        "airband transcription with {} {}{}",
        info.engine,
        info.model,
        info.version.as_deref().map(|v| format!(" ({v})")).unwrap_or_default()
    );
    // The engine's own report wins over the guess made before it loaded.
    if let Some(mut a) = announced() {
        a.engine = info.clone();
        set_announce(Some(a));
    }
    let base_prompt = s.prompt.clone().unwrap_or_else(|| DEFAULT_PROMPT.to_string());
    let mut failures = 0u32;
    while let Some(clip) = queue.pop() {
        let prompt = match &clip.chan.label {
            Some(l) => format!("{l}. {base_prompt}"),
            None => base_prompt.clone(),
        };
        let t0 = std::time::Instant::now();
        let segs = match engine.transcribe(&clip.samples, &prompt) {
            Ok(v) => {
                failures = 0;
                v
            }
            Err(e) => {
                failures += 1;
                if failures <= 3 || failures.is_multiple_of(100) {
                    tracing::warn!("airband transcription of {}: {e:#}", clip.chan.display());
                }
                continue;
            }
        };
        let took = t0.elapsed();
        let Some((text, confidence)) = clean(&segs, s.no_speech_threshold) else {
            tracing::debug!("airband transcription of {} tx {}: nothing said", clip.chan.display(), clip.tx);
            continue;
        };
        tracing::debug!(
            "airband transcript {} tx {} ({:.1} s audio in {:.1} s): {text}",
            clip.chan.display(),
            clip.tx,
            clip.samples.len() as f64 / AUDIO_RATE as f64,
            took.as_secs_f64()
        );
        let t = Arc::new(Transcript {
            stream: clip.stream,
            chan: clip.chan.clone(),
            freq_hz: clip.chan.freq_hz,
            tx: clip.tx,
            start: clip.start,
            end: clip.end(),
            text,
            engine: info.engine.clone(),
            engine_version: info.version.clone(),
            model: info.model.clone(),
            language: (s.language != "auto").then(|| s.language.clone()),
            confidence,
        });
        if let Some(source) = prov.lock().unwrap().get(&clip.chan.freq_hz).cloned() {
            bus.publish(to_message(&t, source));
        }
        hub().publish(AudioEvent::Transcript(t));
    }
}

/// The transcript as an airband bus message (`kind` = `transcript`).
pub fn to_message(t: &Transcript, source: Provenance) -> Message {
    let mut details = serde_json::json!({
        "tx": t.tx,
        "label": t.chan.label,
        "start": t.start.to_rfc3339(),
        "end": t.end.to_rfc3339(),
        "text": t.text,
        "engine": t.engine,
        "model": t.model,
    });
    if let Some(v) = &t.engine_version {
        details["engine_version"] = serde_json::json!(v);
    }
    if let Some(l) = &t.language {
        details["language"] = serde_json::json!(l);
    }
    if let Some(c) = t.confidence {
        details["confidence"] = serde_json::json!((c as f64 * 1000.0).round() / 1000.0);
    }
    if let Some(k) = &t.chan.atc_frequency {
        details["atc_frequency"] = serde_json::json!(k);
    }
    Message {
        mode: Mode::Airband,
        timestamp: Utc::now(),
        frequency_hz: t.freq_hz,
        signal: SignalQuality::default(),
        decode: DecodeQuality { crc_ok: true, ..Default::default() },
        body: MessageBody::Airband { kind: "transcript".into(), details },
        raw: None,
        source,
    }
}

fn make_engine(s: &TranscribeSettings) -> anyhow::Result<Box<dyn Engine>> {
    match kind(s) {
        Kind::Http => Ok(Box::new(HttpEngine::new(s.url.as_deref().unwrap_or_default(), s)?)),
        #[cfg(feature = "whisper")]
        Kind::Whisper => Ok(Box::new(local::LocalEngine::new(s)?)),
        #[cfg(not(feature = "whisper"))]
        Kind::Whisper => {
            anyhow::bail!("xng was built without the `whisper` feature; use --whisper-url with a Whisper server")
        }
        #[cfg(feature = "sherpa")]
        Kind::Sherpa => Ok(Box::new(sherpa_models::SherpaEngine::new(s)?)),
        #[cfg(not(feature = "sherpa"))]
        Kind::Sherpa => anyhow::bail!("xng was built without the `sherpa` feature (sherpa-onnx models)"),
    }
}

// ---- HTTP engine ----

/// Whisper over HTTP: a multipart WAV upload.
pub struct HttpEngine {
    url: String,
    info: EngineInfo,
    model: String,
    language: Option<String>,
    key: Option<String>,
    agent: ureq::Agent,
}

impl HttpEngine {
    pub fn new(url: &str, s: &TranscribeSettings) -> anyhow::Result<Self> {
        Ok(Self {
            url: endpoint(url),
            info: planned(s),
            model: s.model.clone().unwrap_or_else(|| HTTP_MODEL.into()),
            language: (s.language != "auto").then(|| s.language.clone()),
            key: s.api_key.clone().or_else(|| std::env::var("XNG_WHISPER_API_KEY").ok()).filter(|k| !k.is_empty()),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout(Duration::from_secs(120))
                .build(),
        })
    }
}

/// `http://host:port` → `http://host:port/v1/audio/transcriptions`; a URL
/// with a path is used as given.
pub fn endpoint(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match rest.find('/') {
        Some(i) if !rest[i..].trim_end_matches('/').is_empty() => url.to_string(),
        _ => format!("{}/v1/audio/transcriptions", url.trim_end_matches('/')),
    }
}

impl Engine for HttpEngine {
    fn info(&self) -> EngineInfo {
        self.info.clone()
    }

    fn transcribe(&mut self, samples: &[f32], prompt: &str) -> anyhow::Result<Vec<Segment>> {
        let boundary = format!("xng{:016x}", rand_u64());
        let mut fields: Vec<(&str, String)> = vec![
            ("model", self.model.clone()),
            ("response_format", "verbose_json".into()),
            ("temperature", "0".into()),
            ("prompt", prompt.to_string()),
        ];
        if let Some(l) = &self.language {
            fields.push(("language", l.clone()));
        }
        let body = multipart(&boundary, &fields, &wav_bytes(samples));
        let mut req =
            self.agent.post(&self.url).set("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
        if let Some(k) = &self.key {
            req = req.set("Authorization", &format!("Bearer {k}"));
        }
        let resp = match req.send_bytes(&body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                anyhow::bail!("Whisper server answered {code}: {}", text.chars().take(200).collect::<String>())
            }
            Err(e) => anyhow::bail!("Whisper server: {e}"),
        };
        let text = resp.into_string()?;
        parse_response(&text)
    }
}

/// Read a transcription response: verbose JSON with segments, plain JSON
/// with `text`, or plain text.
pub fn parse_response(body: &str) -> anyhow::Result<Vec<Segment>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Ok(vec![Segment { text: body.trim().to_string(), ..Default::default() }]);
    };
    if let Some(e) = v.get("error") {
        anyhow::bail!("Whisper server error: {}", e.get("message").and_then(|m| m.as_str()).unwrap_or(&e.to_string()));
    }
    if let Some(segs) = v.get("segments").and_then(|s| s.as_array()).filter(|s| !s.is_empty()) {
        return Ok(segs
            .iter()
            .map(|s| Segment {
                text: s.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                no_speech: s.get("no_speech_prob").and_then(|p| p.as_f64()).map(|p| p as f32),
                prob: s.get("avg_logprob").and_then(|p| p.as_f64()).map(|p| (p.exp() as f32).clamp(0.0, 1.0)),
            })
            .collect());
    }
    let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
    Ok(vec![Segment { text, ..Default::default() }])
}

fn multipart(boundary: &str, fields: &[(&str, String)], wav: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(wav.len() + 1024);
    for (k, v) in fields {
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes(),
        );
    }
    b.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(wav);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    b
}

/// 16 kHz mono 16-bit WAV.
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&AUDIO_RATE.to_le_bytes());
    b.extend_from_slice(&(AUDIO_RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in xng_mode_airband::codec::to_i16(samples) {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

fn rand_u64() -> u64 {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (n as u64) ^ ((n >> 64) as u64).rotate_left(17) ^ (std::process::id() as u64) << 32
}

// ---- local whisper.cpp engine ----

#[cfg(feature = "whisper")]
mod local {
    use super::{Engine, Segment, TranscribeSettings, DEFAULT_LOCAL_MODEL};
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

    pub struct LocalEngine {
        // The state borrows nothing from the context but must not outlive
        // it; keep both.
        _ctx: WhisperContext,
        state: WhisperState,
        info: super::EngineInfo,
        language: Option<String>,
        threads: i32,
    }

    impl LocalEngine {
        pub fn new(s: &TranscribeSettings) -> anyhow::Result<Self> {
            whisper_rs::install_logging_hooks();
            let want = s.model.clone().unwrap_or_else(|| DEFAULT_LOCAL_MODEL.into());
            let path = super::model_path(&want)?;
            let ctx = WhisperContext::new_with_params(
                path.to_str().ok_or_else(|| anyhow::anyhow!("model path is not UTF-8"))?,
                WhisperContextParameters::default(),
            )
            .map_err(|e| anyhow::anyhow!("loading Whisper model {}: {e}", path.display()))?;
            let state = ctx.create_state().map_err(|e| anyhow::anyhow!("Whisper state: {e}"))?;
            let threads = if s.threads > 0 {
                s.threads as i32
            } else {
                // Leave a core for demodulation.
                std::thread::available_parallelism().map(|n| n.get().saturating_sub(1).clamp(1, 8)).unwrap_or(2)
                    as i32
            };
            Ok(Self {
                _ctx: ctx,
                state,
                info: super::planned(s),
                language: (s.language != "auto").then(|| s.language.clone()),
                threads,
            })
        }
    }

    impl Engine for LocalEngine {
        fn info(&self) -> super::EngineInfo {
            self.info.clone()
        }

        fn transcribe(&mut self, samples: &[f32], prompt: &str) -> anyhow::Result<Vec<Segment>> {
            // Whisper wants at least a second of audio; pad short clips.
            let mut padded;
            let audio = if samples.len() < 16_000 + 1_600 {
                padded = samples.to_vec();
                padded.resize(16_000 + 1_600, 0.0);
                &padded[..]
            } else {
                samples
            };
            let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            p.set_n_threads(self.threads);
            p.set_language(Some(self.language.as_deref().unwrap_or("auto")));
            p.set_detect_language(false);
            p.set_translate(false);
            p.set_no_context(true);
            p.set_no_timestamps(true);
            p.set_suppress_blank(true);
            p.set_suppress_nst(true);
            p.set_temperature(0.0);
            p.set_initial_prompt(prompt);
            p.set_print_special(false);
            p.set_print_progress(false);
            p.set_print_realtime(false);
            p.set_print_timestamps(false);
            self.state.full(p, audio).map_err(|e| anyhow::anyhow!("Whisper: {e}"))?;
            let mut out = Vec::new();
            for seg in self.state.as_iter() {
                let text = seg.to_str_lossy().map(|t| t.into_owned()).unwrap_or_default();
                let (mut sum, mut n) = (0.0f32, 0usize);
                for i in 0..seg.n_tokens() {
                    let Some(tok) = seg.get_token(i) else { continue };
                    let t = tok.to_str_lossy().map(|t| t.into_owned()).unwrap_or_default();
                    // Special tokens look like [_BEG_] or <|en|>.
                    if t.starts_with("[_") || t.starts_with("<|") {
                        continue;
                    }
                    sum += tok.token_probability();
                    n += 1;
                }
                out.push(Segment {
                    text,
                    no_speech: Some(seg.no_speech_probability()),
                    prob: (n > 0).then(|| sum / n as f32),
                });
            }
            Ok(out)
        }
    }
}

/// Known whisper.cpp model names (downloaded on first use).
pub const MODEL_NAMES: &[&str] = &[
    "tiny", "tiny.en", "base", "base.en", "small", "small.en", "medium", "medium.en", "large-v1", "large-v2",
    "large-v3", "large-v3-turbo",
];

/// Where a model name is cached: `$XNG_WHISPER_MODELS`, else
/// `$XDG_CACHE_HOME/xng/whisper`, else `~/.cache/xng/whisper`.
pub fn model_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("XNG_WHISPER_MODELS") {
        return d.into();
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| {
                let mut p = std::path::PathBuf::from(h);
                p.push(".cache");
                p
            })
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("xng").join("whisper")
}

/// A model file for `name`: an existing path is used as is; a known name
/// (optionally with a quantisation suffix such as `-q5_1`) is looked up in
/// [`model_dir`] and downloaded there once.
pub fn model_path(name: &str) -> anyhow::Result<std::path::PathBuf> {
    let p = std::path::Path::new(name);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    let known = MODEL_NAMES.iter().any(|m| name == *m || name.strip_prefix(m).is_some_and(|q| q.starts_with("-q")));
    anyhow::ensure!(
        known && !name.contains(['/', '\\']),
        "Whisper model {name:?} is neither a file nor a known model name ({})",
        MODEL_NAMES.join(", ")
    );
    let dir = model_dir();
    let file = dir.join(format!("ggml-{name}.bin"));
    if file.is_file() {
        return Ok(file);
    }
    std::fs::create_dir_all(&dir)?;
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin");
    tracing::info!("downloading Whisper model {name} to {} (once)", file.display());
    let resp = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .build()
        .get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("downloading {url}: {e}"))?;
    let tmp = file.with_extension(format!("part{}", std::process::id()));
    let r = (|| -> anyhow::Result<u64> {
        let mut out = std::fs::File::create(&tmp)?;
        let n = std::io::copy(&mut resp.into_reader(), &mut out)?;
        out.sync_all()?;
        Ok(n)
    })();
    match r {
        // Real models are tens of MB at least; anything tiny is an error page.
        Ok(n) if n > 1_000_000 => {
            std::fs::rename(&tmp, &file)?;
            tracing::info!("Whisper model {name}: {} MB", n / 1_000_000);
            Ok(file)
        }
        Ok(n) => {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("downloading {url}: got only {n} bytes")
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.context(format!("downloading {url}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_mode_airband::squelch::SquelchMode;
    use xng_mode_airband::Modulation;

    fn chan(f: u64) -> Arc<ChannelMeta> {
        Arc::new(ChannelMeta {
            freq_hz: f,
            label: Some("Tower".into()),
            key: format!("rtl:{f}"),
            atc_frequency: None,
            modulation: Modulation::Am,
            bandwidth_hz: 8_000.0,
            squelch: SquelchMode::Off,
            sdr: None,
        })
    }

    fn audio(f: u64, tx: u64, at: DateTime<Utc>, n: usize) -> AudioEvent {
        AudioEvent::Audio(Arc::new(super::super::hub::AudioFrame {
            stream: StreamId::Channel(f),
            chan: chan(f),
            tx,
            at,
            samples: vec![0.1; n].into(),
            discontinuity: false,
        }))
    }

    fn open(f: u64, tx: u64, at: DateTime<Utc>) -> AudioEvent {
        AudioEvent::Open {
            stream: StreamId::Channel(f),
            chan: chan(f),
            tx,
            at,
            level_dbfs: -20.0,
            noise_dbfs: -40.0,
        }
    }

    fn close(f: u64, tx: u64, at: DateTime<Utc>) -> AudioEvent {
        AudioEvent::Close {
            stream: StreamId::Channel(f),
            chan: chan(f),
            tx,
            at,
            duration_s: 1.0,
            peak_dbfs: -20.0,
            noise_dbfs: -40.0,
        }
    }

    #[test]
    fn collects_whole_transmissions_and_skips_short_ones() {
        let s = TranscribeSettings { min_ms: 500, ..Default::default() };
        let mut c = Collector::new(&s).unwrap();
        let t0 = Utc::now();
        assert!(c.step(&open(118_300_000, 1, t0)).is_empty());
        for i in 0..4 {
            let at = t0 + chrono::Duration::milliseconds(i * 250);
            assert!(c.step(&audio(118_300_000, 1, at, 4_000)).is_empty());
        }
        let clips = c.step(&close(118_300_000, 1, t0 + chrono::Duration::seconds(1)));
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].samples.len(), 16_000);
        assert_eq!(clips[0].tx, 1);
        assert_eq!(clips[0].start, t0);
        // 0.25 s transmission: too short.
        c.step(&open(118_300_000, 2, t0));
        c.step(&audio(118_300_000, 2, t0, 4_000));
        assert!(c.step(&close(118_300_000, 2, t0)).is_empty());
        // Unselected channels are ignored.
        let s = TranscribeSettings { streams: vec!["121.5".into()], ..Default::default() };
        let mut c = Collector::new(&s).unwrap();
        c.step(&open(118_300_000, 1, t0));
        c.step(&audio(118_300_000, 1, t0, 16_000));
        assert!(c.step(&close(118_300_000, 1, t0)).is_empty());
    }

    #[test]
    fn long_transmissions_come_in_pieces() {
        let s = TranscribeSettings { max_s: 2, ..Default::default() };
        let mut c = Collector::new(&s).unwrap();
        let t0 = Utc::now();
        c.step(&open(1, 7, t0));
        let mut clips = Vec::new();
        for i in 0..5 {
            clips.extend(c.step(&audio(1, 7, t0 + chrono::Duration::seconds(i), 16_000)));
        }
        clips.extend(c.step(&close(1, 7, t0 + chrono::Duration::seconds(5))));
        let lens: Vec<usize> = clips.iter().map(|c| c.samples.len()).collect();
        assert_eq!(lens, vec![32_000, 32_000, 16_000]);
        assert!(clips.iter().all(|c| c.tx == 7));
        assert_eq!(clips[1].start, t0 + chrono::Duration::seconds(2));
    }

    #[test]
    fn queue_drops_oldest() {
        let q = Queue::new(2);
        let t0 = Utc::now();
        for tx in 1..=3 {
            q.push(Clip { stream: StreamId::Channel(1), chan: chan(1), tx, start: t0, samples: vec![] });
        }
        q.close();
        assert_eq!(q.pop().unwrap().tx, 2);
        assert_eq!(q.pop().unwrap().tx, 3);
        assert!(q.pop().is_none());
        assert_eq!(q.dropped.load(Ordering::Relaxed), 1);
    }

    fn seg(t: &str) -> Segment {
        Segment { text: t.into(), ..Default::default() }
    }

    #[test]
    fn cleans_whisper_output() {
        assert_eq!(
            clean(&[seg(" United 1234,"), seg(" cleared to land runway 28 right.")], 0.6).unwrap().0,
            "United 1234, cleared to land runway 28 right."
        );
        assert!(clean(&[seg(" Thank you.")], 0.6).is_none());
        assert!(clean(&[seg("[BLANK_AUDIO]")], 0.6).is_none());
        assert!(clean(&[seg(" (static) ")], 0.6).is_none());
        assert_eq!(clean(&[seg("[radio noise] Delta 22, roger")], 0.6).unwrap().0, "Delta 22, roger");
        assert_eq!(
            clean(&[seg("roger roger roger roger roger roger")], 0.6).unwrap().0,
            "roger roger roger"
        );
        assert_eq!(
            clean(&[seg("go ahead go ahead go ahead go ahead go ahead")], 0.6).unwrap().0,
            "go ahead go ahead go ahead"
        );
        // A confident segment survives a high no-speech score; a weak one
        // does not.
        let s = |p| Segment { text: "Skywest 5521 contact ground".into(), no_speech: Some(0.9), prob: Some(p) };
        assert!(clean(&[s(0.8)], 0.6).is_some());
        assert!(clean(&[s(0.2)], 0.6).is_none());
        let (_, conf) = clean(&[Segment { text: "hello".into(), no_speech: None, prob: Some(0.5) }], 0.6).unwrap();
        assert_eq!(conf, Some(0.5));
        let long = "word ".repeat(1_000);
        assert!(clean(&[seg(&long)], 0.6).unwrap().0.len() <= MAX_TEXT);
    }

    #[test]
    fn endpoints_and_responses() {
        assert_eq!(endpoint("http://gpu:8000"), "http://gpu:8000/v1/audio/transcriptions");
        assert_eq!(endpoint("http://gpu:8000/"), "http://gpu:8000/v1/audio/transcriptions");
        assert_eq!(endpoint("http://gpu:8080/inference"), "http://gpu:8080/inference");
        let v = parse_response(
            r#"{"text":"a b","segments":[{"text":" a","avg_logprob":-0.1,"no_speech_prob":0.01},{"text":" b","avg_logprob":-2.0,"no_speech_prob":0.2}]}"#,
        )
        .unwrap();
        assert_eq!(v.len(), 2);
        assert!((v[0].prob.unwrap() - 0.905).abs() < 0.01);
        assert_eq!(parse_response(r#"{"text":"hello"}"#).unwrap()[0].text, "hello");
        assert_eq!(parse_response("plain words\n").unwrap()[0].text, "plain words");
        assert!(parse_response(r#"{"error":{"message":"bad key"}}"#).is_err());
    }

    #[test]
    fn wav_header() {
        let w = wav_bytes(&[0.0; 100]);
        assert_eq!(w.len(), 44 + 200);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
    }

    #[test]
    fn model_names() {
        assert!(model_path("../../etc/passwd").is_err());
        assert!(model_path("huge.en").is_err());
        let p = planned(&TranscribeSettings::default());
        assert_eq!((p.engine.as_str(), p.model.as_str()), ("whisper.cpp", "base.en"));
        let p = planned(&TranscribeSettings { model: Some("/m/ggml-small.en-q5_1.bin".into()), ..Default::default() });
        assert_eq!(p.model, "small.en-q5_1");
        let p = planned(&TranscribeSettings { model: Some("moonshine-base".into()), ..Default::default() });
        assert_eq!((p.engine.as_str(), p.model.as_str()), ("sherpa-onnx", "moonshine-base"));
        let http = TranscribeSettings { url: Some("http://h:8000".into()), ..Default::default() };
        let p = planned(&http);
        assert_eq!((p.engine.as_str(), p.model.as_str(), p.version), ("openai-api", "whisper-1", None));
        let p = planned(&TranscribeSettings { engine: Some("faster-whisper".into()), model: Some("jacktol/atc".into()), ..http });
        assert_eq!((p.engine.as_str(), p.model.as_str()), ("faster-whisper", "jacktol/atc"));
    }

    /// The HTTP engine against a stand-in server.
    #[test]
    fn http_engine_round_trip() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 65_536];
            // Read headers + body (Content-Length).
            loop {
                let n = s.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..h]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                        .unwrap();
                    if buf.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            let body = r#"{"text":" Delta 22 roger","segments":[{"text":" Delta 22 roger","avg_logprob":-0.2,"no_speech_prob":0.02}]}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                .unwrap();
            req
        });
        let s = TranscribeSettings {
            url: Some(format!("http://{addr}")),
            api_key: Some("k123".into()),
            ..Default::default()
        };
        let mut e = HttpEngine::new(s.url.as_deref().unwrap(), &s).unwrap();
        let segs = e.transcribe(&[0.0; 16_000], "Tower. hints").unwrap();
        let (text, _) = clean(&segs, 0.6).unwrap();
        assert_eq!(text, "Delta 22 roger");
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /v1/audio/transcriptions"));
        assert!(req.contains("Bearer k123"));
        assert!(req.contains("name=\"language\"\r\n\r\nen"));
        assert!(req.contains("name=\"prompt\"\r\n\r\nTower. hints"));
        assert!(req.contains("filename=\"audio.wav\""));
    }
}
