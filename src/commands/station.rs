//! Station config file: several decode sessions (mode + SDR/file +
//! channels) plus one shared output set, run as a single process.
//!
//! ```toml
//! station-id = "XX-KSEA-1"
//!
//! [outputs]
//! feed-airframes = true
//! jsonl = "/var/log/xng/messages.jsonl"
//! metrics = "0.0.0.0:9090"
//!
//! # Optional per-mode Airframes feeding. Without it, `feed-airframes`
//! # governs (ACARS-only, id verbatim). With it, each mode feeds its own
//! # ingest in its native format under its own id; asf-2.0 is separate.
//! [outputs.airframes]
//! enabled = true
//! station-id = "XX-KSEA"   # base; auto-suffix derives XX-KSEA-ACARS, …
//! auto-suffix = true
//!
//! [[session]]
//! sdr = "driver=rtlsdr,serial=00000001"
//! gain = 48
//! mode = "acars"
//! sample-rate = 2400000
//! center = "131.000M"
//! channels = ["130.025", "131.550", "131.725"]
//!
//! [[session]]
//! sdr = "driver=airspy"
//! mode = "vdl2"          # rate/center/channels from the mode's plan
//! feed = false           # decode locally but don't feed this decoder
//! ```
//!
//! A radio can be opened only once, so sessions that decode from the same
//! one describe it in a `[[device]]` block and name it with `device`. The
//! device is opened once and every session reads the same samples. Its rate
//! and center are planned from all of their channels unless the block sets
//! them:
//!
//! ```toml
//! [[device]]
//! id = "airspy"
//! sdr = "driver=airspy,serial=0123456789ABCDEF"
//! gain = 20
//!
//! [[session]]
//! device = "airspy"
//! mode = "acars"
//! channels = ["131.125", "131.550", "131.725"]
//!
//! [[session]]
//! device = "airspy"
//! mode = "vdl2"
//! channels = ["136.650", "136.800", "136.975"]
//! ```

