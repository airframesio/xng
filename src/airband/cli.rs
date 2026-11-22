//! `--mode airband` command-line flags (decode, listen). Station configs set
//! the same things in TOML (`[sessions.airband]` and `[outputs.airband]`).

use super::config::{AirbandChannel, AirbandConfig, AirbandOutputs, AtcFeedSettings, RecordFormat, ScanSettings};
use std::path::PathBuf;
use xng_mode_airband::squelch::SquelchMode;
use xng_mode_airband::Modulation;

#[derive(clap::Args, Clone, Debug, Default)]
#[command(next_help_heading = "Airband (--mode airband)")]
pub struct AirbandOpts {
    /// Voice modulation: am (airband) or nfm (land mobile, marine)
    #[arg(long, default_value = "am")]
    pub modulation: Modulation,
    /// Squelch: auto[:SNR_DB] (tracks the noise floor, default 9 dB),
    /// manual:DBFS (fixed level), or off (continuous audio)
    #[arg(long, default_value = "auto")]
    pub squelch: String,
    /// Keep the squelch open this long after the signal drops (ms)
    #[arg(long)]
    pub hang_ms: Option<u32>,
    /// Channel filter width in Hz (default 8000 for AM, 12500 for NFM)
    #[arg(long)]
    pub bandwidth: Option<f64>,
    /// Require this CTCSS tone (Hz) to open the squelch (NFM)
    #[arg(long)]
    pub ctcss: Option<f32>,
    /// Notch out a fixed audio tone (Hz)
    #[arg(long)]
    pub notch: Option<f32>,
    /// Follow carrier offsets up to this many Hz (AFC; off by default)
    #[arg(long)]
    pub afc: Option<f32>,
    /// Audio gain in dB
    #[arg(long)]
    pub audio_gain: Option<f32>,
    /// Channel labels, e.g. 118.3=KSFO Tower,120.5=NorCal Approach
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Airframes ATC frequency keys, e.g. 120.5=airport:KSFO:TWR:120500
    #[arg(long, value_delimiter = ',')]
    pub atc_frequency: Vec<String>,
    /// Also publish a "mix" stream: every channel summed into one
    #[arg(long)]
    pub mix: bool,

    /// Record transmissions under this folder
    #[arg(long)]
    pub record_dir: Option<PathBuf>,
    /// Recording format: wav or ogg (Opus)
    #[arg(long, default_value = "wav")]
    pub record_format: RecordFormat,
    /// Record one file per channel per hour (silence between transmissions)
    /// instead of one file per transmission
    #[arg(long)]
    pub record_continuous: bool,
    /// Don't keep transmissions shorter than this (ms)
    #[arg(long, default_value_t = 0)]
    pub record_min_ms: u32,
    /// Split transmissions longer than this into several files (s; 0 = never)
    #[arg(long, default_value_t = 0)]
    pub record_max_s: u32,
    /// Streams to record: channel frequencies, mix, scan (default every channel)
    #[arg(long, value_delimiter = ',')]
    pub record_streams: Vec<String>,
    /// Send 16 kHz PCM over UDP: host:port[,stream=<freq|mix|scan>][,raw]
    /// (repeatable)
    #[arg(long)]
    pub audio_udp: Vec<String>,
    /// Stream Ogg/Opus to Icecast:
    /// icecast://user:pass@host:port/mount[?stream=...] (repeatable)
    #[arg(long)]
    pub icecast: Vec<String>,

    /// Build a "scan" stream that follows the active channel like a scanner
    #[arg(long)]
    pub scan: bool,
    /// Scan priority channels, highest first (implies --scan)
    #[arg(long, value_delimiter = ',')]
    pub scan_priority: Vec<String>,
    /// Stay on a channel this long after it goes quiet (ms)
    #[arg(long, default_value_t = 2_000)]
    pub scan_hold_ms: u64,
    /// Channels left out of the scan (implies --scan)
    #[arg(long, value_delimiter = ',')]
    pub scan_lockout: Vec<String>,

    /// Feed the Airframes ATC network (after `xng atc login`). Optional
    /// value: ingest host:port
    #[arg(long, num_args = 0..=1, default_missing_value = "atc-ingest.airframes.io:4433")]
    pub atc_feed: Option<String>,
    /// ATC feed transport: auto (QUIC, falling back to TCP), quic or tcp
    #[arg(long, default_value = "auto")]
    pub atc_transport: String,
    /// Use this access token instead of the saved login (dev: dev:<user>)
    #[arg(long)]
    pub atc_token: Option<String>,
    /// Credentials file from `xng atc login` (default ~/.config/xng/atc.json)
    #[arg(long)]
    pub atc_credentials: Option<PathBuf>,
    /// Streams to feed: channel frequencies or scan (default every channel)
    #[arg(long, value_delimiter = ',')]
    pub atc_streams: Vec<String>,
    /// PEM certificate/CA to trust for the ATC ingest (self-hosted, dev)
    #[arg(long)]
    pub atc_ca: Option<PathBuf>,
    /// DANGEROUS: skip ATC ingest certificate checks (dev only)
    #[arg(long)]
    pub atc_insecure: bool,
    /// Station name shown to ATC listeners
    #[arg(long)]
    pub atc_station_name: Option<String>,
    /// Don't show your account's username next to this feeder's feeds
    #[arg(long)]
    pub atc_hide_username: bool,
    /// Opus bitrate for the ATC feed (bit/s)
    #[arg(long, default_value_t = 24_000)]
    pub atc_bitrate: u32,
}

