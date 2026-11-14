//! Embeddable decode engine: run one or more decode sessions in the
//! background and observe them, without the CLI's process-level plumbing
//! (signal handlers, blocking the caller's thread).
//!
//! This is the same decode path `xng listen` / `xng station` drive — the
//! same decoders, reassembly, dedup and outputs — packaged so a front end
//! (the desktop app) can start a session, subscribe to its messages, watch
//! the spectrum and per-channel stats, and stop it again.
//!
//! ```no_run
//! # fn demo(source: Box<dyn xng_sdr::IqSource>, cfg: xng::runtime::SessionConfig) -> anyhow::Result<()> {
//! use xng::engine::Engine;
//! let engine = Engine::start(vec![(source, cfg)], Default::default())?;
//! let mut rx = engine.subscribe();
//! // ... read messages from `rx`, poll `engine.sessions()[0].live` ...
//! engine.stop();
//! engine.join()?;
//! # Ok(()) }
//! ```

use crate::bus::MessageBus;
use crate::runtime::{self, LiveState, SessionConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use xng_sdr::IqSource;
use xng_types::{Message, Mode, StationIdentity};

/// How the engine runs.
#[derive(Clone, Default)]
pub struct EngineOptions {
    /// Stop on Ctrl-C / SIGTERM (a second signal force-quits the process),
    /// as the CLI does. Embedders that own their own lifecycle leave this
    /// off and call [`Engine::stop`].
    pub handle_signals: bool,
    /// Publish into this bus instead of a fresh one, so the caller can
    /// subscribe before the first message is decoded.
    pub bus: Option<MessageBus>,
}

/// One running decode session, as the engine resolved it.
pub struct SessionInfo {
    pub mode: Mode,
    /// Capture center actually tuned (Hz).
    pub center_hz: u64,
    /// Capture sample rate (S/s).
    pub sample_rate: f64,
    /// Channels being decoded (Hz).
    pub channels_hz: Vec<u64>,
    /// The session descriptor the dashboard / `xng status` show.
    pub descriptor: serde_json::Value,
    /// Live per-channel stats and spectrum for this session.
    pub live: Arc<LiveState>,
}

/// Where the engine is in its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineState {
    /// Decoding.
    Running,
    /// Every source ended or [`Engine::stop`] was honoured.
    Finished,
    /// The engine stopped on an error.
    Failed(String),
}

/// Per-channel decode summary reported when a session ends:
/// `(freq_hz, frames, crc_ok)`.
pub type ChannelSummary = (u64, u64, u64);

/// A running set of decode sessions sharing one message bus and one set of
/// outputs (taken from the first session's [`runtime::OutputConfig`], as in
/// `xng station`).
pub struct Engine {
    bus: MessageBus,
    stop: Arc<AtomicBool>,
    sessions: Vec<SessionInfo>,
    state: Arc<Mutex<EngineState>>,
    summary: Arc<Mutex<Vec<ChannelSummary>>>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl Engine {
    /// Build every session's decoders (so tuning errors surface here, before
    /// anything runs), then start decoding on a background thread.
    pub fn start(
        sessions: Vec<(Box<dyn IqSource>, SessionConfig)>,
        opts: EngineOptions,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!sessions.is_empty(), "no sessions to run");

        struct Prepared {
            source: Box<dyn IqSource>,
            decoders: Vec<(u64, runtime::ModeChannel)>,
            cfg: SessionConfig,
            capture_center: u64,
            live: Arc<LiveState>,
        }

        let multi = sessions.len() > 1;
        let mut prepared = Vec::with_capacity(sessions.len());
        let mut infos = Vec::with_capacity(sessions.len());
        for (source, cfg) in sessions {
            let sample_rate = source.sample_rate();
            let capture_center =
                if cfg.center_hz > 0 { cfg.center_hz } else { source.center_freq_hz() };
            let decoders = runtime::build_decoders(sample_rate, capture_center, &cfg).map_err(|e| {
                if multi { anyhow::anyhow!("[{}] {e}", cfg.mode) } else { e }
            })?;
            tracing::info!(
                "engine session: {} with {} channel(s) at {:.0} S/s centered {:.3} MHz",
                cfg.mode,
                cfg.channels_hz.len(),
                sample_rate,
                capture_center as f64 / 1e6
            );
            let descriptor = runtime::session_descriptor(
                &cfg.sdr,
                cfg.mode,
                capture_center,
                &cfg.channels_hz,
                sample_rate,
                cfg.receiver_pos,
            );
            let live = LiveState::new();
            infos.push(SessionInfo {
                mode: cfg.mode,
                center_hz: capture_center,
                sample_rate,
                channels_hz: cfg.channels_hz.clone(),
                descriptor,
                live: live.clone(),
            });
            prepared.push(Prepared { source, decoders, cfg, capture_center, live });
        }

        let bus = opts.bus.unwrap_or_default();
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(EngineState::Running));
        let summary = Arc::new(Mutex::new(Vec::new()));
        let descriptors: Vec<serde_json::Value> =
            infos.iter().map(|i| i.descriptor.clone()).collect();
        let station = StationIdentity::new(prepared[0].cfg.station_ident.clone());
        let outputs = prepared[0].cfg.outputs.clone();
        let metrics_live = infos[0].live.clone();
        let metrics_label =
            if multi { "station".to_string() } else { infos[0].mode.as_str().to_string() };

