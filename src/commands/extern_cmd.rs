//! `xng extern` — second-class wrapped external decoders: spawn a child
//! decoder (or read stdin) and normalize its JSON output onto the bus,
//! so wrapped decoders get every xng output (asf-2.0, feeds, JSONL,
//! console) and the xng-acars application layer.

use crate::bus::{MessageBus, WeakBus};
use crate::engine::EngineState;
use crate::outputs::console;
use crate::outputs::{acarsdec_json, jsonl};
use crate::runtime::OutputConfig;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::io::{AsyncBufReadExt, BufReader};
use xng_types::{
    AcarsCore, AppInfo, DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality,
    StationIdentity,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExternFormat {
    Dumphfdl,
    Dumpvdl2,
    Acarsdec,
}

impl std::str::FromStr for ExternFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "dumphfdl" | "hfdl" => Ok(Self::Dumphfdl),
            "dumpvdl2" | "vdl2" => Ok(Self::Dumpvdl2),
            "acarsdec" | "acars" => Ok(Self::Acarsdec),
            other => Err(format!("unknown extern format: {other}")),
        }
    }
}

/// Find the first object under `key` anywhere in the JSON tree.
fn find_object<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(found);
            }
            map.values().find_map(|child| find_object(child, key))
        }
        Value::Array(items) => items.iter().find_map(|child| find_object(child, key)),
        _ => None,
    }
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

fn char_of(v: &Value, keys: &[&str]) -> Option<char> {
    str_of(v, keys).and_then(|s| s.chars().next())
}

/// Parse one JSON line from an external decoder into a normalized
/// message. Tolerant: unknown layouts still produce a raw event.
pub fn parse_line(line: &str, format: ExternFormat, station: &StationIdentity) -> Option<Message> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let (mode, envelope) = match format {
        ExternFormat::Dumphfdl => (Mode::Hfdl, v.get("hfdl").unwrap_or(&v)),
        ExternFormat::Dumpvdl2 => (Mode::Vdl2, v.get("vdl2").unwrap_or(&v)),
        ExternFormat::Acarsdec => (Mode::AcarsPoa, &v),
    };

    // Timestamp: {"t":{"sec","usec"}} or float "timestamp".
    let timestamp: DateTime<Utc> = envelope
        .get("t")
        .and_then(|t| {
            let sec = t.get("sec")?.as_i64()?;
            let usec = t.get("usec").and_then(Value::as_i64).unwrap_or(0);
            Utc.timestamp_opt(sec, (usec * 1000) as u32).single()
        })
        .or_else(|| {
            let ts = envelope.get("timestamp")?.as_f64()?;
            Utc.timestamp_opt(ts as i64, ((ts.fract()) * 1e9) as u32).single()
        })
        .unwrap_or_else(Utc::now);

    // Frequency: Hz int (dumphfdl/dumpvdl2) or MHz float (acarsdec).
    let frequency_hz = envelope
        .get("freq")
        .and_then(|f| {
            if let Some(hz) = f.as_u64() {
                Some(if hz < 1_000_000 { (f.as_f64()? * 1e6) as u64 } else { hz })
            } else {
                f.as_f64().map(|mhz| (mhz * 1e6) as u64)
            }
        })
        .unwrap_or(0);

    let signal = SignalQuality {
        rssi_db: envelope
            .get("sig_level")
            .or_else(|| envelope.get("level"))
            .and_then(Value::as_f64)
            .map(|x| x as f32),
        noise_db: envelope.get("noise_level").and_then(Value::as_f64).map(|x| x as f32),
        freq_skew_hz: envelope.get("freq_skew").and_then(Value::as_f64).map(|x| x as f32),
        ..Default::default()
    };

    // ACARS content anywhere in the tree.
    let acars = if format == ExternFormat::Acarsdec {
        Some(envelope)
    } else {
        find_object(envelope, "acars")
    };
    let body = match acars {
        Some(a) if a.get("label").is_some() => {
            let label = str_of(a, &["label"]).unwrap_or("").to_string();
            let text = str_of(a, &["text", "msg_text"]).unwrap_or("").to_string();
            let block_id = char_of(a, &["blk_id", "block_id"]);
            let downlink = block_id.map(|c| c.is_ascii_digit()).unwrap_or(false);
            let appdec = xng_acars::decode(&label, &text, downlink);
            MessageBody::Acars(AcarsCore {
                mode: char_of(a, &["mode"]).unwrap_or('2'),
                tail: str_of(a, &["reg", "tail"]).map(|s| s.trim_start_matches('.').to_string()),
                label,
                sublabel: appdec.sublabel.or(str_of(a, &["sublabel"]).map(String::from)),
                mfi: appdec.mfi.or(str_of(a, &["mfi"]).map(String::from)),
                block_id,
                ack: match a.get("ack") {
                    Some(Value::Bool(false)) => None,
                    Some(Value::String(s)) => s.chars().next(),
                    _ => None,
                },
                flight: str_of(a, &["flight"]).map(String::from),
                msg_num: str_of(a, &["msg_num", "msgno"]).map(String::from),
                text,
                more_to_come: false,
                reassembled: false,
                assstat: str_of(a, &["assstat"]).map(String::from),
                app: appdec.app.map(|x| serde_json::to_value(&x).unwrap_or_default()),
                vdl2_link: None,
            })
        }
        _ => MessageBody::Hfdl {
            kind: "extern".into(),
            details: envelope.clone(),
        },
    };
    let crc_ok = acars
        .and_then(|a| a.get("err").map(|e| e != &Value::Bool(true)))
        .unwrap_or(true);

    Some(Message {
        mode,
        timestamp,
        frequency_hz,
        signal,
        decode: DecodeQuality { crc_ok, fec_corrected: None, errors: None },
        body,
        raw: None,
        source: Provenance {
            station: station.clone(),
            app: AppInfo::xng(),
            sdr: None,
            channel: None,
        },
    })
}

