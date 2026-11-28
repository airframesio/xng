//! Airband session and output configuration (CLI flags and station TOML).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use xng_mode_airband::squelch::SquelchMode;
use xng_mode_airband::{ChannelConfig, Modulation};

/// Per-session airband settings: defaults for every channel plus per-channel
/// overrides and metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AirbandConfig {
    /// Settings every channel starts from.
    pub defaults: ChannelConfig,
    /// Per-channel label, ATC mapping and overrides.
    #[serde(rename = "channel")]
    pub channels: Vec<AirbandChannel>,
    /// Publish a `mix` stream summing every channel of this session.
    pub mix: bool,
}

/// One channel's metadata and overrides. `freq` accepts the same spellings
/// as `--channels` (MHz like `118.300`, or Hz).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AirbandChannel {
    pub freq: String,
    /// Display name ("KSFO Tower").
    pub label: Option<String>,
    /// Airframes ATC frequency key, e.g. `airport:KSFO:TWR:120500`. When
    /// unset the ATC feeder sends only the frequency and the server resolves
    /// it from the station location.
    pub atc_frequency: Option<String>,
    /// Stable feed key for the ATC feeder (defaults to `<sdr>:<freq>`).
    pub key: Option<String>,
    pub modulation: Option<Modulation>,
    pub bandwidth_hz: Option<f64>,
    pub squelch: Option<SquelchMode>,
    pub open_ms: Option<u32>,
    pub hang_ms: Option<u32>,
    pub ctcss_hz: Option<f32>,
    pub notch_hz: Option<f32>,
    pub highpass_hz: Option<f32>,
    pub lowpass_hz: Option<f32>,
    pub gain_db: Option<f32>,
    pub afc_max_hz: Option<f32>,
    pub agc: Option<bool>,
}

impl AirbandChannel {
    /// This channel's settings: `defaults` with every set override applied.
    pub fn merged(&self, defaults: &ChannelConfig) -> ChannelConfig {
        let mut c = defaults.clone();
        if let Some(v) = self.modulation {
            c.modulation = v;
        }
        if self.bandwidth_hz.is_some() {
            c.bandwidth_hz = self.bandwidth_hz;
        }
        if let Some(v) = self.squelch {
            c.squelch = v;
        }
        if let Some(v) = self.open_ms {
            c.open_ms = v;
        }
        if let Some(v) = self.hang_ms {
            c.hang_ms = v;
        }
        if self.ctcss_hz.is_some() {
            c.ctcss_hz = self.ctcss_hz;
        }
        if self.notch_hz.is_some() {
            c.notch_hz = self.notch_hz;
        }
        if let Some(v) = self.highpass_hz {
            c.highpass_hz = v;
        }
        if let Some(v) = self.lowpass_hz {
            c.lowpass_hz = v;
        }
        if let Some(v) = self.gain_db {
            c.gain_db = v;
        }
        if let Some(v) = self.afc_max_hz {
            c.afc_max_hz = v;
        }
        if let Some(v) = self.agc {
            c.agc = v;
        }
        c
    }

    pub fn freq_hz(&self) -> anyhow::Result<u64> {
        crate::freq::parse_hz(&self.freq)
    }
}

impl AirbandConfig {
    /// Settings and metadata for the channel at `freq_hz`.
    pub fn channel(&self, freq_hz: u64) -> (ChannelConfig, Option<&AirbandChannel>) {
        let ch = self.channels.iter().find(|c| c.freq_hz().ok() == Some(freq_hz));
        let cfg = ch.map(|c| c.merged(&self.defaults)).unwrap_or_else(|| self.defaults.clone());
        (cfg, ch)
    }

    /// Check every channel entry parses and validates.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.defaults.validate().map_err(|e| anyhow::anyhow!("airband defaults: {e}"))?;
        for c in &self.channels {
            let f = c.freq_hz().map_err(|e| anyhow::anyhow!("airband channel {:?}: {e}", c.freq))?;
            c.merged(&self.defaults)
                .validate()
                .map_err(|e| anyhow::anyhow!("airband channel {:.4} MHz: {e}", f as f64 / 1e6))?;
        }
        Ok(())
    }
}