        let thread = std::thread::Builder::new().name("xng-engine".into()).spawn({
            let (bus, stop, state, summary) = (bus.clone(), stop.clone(), state.clone(), summary.clone());
            move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("xng-engine-io")
                    .build()?;
                let result: anyhow::Result<()> = rt.block_on(async {
                    if let Some(addr) = outputs.metrics.clone() {
                        if multi {
                            tracing::info!("engine metrics: per-channel series from the first session");
                        }
                        tokio::spawn(async move {
                            if let Err(e) =
                                crate::outputs::metrics::serve(addr, metrics_live, metrics_label).await
                            {
                                tracing::warn!("metrics endpoint failed: {e}");
                            }
                        });
                    }
                    let output_tasks = runtime::spawn_outputs(&bus, &outputs, &station, &descriptors);
                    if let (Some(mmsi), Some(pos)) =
                        (outputs.own_ship_mmsi, prepared.iter().find_map(|p| p.cfg.receiver_pos))
                    {
                        runtime::spawn_own_ship_beacon(&bus, &stop, &station, mmsi, pos);
                    }
                    if opts.handle_signals {
                        runtime::spawn_interrupt_handler(stop.clone(), "engine");
                    }

                    let mut decode_tasks = Vec::new();
                    for mut prep in prepared {
                        let (bus, stop, station) = (bus.clone(), stop.clone(), station.clone());
                        decode_tasks.push(tokio::task::spawn_blocking(move || {
                            let mut reasm = (
                                xng_acars::reasm::Reassembler::new(runtime::reasm_timeout_s(prep.cfg.mode)),
                                xng_acars::miam::FileReassembler::new(),
                            );
                            let sample_rate = prep.source.sample_rate();
                            runtime::decode_loop(
                                &mut *prep.source,
                                std::mem::take(&mut prep.decoders),
                                station,
                                prep.cfg.sdr.clone(),
                                bus,
                                stop,
                                Some((prep.live, prep.capture_center, sample_rate)),
                                Some(&mut reasm),
                                prep.cfg.label_filter.clone(),
                                prep.cfg.ais_filter.clone(),
                            )
                        }));
                    }
                    let mut first_err = None;
                    for t in decode_tasks {
                        match t.await? {
                            Ok(stats) => summary.lock().unwrap().extend(stats),
                            Err(e) => {
                                tracing::warn!("session ended with error: {e}");
                                first_err.get_or_insert(e);
                            }
                        }
                    }
                    // Every decoder is done: make sure auxiliary tasks
                    // (own-ship beacon) see the stop, then close the bus
                    // so the outputs drain and exit.
                    stop.store(true, Ordering::Relaxed);
                    drop(bus);
                    for t in output_tasks {
                        if let Err(e) = t.await? {
                            tracing::warn!("output error: {e}");
                        }
                    }
                    first_err.map_or(Ok(()), Err)
                });
                *state.lock().unwrap() = match &result {
                    Ok(()) => EngineState::Finished,
                    Err(e) => EngineState::Failed(format!("{e:#}")),
                };
                // Let output tasks still parked on sockets go without
                // blocking the join.
                rt.shutdown_background();
                result
            }
        })?;

        Ok(Self { bus, stop, sessions: infos, state, summary, thread: Some(thread) })
    }

    /// Subscribe to every decoded message. Slow subscribers lag (dropping
    /// the oldest) rather than back-pressuring the decoders.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Message>> {
        self.bus.subscribe()
    }

    /// The bus the sessions publish into.
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }

    /// The running sessions, in the order they were given.
    pub fn sessions(&self) -> &[SessionInfo] {
        &self.sessions
    }

    /// Ask every session to stop. Returns immediately; [`Engine::join`]
    /// waits for the sources to close.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Current lifecycle state.
    pub fn state(&self) -> EngineState {
        self.state.lock().unwrap().clone()
    }

    /// True once every session has ended (source exhausted, stopped, or
    /// failed).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Per-channel frame counts, filled in as sessions end.
    pub fn summary(&self) -> Vec<ChannelSummary> {
        self.summary.lock().unwrap().clone()
    }

    /// Wait for the engine to finish and return how it ended.
    pub fn join(mut self) -> anyhow::Result<()> {
        self.join_inner()
    }

    fn join_inner(&mut self) -> anyhow::Result<()> {
        match self.thread.take() {
            Some(t) => t.join().map_err(|_| anyhow::anyhow!("engine thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for Engine {
    /// Dropping a running engine stops it and waits for the sources to
    /// close, so an SDR is never left streaming.
    fn drop(&mut self) {
        self.stop();
        let _ = self.join_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outputs::console::ConsoleFormat;
    use xng_sdr::{FileIqSource, IqFormat};

    fn quiet_outputs() -> runtime::OutputConfig {
        runtime::OutputConfig {
            console: ConsoleFormat::Off,
            jsonl: None,
            udp: vec![],
            asf2_grpc: None,
            asf2_quic: None,
            asf2_quic_trust: crate::outputs::asf2_quic::TrustMode::SystemRoots,
            metrics: None,
            sbs: None,
            beast: None,
            nmea_tcp: None,
            nmea_udp: None,
            nmea_tag_blocks: false,
            gsmtap: None,
            iridium_satmap: None,
            http: None,
            map: Default::default(),
            mqtt: None,
            mqtt_topic: "xng".into(),
            zmq: vec![],
            airframes: None,
            own_ship_mmsi: None,
        }
    }

    fn silent_file(dir: &std::path::Path, samples: usize) -> std::path::PathBuf {
        let path = dir.join("silence.cf32");
        std::fs::write(&path, vec![0u8; samples * 8]).unwrap();
        path
    }

    fn cfg(mode: Mode, center_hz: u64, channels_hz: Vec<u64>) -> SessionConfig {
        SessionConfig {
            mode,
            center_hz,
            channels_hz,
            station_ident: "XNG-TEST".into(),
            sdr: None,
            outputs: quiet_outputs(),
            receiver_pos: None,
            label_filter: Default::default(),
            ais_filter: Default::default(),
            demod_effort: runtime::DemodEffort::Live,
            max_ppm: None,
        }
    }

    fn tempdir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("xng-engine-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn runs_a_file_session_to_completion() {
        let dir = tempdir();
        let path = silent_file(&dir, 2_400_000 / 4);
        let src = FileIqSource::open(&path, IqFormat::Cf32, 2_400_000.0, 131_500_000).unwrap();
        let engine = Engine::start(
            vec![(Box::new(src), cfg(Mode::AcarsPoa, 131_500_000, vec![131_550_000, 131_725_000]))],
            EngineOptions::default(),
        )
        .unwrap();
        assert_eq!(engine.sessions().len(), 1);
        assert_eq!(engine.sessions()[0].channels_hz, vec![131_550_000, 131_725_000]);
        let live = engine.sessions()[0].live.clone();
        let summary_len = {
            while !engine.is_finished() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            engine.summary().len()
        };
        assert_eq!(engine.state(), EngineState::Finished);
        assert_eq!(summary_len, 2);
        assert!(live.samples.load(Ordering::Relaxed) > 0);
        engine.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_a_channel_outside_the_capture() {
        let dir = tempdir();
        let path = silent_file(&dir, 1024);
        let src = FileIqSource::open(&path, IqFormat::Cf32, 2_400_000.0, 131_500_000).unwrap();
        let err = Engine::start(
            vec![(Box::new(src), cfg(Mode::AcarsPoa, 131_500_000, vec![136_900_000]))],
            EngineOptions::default(),
        )
        .err()
        .expect("out-of-capture channel must fail");
        assert!(err.to_string().contains("outside the capture"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stop_ends_an_endless_source() {
        struct Endless;
        impl IqSource for Endless {
            fn read(&mut self, buf: &mut [num_complex::Complex<f32>]) -> Result<usize, xng_sdr::SdrError> {
                std::thread::sleep(std::time::Duration::from_millis(1));
                buf.fill(num_complex::Complex::new(0.0, 0.0));
                Ok(buf.len())
            }
            fn sample_rate(&self) -> f64 {
                2_400_000.0
            }
            fn center_freq_hz(&self) -> u64 {
                131_500_000
            }
        }
        let engine = Engine::start(
            vec![(Box::new(Endless), cfg(Mode::AcarsPoa, 131_500_000, vec![131_550_000]))],
            EngineOptions::default(),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(engine.state(), EngineState::Running);
        engine.stop();
        engine.join().unwrap();
    }
}