type OutputTask = tokio::task::JoinHandle<Result<(), std::io::Error>>;

/// Start the outputs a wrapped decoder supports: console, JSONL, UDP and
/// the asf-2.0 feeds.
fn spawn_outputs(bus: &MessageBus, outputs: &OutputConfig, station: &StationIdentity) -> Vec<OutputTask> {
    let mut tasks = Vec::new();
    if outputs.console != console::ConsoleFormat::Off {
        tasks.push(tokio::spawn({
            let rx = bus.subscribe();
            let fmt = outputs.console;
            async move {
                console::run(rx, fmt).await;
                Ok::<(), std::io::Error>(())
            }
        }));
    }
    if let Some(path) = outputs.jsonl.clone() {
        let rx = bus.subscribe();
        tasks.push(tokio::spawn(async move { jsonl::run(rx, &path).await }));
    }
    for target in outputs.udp.clone() {
        tasks.push(tokio::spawn(acarsdec_json::run(bus.subscribe(), target)));
    }
    if let Some(url) = outputs.asf2_grpc.clone() {
        let (id, ident) = (station.id.to_string(), station.ident.clone());
        tasks.push(tokio::spawn(crate::outputs::asf2_grpc::run(bus.subscribe(), url, id, ident)));
    }
    if let Some(target) = outputs.asf2_quic.clone() {
        let (id, ident) = (station.id.to_string(), station.ident.clone());
        tasks.push(tokio::spawn(crate::outputs::asf2_quic::run(
            bus.subscribe(),
            target,
            outputs.asf2_quic_trust.clone(),
            id,
            ident,
        )));
    }
    tasks
}

/// Close the bus and let every output drain.
async fn finish_outputs(bus: MessageBus, tasks: Vec<OutputTask>) {
    drop(bus);
    for t in tasks {
        match t.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("output error: {e}"),
            Err(e) => tracing::warn!("output task failed: {e}"),
        }
    }
}

/// Read decoder JSON lines from `reader` onto the bus until it ends or
/// `stop` is set. Returns how many messages were normalized.
async fn pump_lines<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    format: ExternFormat,
    station: &StationIdentity,
    bus: &MessageBus,
    stop: &AtomicBool,
) -> std::io::Result<u64> {
    let mut lines = BufReader::new(reader).lines();
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut count = 0;
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => {
                    if let Some(msg) = parse_line(&line, format, station) {
                        bus.publish(msg);
                        count += 1;
                    }
                }
                None => return Ok(count),
            },
            _ = poll.tick() => {
                if stop.load(Ordering::Relaxed) {
                    return Ok(count);
                }
            }
        }
    }
}