/// Where airband audio goes. Shared by every session (like the other
/// outputs, station mode takes it from the station-level config).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AirbandOutputs {
    /// Record transmissions under this folder.
    pub record_dir: Option<PathBuf>,
    /// `wav` (default) or `ogg` (Opus; needs the `opus` build feature).
    pub record_format: RecordFormat,
    /// One file per channel per hour with silence between transmissions,
    /// instead of one file per transmission.
    pub record_continuous: bool,
    /// Skip transmissions shorter than this (ms) when splitting.
    pub record_min_ms: u32,
    /// Start a new file after this many seconds of one transmission.
    pub record_max_s: u32,
    /// Which streams to record: channel frequencies, `mix`, `scan`. Empty =
    /// every channel.
    pub record_streams: Vec<String>,
    /// Raw PCM over UDP targets: `host:port[,stream=<freq|mix|scan>][,raw]`.
    pub udp: Vec<String>,
    /// Icecast mounts: `icecast://user:pass@host:port/mount[?stream=...]`.
    pub icecast: Vec<String>,
    /// Build the `scan` stream with these settings.
    pub scan: Option<ScanSettings>,
    /// Feed the Airframes ATC aggregator.
    pub atc: Option<AtcFeedSettings>,
    /// Transcribe transmissions with Whisper.
    pub transcribe: Option<TranscribeSettings>,
}

impl AirbandOutputs {
    /// Check every target parses and that what needs Opus has it.
    pub fn validate(&self) -> anyhow::Result<()> {
        parse_streams(&self.record_streams)?;
        for t in &self.udp {
            crate::outputs::airband_udp::parse_target(t)?;
        }
        let needs_opus = self.record_dir.is_some() && self.record_format == RecordFormat::Ogg
            || !self.icecast.is_empty()
            || self.atc.is_some();
        anyhow::ensure!(
            cfg!(feature = "opus") || !needs_opus,
            "Ogg recordings, Icecast and the ATC feed need xng built with the `opus` feature"
        );
        #[cfg(feature = "opus")]
        for u in &self.icecast {
            crate::outputs::airband_icecast::parse_url(u)?;
        }
        if let Some(s) = &self.scan {
            s.to_scan_config()?;
        }
        if let Some(t) = &self.transcribe {
            t.validate()?;
        }
        if let Some(a) = &self.atc {
            anyhow::ensure!(a.server.contains(':'), "ATC ingest {:?} needs host:port", a.server);
            anyhow::ensure!(
                matches!(a.transport.as_str(), "auto" | "quic" | "tcp"),
                "ATC transport must be auto, quic or tcp (got {:?})",
                a.transport
            );
            anyhow::ensure!((6_000..=64_000).contains(&a.bitrate), "ATC bitrate must be 6000 to 64000 bit/s");
            anyhow::ensure!(
                !parse_streams(&a.streams)?.contains(&super::StreamId::Mix),
                "the ATC feed cannot carry the mix stream (listeners need the active frequency); feed channels or scan"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordFormat {
    #[default]
    Wav,
    Ogg,
}

impl std::str::FromStr for RecordFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "wav" => Ok(Self::Wav),
            "ogg" | "opus" => Ok(Self::Ogg),
            o => Err(format!("unknown record format {o:?} (wav|ogg)")),
        }
    }
}

/// Scanner settings for the `scan` stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ScanSettings {
    /// Priority channels, highest first (MHz or Hz).
    pub priority: Vec<String>,
    /// Stay on a channel this long after it goes quiet (ms).
    pub hold_ms: u64,
    /// Let a priority channel interrupt a lower one mid-transmission.
    pub preempt: bool,
    /// Channels never scanned.
    pub lockout: Vec<String>,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self { priority: Vec::new(), hold_ms: 2_000, preempt: true, lockout: Vec::new() }
    }
}

impl ScanSettings {
    pub fn to_scan_config(&self) -> anyhow::Result<xng_mode_airband::scan::ScanConfig> {
        let parse = |v: &[String]| -> anyhow::Result<Vec<u64>> { v.iter().map(|s| crate::freq::parse_hz(s)).collect() };
        Ok(xng_mode_airband::scan::ScanConfig {
            priority: parse(&self.priority)?,
            hold_ms: self.hold_ms,
            preempt: self.preempt,
            lockout: parse(&self.lockout)?,
        })
    }
}

