//! A running decode session and the pump that feeds the UI.
//!
//! The engine decodes on its own threads exactly as the CLI does. The pump
//! is a single extra thread that drains the message bus, folds messages
//! into the shared dashboard aggregator, and sends the UI batched updates
//! at fixed rates — so a burst of thousands of messages a second costs the
//! UI a handful of IPC calls, never one per message.

use crate::request::{self, SessionRequest, Source};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::Channel;
use tokio::sync::broadcast::error::TryRecvError;
use xng::engine::{Engine, EngineOptions, EngineState};
use xng::outputs::console;
use xng::outputs::http::Dashboard;
use xng::runtime::SessionConfig;
use xng_sdr::{FileIqSource, IqFormat, IqSource};
use xng_types::{Message, SdrInfo};

/// Messages sent per batch at most; the rest of a burst is counted as
/// skipped (the outputs and the dashboard still see every message).
const MAX_BATCH: usize = 500;
const MESSAGE_EVERY: Duration = Duration::from_millis(120);
const LIVE_EVERY: Duration = Duration::from_millis(100);
const ENTITIES_EVERY: Duration = Duration::from_millis(1000);
const TICK: Duration = Duration::from_millis(25);

/// One decoded message as the UI shows it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageItem {
    pub id: u64,
    /// Reception time, unix milliseconds.
    pub ts: i64,
    pub mode: &'static str,
    pub freq: u64,
    pub crc_ok: bool,
    pub rssi: Option<f32>,
    pub snr: Option<f32>,
    /// The CLI's one-line summary of what was decoded.
    pub summary: String,
    /// The full normalized message (what `--json` prints).
    pub detail: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStat {
    pub freq: u64,
    pub frames: u64,
    pub crc_ok: u64,
    pub level_db: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLive {
    pub samples: u64,
    pub channels: Vec<ChannelStat>,
    /// FFT-shifted power spectrum (dBFS), when one has been computed since
    /// the last update.
    pub spectrum: Option<Vec<f32>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub mode: &'static str,
    pub center_hz: u64,
    pub sample_rate: f64,
    pub channels_hz: Vec<u64>,
    pub source: String,
}

/// Everything the pump tells the UI.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    #[serde(rename_all = "camelCase")]
    Started { station: String, sessions: Vec<SessionSummary> },
    #[serde(rename_all = "camelCase")]
    /// `skipped` messages were left out of `items`; `lagged` of those
    /// were lost to this reader altogether, so the map missed them too.
    Messages { items: Vec<MessageItem>, skipped: u64, lagged: u64 },
    #[serde(rename_all = "camelCase")]
    Live { sessions: Vec<SessionLive>, elapsed_ms: u64 },
    #[serde(rename_all = "camelCase")]
    Entities { state: serde_json::Value },
    #[serde(rename_all = "camelCase")]
    Ended { error: Option<String> },
}

/// A session the app is running.
pub struct Running {
    engine: Arc<Engine>,
    quit: Arc<AtomicBool>,
    pump: Option<std::thread::JoinHandle<()>>,
    dashboard: Arc<Mutex<Dashboard>>,
}

impl Running {
    /// Open the source, build the engine and start pumping events to `ch`.
    pub fn start(req: &SessionRequest, ch: Channel<Event>) -> anyhow::Result<Self> {
        let (mode, rate, center_hz, channels_hz) = request::resolve(req)?;
        let tune = req.tune()?;
        let outputs = req.output_config(mode)?;
        let station_ident = req.station_ident();

        let (source, sdr, source_label): (Box<dyn IqSource>, SdrInfo, String) = match &req.source {
            Source::Device { selector, gain } => {
                let (src, backend) = xng::sdr_open::open_sdr(selector, rate, center_hz, *gain)?;
                let serial = xng::sdr_args::SdrArgs::parse(selector).serial;
                let info = SdrInfo { id: selector.clone(), driver: backend.into(), serial };
                (src, info, if selector.is_empty() { "first SDR found".into() } else { selector.clone() })
            }
            Source::File { path, format } => {
                let fmt = match request::nonempty(format) {
                    Some(f) => f.parse::<IqFormat>().map_err(|e| anyhow::anyhow!(e))?,
                    None => IqFormat::from_extension(path).ok_or_else(|| {
                        anyhow::anyhow!("Can't tell this recording's sample format; choose cf32, cs16, cs8 or cu8")
                    })?,
                };
                let src = FileIqSource::open(path, fmt, rate, center_hz)?;
                let info = SdrInfo { id: "file".into(), driver: "file".into(), serial: None };
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                (Box::new(src), info, name)
            }
        };

        let demod_default = match req.source {
            Source::Device { .. } => xng::runtime::DemodEffort::Live,
            Source::File { .. } => xng::runtime::DemodEffort::Max,
        };
        let cfg = SessionConfig {
            mode,
            center_hz,
            channels_hz,
            station_ident: station_ident.clone(),
            sdr: Some(sdr),
            outputs,
            receiver_pos: xng::tuning::parse_receiver_pos(&tune.receiver_pos)?,
            label_filter: tune.label_filter(),
            ais_filter: req.ais_filter(),
            demod_effort: tune.demod_effort.unwrap_or(demod_default),
            max_ppm: tune.max_ppm,
        };

        let engine = Arc::new(Engine::start(vec![(source, cfg)], EngineOptions { handle_signals: false })?);
        // The first subscription was attached before decoding began.
        let rx = engine.subscribe();

        let summaries: Vec<SessionSummary> = engine
            .sessions()
            .iter()
            .map(|s| SessionSummary {
                mode: s.mode.as_str(),
                center_hz: s.center_hz,
                sample_rate: s.sample_rate,
                channels_hz: s.channels_hz.clone(),
                source: source_label.clone(),
            })
            .collect();
        let descriptors = engine.sessions().iter().map(|s| s.descriptor.clone()).collect();
        let dashboard = Arc::new(Mutex::new(Dashboard::new(station_ident.clone(), descriptors)));
        let _ = ch.send(Event::Started { station: station_ident, sessions: summaries });

        let quit = Arc::new(AtomicBool::new(false));
        let pump = std::thread::Builder::new().name("xng-ui-pump".into()).spawn({
            let (engine, quit, dashboard) = (engine.clone(), quit.clone(), dashboard.clone());
            move || pump(engine, rx, ch, quit, dashboard)
        })?;
        Ok(Self { engine, quit, pump: Some(pump), dashboard })
    }