/// Start the decoder. `windowless` keeps Windows from opening a console
/// window for it when the caller is a GUI app.
/// A GUI caller also has no terminal for the decoder's own messages, so
/// its stderr is piped for [`stderr_tail`] instead of inherited.
fn spawn_child(command: &[String], windowless: bool) -> anyhow::Result<tokio::process::Child> {
    tracing::info!("spawning: {}", command.join(" "));
    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.args(&command[1..]).stdout(std::process::Stdio::piped()).kill_on_drop(true);
    if windowless {
        cmd.stderr(std::process::Stdio::piped());
    }
    #[cfg(windows)]
    if windowless {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| anyhow::anyhow!("could not start {:?}: {e}", command[0]))
}

/// How many of the decoder's last stderr lines an error message carries.
const STDERR_TAIL_LINES: usize = 6;

/// Follow a piped stderr in the background, logging each line and keeping
/// the last few: they are what says why a decoder gave up.
type StderrTail = Arc<Mutex<std::collections::VecDeque<String>>>;

fn stderr_tail(child: &mut tokio::process::Child) -> (StderrTail, Option<tokio::task::JoinHandle<()>>) {
    let tail: StderrTail = Arc::default();
    let reader = child.stderr.take().map(|stderr| {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    continue;
                }
                tracing::info!("decoder: {line}");
                let mut t = tail.lock().unwrap();
                if t.len() == STDERR_TAIL_LINES {
                    t.pop_front();
                }
                t.push_back(line);
            }
        })
    });
    (tail, reader)
}

/// Pump a spawned decoder until it exits or `stop` is set (then it is
/// killed). Returns the message count and, when the decoder ended on its
/// own, how it exited.
async fn pump_child(
    mut child: tokio::process::Child,
    format: ExternFormat,
    station: &StationIdentity,
    bus: &MessageBus,
    stop: &AtomicBool,
) -> std::io::Result<(u64, Option<std::process::ExitStatus>)> {
    let stdout = child.stdout.take().expect("piped stdout");
    let count = pump_lines(stdout, format, station, bus, stop).await;
    if stop.load(Ordering::Relaxed) || count.is_err() {
        let _ = child.kill().await;
        return Ok((count?, None));
    }
    let status = child.wait().await.ok();
    Ok((count?, status))
}

pub fn run(
    format: ExternFormat,
    command: &[String],
    station_ident: String,
    outputs: OutputConfig,
) -> anyhow::Result<()> {
    let station = StationIdentity::new(station_ident);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        let tasks = spawn_outputs(&bus, &outputs, &station);
        let stop = Arc::new(AtomicBool::new(false));
        tokio::spawn({
            let stop = stop.clone();
            async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    stop.store(true, Ordering::Relaxed);
                }
            }
        });
        let count = if command.is_empty() {
            tracing::info!("reading external decoder JSON from stdin");
            pump_lines(tokio::io::stdin(), format, &station, &bus, &stop).await?
        } else {
            pump_child(spawn_child(command, false)?, format, &station, &bus, &stop).await?.0
        };
        finish_outputs(bus, tasks).await;
        tracing::info!("extern session: {count} message(s) normalized");
        Ok(())
    })
}

