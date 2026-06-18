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

use crate::outputs::airframes::{
    auto_suffix, default_endpoint, has_serializer, AirframesRouter, AirframesTarget, Route,
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
    pub aircraft_db: Option<PathBuf>,
    pub mqtt: Option<String>,
    pub mqtt_topic: Option<String>,
    pub asf2_grpc: Option<String>,
    pub asf2_quic: Option<String>,
    #[serde(default)]
    pub json: bool,
    /// Per-mode Airframes feeding. When omitted, the legacy `feed-airframes`
    /// boolean governs (ACARS-only, station id verbatim).
    pub airframes: Option<AirframesToml>,
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
    /// Disable Airframes feeding for this decoder even when feeding is on
    /// globally.
    pub feed: Option<bool>,
    /// Override the Airframes station id used for this decoder's messages.
    pub airframes_station_id: Option<String>,
}

pub fn load(path: &Path) -> anyhow::Result<StationFile> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let f: StationFile = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    anyhow::ensure!(!f.sessions.is_empty(), "{}: no [[session]] entries", path.display());
    for (i, sess) in f.sessions.iter().enumerate() {
        anyhow::ensure!(
            sess.sdr.is_some() ^ sess.file.is_some(),
            "session {}: exactly one of `sdr` or `file` is required",
            i + 1
        );
    }
    Ok(f)
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
/// represented here. Modes without a public per-port ingest (IMSL/IRDM/STD-C/
/// Aero-C/ADS-B) and modes whose native serializer isn't implemented yet are
/// skipped (fed via asf-2.0 only).
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
            // IMSL/IRDM/STD-C/Aero-C/ADS-B: no public per-port ingest.
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

        // A station session is one SDR (or a file); messages carry this id in
        // their provenance, letting the router target it precisely.
        let sdr_id = sess.sdr.clone().unwrap_or_else(|| "file".to_string());
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
    fn vdl2_configured_but_serializer_not_ready() {
        // VDL2's native serializer (FEED-2.1) isn't implemented yet, so the
        // mode is not routed even when explicitly configured.
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
        assert!(airframes_router(&f).is_empty());
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
