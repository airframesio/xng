//! Tools: `xng scan`, `xng survey` and `xng ingest` run from the app.
//!
//! All are the CLI's own code (`xng::commands::{scan, survey}`), driven
//! through a [`Reporter`] that forwards their progress lines to the UI and
//! lets Stop wind them down early. The report they return is the same
//! structure the CLI writes with `--out` (ingest reports what it received).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use tauri::ipc::Channel;
use xng::commands::{ingest, scan, survey, Reporter};
use xng_types::Mode;

/// What the UI asks a tool to do. Fields mirror the CLI flags.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolRequest {
    /// `xng scan`
    #[serde(rename_all = "camelCase")]
    Scan {
        sdr: String,
        gain: Option<f64>,
        modes: Vec<String>,
        dwell_secs: u64,
    },
    /// `xng survey`
    #[serde(rename_all = "camelCase")]
    Survey {
        sdr: String,
        gain: Option<f64>,
        mode: String,
        sample_rate: Option<f64>,
        #[serde(default)]
        channels: Vec<String>,
        duration_secs: u64,
        interim_secs: u64,
        scan_first: bool,
        scan_dwell_secs: u64,
        tune_gain: bool,
        tune_dwell_secs: u64,
        rotate_dwell_secs: u64,
        show_messages: bool,
    },
    /// `xng ingest`: the reference asf-2.0 server.
    #[serde(rename_all = "camelCase")]
    Ingest {
        /// gRPC listen address, e.g. `0.0.0.0:6001`.
        grpc: Option<String>,
        /// QUIC listen address, e.g. `0.0.0.0:6011`.
        quic: Option<String>,
        /// Where to write the QUIC certificate for feeders to pin.
        quic_cert_out: Option<String>,
    },
}

/// What a running tool tells the UI.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolEvent {
    /// One line of progress, as the CLI would print it.
    Line { text: String },
    /// Finished. `stopped` is set when the user ended it early; the report
    /// then covers what was measured up to that point.
    Done { report: serde_json::Value, stopped: bool },
    Failed { message: String },
}

/// A scan or survey in progress.
pub struct ToolTask {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ToolTask {
    /// Validate `request` and run it on its own thread, holding `lock`
    /// (the app's SDR lock) for as long as it has a device open. A tool
    /// that uses no radio never takes the lock.
    ///
    /// `cancel` is the tool's stop flag. The caller makes it, so a stop
    /// asked for before the tool exists is already set when its thread
    /// starts: the tool then ends at its first check and still reports.
    pub fn start<G: 'static>(
        request: ToolRequest,
        events: Channel<ToolEvent>,
        lock: fn() -> G,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        let job = Job::parse(request)?;
        let thread = {
            let cancel = cancel.clone();
            std::thread::Builder::new().name("xng-tool".into()).spawn(move || {
                let _held = job.uses_radio().then(lock);
                run(&job, &events, &cancel);
            })?
        };
        Ok(Self { cancel, thread: Some(thread) })
    }

    /// Ask the tool to finish early. It still sends its report.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Cancel and wait until the device is released.
    pub fn stop(self) {
        self.cancel();
        self.wait();
    }