/// A wrapped external decoder running in the background, for front ends
/// that show its messages themselves: the same normalization and outputs
/// as `xng extern`, without the CLI's console or Ctrl-C handling.
pub struct ExternSession {
    bus: WeakBus,
    first_rx: Mutex<Option<broadcast::Receiver<Arc<Message>>>>,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<EngineState>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ExternSession {
    /// Spawn `command` and start normalizing its JSON output. Fails if the
    /// command cannot be started.
    pub fn start(
        format: ExternFormat,
        command: Vec<String>,
        station_ident: String,
        outputs: OutputConfig,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!command.is_empty(), "no decoder command given");
        let station = StationIdentity::new(station_ident);
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(EngineState::Running));
        // The worker reports whether the decoder started, with the bus
        // handles when it did.
        type Ready = anyhow::Result<(WeakBus, broadcast::Receiver<Arc<Message>>)>;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Ready>();
        let thread = std::thread::Builder::new().name("xng-extern".into()).spawn({
            let (stop, state) = (stop.clone(), state.clone());
            move || {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.into()));
                        return;
                    }
                };
                let result: anyhow::Result<()> = rt.block_on(async {
                    let mut child = match spawn_child(&command, true) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return Ok(());
                        }
                    };
                    let (said, said_reader) = stderr_tail(&mut child);
                    let bus = MessageBus::new();
                    let _ = ready_tx.send(Ok((bus.downgrade(), bus.subscribe())));
                    let tasks = spawn_outputs(&bus, &outputs, &station);
                    let pumped = pump_child(child, format, &station, &bus, &stop).await;
                    finish_outputs(bus, tasks).await;
                    let (count, status) = pumped?;
                    tracing::info!("extern session: {count} message(s) normalized");
                    match status {
                        Some(s) if !s.success() => {
                            // The reader ends when the decoder's stderr
                            // closes; give it a moment to take in the
                            // last lines.
                            if let Some(r) = said_reader {
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), r).await;
                            }
                            let said: Vec<String> = said.lock().unwrap().iter().cloned().collect();
                            let why = if said.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", said.join(" | "))
                            };
                            anyhow::bail!("{} stopped ({s}) after {count} message(s){why}", command[0])
                        }
                        _ => Ok(()),
                    }
                });
                *state.lock().unwrap() = match result {
                    Ok(()) => EngineState::Finished,
                    Err(e) => EngineState::Failed(format!("{e:#}")),
                };
                rt.shutdown_background();
            }
        })?;
        let (bus, rx) = ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("the decoder worker ended before starting"))??;
        Ok(Self { bus, first_rx: Mutex::new(Some(rx)), stop, state, thread: Some(thread) })
    }

    /// Subscribe to normalized messages. The first call returns a receiver
    /// attached before the decoder produced anything.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Message>> {
        if let Some(rx) = self.first_rx.lock().unwrap().take() {
            return rx;
        }
        self.bus.subscribe().unwrap_or_else(|| broadcast::channel(1).1)
    }

    /// Ask the decoder to stop; it is killed and the outputs drain.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn state(&self) -> EngineState {
        self.state.lock().unwrap().clone()
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
}

