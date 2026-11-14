//! What the UI asks for, and how it maps onto the same configuration the
//! `xng` CLI builds from its flags. Every CLI tuning, filter and output
//! option has a field here; empty strings from form fields mean "unset".

use serde::Deserialize;
use std::path::PathBuf;
use xng::outputs::{self, console::ConsoleFormat};
use xng::runtime::{AisFilter, DemodEffort, OutputConfig};
use xng::tuning::Tune;

/// Where IQ samples come from.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// A live SDR, by its `--sdr` selector.
    #[serde(rename_all = "camelCase")]
    Device {
        selector: String,
        /// Tuner gain in dB; hardware AGC when absent.
        gain: Option<f64>,
    },
    /// A recorded IQ file.
    #[serde(rename_all = "camelCase")]
    File {
        path: PathBuf,
        /// cf32 / cs16 / cs8 / cu8; guessed from the extension when absent.
        format: Option<String>,
    },
}

/// AIS-only filters (`xng station` session keys).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AisRequest {
    pub include_types: Vec<u8>,
    pub exclude_types: Vec<u8>,
    pub include_mmsi: Vec<u32>,
    pub exclude_mmsi: Vec<u32>,
    pub min_interval_s: Option<f64>,
    pub dedup_window_s: Option<f64>,
}

/// Every output the CLI offers (`xng listen --help`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OutputsRequest {
    pub jsonl: Option<String>,
    pub udp: Vec<String>,
    pub feed_airframes: bool,
    pub asf2_grpc: Option<String>,
    pub asf2_quic: Option<String>,
    pub asf2_quic_ca: Option<String>,
    pub asf2_quic_insecure: bool,
    pub metrics: Option<String>,
    pub sbs: Option<String>,
    pub beast: Option<String>,
    pub nmea_tcp: Option<String>,
    pub nmea_udp: Option<String>,
    pub nmea_tag_blocks: bool,
    pub gsmtap: Option<String>,
    pub iridium_satmap: Option<String>,
    pub gs_file: Option<String>,
    pub aircraft_db: Option<String>,
    pub http: Option<String>,
    pub mqtt: Option<String>,
    pub mqtt_topic: Option<String>,
    pub zmq: Vec<String>,
    pub own_ship_mmsi: Option<u32>,
}

/// One decode session as the UI describes it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest {
    pub source: Source,
    pub mode: String,
    #[serde(default)]
    pub sample_rate: Option<f64>,
    #[serde(default)]
    pub center_freq: Option<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub receiver_pos: Option<String>,
    #[serde(default)]
    pub filter_labels: Vec<String>,
    #[serde(default)]
    pub exclude_labels: Vec<String>,
    #[serde(default)]
    pub demod_effort: Option<String>,
    #[serde(default)]
    pub max_ppm: Option<f64>,
    #[serde(default)]
    pub ais: AisRequest,
    #[serde(default)]
    pub station_id: Option<String>,
    #[serde(default)]
    pub outputs: OutputsRequest,
}

