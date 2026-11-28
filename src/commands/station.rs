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
//! # Optionally send a mode to several places, e.g. Airframes and acarshub.
//! [outputs.airframes.vdl2]
//! targets = ["tcp://feed.airframes.io:5553", "192.168.1.20:5555"]
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

use crate::outputs::airframes::{
    auto_suffix, default_endpoint, has_serializer, AirframesRouter, AirframesTarget, Proto, Route,
};
use serde::Deserialize;
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
    /// Radiosondes (payload summaries to the sonde intake, UDP 5573).
    pub sonde: Option<AirframesModeToml>,
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
    /// Send this mode to several places instead of one, e.g. Airframes and a
    /// local acarshub: `["tcp://feed.airframes.io:5553", "192.168.1.20:15555"]`.
    /// Each entry is `host:port`, optionally prefixed `udp://`, `tcp://` or
    /// (AIS) `http://`; without a prefix it uses the mode's usual transport.
    /// Every message goes to every target. Replaces `host`/`port`.
    pub targets: Option<Vec<FeedTargetToml>>,
}

/// One `targets` entry: `[udp|tcp|http://]host:port`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(try_from = "String")]
pub struct FeedTargetToml {
    /// `None` = the mode's usual transport.
    pub proto: Option<Proto>,
    pub host: String,
    pub port: u16,
}

impl TryFrom<String> for FeedTargetToml {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let (proto, rest) = match s.split_once("://") {
            Some((scheme, rest)) => (Some(scheme.parse::<Proto>()?), rest),
            None => (None, s.as_str()),
        };
        let example = "like `feed.airframes.io:5550` or `tcp://192.168.1.20:5553`";
        let (host, port) = rest
            .rsplit_once(':')
            .ok_or_else(|| format!("target `{s}` needs host:port, {example}"))?;
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|&p| p != 0)
            .ok_or_else(|| format!("target `{s}`: `{port}` isn't a port number"))?;
        if host.is_empty() || host.contains('/') {
            return Err(format!("target `{s}` needs host:port, {example}"));
        }
        Ok(Self { proto, host: host.to_string(), port })
    }
}

fn default_true() -> bool {
    true
}

impl AirframesToml {
    /// Catch the `targets` mistakes the schema can't: giving it together with
    /// `host`/`port`, an empty list, or a transport the mode's format can't
    /// use (AIS posts batches over HTTP; the others send lines over UDP or TCP).
    pub fn check(&self) -> Result<(), String> {
        let blocks = [
            ("acars", &self.acars, Mode::AcarsPoa),
            ("vdl2", &self.vdl2, Mode::Vdl2),
            ("hfdl", &self.hfdl, Mode::Hfdl),
            ("ais", &self.ais, Mode::Ais),
            ("aero", &self.aero, Mode::AeroL),
            ("iridium", &self.iridium, Mode::Iridium),
        ];
        for (name, block, mode) in blocks {
            let Some(targets) = block.as_ref().and_then(|b| b.targets.as_ref()) else {
                continue;
            };
            let b = block.as_ref().expect("targets come from a block");
            let at = format!("[outputs.airframes.{name}]");
            if b.host.is_some() || b.port.is_some() {
                return Err(format!("{at}: give either `targets` or `host`/`port`, not both"));
            }
            if targets.is_empty() {
                return Err(format!(
                    "{at}: `targets` is empty; leave it out to feed the usual ingest, or set `enabled = false`"
                ));
            }
            for t in targets {
                let Some(p) = t.proto else { continue };
                if mode == Mode::Ais && p != Proto::Http {
                    return Err(format!("{at}: AIS is posted over http, not {}", p.as_str()));
                }
                if mode != Mode::Ais && p == Proto::Http {
                    return Err(format!("{at}: {name} is sent over udp or tcp, not http"));
                }
            }
        }
        Ok(())
    }