    /// Wait for the tool to finish on its own.
    pub fn wait(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A request with its modes and frequencies parsed.
enum Job {
    Scan { sdr: String, gain: Option<f64>, modes: Vec<Mode>, dwell_secs: u64 },
    Survey(SurveyJob),
    Ingest { grpc: Option<String>, quic: Option<String>, quic_cert_out: Option<std::path::PathBuf> },
}

/// `SurveyOpts` holds file paths and is not `Clone`; keep the pieces.
struct SurveyJob {
    sdr: String,
    gain: Option<f64>,
    mode: Mode,
    sample_rate: Option<f64>,
    channels: Vec<u64>,
    duration_secs: u64,
    interim_secs: u64,
    scan_first: bool,
    scan_dwell_secs: u64,
    tune_gain: bool,
    tune_dwell_secs: u64,
    rotate_dwell_secs: u64,
    show_messages: bool,
}

fn parse_mode(s: &str) -> anyhow::Result<Mode> {
    s.parse().map_err(|e: String| anyhow::anyhow!(e))
}

fn filled(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

impl Job {
    fn uses_radio(&self) -> bool {
        !matches!(self, Job::Ingest { .. })
    }

    fn parse(request: ToolRequest) -> anyhow::Result<Self> {
        Ok(match request {
            ToolRequest::Ingest { grpc, quic, quic_cert_out } => {
                let (grpc, quic) = (filled(grpc), filled(quic));
                anyhow::ensure!(grpc.is_some() || quic.is_some(), "Turn on gRPC, QUIC or both");
                for addr in grpc.iter().chain(&quic) {
                    addr.parse::<std::net::SocketAddr>()
                        .map_err(|_| anyhow::anyhow!("{addr} is not an address like 0.0.0.0:6001"))?;
                }
                Job::Ingest { grpc, quic, quic_cert_out: filled(quic_cert_out).map(Into::into) }
            }
            ToolRequest::Scan { sdr, gain, modes, dwell_secs } => {
                anyhow::ensure!(!modes.is_empty(), "Pick at least one signal to scan for");
                anyhow::ensure!(dwell_secs > 0, "The dwell time must be at least one second");
                let modes = modes.iter().map(|m| parse_mode(m)).collect::<anyhow::Result<_>>()?;
                Job::Scan { sdr, gain, modes, dwell_secs }
            }
            ToolRequest::Survey {
                sdr,
                gain,
                mode,
                sample_rate,
                channels,
                duration_secs,
                interim_secs,
                scan_first,
                scan_dwell_secs,
                tune_gain,
                tune_dwell_secs,
                rotate_dwell_secs,
                show_messages,
            } => {
                anyhow::ensure!(duration_secs > 0, "The survey must run for at least one second");
                anyhow::ensure!(
                    interim_secs > 0 && rotate_dwell_secs > 0 && scan_dwell_secs > 0 && tune_dwell_secs > 0,
                    "Dwell and interval times must be at least one second"
                );
                Job::Survey(SurveyJob {
                    sdr,
                    gain,
                    mode: parse_mode(&mode)?,
                    sample_rate,
                    channels: channels
                        .iter()
                        .map(|c| xng::freq::parse_hz(c))
                        .collect::<anyhow::Result<_>>()?,
                    duration_secs,
                    interim_secs,
                    scan_first,
                    scan_dwell_secs,
                    tune_gain,
                    tune_dwell_secs,
                    rotate_dwell_secs,
                    show_messages,
                })
            }
        })
    }
}

fn run(job: &Job, events: &Channel<ToolEvent>, cancel: &Arc<AtomicBool>) {
    let reporter = {
        let events = events.clone();
        Reporter::new(
            move |line| {
                let _ = events.send(ToolEvent::Line { text: line.to_owned() });
            },
            cancel.clone(),
        )
    };
    let report = match job {
        Job::Scan { sdr, gain, modes, dwell_secs } => {
            scan::run_with(&reporter, sdr, *gain, modes, *dwell_secs)
                .and_then(|r| Ok(serde_json::to_value(r)?))
        }
        Job::Survey(s) => survey::run_with(
            &reporter,
            survey::SurveyOpts {
                sdr: s.sdr.clone(),
                gain: s.gain,
                mode: s.mode,
                sample_rate: s.sample_rate,
                channels: s.channels.clone(),
                duration_secs: s.duration_secs,
                interim_secs: s.interim_secs,
                scan_first: s.scan_first,
                scan_dwell_secs: s.scan_dwell_secs,
                tune_gain: s.tune_gain,
                tune_dwell_secs: s.tune_dwell_secs,
                rotate_dwell_secs: s.rotate_dwell_secs,
                show_messages: s.show_messages,
                jsonl: None,
                out_json: None,
            },
        )
        .and_then(|r| Ok(serde_json::to_value(r)?)),
        Job::Ingest { grpc, quic, quic_cert_out } => {
            ingest::run_with(&reporter, grpc.clone(), quic.clone(), quic_cert_out.as_deref())
                .and_then(|r| Ok(serde_json::to_value(r)?))
        }
    };
    let _ = events.send(match report {
        Ok(report) => ToolEvent::Done { report, stopped: cancel.load(Ordering::Relaxed) },
        Err(e) => ToolEvent::Failed { message: format!("{e:#}") },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(modes: &[&str], dwell_secs: u64) -> ToolRequest {
        ToolRequest::Scan {
            sdr: String::new(),
            gain: None,
            modes: modes.iter().map(|m| m.to_string()).collect(),
            dwell_secs,
        }
    }

    #[test]
    fn requests_parse_from_the_ui_shape() {
        let r: ToolRequest = serde_json::from_value(serde_json::json!({
            "kind": "survey", "sdr": "driver=rtlsdr", "gain": null, "mode": "vdl2",
            "sampleRate": null, "channels": ["136.975M"], "durationSecs": 900, "interimSecs": 300,
            "scanFirst": true, "scanDwellSecs": 90, "tuneGain": false, "tuneDwellSecs": 20,
            "rotateDwellSecs": 60, "showMessages": false,
        }))
        .unwrap();
        let Job::Survey(job) = Job::parse(r).unwrap() else { panic!("expected a survey") };
        assert_eq!(job.mode, Mode::Vdl2);
        assert_eq!(job.channels, vec![136_975_000]);
        assert!(job.scan_first);
    }

    /// With no radio to open, a scan still runs to the end and reports: each
    /// capture group is skipped and the UI gets lines and an empty report.
    #[test]
    fn scan_without_a_radio_still_reports() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events = {
            let seen = seen.clone();
            Channel::new(move |body| {
                if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                    seen.lock().unwrap().push(serde_json::from_str::<serde_json::Value>(&json).unwrap());
                }
                Ok(())
            })
        };
        let request = ToolRequest::Scan {
            sdr: "driver=xng-test-no-such-radio".into(),
            gain: None,
            modes: vec!["ais".into()],
            dwell_secs: 1,
        };
        ToolTask::start(request, events, || (), Arc::default()).unwrap().wait();
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|e| e["type"] == "line"), "{seen:?}");
        let done = seen.last().unwrap();
        assert_eq!(done["type"], "done", "{seen:?}");
        assert_eq!(done["stopped"], false);
        assert!(done["report"]["configs"].as_array().unwrap().is_empty());
    }

    /// A stop asked for before the tool exists is in force from its first
    /// check: no capture group is tried and the report says it was stopped.
    #[test]
    fn a_stop_set_before_the_start_ends_the_tool_at_once() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events = {
            let seen = seen.clone();
            Channel::new(move |body| {
                if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                    seen.lock().unwrap().push(serde_json::from_str::<serde_json::Value>(&json).unwrap());
                }
                Ok(())
            })
        };
        let request = ToolRequest::Scan {
            sdr: "driver=xng-test-no-such-radio".into(),
            gain: None,
            modes: vec!["ais".into(), "acars".into()],
            dwell_secs: 1,
        };
        let stop = Arc::new(AtomicBool::new(true));
        ToolTask::start(request, events, || (), stop).unwrap().wait();
        let seen = seen.lock().unwrap();
        let done = seen.last().unwrap();
        assert_eq!(done["type"], "done", "{seen:?}");
        assert_eq!(done["stopped"], true);
        let skipped = |e: &serde_json::Value| e["text"].as_str().is_some_and(|t| t.contains("skipped"));
        assert!(!seen.iter().any(skipped), "{seen:?}");
    }

    /// Ingest needs no radio: it must run while something else holds the
    /// SDR lock, and stop when asked.
    #[test]
    fn ingest_runs_without_the_radio_and_stops() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let events = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                let _ = tx.lock().unwrap().send(serde_json::from_str::<serde_json::Value>(&json).unwrap());
            }
            Ok(())
        });
        let request = ToolRequest::Ingest { grpc: Some("127.0.0.1:0".into()), quic: None, quic_cert_out: None };
        fn never() {
            panic!("ingest must not take the SDR lock");
        }
        let task = ToolTask::start(request, events, never, Arc::default()).unwrap();
        let first = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        assert_eq!(first["type"], "line", "{first}");
        task.stop();
        let last = rx.iter().last().unwrap();
        assert_eq!(last["type"], "done", "{last}");
        assert_eq!(last["stopped"], true);
        assert_eq!(last["report"]["messages"], 0);
    }

    #[test]
    fn ingest_addresses_are_checked_up_front() {
        let bad = ToolRequest::Ingest { grpc: Some("localhost".into()), quic: None, quic_cert_out: None };
        assert!(Job::parse(bad).is_err());
        let none = ToolRequest::Ingest { grpc: Some("  ".into()), quic: None, quic_cert_out: None };
        assert!(Job::parse(none).is_err());
    }

    #[test]
    fn bad_requests_are_refused_before_any_device_is_opened() {
        assert!(Job::parse(scan(&[], 20)).is_err());
        assert!(Job::parse(scan(&["acars"], 0)).is_err());
        assert!(Job::parse(scan(&["not-a-mode"], 20)).is_err());
        assert!(Job::parse(scan(&["acars", "ais"], 20)).is_ok());
    }
}