use crate::outputs::airframes::{
    auto_suffix, default_endpoint, has_serializer, AirframesRouter, AirframesTarget, Route,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use xng_types::Mode;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct StationFile {
    pub station_id: String,
    #[serde(default)]
    pub outputs: OutputsToml,
    #[serde(rename = "session")]
    pub sessions: Vec<SessionToml>,
    /// Radios (or recordings) that several sessions read at once.
    #[serde(default, rename = "device")]
    pub devices: Vec<DeviceToml>,
}

/// A radio or recording opened once and read by every session that names its
/// `id` with `device`.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DeviceToml {
    /// The name sessions use to read it.
    pub id: String,
    /// SDR selector (`driver=airspy,serial=…`); mutually exclusive with
    /// `file`.
    pub sdr: Option<String>,
    /// IQ file replay instead of live hardware.
    pub file: Option<PathBuf>,
    /// Sample format for `file` (cf32/cs16/cs8/cu8); guessed from the
    /// extension when omitted.
    pub format: Option<String>,
    /// Tuner gain in dB; hardware AGC when omitted.
    pub gain: Option<f64>,
    /// Capture rate. A radio may leave it out to have it planned from the
    /// sessions' channels; a recording must give it.
    pub sample_rate: Option<f64>,
    /// Capture center. A radio may leave it out to have it planned from the
    /// sessions' channels; a recording must give it.
    pub center: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OutputsToml {
    #[serde(default)]
    pub feed_airframes: bool,
    /// Airband audio outputs: recordings, UDP, Icecast, scanner, ATC feed.
    #[serde(default)]
    pub airband: crate::airband::AirbandOutputs,
    pub jsonl: Option<PathBuf>,
    pub metrics: Option<String>,
    #[serde(default)]
    pub udp: Vec<String>,
    pub sbs: Option<String>,
    pub beast: Option<String>,
    pub nmea_tcp: Option<String>,
    pub nmea_udp: Option<String>,
    pub nmea_tag_blocks: Option<bool>,
    pub gsmtap: Option<String>,
    pub iridium_satmap: Option<String>,
    pub http: Option<String>,
    /// Dashboard basemap (`[outputs.map]`).
    pub map: Option<MapToml>,
    pub aircraft_db: Option<PathBuf>,
    pub mqtt: Option<String>,
    pub mqtt_topic: Option<String>,
    /// ZMQ PUB endpoints; emits `[mode, json]` multipart frames. One shared
    /// PUB socket attaches to every endpoint (fan-out). Each binds by default
    /// (`tcp://…`/`ipc://…`); prefix `connect:` to connect instead.
    #[serde(default)]
    pub zmq: Vec<String>,
    /// Own-ship MMSI: emit an AIVDO position report for the station's own
    /// `receiver-pos` so chart plotters show the receiver (AIS-5c).
    pub own_ship_mmsi: Option<u32>,
    pub asf2_grpc: Option<String>,
    pub asf2_quic: Option<String>,
    #[serde(default)]
    pub json: bool,
    /// Per-mode Airframes feeding. When omitted, the legacy `feed-airframes`
    /// boolean governs (ACARS-only, station id verbatim).
    pub airframes: Option<AirframesToml>,
}

/// Dashboard basemap: an OpenFreeMap style (vector, keyless) by default, any
/// MapLibre style via `style-url`, or OpenStreetMap raster tiles.
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MapToml {
    /// OpenFreeMap style: dark (default), positron, bright, liberty, fiord.
    pub style: Option<String>,
    /// Any MapLibre style URL (e.g. a self-hosted OpenFreeMap); overrides `style`.
    pub style_url: Option<String>,
    /// `vector` (default) or `raster` (OpenStreetMap tiles).
    pub format: Option<String>,
}

/// Per-mode Airframes feeding configuration. Each supported mode feeds its
/// own ingest in that ingest's native wire format under its own station id;
/// asf-2.0 (`asf2-grpc`/`asf2-quic`) is fed separately and is not affected.
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AirframesToml {
    /// Master switch for per-port Airframes feeding (default: on when the
    /// block is present).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Base station id; defaults to the top-level `station-id`.
    pub station_id: Option<String>,
    /// Derive a per-mode station id by appending the mode suffix
    /// (e.g. `KE-KSEA` → `KE-KSEA-ACARS`). Off by default (id used verbatim).
    #[serde(default)]
    pub auto_suffix: bool,
    pub acars: Option<AirframesModeToml>,
    pub vdl2: Option<AirframesModeToml>,
    pub hfdl: Option<AirframesModeToml>,
    pub ais: Option<AirframesModeToml>,
    /// Inmarsat Aero (both `aero-l` and `aero-c` sessions).
    pub aero: Option<AirframesModeToml>,
    pub iridium: Option<AirframesModeToml>,
}

/// Per-mode override block under `[outputs.airframes.<mode>]`.
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AirframesModeToml {
    /// Enable/disable this mode's feed (default: the block's `enabled`).
    pub enabled: Option<bool>,
    /// Station id for this mode (overrides `auto-suffix` and the base).
    pub station_id: Option<String>,
    /// Override the ingest host (default: the verified public ingest).
    pub host: Option<String>,
    /// Override the ingest port.
    pub port: Option<u16>,
}

fn default_true() -> bool {
    true
}

impl AirframesToml {
    fn mode_cfg(&self, mode: Mode) -> Option<&AirframesModeToml> {
        match mode {
            Mode::AcarsPoa => self.acars.as_ref(),
            Mode::Vdl2 => self.vdl2.as_ref(),
            Mode::Hfdl => self.hfdl.as_ref(),
            Mode::Ais => self.ais.as_ref(),
            Mode::AeroL | Mode::AeroC => self.aero.as_ref(),
            Mode::Iridium => self.iridium.as_ref(),
            _ => None,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SessionToml {
    /// SDR selector (`driver=rtlsdr,serial=…`). A session reads exactly one
    /// of `sdr`, `device` or `file`.
    pub sdr: Option<String>,
    /// `id` of a `[[device]]` this session shares with others.
    pub device: Option<String>,
    /// IQ file replay instead of live hardware.
    pub file: Option<PathBuf>,
    /// Sample format for `file` (cf32/cs16/cs8/cu8); guessed from the
    /// extension when omitted.
    pub format: Option<String>,
    pub gain: Option<f64>,
    pub mode: String,
    pub sample_rate: Option<f64>,
    pub center: Option<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub receiver_pos: Option<String>,
    pub demod_effort: Option<String>,
    /// VDL2 only: reject bursts whose carrier offset exceeds this many ppm.
    pub max_ppm: Option<f64>,
    /// AIS output shaping (AIS-5h) — empty/None = pass everything.
    #[serde(default)]
    pub ais_include_types: Vec<u8>,
    #[serde(default)]
    pub ais_exclude_types: Vec<u8>,
    #[serde(default)]
    pub ais_include_mmsi: Vec<u32>,
    #[serde(default)]
    pub ais_exclude_mmsi: Vec<u32>,
    /// Minimum seconds between dynamic AIS position reports per MMSI.
    pub ais_min_interval: Option<f64>,
    /// Drop content-duplicate AIS reports within this many seconds.
    pub ais_dedup_window: Option<f64>,
    /// Disable Airframes feeding for this decoder even when feeding is on
    /// globally.
    pub feed: Option<bool>,
    /// Override the Airframes station id used for this decoder's messages.
    pub airframes_station_id: Option<String>,
    /// Airband channel defaults, per-channel overrides and the mix stream.
    #[serde(default)]
    pub airband: crate::airband::AirbandConfig,
}

pub fn load(path: &Path) -> anyhow::Result<StationFile> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let f: StationFile = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    anyhow::ensure!(!f.sessions.is_empty(), "{}: no [[session]] entries", path.display());
    check_sources(&f)?;
    Ok(f)
}

/// Who opens an SDR selector, for the "opened twice" check.
enum Opener<'a> {
    Device(&'a str),
    Session(usize),
}

/// Check that every session reads exactly one source, that `device` names a
/// `[[device]]`, and that no radio is opened twice.
///
/// Only a selector that names a serial is one particular radio. Two sessions
/// with a bare `driver=rtlsdr` each open the next free dongle, as before.
fn check_sources(f: &StationFile) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    let mut openers: HashMap<String, Opener> = HashMap::new();
    let used = |id: &str| f.sessions.iter().any(|s| s.device.as_deref() == Some(id));
    for d in &f.devices {
        anyhow::ensure!(!d.id.trim().is_empty(), "[[device]]: `id` is required");
        anyhow::ensure!(
            ids.insert(d.id.as_str()),
            "[[device]] `{}` is defined twice",
            d.id
        );
        anyhow::ensure!(
            d.sdr.is_some() ^ d.file.is_some(),
            "[[device]] {}: exactly one of `sdr` or `file` is required",
            d.id
        );
        anyhow::ensure!(
            d.file.is_none() || (d.sample_rate.is_some() && d.center.is_some()),
            "[[device]] {}: a recording needs `sample-rate` and `center`",
            d.id
        );
        // A [[device]] no session reads is never opened, so it holds no radio.
        let sdr = d.sdr.as_deref().filter(|_| used(&d.id));
        if let Some((sdr, key)) = sdr.and_then(|s| Some((s, radio_key(s)?))) {
            if let Some(Opener::Device(other)) = openers.insert(key, Opener::Device(&d.id)) {
                anyhow::bail!(
                    "[[device]] {other} and {} both open `{sdr}`; keep one of them",
                    d.id
                );
            }
        }
    }
    for (i, s) in f.sessions.iter().enumerate() {
        let n = i + 1;
        let label = format!("session {n} ({})", s.mode);
        let sources = [s.sdr.is_some(), s.device.is_some(), s.file.is_some()];
        anyhow::ensure!(
            sources.iter().filter(|&&b| b).count() == 1,
            "{label}: exactly one of `sdr`, `device` or `file` is required"
        );
        if let Some(id) = &s.device {
            anyhow::ensure!(
                ids.contains(id.as_str()),
                "{label}: no [[device]] has id `{id}`"
            );
            for (key, set) in [
                ("gain", s.gain.is_some()),
                ("sample-rate", s.sample_rate.is_some()),
                ("center", s.center.is_some()),
                ("format", s.format.is_some()),
            ] {
                anyhow::ensure!(
                    !set,
                    "{label}: `{key}` belongs on [[device]] {id}, which this session shares"
                );
            }
            anyhow::ensure!(
                !session_channels(s).is_empty(),
                "{label}: a session on a shared device needs `channels`"
            );
            // Their messages would carry the same radio and mode, so the
            // outputs couldn't route them apart.
            if let Some(j) = f.sessions[..i]
                .iter()
                .position(|o| o.device == s.device && o.mode == s.mode)
            {
                anyhow::bail!(
                    "{label}: session {} already decodes {} from [[device]] {id}; \
                     list all of its channels in one session",
                    j + 1,
                    s.mode
                );
            }
        }
        let Some(sdr) = s.sdr.as_deref() else { continue };
        let Some(key) = radio_key(sdr) else { continue };
        match openers.get(&key) {
            Some(Opener::Device(id)) => anyhow::bail!(
                "{label}: `{sdr}` is already opened by [[device]] {id}; use `device = \"{id}\"` instead of `sdr`"
            ),
            Some(Opener::Session(first)) => anyhow::bail!(share_hint(*first, n, sdr)),
            None => {
                openers.insert(key, Opener::Session(n));
            }
        }
    }
    Ok(())
}

/// Which radio a selector names, for comparing two of them: its driver and
/// serial, or `None` when it names no serial. Airspy serials are numbers, so
/// `0x1234` and `0000000000001234` are the same radio.
fn radio_key(sdr: &str) -> Option<String> {
    let a = crate::sdr_args::SdrArgs::parse(sdr);
    let driver = a.driver.unwrap_or_default().to_ascii_lowercase();
    let serial = a.serial?.trim().to_ascii_lowercase();
    let serial = match driver.as_str() {
        "airspy" | "airspyhf" => {
            let hex = serial.strip_prefix("0x").unwrap_or(&serial);
            u64::from_str_radix(hex, 16).map_or(serial.clone(), |v| format!("{v:x}"))
        }
        _ => serial,
    };
    Some(format!("{driver}:{serial}"))
}

/// The error for two sessions that open the same radio, with the
/// `[[device]]` block that shares it.
fn share_hint(first: usize, second: usize, sdr: &str) -> String {
    let id = crate::sdr_args::SdrArgs::parse(sdr)
        .driver
        .unwrap_or_else(|| "radio".into());
    format!(
        "sessions {first} and {second} both open `{sdr}`, and a radio can be opened only once. \
         To share it, describe it once:\n\n    [[device]]\n    id = \"{id}\"\n    sdr = \"{sdr}\"\n\n\
         then write `device = \"{id}\"` instead of `sdr` in both sessions, and move their \
         `gain`, `sample-rate` and `center` to the [[device]]."
    )
}

/// The channels a session lists: `channels`, or for airband the frequencies
/// of its `[[session.airband.channel]]` tables.
fn session_channels(sess: &SessionToml) -> Vec<String> {
    if sess.channels.is_empty() {
        sess.airband
            .channels
            .iter()
            .map(|c| c.freq.clone())
            .collect()
    } else {
        sess.channels.clone()
    }
}

/// Where a session's samples come from, after following its `device`.
pub enum SessionSource<'a> {
    /// A live radio, by its SDR selector.
    Sdr(&'a str),
    /// A recording, by its path.
    File(&'a Path),
}

impl StationFile {
    /// Read the file's relative paths (recordings, the JSONL log, the
    /// aircraft database) from `dir` instead of the working directory. A
    /// front end started from anywhere passes the station file's own
    /// folder; `xng station` keeps the shell's working directory.
    pub fn resolve_paths(&mut self, dir: &Path) {
        let fix = |p: &mut Option<PathBuf>| {
            if let Some(path) = p {
                if path.is_relative() {
                    *path = dir.join(&*path);
                }
            }
        };
        fix(&mut self.outputs.jsonl);
        fix(&mut self.outputs.aircraft_db);
        fix(&mut self.outputs.airband.record_dir);
        if let Some(a) = &mut self.outputs.airband.atc {
            fix(&mut a.credentials);
            fix(&mut a.ca);
        }
        for s in &mut self.sessions {
            fix(&mut s.file);
        }
        for d in &mut self.devices {
            fix(&mut d.file);
        }
    }

    /// The SDR or recording a session reads, following its `device`.
    pub fn source_of<'a>(&'a self, s: &'a SessionToml) -> Option<SessionSource<'a>> {
        let (sdr, file) = match &s.device {
            Some(id) => {
                let d = self.devices.iter().find(|d| &d.id == id)?;
                (&d.sdr, &d.file)
            }
            None => (&s.sdr, &s.file),
        };
        match (sdr, file) {
            (Some(sdr), _) => Some(SessionSource::Sdr(sdr)),
            (_, Some(path)) => Some(SessionSource::File(path)),
            _ => None,
        }
    }
}

/// A `[[device]]` opened once, split among the sessions that read it.
struct Tap {
    /// One stream per session still to be built.
    consumers: Vec<xng_sdr::tee::TeeSource>,
    rate: f64,
    center_hz: u64,
    info: xng_types::SdrInfo,
}

/// Plan and open every `[[device]]` that a session reads, splitting each
/// one's samples into a stream per session. Live radios drop samples for a
/// session that falls behind rather than stall the others; recordings make
/// every session see every sample.
fn open_devices(st: &StationFile) -> anyhow::Result<HashMap<String, Tap>> {
    use crate::commands::capture::plan_capture;
    use crate::commands::scan::{channel_rate, plan, RateCaps};
    use crate::runtime::passband_hz as passband;
    use xng_sdr::tee::{tee, Overflow};

    let mut taps = HashMap::new();
    for dev in &st.devices {
        let users: Vec<&SessionToml> = st
            .sessions
            .iter()
            .filter(|s| s.device.as_deref() == Some(dev.id.as_str()))
            .collect();
        if users.is_empty() {
            tracing::warn!(
                "[[device]] {}: no session reads it, so it isn't opened",
                dev.id
            );
            continue;
        }
        let ctx = |e: String| anyhow::anyhow!("[[device]] {}: {e}", dev.id);
        let mut needs = Vec::new();
        let mut min_rate = 0.0f64;
        for s in &users {
            let mode: Mode = s.mode.parse().map_err(ctx)?;
            let pb = passband(mode);
            min_rate = min_rate.max(match mode {
                // Whole-capture modes demodulate the capture itself.
                Mode::Uat => xng_mode_uat::CHANNEL_RATE,
                _ if pb == 0.0 => plan(mode).0,
                // Every channel is brought down to the decoder's own rate.
                _ => channel_rate(mode),
            });
            for c in session_channels(s) {
                needs.push((crate::freq::parse_hz(&c)?, pb));
            }
        }
        let center = dev
            .center
            .as_deref()
            .map(crate::freq::parse_hz)
            .transpose()?;
        let caps = match (&dev.sdr, dev.sample_rate) {
            (Some(sdr), None) => {
                let caps = crate::sdr_open::probe_rate_caps(sdr);
                if matches!(caps, RateCaps::Unknown) {
                    return Err(ctx(format!(
                        "couldn't ask `{sdr}` which sample rates it supports (not connected, in use \
                         by another program, or no driver for it in this build); set `sample-rate`"
                    )));
                }
                caps
            }
            _ => RateCaps::Unknown,
        };
        let (rate, center_hz) =
            plan_capture(&needs, dev.sample_rate, center, min_rate, &caps).map_err(ctx)?;
        tracing::info!(
            "[[device]] {}: {} session(s), {} channel(s) at {} S/s centered {:.4} MHz",
            dev.id,
            users.len(),
            needs.len(),
            rate as u64,
            center_hz as f64 / 1e6
        );
        let label = format!("[[device]] {}", dev.id);
        let (source, info, overflow) = match (&dev.sdr, &dev.file) {
            (Some(sdr), None) => {
                let (src, backend) = crate::sdr_open::open_sdr(sdr, rate, center_hz, dev.gain)?;
                let info = xng_types::SdrInfo {
                    id: sdr.clone(),
                    driver: backend.to_string(),
                    serial: None,
                };
                (src, info, Overflow::Drop)
            }
            (None, Some(path)) => {
                let fmt = file_format(&label, dev.format.as_deref(), path)?;
                let src: Box<dyn xng_sdr::IqSource> =
                    Box::new(xng_sdr::FileIqSource::open(path, fmt, rate, center_hz)?);
                (src, file_info(), Overflow::Block)
            }
            _ => unreachable!("validated in station::load"),
        };
        let consumers = tee(source, users.len(), overflow);
        taps.insert(
            dev.id.clone(),
            Tap {
                consumers,
                rate,
                center_hz,
                info,
            },
        );
    }
    Ok(taps)
}

/// The sample format of a recording: `format`, or its file extension.
fn file_format(
    label: &str,
    format: Option<&str>,
    path: &Path,
) -> anyhow::Result<xng_sdr::IqFormat> {
    match format {
        Some(f) => f
            .parse::<xng_sdr::IqFormat>()
            .map_err(|e| anyhow::anyhow!("{label}: {e}")),
        None => xng_sdr::IqFormat::from_extension(path)
            .ok_or_else(|| anyhow::anyhow!("{label}: pass `format`")),
    }
}

/// The `SdrInfo` messages from a recording carry.
fn file_info() -> xng_types::SdrInfo {
    xng_types::SdrInfo {
        id: "file".into(),
        driver: "file".into(),
        serial: None,
    }
}

/// Turn a loaded station file into the sessions it describes, opening each
/// SDR or recording, exactly as `xng station` runs them. The shared outputs
/// ride on every session's config (the first one's are used). `console` is
/// the caller's choice: the CLI prints, an embedding front end shows its own
/// views.
pub fn build(
    st: &StationFile,
    console: crate::outputs::console::ConsoleFormat,
) -> anyhow::Result<Vec<(Box<dyn xng_sdr::IqSource>, crate::runtime::SessionConfig)>> {
    use crate::sdr_open::open_sdr;
    use crate::tuning::{parse_receiver_pos, parse_tune, resolve_tune_auto};
    use xng_sdr::FileIqSource;

    // Shared outputs (the first session's SessionConfig carries them).
    // Airframes feeding is routed per-mode/per-session by the router below,
    // not appended to the generic `udp` sinks.
    let udp = st.outputs.udp.clone();
    let outputs = crate::runtime::OutputConfig {
        console,
        jsonl: st.outputs.jsonl.clone(),
        udp,
        asf2_grpc: st.outputs.asf2_grpc.clone(),
        asf2_quic: st.outputs.asf2_quic.clone(),
        asf2_quic_trust: crate::outputs::asf2_quic::TrustMode::SystemRoots,
        metrics: st.outputs.metrics.clone(),
        sbs: st.outputs.sbs.clone(),
        beast: st.outputs.beast.clone(),
        nmea_tcp: st.outputs.nmea_tcp.clone(),
        nmea_udp: st.outputs.nmea_udp.clone(),
        nmea_tag_blocks: st.outputs.nmea_tag_blocks.unwrap_or(false),
        gsmtap: st.outputs.gsmtap.clone(),
        iridium_satmap: st.outputs.iridium_satmap.clone(),
        http: st.outputs.http.clone(),
        map: match &st.outputs.map {
            Some(m) => crate::outputs::map::MapConfig::new(m.style.as_deref(), m.style_url.as_deref(), m.format.as_deref())?,
            None => Default::default(),
        },
        mqtt: st.outputs.mqtt.clone(),
        mqtt_topic: st.outputs.mqtt_topic.clone().unwrap_or_else(|| "xng".into()),
        zmq: st.outputs.zmq.clone(),
        airframes: Some(airframes_router(&st)),
        own_ship_mmsi: st.outputs.own_ship_mmsi,
        airband: {
            st.outputs.airband.validate().map_err(|e| anyhow::anyhow!("[outputs.airband]: {e}"))?;
            st.outputs.airband.clone()
        },
    };

    if let Some(p) = &st.outputs.aircraft_db {
        let n = crate::outputs::dbinfo::AircraftDb::load(p)?;
        tracing::info!("aircraft db: {n} entries");
    }
    if let Some(src) = &st.outputs.iridium_satmap {
        match crate::satmap::init(src) {
            Ok(n) => tracing::info!("iridium satmap: {n} satellites ({src})"),
            Err(e) => tracing::warn!("iridium satmap disabled: {e}"),
        }
    }
    // APRS satellite (TLE) overhead correlation: if an APRS session carries a
    // receiver position, fetch the amateur-satellite TLEs so a space-based APRS
    // reception (145.825 MHz / ISS digipeat) can be attributed to the
    // satellite(s) in view. Best-effort (network) — failure just leaves it off.
    if let Some(pos) = st
        .sessions
        .iter()
        .filter(|s| s.mode.eq_ignore_ascii_case("aprs"))
        .find_map(|s| parse_receiver_pos(&s.receiver_pos).ok().flatten())
    {
        match crate::satmap::init_aprs("auto", pos) {
            Ok(n) => tracing::info!("APRS satellite map: {n} amateur sats (space-reception correlation on)"),
            Err(e) => tracing::warn!("APRS satellite correlation disabled: {e}"),
        }
    }
    let mut taps = open_devices(st)?;
    let mut sessions = Vec::new();
    for (i, sess) in st.sessions.iter().enumerate() {
        let label = format!("session {} ({})", i + 1, sess.mode);
        sess.airband.validate().map_err(|e| anyhow::anyhow!("{label} [session.airband]: {e}"))?;
        // A session on a shared device takes the device's capture.
        let shared = sess
            .device
            .as_ref()
            .map(|id| (taps[id].rate, taps[id].center_hz));
        // Tuning: explicit values, or the mode plan via the same
        // derivation the TUI uses.
        let tune = crate::tuning::Tune {
            mode: sess.mode.clone(),
            sample_rate: shared.map(|(rate, _)| rate).or(sess.sample_rate),
            center_freq: shared
                .map(|(_, center)| format!("{center}Hz"))
                .or_else(|| sess.center.clone()),
            // Airband sessions may list their channels only as
            // [[session.airband.channel]] tables.
            channels: session_channels(sess),
            receiver_pos: sess.receiver_pos.clone(),
            filter_labels: Vec::new(),
            exclude_labels: Vec::new(),
            demod_effort: sess
                .demod_effort
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|e: String| anyhow::anyhow!("{label}: {e}"))?,
            max_ppm: sess.max_ppm,
        };
        let (mode, rate, center_hz, channels) = if tune.sample_rate.is_some()
            && tune.center_freq.is_some()
            && !tune.channels.is_empty()
        {
            parse_tune(&tune)?
        } else if let Some(sdr) = &sess.sdr {
            resolve_tune_auto(&tune, sdr)?
        } else {
            anyhow::bail!("{label}: file sessions need sample-rate, center, and channels");
        };

        let (source, sdr_info): (Box<dyn xng_sdr::IqSource>, Option<xng_types::SdrInfo>) =
            match (&sess.sdr, &sess.file, &sess.device) {
                (None, None, Some(id)) => {
                    let tap = taps.get_mut(id).expect("opened above");
                    let src = tap.consumers.pop().expect("one stream per session");
                    (Box::new(src), Some(tap.info.clone()))
                }
                (Some(sdr), None, None) => {
                    let (src, backend) = open_sdr(sdr, rate, center_hz, sess.gain)?;
                    let info = xng_types::SdrInfo {
                        id: sdr.clone(),
                        driver: backend.to_string(),
                        serial: None,
                    };
                    (src, Some(info))
                }
                (None, Some(path), None) => {
                    let fmt = file_format(&label, sess.format.as_deref(), path)?;
                    let src = FileIqSource::open(path, fmt, rate, center_hz)?;
                    (Box::new(src), Some(file_info()))
                }
                _ => unreachable!("validated in station::load"),
            };

        sessions.push((
            source,
            crate::runtime::SessionConfig {
                mode,
                center_hz,
                channels_hz: channels,
                station_ident: st.station_id.clone(),
                sdr: sdr_info,
                outputs: outputs.clone(),
                receiver_pos: parse_receiver_pos(&sess.receiver_pos)?,
                label_filter: Default::default(),
                ais_filter: crate::runtime::AisFilter {
                    include_types: sess.ais_include_types.clone(),
                    exclude_types: sess.ais_exclude_types.clone(),
                    include_mmsi: sess.ais_include_mmsi.clone(),
                    exclude_mmsi: sess.ais_exclude_mmsi.clone(),
                    min_interval_s: sess.ais_min_interval,
                    dedup_window_s: sess.ais_dedup_window,
                },
                demod_effort: tune.demod_effort.unwrap_or(crate::runtime::DemodEffort::Live),
                max_ppm: tune.max_ppm,
                airband: sess.airband.clone(),
            },
        ));
    }
    // Airband streams are named by frequency, so a frequency may be heard by
    // one session only.
    let mut seen = std::collections::HashSet::new();
    for (_, cfg) in &sessions {
        if cfg.mode == xng_types::Mode::Airband {
            for &f in &cfg.channels_hz {
                anyhow::ensure!(
                    seen.insert(f),
                    "airband channel {:.3} MHz is listed more than once (each frequency can be heard by one session)",
                    f as f64 / 1e6
                );
            }
        }
    }
    Ok(sessions)
}

/// Build the per-session Airframes feed router from a station config.
///
/// Precedence for a session's feed: a session `feed = false` disables it;
/// otherwise `[outputs.airframes.<mode>].enabled` (default = block `enabled`).
/// The station id is the first of: session `airframes-station-id`, the mode
/// block's `station-id`, `auto-suffix(base, mode)` when enabled, then the base
/// (`[outputs.airframes].station-id` or the top-level `station-id`).
///
/// asf-2.0 carries every mode separately under the canonical ident and is not
/// represented here. Modes without a per-port ingest (STD-C, ADS-B, UAT, ...)
/// are skipped (fed via asf-2.0 only).
pub fn airframes_router(f: &StationFile) -> AirframesRouter {
    // Resolve the effective config, honoring the legacy `feed-airframes` bool
    // (ACARS-only, verbatim id) when no `[outputs.airframes]` block is given.
    let (af, legacy_acars_only) = match &f.outputs.airframes {
        Some(a) => (a.clone(), false),
        None if f.outputs.feed_airframes => (AirframesToml { enabled: true, ..Default::default() }, true),
        None => return AirframesRouter::default(),
    };
    if !af.enabled {
        return AirframesRouter::default();
    }

    let base = af.station_id.clone().unwrap_or_else(|| f.station_id.clone());
    let mut routes = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for sess in &f.sessions {
        let Ok(mode) = sess.mode.parse::<Mode>() else { continue };
        if legacy_acars_only && mode != Mode::AcarsPoa {
            continue;
        }
        if sess.feed == Some(false) {
            continue;
        }
        let mode_cfg = af.mode_cfg(mode);
        if mode_cfg.and_then(|m| m.enabled) == Some(false) {
            continue;
        }
        let Some((dhost, dport, proto)) = default_endpoint(mode) else {
            // STD-C/ADS-B/UAT/...: no per-port ingest.
            if mode_cfg.is_some() {
                tracing::info!(
                    "airframes: {} has no public per-port ingest — fed via asf-2.0 only",
                    mode.as_str()
                );
            }
            continue;
        };
        if !has_serializer(mode) {
            if mode_cfg.is_some() {
                tracing::info!(
                    "airframes: {} feed configured but its native serializer isn't implemented yet — fed via asf-2.0 only",
                    mode.as_str()
                );
            }
            continue;
        }

        let station_id = sess
            .airframes_station_id
            .clone()
            .or_else(|| mode_cfg.and_then(|m| m.station_id.clone()))
            .or_else(|| af.auto_suffix.then(|| auto_suffix(&base, mode)))
            .unwrap_or_else(|| base.clone());
        let host = mode_cfg.and_then(|m| m.host.clone()).unwrap_or_else(|| dhost.to_string());
        let port = mode_cfg.and_then(|m| m.port).unwrap_or(dport);

        let key = (station_id.clone(), format!("{host}:{port}"));
        if !seen.insert(key) {
            tracing::warn!(
                "airframes: two decoders feed the same id+endpoint ({station_id} → {host}:{port}); Airframes dedup may inflate counts"
            );
        }

        // Messages carry the id of the radio they came from (a session's own
        // `sdr`, or its `[[device]]`'s) or "file", letting the router target
        // the session precisely.
        let sdr_id = match f.source_of(sess) {
            Some(SessionSource::Sdr(sdr)) => sdr.to_string(),
            _ => "file".to_string(),
        };
        routes.push(Route {
            sdr_id: Some(sdr_id),
            mode,
            target: AirframesTarget { host, port, proto, station_id },
        });
    }

    AirframesRouter { routes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let f: StationFile =
            toml::from_str(include_str!("../../contrib/station.example.toml")).unwrap();
        assert_eq!(f.station_id, "XX-KSEA-1");
        assert_eq!(f.sessions.len(), 3);
        assert!(f.outputs.feed_airframes);
        assert_eq!(f.sessions[1].mode, "vdl2");
        assert!(f.sessions[1].channels.is_empty()); // plan-derived
    }

    #[test]
    fn parses_airband_example() {
        let mut f: StationFile =
            toml::from_str(include_str!("../../contrib/station.airband.example.toml")).unwrap();
        assert_eq!(f.sessions.len(), 2);
        let a = &f.sessions[0].airband;
        a.validate().unwrap();
        assert!(a.mix);
        assert_eq!(a.channels.len(), 3);
        assert_eq!(a.channel(120_500_000).1.unwrap().atc_frequency.as_deref(), Some("airport:KSFO:TWR:120500"));
        assert_eq!(
            a.channel(120_900_000).0.squelch,
            xng_mode_airband::squelch::SquelchMode::Manual { dbfs: -48.0 }
        );
        assert_eq!(f.sessions[1].airband.channel(135_100_000).0.modulation, xng_mode_airband::Modulation::Nfm);
        let o = &f.outputs.airband;
        // The example uses Ogg recordings, Icecast and the ATC feed, which
        // all need Opus.
        #[cfg(feature = "opus")]
        o.validate().unwrap();
        #[cfg(not(feature = "opus"))]
        assert!(o.validate().is_err());
        assert_eq!(o.scan.as_ref().unwrap().priority, vec!["120.5".to_string()]);
        let atc = o.atc.as_ref().unwrap();
        assert_eq!(atc.location_precision_m, 500);
        assert_eq!(atc.server, "atc-ingest.airframes.io:4433");
        f.resolve_paths(Path::new("/srv"));
        assert_eq!(f.outputs.airband.record_dir.as_deref(), Some(Path::new("/var/lib/xng/airband")));
    }

    #[test]
    fn relative_paths_resolve_against_a_given_folder() {
        let toml = r#"
station-id = "X"
[outputs]
jsonl = "log/out.jsonl"
aircraft-db = "/abs/db.csv"
[[session]]
file = "rec/a.cf32"
mode = "acars"
[[session]]
sdr = "driver=rtlsdr"
mode = "vdl2"
"#;
        let mut f: StationFile = toml::from_str(toml).unwrap();
        f.resolve_paths(Path::new("/stations/home"));
        assert_eq!(f.outputs.jsonl.as_deref(), Some(Path::new("/stations/home/log/out.jsonl")));
        assert_eq!(f.outputs.aircraft_db.as_deref(), Some(Path::new("/abs/db.csv")));
        assert_eq!(f.sessions[0].file.as_deref(), Some(Path::new("/stations/home/rec/a.cf32")));
        assert!(f.sessions[1].file.is_none());
    }

    #[test]
    fn rejects_sdr_and_file_together() {
        let toml = r#"
station-id = "X"
[[session]]
sdr = "driver=rtlsdr"
file = "x.cf32"
mode = "acars"
"#;
        let f: StationFile = toml::from_str(toml).unwrap();
        // load() enforces the exclusivity; emulate via the same check
        assert!(!(f.sessions[0].sdr.is_some() ^ f.sessions[0].file.is_some()));
    }

    #[test]
    fn rejects_unknown_fields() {
        let toml = r#"
station-id = "X"
typo-field = 1
[[session]]
sdr = "driver=rtlsdr"
mode = "acars"
"#;
        assert!(toml::from_str::<StationFile>(toml).is_err());
    }

    #[test]
    fn legacy_feed_airframes_routes_acars_only() {
        // The old boolean still feeds ACARS only, station id verbatim — the
        // live-station behavior must not change.
        let toml = r#"
station-id = "KE-KSEA-TEST"
[outputs]
feed-airframes = true
[[session]]
sdr = "driver=rtlsdr,serial=1"
mode = "acars"
[[session]]
sdr = "driver=rtlsdr,serial=2"
mode = "vdl2"
"#;
        let f: StationFile = toml::from_str(toml).unwrap();
        let r = airframes_router(&f);
        assert_eq!(r.routes.len(), 1, "only the ACARS session feeds");
        let route = &r.routes[0];
        assert_eq!(route.mode, Mode::AcarsPoa);
        assert_eq!(route.target.station_id, "KE-KSEA-TEST"); // verbatim, no suffix
        assert_eq!(route.target.port, 5550);
        assert_eq!(route.sdr_id.as_deref(), Some("driver=rtlsdr,serial=1"));
    }

    #[test]
    fn airframes_block_resolves_per_session() {
        let toml = r#"
station-id = "KE-KSEA"
[outputs.airframes]
enabled = true
auto-suffix = true
[[session]]
sdr = "rtl-acars"
mode = "acars"
[[session]]
sdr = "rtl-acars2"
mode = "acars"
feed = false
[[session]]
sdr = "rtl-acars3"
mode = "acars"
airframes-station-id = "KE-KSEA-CUSTOM"
"#;
        let f: StationFile = toml::from_str(toml).unwrap();
        let r = airframes_router(&f);
        assert_eq!(r.routes.len(), 2, "feed=false session is dropped");
        let s1 = r.routes.iter().find(|x| x.sdr_id.as_deref() == Some("rtl-acars")).unwrap();
        assert_eq!(s1.target.station_id, "KE-KSEA-ACARS"); // auto-suffix
        let s3 = r.routes.iter().find(|x| x.sdr_id.as_deref() == Some("rtl-acars3")).unwrap();
        assert_eq!(s3.target.station_id, "KE-KSEA-CUSTOM"); // session override wins
        assert!(r.routes.iter().all(|x| x.sdr_id.as_deref() != Some("rtl-acars2")));
    }

    #[test]
    fn airframes_disabled_block_feeds_nothing() {
        let toml = r#"
station-id = "KE-KSEA"
[outputs.airframes]
enabled = false
[[session]]
sdr = "rtl-acars"
mode = "acars"
"#;
        let f: StationFile = toml::from_str(toml).unwrap();
        assert!(airframes_router(&f).is_empty());
    }

    #[test]
    fn vdl2_routes_via_dumpvdl2_serializer() {
        // VDL2's native serializer (FEED-2.1, dumpvdl2 decoded:json) is wired,
        // so a configured VDL2 decoder routes to the VDL2 ingest (:5552).
        let toml = r#"
station-id = "KE-KSEA"
[outputs.airframes]
enabled = true
[outputs.airframes.vdl2]
enabled = true
[[session]]
sdr = "rtl-vdl2"
mode = "vdl2"
"#;
        let f: StationFile = toml::from_str(toml).unwrap();
        let r = airframes_router(&f);
        let target = r.resolve(xng_types::Mode::Vdl2, Some("rtl-vdl2")).expect("VDL2 routed");
        assert_eq!(target.port, 5552);
    }

    #[test]
    fn rejects_unknown_airframes_mode_table() {
        // ADS-B has no Airframes ingest; the schema rejects the sub-table.
        let toml = r#"
station-id = "X"
[outputs.airframes.adsb]
enabled = true
[[session]]
sdr = "rtl"
mode = "adsb"
"#;
        assert!(toml::from_str::<StationFile>(toml).is_err());
    }

    /// Parse a station file from TOML and run the `load` checks on it.
    fn parse(text: &str) -> anyhow::Result<StationFile> {
        let f: StationFile = toml::from_str(text)?;
        check_sources(&f)?;
        Ok(f)
    }

    /// The load error for `text`, which must fail.
    fn load_error(text: &str) -> String {
        format!("{:#}", parse(text).expect_err("should be refused"))
    }

    /// The shared-device example parses, and its sessions read the device.
    #[test]
    fn parses_shared_example() {
        let f = parse(include_str!("../../contrib/station.shared.example.toml")).unwrap();
        assert_eq!(f.devices.len(), 1);
        assert_eq!(f.sessions.len(), 3);
        assert_eq!(f.sessions[0].device.as_deref(), Some("airspy"));
        match f.source_of(&f.sessions[1]) {
            Some(SessionSource::Sdr(sdr)) => assert!(sdr.starts_with("driver=airspy")),
            _ => panic!("session 2 should read the airspy"),
        }
        assert!(
            matches!(f.source_of(&f.sessions[2]), Some(SessionSource::Sdr(s)) if s.contains("rtlsdr"))
        );
    }

    /// A session reading a `[[device]]` is fed to Airframes: its route is
    /// keyed by the radio its messages come from, not by "file".
    #[test]
    fn shared_device_sessions_route_to_airframes() {
        let f = parse(include_str!("../../contrib/station.shared.example.toml")).unwrap();
        let r = airframes_router(&f);
        let airspy = "driver=airspy,serial=0123456789ABCDEF";
        assert!(r.resolve(Mode::AcarsPoa, Some(airspy)).is_some());
        assert!(r.resolve(Mode::AcarsPoa, Some("file")).is_none());
    }

    /// Two sessions on one serial-numbered radio are refused, and the error
    /// shows the [[device]] block that shares it.
    #[test]
    fn one_radio_in_two_sessions_suggests_a_device() {
        let e = load_error(
            r#"
            station-id = "T"
            [[session]]
            sdr = "driver=airspy,serial=0x1234"
            mode = "acars"
            [[session]]
            sdr = "driver=airspy,serial=0x1234"
            mode = "vdl2"
            "#,
        );
        assert!(e.contains("sessions 1 and 2 both open"), "{e}");
        assert!(e.contains("[[device]]\n    id = \"airspy\""), "{e}");
    }

    /// A selector without a serial opens the next free radio, so two of
    /// them are still two radios.
    #[test]
    fn bare_driver_twice_is_still_allowed() {
        parse(
            r#"
            station-id = "T"
            [[session]]
            sdr = "driver=rtlsdr"
            mode = "acars"
            [[session]]
            sdr = "driver=rtlsdr"
            mode = "ais"
            "#,
        )
        .unwrap();
    }

    /// The mistakes a shared-device config can make, each with its own error.
    #[test]
    fn shared_device_mistakes_are_refused() {
        let device = r#"
            station-id = "T"
            [[device]]
            id = "a"
            sdr = "driver=airspy,serial=0x1234"
        "#;
        let cases = [
            ("[[session]]\ndevice = \"b\"\nmode = \"acars\"\nchannels = [\"131.550\"]", "no [[device]] has id `b`"),
            (
                "[[session]]\ndevice = \"a\"\ngain = 10\nmode = \"acars\"\nchannels = [\"131.550\"]",
                "`gain` belongs on [[device]] a",
            ),
            (
                "[[session]]\ndevice = \"a\"\nsample-rate = 2500000\nmode = \"acars\"\nchannels = [\"131.550\"]",
                "`sample-rate` belongs on [[device]] a",
            ),
            ("[[session]]\ndevice = \"a\"\nmode = \"acars\"", "needs `channels`"),
            (
                "[[session]]\ndevice = \"a\"\nsdr = \"driver=rtlsdr\"\nmode = \"acars\"\nchannels = [\"131.550\"]",
                "exactly one of `sdr`, `device` or `file`",
            ),
            (
                "[[session]]\ndevice = \"a\"\nmode = \"vdl2\"\nchannels = [\"136.975\"]\n\
                 [[session]]\nsdr = \"driver=airspy,serial=0x1234\"\nmode = \"acars\"",
                "already opened by [[device]] a",
            ),
            ("[[device]]\nid = \"a\"\nsdr = \"driver=rtlsdr\"\n[[session]]\ndevice = \"a\"\nmode = \"acars\"\nchannels = [\"131.550\"]", "defined twice"),
            (
                "[[device]]\nid = \"f\"\nfile = \"x.cf32\"\n[[session]]\ndevice = \"f\"\nmode = \"acars\"\nchannels = [\"131.550\"]",
                "a recording needs `sample-rate` and `center`",
            ),
        ];
        for (extra, want) in cases {
            let e = load_error(&format!("{device}\n{extra}"));
            assert!(e.contains(want), "want {want:?}, got: {e}");
        }
    }

    /// A recording shared by an ACARS and a VDL2 session is opened once, and
    /// both sessions get its capture and the same samples.
    #[test]
    fn sessions_on_a_shared_recording_read_the_same_samples() {
        let path = std::env::temp_dir().join(format!("xng-shared-{}.cf32", std::process::id()));
        let samples: Vec<u8> = (0..200_000u32)
            .flat_map(|k| {
                let (re, im) = ((k % 97) as f32 / 97.0, (k % 89) as f32 / -89.0);
                re.to_le_bytes().into_iter().chain(im.to_le_bytes())
            })
            .collect();
        std::fs::write(&path, samples).unwrap();
        let st = parse(&format!(
            r#"
            station-id = "T"
            [[device]]
            id = "rec"
            file = {}
            sample-rate = 10000000
            center = "132.9875M"
            [[session]]
            device = "rec"
            mode = "acars"
            channels = ["131.550", "131.725"]
            [[session]]
            device = "rec"
            mode = "vdl2"
            channels = ["136.975"]
            "#,
            toml::Value::String(path.display().to_string())
        ))
        .unwrap();
        let built = build(&st, crate::outputs::console::ConsoleFormat::Off);
        std::fs::remove_file(&path).ok();
        let mut sessions = built.unwrap();
        assert_eq!(sessions.len(), 2);
        for (src, cfg) in &sessions {
            assert_eq!(src.sample_rate(), 10e6);
            assert_eq!(cfg.center_hz, 132_987_500);
            assert_eq!(cfg.sdr.as_ref().unwrap().driver, "file");
        }
        assert_eq!(sessions[1].1.channels_hz, vec![136_975_000]);
        let mut read = |i: usize| {
            let mut buf = vec![num_complex::Complex::new(0.0f32, 0.0); 4096];
            let n = sessions[i].0.read(&mut buf).unwrap();
            buf.truncate(n);
            buf
        };
        let (a, b) = (read(0), read(1));
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    /// A [[device]] that no session reads holds no radio, so a session may
    /// still open that radio itself.
    #[test]
    fn an_unused_device_reserves_nothing() {
        parse(
            r#"
            station-id = "T"
            [[device]]
            id = "spare"
            sdr = "driver=airspy,serial=0x1234"
            [[session]]
            sdr = "driver=airspy,serial=0x1234"
            mode = "vdl2"
            "#,
        )
        .unwrap();
    }

    /// Two spellings of one Airspy serial are the same radio.
    #[test]
    fn airspy_serials_are_compared_as_numbers() {
        let e = load_error(
            r#"
            station-id = "T"
            [[device]]
            id = "airspy"
            sdr = "driver=airspy,serial=0x1234"
            [[session]]
            device = "airspy"
            mode = "acars"
            channels = ["131.550"]
            [[session]]
            sdr = "serial=0000000000001234,driver=airspy"
            mode = "vdl2"
            "#,
        );
        assert!(e.contains("already opened by [[device]] airspy"), "{e}");
    }

    /// Two sessions of one mode on one device are refused: their messages
    /// couldn't be routed apart.
    #[test]
    fn one_mode_twice_on_a_device_is_refused() {
        let e = load_error(
            r#"
            station-id = "T"
            [[device]]
            id = "airspy"
            sdr = "driver=airspy,serial=0x1234"
            [[session]]
            device = "airspy"
            mode = "acars"
            channels = ["131.550"]
            [[session]]
            device = "airspy"
            mode = "acars"
            channels = ["131.725"]
            feed = false
            "#,
        );
        assert!(e.contains("session 1 already decodes acars"), "{e}");
    }

    /// A shared recording slower than a decoder's own input rate is refused
    /// before anything is opened.
    #[test]
    fn a_shared_capture_below_the_decoder_rate_is_refused() {
        let st = parse(
            r#"
            station-id = "T"
            [[device]]
            id = "rec"
            file = "/nonexistent.cf32"
            sample-rate = 20000
            center = "131.550M"
            [[session]]
            device = "rec"
            mode = "acars"
            channels = ["131.550"]
            "#,
        )
        .unwrap();
        let e = format!("{:#}", build(&st, crate::outputs::console::ConsoleFormat::Off).err().unwrap());
        assert!(e.contains("below"), "{e}");
    }
}