    fn mode_cfg(&self, mode: Mode) -> Option<&AirframesModeToml> {
        match mode {
            Mode::AcarsPoa => self.acars.as_ref(),
            Mode::Vdl2 => self.vdl2.as_ref(),
            Mode::Hfdl => self.hfdl.as_ref(),
            Mode::Ais => self.ais.as_ref(),
            Mode::AeroL | Mode::AeroC => self.aero.as_ref(),
            Mode::Iridium => self.iridium.as_ref(),
            Mode::Sonde => self.sonde.as_ref(),
            _ => None,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SessionToml {
    /// SDR selector (`driver=rtlsdr,serial=…`); mutually exclusive
    /// with `file`.
    pub sdr: Option<String>,
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
    if let Some(af) = &f.outputs.airframes {
        af.check().map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    }
    for (i, sess) in f.sessions.iter().enumerate() {
        anyhow::ensure!(
            sess.sdr.is_some() ^ sess.file.is_some(),
            "session {}: exactly one of `sdr` or `file` is required",
            i + 1
        );
    }
    Ok(f)
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
    use xng_sdr::{FileIqSource, IqFormat};

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
        airframes: {
            if let Some(af) = &st.outputs.airframes {
                af.check().map_err(|e| anyhow::anyhow!(e))?;
            }
            Some(airframes_router(st))
        },
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
            Ok(n) => {
                tracing::info!("iridium satmap: {n} satellites ({src})");
                // Receiver location from ring-alert Doppler needs the TLEs.
                crate::doppler_locate::enable(0.0);
            }
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
    let mut sessions = Vec::new();
    for (i, sess) in st.sessions.iter().enumerate() {
        let label = format!("session {} ({})", i + 1, sess.mode);
        sess.airband.validate().map_err(|e| anyhow::anyhow!("{label} [session.airband]: {e}"))?;
        // Airband sessions may list their channels only as
        // [[session.airband.channel]] tables.
        let mut chan_list = sess.channels.clone();
        if chan_list.is_empty() {
            chan_list = sess.airband.channels.iter().map(|c| c.freq.clone()).collect();
        }
        // Tuning: explicit values, or the mode plan via the same
        // derivation the TUI uses.
        let tune = crate::tuning::Tune {
            mode: sess.mode.clone(),
            sample_rate: sess.sample_rate,
            center_freq: sess.center.clone(),
            channels: chan_list,
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
            match (&sess.sdr, &sess.file) {
                (Some(sdr), None) => {
                    let (src, backend) = open_sdr(sdr, rate, center_hz, sess.gain)?;
                    let info = xng_types::SdrInfo {
                        id: sdr.clone(),
                        driver: backend.to_string(),
                        serial: None,
                    };
                    (src, Some(info))
                }
                (None, Some(path)) => {
                    let fmt = match &sess.format {
                        Some(f) => f
                            .parse::<IqFormat>()
                            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?,
                        None => IqFormat::from_extension(path)
                            .ok_or_else(|| anyhow::anyhow!("{label}: pass `format`"))?,
                    };
                    let src = FileIqSource::open(path, fmt, rate, center_hz)?;
                    (
                        Box::new(src),
                        Some(xng_types::SdrInfo {
                            id: "file".into(),
                            driver: "file".into(),
                            serial: None,
                        }),
                    )
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
        // One target by default (the public ingest, or `host`/`port`), or
        // every entry of `targets`.
        let endpoints: Vec<(String, u16, Proto)> = match mode_cfg.and_then(|m| m.targets.as_ref()) {
            Some(targets) => targets
                .iter()
                .map(|t| (t.host.clone(), t.port, t.proto.unwrap_or(proto)))
                .collect(),
            None => vec![(
                mode_cfg.and_then(|m| m.host.clone()).unwrap_or_else(|| dhost.to_string()),
                mode_cfg.and_then(|m| m.port).unwrap_or(dport),
                proto,
            )],
        };

        // A station session is one SDR (or a file); messages carry this id in
        // their provenance, letting the router target it precisely.
        let sdr_id = sess.sdr.clone().unwrap_or_else(|| "file".to_string());
        for (host, port, proto) in endpoints {
            let key = (station_id.clone(), format!("{host}:{port}"));
            if !seen.insert(key) {
                tracing::warn!(
                    "airframes: two decoders feed the same id+endpoint ({station_id} → {host}:{port}); Airframes dedup may inflate counts"
                );
            }
            routes.push(Route {
                sdr_id: Some(sdr_id.clone()),
                mode,
                target: AirframesTarget { host, port, proto, station_id: station_id.clone() },
            });
        }
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
    fn a_mode_can_feed_several_targets() {
        let toml = r#"
station-id = "XX-KHVN"
[outputs.airframes]
[outputs.airframes.vdl2]
station-id = "XX-KHVN-VDL2"
targets = ["tcp://feed.airframes.io:5553", "192.168.1.20:15555"]
[outputs.airframes.acars]
targets = ["feed.airframes.io:5550", "udp://192.168.1.20:15550"]
[[session]]
sdr = "rtl-vdl2"
mode = "vdl2"
[[session]]
sdr = "rtl-acars"
mode = "acars"
"#;
        let f: StationFile = toml::from_str(toml).unwrap();
        f.outputs.airframes.as_ref().unwrap().check().unwrap();
        let r = airframes_router(&f);
        let got = |mode, sdr| -> Vec<(String, Proto, String)> {
            r.resolve_all(mode, Some(sdr))
                .iter()
                .map(|t| (t.endpoint(), t.proto, t.station_id.clone()))
                .collect()
        };
        assert_eq!(
            got(Mode::Vdl2, "rtl-vdl2"),
            [
                ("feed.airframes.io:5553".into(), Proto::Tcp, "XX-KHVN-VDL2".into()),
                // No prefix: VDL2's usual transport.
                ("192.168.1.20:15555".into(), Proto::Udp, "XX-KHVN-VDL2".into()),
            ]
        );
        assert_eq!(
            got(Mode::AcarsPoa, "rtl-acars"),
            [
                ("feed.airframes.io:5550".into(), Proto::Udp, "XX-KHVN".into()),
                ("192.168.1.20:15550".into(), Proto::Udp, "XX-KHVN".into()),
            ]
        );
    }

    #[test]
    fn feed_target_strings_are_checked() {
        let parse = |s: &str| FeedTargetToml::try_from(s.to_string());
        assert_eq!(
            parse("tcp://feed.airframes.io:5553").unwrap(),
            FeedTargetToml { proto: Some(Proto::Tcp), host: "feed.airframes.io".into(), port: 5553 }
        );
        assert_eq!(parse("[::1]:5550").unwrap().host, "[::1]");
        for bad in ["feed.airframes.io", "zmq://h:1", "h:0", "h:port", ":5550", "tcp://h:5553/x"] {
            assert!(parse(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn target_mistakes_are_refused() {
        let check = |block: &str| {
            let toml = format!("station-id = \"X\"\n[outputs.airframes]\n{block}\n[[session]]\nsdr = \"r\"\nmode = \"vdl2\"\n");
            toml::from_str::<StationFile>(&toml).unwrap().outputs.airframes.unwrap().check()
        };
        let err = check("[outputs.airframes.vdl2]\nport = 5553\ntargets = [\"h:1\"]").unwrap_err();
        assert!(err.contains("not both"), "{err}");
        let err = check("[outputs.airframes.vdl2]\ntargets = []").unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let err = check("[outputs.airframes.vdl2]\ntargets = [\"http://h:1\"]").unwrap_err();
        assert!(err.contains("udp or tcp"), "{err}");
        let err = check("[outputs.airframes.ais]\ntargets = [\"udp://h:1\"]").unwrap_err();
        assert!(err.contains("http"), "{err}");
        assert!(check("[outputs.airframes.ais]\ntargets = [\"h:5599\", \"http://h2:80\"]").is_ok());
        // A malformed entry fails while parsing.
        let toml = "station-id = \"X\"\n[outputs.airframes.vdl2]\ntargets = [\"nope\"]\n[[session]]\nsdr = \"r\"\nmode = \"vdl2\"\n";
        assert!(toml::from_str::<StationFile>(toml).is_err());
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
}