impl Drop for ExternSession {
    /// Never leave the wrapped decoder running behind a dropped session.
    fn drop(&mut self) {
        self.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station() -> StationIdentity {
        StationIdentity::new("XX-TEST")
    }

    #[test]
    fn parses_dumphfdl_acars() {
        let line = r#"{"hfdl":{"app":{"name":"dumphfdl","ver":"1.6.0"},"station":"XX","t":{"sec":1717000000,"usec":250000},"freq":10063000,"bit_rate":1800,"sig_level":-18.5,"noise_level":-39.1,"lpdu":{"src":{"type":"Aircraft"},"hfnpdu":{"type":"ACARS","acars":{"err":false,"mode":"2","reg":".N471XG","ack":"!","label":"H1","blk_id":"3","msg_num":"M42A","flight":"UA0042","text":"POSN 4737.2N"}}}}}"#;
        let m = parse_line(line, ExternFormat::Dumphfdl, &station()).unwrap();
        assert_eq!(m.mode, Mode::Hfdl);
        assert_eq!(m.frequency_hz, 10_063_000);
        assert_eq!(m.signal.rssi_db, Some(-18.5));
        let MessageBody::Acars(a) = &m.body else { panic!("expected acars") };
        assert_eq!(a.tail.as_deref(), Some("N471XG"));
        assert_eq!(a.label, "H1");
        assert_eq!(a.flight.as_deref(), Some("UA0042"));
    }

    #[test]
    fn parses_acarsdec_flat() {
        let line = r#"{"timestamp":1717000123.456,"station_id":"XX","channel":2,"freq":131.550,"level":-21.0,"error":0,"mode":"2","label":"B6","block_id":"4","ack":false,"tail":"VT-ANB","flight":"AI0142","msgno":"M11A","text":"/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5","end":true}"#;
        let m = parse_line(line, ExternFormat::Acarsdec, &station()).unwrap();
        assert_eq!(m.mode, Mode::AcarsPoa);
        assert_eq!(m.frequency_hz, 131_550_000);
        let MessageBody::Acars(a) = &m.body else { panic!() };
        assert_eq!(a.label, "B6");
        // The xng application layer runs on wrapped output too.
        assert_eq!(a.app.as_ref().unwrap()["app"], "adsc");
    }

    #[test]
    fn parses_dumpvdl2_avlc_acars() {
        let line = r#"{"vdl2":{"app":{"name":"dumpvdl2"},"t":{"sec":1717000200,"usec":1000},"freq":136975000,"sig_level":-25.2,"freq_skew":1.2,"avlc":{"src":{"addr":"A1B2C3"},"dst":{"addr":"10A234"},"acars":{"err":false,"mode":"2","reg":".N818WX","label":"Q0","blk_id":"A","text":""}}}}"#;
        let m = parse_line(line, ExternFormat::Dumpvdl2, &station()).unwrap();
        assert_eq!(m.mode, Mode::Vdl2);
        assert_eq!(m.frequency_hz, 136_975_000);
        let MessageBody::Acars(a) = &m.body else { panic!() };
        assert_eq!(a.tail.as_deref(), Some("N818WX"));
    }

    #[test]
    fn non_acars_becomes_raw_event() {
        let line = r#"{"hfdl":{"t":{"sec":1717000300},"freq":10063000,"spdu":{"gs":{"id":7}}}}"#;
        let m = parse_line(line, ExternFormat::Dumphfdl, &station()).unwrap();
        assert!(matches!(m.body, MessageBody::Hfdl { .. }));
    }

    fn quiet() -> OutputConfig {
        OutputConfig {
            console: console::ConsoleFormat::Off,
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
            airband: Default::default(),
        }
    }

    fn wait(s: &ExternSession) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !s.is_finished() {
            assert!(std::time::Instant::now() < deadline, "session should finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_session_normalizes_a_decoder_and_finishes() {
        let line = r#"{"timestamp":1717000123.4,"freq":131.550,"mode":"2","label":"H1","block_id":"4","tail":"N1","text":"HI"}"#;
        let cmd = vec!["sh".to_string(), "-c".to_string(), format!("echo '{line}'; echo junk")];
        let s = ExternSession::start(ExternFormat::Acarsdec, cmd, "XX-TEST".into(), quiet()).unwrap();
        let mut rx = s.subscribe();
        wait(&s);
        assert_eq!(s.state(), EngineState::Finished);
        let m = rx.try_recv().expect("one message");
        assert_eq!(m.frequency_hz, 131_550_000);
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)));
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_decoder_is_reported() {
        let script = "echo 'unable to open device 0' >&2; exit 3";
        let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        let s = ExternSession::start(ExternFormat::Acarsdec, cmd, "XX-TEST".into(), quiet()).unwrap();
        wait(&s);
        let EngineState::Failed(e) = s.state() else { panic!("expected a failure") };
        assert!(e.contains("stopped"), "{e}");
        // What the decoder said on stderr is the reason the user needs.
        assert!(e.contains("unable to open device 0"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn stop_kills_a_decoder_that_never_ends() {
        let cmd = vec!["sh".to_string(), "-c".to_string(), "exec sleep 600".to_string()];
        let s = ExternSession::start(ExternFormat::Acarsdec, cmd, "XX-TEST".into(), quiet()).unwrap();
        s.stop();
        wait(&s);
        assert_eq!(s.state(), EngineState::Finished);
    }

    #[test]
    fn a_missing_decoder_is_refused() {
        let cmd = vec!["xng-test-no-such-decoder".to_string()];
        let err = ExternSession::start(ExternFormat::Acarsdec, cmd, "XX-TEST".into(), quiet()).err().unwrap();
        assert!(err.to_string().contains("could not start"), "{err}");
    }
}