    /// Stop decoding and wait for the source to close.
    pub fn stop(mut self) {
        self.engine.stop();
        if let Some(p) = self.pump.take() {
            let _ = p.join();
        }
        // Dropping the last engine handle joins its threads.
    }

    /// A handle to the dashboard that outlives the session, so the final
    /// picture can still be exported after decoding has stopped.
    pub fn dashboard(&self) -> Arc<Mutex<Dashboard>> {
        self.dashboard.clone()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        self.engine.stop();
        if let Some(p) = self.pump.take() {
            let _ = p.join();
        }
    }
}

fn item(id: u64, m: &Message) -> MessageItem {
    MessageItem {
        id,
        ts: m.timestamp.timestamp_millis(),
        mode: m.mode.as_str(),
        freq: m.frequency_hz,
        crc_ok: m.decode.crc_ok,
        rssi: m.signal.rssi_db,
        snr: m.signal.snr_db,
        summary: console::summary(m),
        detail: serde_json::to_value(m).unwrap_or(serde_json::Value::Null),
    }
}

fn pump(
    engine: Arc<Engine>,
    mut rx: tokio::sync::broadcast::Receiver<Arc<Message>>,
    ch: Channel<Event>,
    quit: Arc<AtomicBool>,
    dashboard: Arc<Mutex<Dashboard>>,
) {
    let started = Instant::now();
    let (mut last_msgs, mut last_live, mut last_ents) = (started, started, started);
    let mut batch: Vec<MessageItem> = Vec::new();
    let (mut skipped, mut lagged): (u64, u64) = (0, 0);
    let mut next_id: u64 = 0;
    let mut closed = false;

    loop {
        // Drain whatever arrived since the last tick, but not for longer
        // than a tick: a sustained burst must not hold up the timed
        // updates below. What is left stays queued for the next pass.
        let drain_started = Instant::now();
        let mut drained = 0u32;
        loop {
            drained += 1;
            if drained % 64 == 0 && drain_started.elapsed() >= TICK {
                break;
            }
            match rx.try_recv() {
                Ok(m) => {
                    next_id += 1;
                    dashboard.lock().unwrap().update(&m);
                    if batch.len() < MAX_BATCH {
                        batch.push(item(next_id, &m));
                    } else {
                        skipped += 1;
                    }
                }
                // The bus overwrote these before the pump read them: they
                // never reached the dashboard, so report them apart.
                Err(TryRecvError::Lagged(n)) => {
                    skipped += n;
                    lagged += n;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    closed = true;
                    break;
                }
            }
        }

        let now = Instant::now();
        // Only a closed bus ends the session. The engine closes it before
        // it reports itself finished, so going by `is_finished` alone could
        // leave the last messages unread.
        let finished = closed;
        if (now - last_msgs >= MESSAGE_EVERY || finished) && (!batch.is_empty() || skipped > 0) {
            let items = std::mem::take(&mut batch);
            let _ = ch.send(Event::Messages { items, skipped, lagged });
            (skipped, lagged) = (0, 0);
            last_msgs = now;
        }
        if now - last_live >= LIVE_EVERY || finished {
            let sessions = engine
                .sessions()
                .iter()
                .map(|s| {
                    let mut channels: Vec<ChannelStat> = s
                        .live
                        .stats
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|&(freq, frames, crc_ok, level_db)| ChannelStat { freq, frames, crc_ok, level_db })
                        .collect();
                    channels.sort_by_key(|c| c.freq);
                    SessionLive {
                        samples: s.live.samples.load(Ordering::Relaxed),
                        channels,
                        spectrum: s.live.spectrum.lock().unwrap().take().map(|f| f.bins_db),
                    }
                })
                .collect();
            let _ = ch.send(Event::Live { sessions, elapsed_ms: started.elapsed().as_millis() as u64 });
            last_live = now;
        }
        if now - last_ents >= ENTITIES_EVERY || finished {
            let state = dashboard.lock().unwrap().entities();
            let _ = ch.send(Event::Entities { state });
            last_ents = now;
        }
        if finished {
            // Wait for the engine's own verdict before reporting.
            while !engine.is_finished() && !quit.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
            }
            let error = match engine.state() {
                EngineState::Failed(e) => Some(e),
                _ => None,
            };
            let _ = ch.send(Event::Ended { error });
            return;
        }
        if quit.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tauri::ipc::InvokeResponseBody;
    use xng_mode_acars::modulate::{burst_iq, FrameSpec};

    /// A 2.4 MS/s capture centred on 131.500 MHz with one ACARS burst on
    /// 131.550 MHz, written as cf32.
    fn capture() -> std::path::PathBuf {
        let fs = 2_400_000.0;
        let burst = burst_iq(
            &FrameSpec {
                mode: '2',
                tail: "N471XG",
                ack: None,
                label: "H1",
                block_id: '3',
                msg_num: Some("M42A"),
                flight: Some("XG0042"),
                text: "POSN 4737.2N 12218.1W AT 120000Z",
                etb: false,
            },
            fs,
            50_000.0,
            0.4,
        );
        let mut iq = vec![num_complex::Complex::new(0.0f32, 0.0f32); burst.len() + 72_000];
        for (i, s) in burst.iter().enumerate() {
            iq[i + 24_000] += s;
        }
        let mut bytes = Vec::with_capacity(iq.len() * 8);
        for s in &iq {
            bytes.extend_from_slice(&s.re.to_le_bytes());
            bytes.extend_from_slice(&s.im.to_le_bytes());
        }
        let path = std::env::temp_dir().join(format!("xng-desktop-test-{}.cf32", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The whole path the UI drives: a request as the front end sends it,
    /// through the real engine, back out as the events the UI renders.
    #[test]
    fn decodes_a_recording_end_to_end() {
        let path = capture();
        let req: SessionRequest = serde_json::from_value(serde_json::json!({
            "source": { "kind": "file", "path": path, "format": null },
            "mode": "acars",
            "sampleRate": 2400000.0,
            "centerFreq": "131.500M",
            "channels": ["131.550"],
        }))
        .unwrap();

        let (tx, rx) = mpsc::channel::<serde_json::Value>();
        let tx = std::sync::Mutex::new(tx);
        let ch: Channel<Event> = Channel::new(move |body| {
            if let InvokeResponseBody::Json(s) = body {
                let _ = tx.lock().unwrap().send(serde_json::from_str(&s).unwrap());
            }
            Ok(())
        });
        let running = Running::start(&req, ch).unwrap();

        let mut kinds = Vec::new();
        let mut summaries = Vec::new();
        let mut saw_spectrum = false;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let ev = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("session should end");
            let kind = ev["type"].as_str().unwrap().to_owned();
            if kind == "messages" {
                for m in ev["items"].as_array().unwrap() {
                    summaries.push(m["summary"].as_str().unwrap().to_owned());
                    assert_eq!(m["mode"], "acars");
                    assert_eq!(m["freq"], 131_550_000u64);
                    assert_eq!(m["detail"]["body"]["tail"], "N471XG");
                }
            }
            if kind == "live" && !ev["sessions"][0]["spectrum"].is_null() {
                saw_spectrum = true;
                assert_eq!(ev["sessions"][0]["spectrum"].as_array().unwrap().len(), 512);
            }
            if kind == "ended" {
                assert!(ev["error"].is_null(), "{ev}");
                kinds.push(kind);
                break;
            }
            kinds.push(kind);
        }
        assert_eq!(kinds.first().map(String::as_str), Some("started"));
        assert!(kinds.iter().any(|k| k == "entities"));
        assert!(saw_spectrum, "a spectrum frame should reach the UI");
        assert_eq!(summaries.len(), 1, "{summaries:?}");
        assert!(summaries[0].contains("N471XG XG0042 lbl=H1 ok"), "{summaries:?}");
        // The shared dashboard saw the same message.
        let state = running.dashboard().lock().unwrap().entities();
        assert_eq!(state["totals"]["acars"], 1);
        running.stop();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_recording_without_a_sample_rate_is_refused_plainly() {
        let req: SessionRequest = serde_json::from_value(serde_json::json!({
            "source": { "kind": "file", "path": "/nonexistent.cf32", "format": null },
            "mode": "acars",
        }))
        .unwrap();
        let err = request::resolve(&req).unwrap_err().to_string();
        assert!(err.contains("sample rate"), "{err}");
    }

    #[test]
    fn feeding_airframes_requires_a_station_id() {
        let req: SessionRequest = serde_json::from_value(serde_json::json!({
            "source": { "kind": "device", "selector": "", "gain": null },
            "mode": "acars",
            "outputs": { "feedAirframes": true },
        }))
        .unwrap();
        let err = req.output_config(xng_types::Mode::AcarsPoa).err().expect("must be refused").to_string();
        assert!(err.contains("station ID"), "{err}");
    }
}