/// Airframes ATC feeder settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AtcFeedSettings {
    /// Ingest `host:port`.
    pub server: String,
    /// `auto` (QUIC, falling back to TCP), `quic` or `tcp`.
    pub transport: String,
    /// Credentials file written by `xng atc login` (default
    /// `~/.config/xng/atc.json`).
    pub credentials: Option<PathBuf>,
    /// A fixed access token instead of the credentials file (dev servers
    /// accept `dev:<user>`).
    pub token: Option<String>,
    /// Station name shown to listeners.
    pub station_name: Option<String>,
    /// The ATC station's UUID on Airframes (HELLO `station.uuid`), to put
    /// this feeder on an existing ATC station of the account. Default: the
    /// one saved in station-uuids.json, first the one Airframes derives
    /// from the feeder id.
    pub station_uuid: Option<String>,
    /// Ask the network not to show the account's username next to this
    /// feeder's feeds.
    pub hide_username: bool,
    /// Round the published station position to this many metres.
    pub location_precision_m: u32,
    /// Opus bitrate (bit/s).
    pub bitrate: u32,
    /// PEM file with the server certificate/CA to trust (self-hosted or dev).
    pub ca: Option<PathBuf>,
    /// Skip certificate verification (dev only).
    pub insecure: bool,
    /// Which streams to feed: channel frequencies, `mix`, `scan`. Empty =
    /// every channel.
    pub streams: Vec<String>,
    /// Receiver description shown to listeners (`{"type":"rtlsdr",...}`).
    pub receiver: Option<serde_json::Value>,
    /// Look each channel without an `atc-frequency` up in the Airframes ATC
    /// catalog by carrier and station position, and feed it under the entry
    /// found (also its name as the label when the channel has none).
    pub catalog_match: bool,
    /// api-server base URL for catalog lookups (default: the one the
    /// sign-in belongs to, else https://api.airframes.io).
    pub api: Option<String>,
}

impl Default for AtcFeedSettings {
    fn default() -> Self {
        Self {
            server: "atc-ingest.airframes.io:4433".into(),
            transport: "auto".into(),
            credentials: None,
            token: None,
            station_name: None,
            station_uuid: None,
            hide_username: false,
            location_precision_m: 100,
            bitrate: 24_000,
            ca: None,
            insecure: false,
            streams: Vec::new(),
            receiver: None,
            catalog_match: true,
            api: None,
        }
    }
}

/// Speech-to-text for airband transmissions. Either a local whisper.cpp
/// model (`model`, needs the `whisper` build feature) or a Whisper HTTP
/// server (`url`: an OpenAI-compatible `/v1/audio/transcriptions`, or
/// whisper.cpp's `server` with its `/inference` path).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct TranscribeSettings {
    /// Speech engine. Local: `whisper.cpp` or `sherpa-onnx` (default: picked
    /// from `model`; with no model, sherpa-onnx in a build that has it, else
    /// whisper.cpp). With `url`: what the
    /// server runs, reported to Airframes ATC (`faster-whisper`, ...;
    /// default `openai-api`).
    pub engine: Option<String>,
    /// Local: a model name fetched once into the cache, or a model file or
    /// directory. whisper.cpp: `tiny.en`, `base.en`, `small.en`,
    /// `large-v3-turbo`, ... or a ggml file. sherpa-onnx:
    /// `skycontrol-atc-110m` (default), `moonshine-tiny`, `moonshine-base`,
    /// `parakeet-110m`, `parakeet-tdt-0.6b-v2`, `sense-voice`, ... or an
    /// exported model directory.
    /// With `url`: the model name sent to the server (default `whisper-1`).
    pub model: Option<String>,
    /// Whisper HTTP server. A bare `http://host:port` gets
    /// `/v1/audio/transcriptions` appended.
    pub url: Option<String>,
    /// Bearer token for `url` (or set XNG_WHISPER_API_KEY).
    pub api_key: Option<String>,
    /// Spoken language (ISO 639-1), or `auto` to let Whisper detect it.
    pub language: String,
    /// Context given to Whisper ahead of each transmission; the default
    /// primes it for ATC phraseology. The channel label is added in front.
    pub prompt: Option<String>,
    /// Which channels to transcribe (frequencies). Empty = every channel.
    pub streams: Vec<String>,
    /// Skip transmissions shorter than this (ms).
    pub min_ms: u32,
    /// Cut longer transmissions into pieces of at most this many seconds
    /// (Whisper works on 30 s windows).
    pub max_s: u32,
    /// CPU threads for the local model (0 = a sensible default).
    pub threads: u32,
    /// Transmissions waiting for Whisper; the oldest are dropped beyond this.
    pub queue: usize,
    /// Drop text Whisper itself rates as probably not speech (0..1).
    pub no_speech_threshold: f32,
}

impl Default for TranscribeSettings {
    fn default() -> Self {
        Self {
            engine: None,
            model: None,
            url: None,
            api_key: None,
            language: "en".into(),
            prompt: None,
            streams: Vec::new(),
            min_ms: 600,
            max_s: 28,
            threads: 0,
            queue: 16,
            no_speech_threshold: 0.6,
        }
    }
}