/// Parse `auto[:snr]`, `manual:dbfs`, `off`.
pub fn parse_squelch(s: &str) -> anyhow::Result<SquelchMode> {
    s.parse::<SquelchMode>().map_err(|e| anyhow::anyhow!(e))
}

fn freq_map(v: &[String], what: &str) -> anyhow::Result<Vec<(u64, String)>> {
    v.iter()
        .map(|kv| {
            let (f, val) =
                kv.split_once('=').ok_or_else(|| anyhow::anyhow!("{what} entry {kv:?} must be FREQ=VALUE"))?;
            Ok((crate::freq::parse_hz(f)?, val.trim().to_string()))
        })
        .collect()
}

impl AirbandOpts {
    /// Session settings these flags describe.
    pub fn session(&self) -> anyhow::Result<AirbandConfig> {
        let mut c = AirbandConfig { mix: self.mix, ..Default::default() };
        let d = &mut c.defaults;
        d.modulation = self.modulation;
        d.squelch = parse_squelch(&self.squelch)?;
        if let Some(v) = self.hang_ms {
            d.hang_ms = v;
        }
        d.bandwidth_hz = self.bandwidth;
        d.ctcss_hz = self.ctcss;
        d.notch_hz = self.notch;
        if let Some(v) = self.afc {
            d.afc_max_hz = v;
        }
        if let Some(v) = self.audio_gain {
            d.gain_db = v;
        }
        let mut chans: Vec<AirbandChannel> = Vec::new();
        let mut entry = |f: u64| -> usize {
            match chans.iter().position(|c| c.freq_hz().ok() == Some(f)) {
                Some(i) => i,
                None => {
                    chans.push(AirbandChannel { freq: f.to_string(), ..Default::default() });
                    chans.len() - 1
                }
            }
        };
        let mut labels = Vec::new();
        for (f, l) in freq_map(&self.labels, "--labels")? {
            labels.push((entry(f), l));
        }
        let mut atc = Vec::new();
        for (f, k) in freq_map(&self.atc_frequency, "--atc-frequency")? {
            atc.push((entry(f), k));
        }
        for (i, l) in labels {
            chans[i].label = Some(l);
        }
        for (i, k) in atc {
            chans[i].atc_frequency = Some(k);
        }
        c.channels = chans;
        c.validate()?;
        Ok(c)
    }

    /// Output settings these flags describe.
    pub fn outputs(&self) -> anyhow::Result<AirbandOutputs> {
        let scan =
            (self.scan || !self.scan_priority.is_empty() || !self.scan_lockout.is_empty()).then(|| ScanSettings {
                priority: self.scan_priority.clone(),
                hold_ms: self.scan_hold_ms,
                preempt: true,
                lockout: self.scan_lockout.clone(),
            });
        if let Some(s) = &scan {
            s.to_scan_config()?;
        }
        let atc = self.atc_feed.as_ref().map(|server| AtcFeedSettings {
            server: server.clone(),
            transport: self.atc_transport.clone(),
            token: self.atc_token.clone(),
            credentials: self.atc_credentials.clone(),
            streams: self.atc_streams.clone(),
            ca: self.atc_ca.clone(),
            insecure: self.atc_insecure,
            station_name: self.atc_station_name.clone(),
            hide_username: self.atc_hide_username,
            bitrate: self.atc_bitrate,
            ..Default::default()
        });
        let o = AirbandOutputs {
            record_dir: self.record_dir.clone(),
            record_format: self.record_format,
            record_continuous: self.record_continuous,
            record_min_ms: self.record_min_ms,
            record_max_s: self.record_max_s,
            record_streams: self.record_streams.clone(),
            udp: self.audio_udp.clone(),
            icecast: self.icecast.clone(),
            scan,
            atc,
        };
        o.validate()?;
        Ok(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_mode_airband::squelch::DEFAULT_SNR_DB;

    #[test]
    fn squelch_spellings() {
        assert_eq!(parse_squelch("auto").unwrap(), SquelchMode::Auto { snr_db: DEFAULT_SNR_DB });
        assert_eq!(parse_squelch("auto:12").unwrap(), SquelchMode::Auto { snr_db: 12.0 });
        assert_eq!(parse_squelch("manual:-42.5").unwrap(), SquelchMode::Manual { dbfs: -42.5 });
        assert_eq!(parse_squelch("OFF").unwrap(), SquelchMode::Off);
        assert!(parse_squelch("manual").is_err());
        assert!(parse_squelch("loud").is_err());
    }

    #[test]
    fn labels_and_atc_keys() {
        let o = AirbandOpts {
            squelch: "auto".into(),
            labels: vec!["118.3=KSFO Tower".into(), "120.5=Approach".into()],
            atc_frequency: vec!["118.300=airport:KSFO:TWR:118300".into()],
            ..Default::default()
        };
        let c = o.session().unwrap();
        assert_eq!(c.channels.len(), 2);
        let (_, t) = c.channel(118_300_000);
        let t = t.unwrap();
        assert_eq!(t.label.as_deref(), Some("KSFO Tower"));
        assert_eq!(t.atc_frequency.as_deref(), Some("airport:KSFO:TWR:118300"));
        assert!(AirbandOpts { labels: vec!["118.3".into()], squelch: "auto".into(), ..Default::default() }
            .session()
            .is_err());
    }
}