/// `Some(s)` only when `s` has something other than whitespace in it.
pub fn nonempty(v: &Option<String>) -> Option<String> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn nonempty_list(v: &[String]) -> Vec<String> {
    v.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

impl SessionRequest {
    /// The library-level tune request.
    pub fn tune(&self) -> anyhow::Result<Tune> {
        Ok(Tune {
            mode: self.mode.clone(),
            sample_rate: self.sample_rate.filter(|r| *r > 0.0),
            center_freq: nonempty(&self.center_freq),
            channels: nonempty_list(&self.channels),
            receiver_pos: nonempty(&self.receiver_pos),
            filter_labels: nonempty_list(&self.filter_labels),
            exclude_labels: nonempty_list(&self.exclude_labels),
            demod_effort: nonempty(&self.demod_effort)
                .map(|s| s.parse::<DemodEffort>())
                .transpose()
                .map_err(|e| anyhow::anyhow!(e))?,
            max_ppm: self.max_ppm,
        })
    }

    /// Station ident, as `--station-id` (default `XNG-DESKTOP`).
    pub fn station_ident(&self) -> String {
        nonempty(&self.station_id).unwrap_or_else(|| "XNG-DESKTOP".to_owned())
    }

    pub fn ais_filter(&self) -> AisFilter {
        AisFilter {
            include_types: self.ais.include_types.clone(),
            exclude_types: self.ais.exclude_types.clone(),
            include_mmsi: self.ais.include_mmsi.clone(),
            exclude_mmsi: self.ais.exclude_mmsi.clone(),
            min_interval_s: self.ais.min_interval_s,
            dedup_window_s: self.ais.dedup_window_s,
        }
    }

    /// Build the output config exactly as `xng listen` does from its flags
    /// — except the console, which the app replaces with its own views.
    pub fn output_config(&self, mode: xng_types::Mode) -> anyhow::Result<OutputConfig> {
        let o = &self.outputs;
        let ca = nonempty(&o.asf2_quic_ca);
        anyhow::ensure!(
            !(o.asf2_quic_insecure && ca.is_some()),
            "Choose either a trusted CA file or insecure mode for asf-2.0 QUIC, not both"
        );
        let quic_trust = if o.asf2_quic_insecure {
            outputs::asf2_quic::TrustMode::Insecure
        } else if let Some(ca) = ca {
            outputs::asf2_quic::TrustMode::CaFile(PathBuf::from(ca))
        } else {
            outputs::asf2_quic::TrustMode::SystemRoots
        };
        let ident = self.station_ident();
        if o.feed_airframes {
            anyhow::ensure!(
                nonempty(&self.station_id).is_some(),
                "Feeding Airframes needs a station ID (e.g. XX-KSEA-ACARS1)"
            );
        }
        if let Some(p) = nonempty(&o.gs_file) {
            outputs::console::load_gs_names(std::path::Path::new(&p))?;
        }
        if let Some(p) = nonempty(&o.aircraft_db) {
            let n = outputs::dbinfo::AircraftDb::load(std::path::Path::new(&p))?;
            tracing::info!("aircraft db: {n} entries");
        }
        let satmap = nonempty(&o.iridium_satmap);
        if let Some(src) = &satmap {
            match xng::satmap::init(src) {
                Ok(n) => tracing::info!("iridium satmap: {n} satellites ({src})"),
                Err(e) => tracing::warn!("iridium satmap disabled: {e}"),
            }
        }
        Ok(OutputConfig {
            console: ConsoleFormat::Off,
            jsonl: nonempty(&o.jsonl).map(PathBuf::from),
            udp: nonempty_list(&o.udp),
            asf2_grpc: nonempty(&o.asf2_grpc),
            asf2_quic: nonempty(&o.asf2_quic),
            asf2_quic_trust: quic_trust,
            metrics: nonempty(&o.metrics),
            sbs: nonempty(&o.sbs),
            beast: nonempty(&o.beast),
            nmea_tcp: nonempty(&o.nmea_tcp),
            nmea_udp: nonempty(&o.nmea_udp),
            nmea_tag_blocks: o.nmea_tag_blocks,
            gsmtap: nonempty(&o.gsmtap),
            iridium_satmap: satmap,
            http: nonempty(&o.http),
            map: Default::default(),
            mqtt: nonempty(&o.mqtt),
            mqtt_topic: nonempty(&o.mqtt_topic).unwrap_or_else(|| "xng".into()),
            zmq: nonempty_list(&o.zmq),
            airframes: Some(outputs::airframes::cli_router(o.feed_airframes, &ident, &[mode])),
            own_ship_mmsi: o.own_ship_mmsi,
        })
    }
}

/// The capture plan a request resolves to.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTune {
    pub mode: String,
    pub sample_rate: f64,
    pub center_hz: u64,
    pub channels_hz: Vec<u64>,
}

/// Resolve tuning the way the CLI does: fully explicit requests are parsed
/// strictly (as `xng listen`); anything left blank is derived from the
/// mode's built-in plan (as `xng tui` / `xng station`).
pub fn resolve(req: &SessionRequest) -> anyhow::Result<(xng_types::Mode, f64, u64, Vec<u64>)> {
    let tune = req.tune()?;
    if tune.sample_rate.is_some() && tune.center_freq.is_some() && !tune.channels.is_empty() {
        return xng::tuning::parse_tune(&tune);
    }
    match &req.source {
        Source::Device { selector, .. } => xng::tuning::resolve_tune_auto(&tune, selector),
        Source::File { .. } => {
            anyhow::ensure!(
                tune.sample_rate.is_some(),
                "A recording needs its sample rate (the rate it was captured at)"
            );
            // Rate is set, so nothing probes a device.
            xng::tuning::resolve_tune_auto(&tune, "")
        }
    }
}