impl TranscribeSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        let streams = parse_streams(&self.streams)?;
        anyhow::ensure!(
            streams.iter().all(|s| matches!(s, super::StreamId::Channel(_))),
            "transcription works on channels; list frequencies in its streams (not mix or scan)"
        );
        if let Some(u) = &self.url {
            anyhow::ensure!(
                u.starts_with("http://") || u.starts_with("https://"),
                "Whisper URL must start with http:// or https://"
            );
        } else {
            use super::transcribe::{kind, Kind};
            anyhow::ensure!(
                matches!(self.engine.as_deref(), None | Some("whisper.cpp" | "whisper" | "sherpa-onnx" | "sherpa")),
                "local transcription engine must be whisper.cpp or sherpa-onnx (got {:?})",
                self.engine.as_deref().unwrap_or_default()
            );
            match kind(self) {
                Kind::Sherpa => anyhow::ensure!(
                    cfg!(feature = "sherpa"),
                    "sherpa-onnx models need xng built with the `sherpa` feature; or point --whisper-url at a Whisper server"
                ),
                _ => anyhow::ensure!(
                    cfg!(feature = "whisper"),
                    "local Whisper needs xng built with the `whisper` feature (or `sherpa` for Moonshine/Parakeet); \
                     or point --whisper-url at a Whisper server"
                ),
            }
        }
        if let Some(e) = &self.engine {
            anyhow::ensure!(
                !e.is_empty()
                    && e.len() <= 32
                    && e.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)),
                "transcription engine {e:?} must be a short lowercase id like faster-whisper"
            );
        }
        anyhow::ensure!(
            self.language == "auto" || (2..=3).contains(&self.language.len()),
            "Whisper language must be a language code like en, or auto"
        );
        anyhow::ensure!((1..=30).contains(&self.max_s), "Whisper max-s must be 1 to 30");
        anyhow::ensure!(self.queue >= 1, "Whisper queue must be at least 1");
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.no_speech_threshold),
            "Whisper no-speech-threshold must be between 0 and 1"
        );
        Ok(())
    }
}

/// A stream selector as written in config: a channel frequency, `mix` or
/// `scan`.
pub fn parse_stream(s: &str) -> anyhow::Result<super::StreamId> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mix" => Ok(super::StreamId::Mix),
        "scan" => Ok(super::StreamId::Scan),
        f => Ok(super::StreamId::Channel(crate::freq::parse_hz(f)?)),
    }
}

/// Parse a stream list; empty means "every channel".
pub fn parse_streams(v: &[String]) -> anyhow::Result<Vec<super::StreamId>> {
    v.iter().map(|s| parse_stream(s)).collect()
}

/// True if `stream` is selected by `sel` (empty = every channel stream).
pub fn selected(sel: &[super::StreamId], stream: super::StreamId) -> bool {
    if sel.is_empty() {
        matches!(stream, super::StreamId::Channel(_))
    } else {
        sel.contains(&stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_session_block() {
        let t = r#"
            mix = true
            [defaults]
            squelch = "auto:10"
            hang-ms = 400
            [[channel]]
            freq = "118.300"
            label = "Tower"
            atc-frequency = "airport:KSFO:TWR:118300"
            [[channel]]
            freq = "462.5625"
            modulation = "nfm"
            ctcss-hz = 100.0
        "#;
        let c: AirbandConfig = toml::from_str(t).unwrap();
        c.validate().unwrap();
        assert!(c.mix);
        let (tower, meta) = c.channel(118_300_000);
        assert_eq!(meta.unwrap().label.as_deref(), Some("Tower"));
        assert_eq!(tower.hang_ms, 400);
        assert_eq!(tower.squelch, SquelchMode::Auto { snr_db: 10.0 });
        let (frs, _) = c.channel(462_562_500);
        assert_eq!(frs.modulation, Modulation::Nfm);
        assert_eq!(frs.ctcss_hz, Some(100.0));
        assert_eq!(frs.hang_ms, 400);
        // Unlisted channel gets the defaults.
        let (other, meta) = c.channel(121_500_000);
        assert!(meta.is_none());
        assert_eq!(other.hang_ms, 400);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<AirbandConfig>("[[channel]]\nfreq = \"118.3\"\nsquelchh = 1").is_err());
    }

    #[test]
    fn streams() {
        assert_eq!(parse_stream("scan").unwrap(), super::super::StreamId::Scan);
        assert_eq!(parse_stream("118.3").unwrap(), super::super::StreamId::Channel(118_300_000));
        assert!(selected(&[], super::super::StreamId::Channel(1)));
        assert!(!selected(&[], super::super::StreamId::Mix));
    }
}
