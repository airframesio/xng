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
//! ```

use serde::Deserialize;
use std::path::{Path, PathBuf};

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
    pub gsmtap: Option<String>,
    pub http: Option<String>,
    pub aircraft_db: Option<PathBuf>,
    pub mqtt: Option<String>,
    pub mqtt_topic: Option<String>,
    pub asf2_grpc: Option<String>,
    pub asf2_quic: Option<String>,
    #[serde(default)]
    pub json: bool,
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
}
