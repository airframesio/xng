//! Decode session runtime: drives an IQ source through per-channel decoders
//! on a blocking thread, publishes normalized messages to the bus, and fans
//! out to the configured outputs on the tokio runtime.

use crate::bus::MessageBus;
use crate::outputs::console::{self, ConsoleFormat};
use crate::outputs::{acarsdec_json, jsonl};
use num_complex::Complex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use xng_mode_acars::AcarsChannelDecoder;
use xng_mode_adsb::AdsbDecoder;
use xng_mode_aero::{AeroBurstDecoder, AeroChannelDecoder};
use xng_mode_ais::AisChannelDecoder;
use xng_mode_hfdl::HfdlChannelDecoder;
use xng_mode_iridium::{IridiumChannelDecoder, IridiumWidebandDecoder};
use xng_mode_stdc::StdcChannelDecoder;
use xng_mode_vdl2::Vdl2ChannelDecoder;
// New-mode decode cores (IQ demod + ChannelDecoder + to_message per crate).
use xng_mode_adsl::AdslChannelDecoder;
use xng_mode_atcs::AtcsChannelDecoder;
use xng_mode_aprs::AprsChannelDecoder;
use xng_mode_dsc::DscChannelDecoder;
use xng_mode_eot::EotChannelDecoder;
use xng_mode_flex::FlexChannelDecoder;
use xng_mode_navtex::NavtexChannelDecoder;
use xng_mode_pocsag::PocsagChannelDecoder;
use xng_mode_vdes::VdesChannelDecoder;
use xng_mode_sarsat::SarsatChannelDecoder;
use xng_mode_sonde::SondeChannelDecoder;
use xng_mode_uat::UatChannelDecoder;
use xng_sdr::{IqSource, SdrError};
use xng_types::{AppInfo, ChannelInfo, Message, MessageBody, Mode, Provenance, SdrInfo, StationIdentity};

#[derive(Clone)]
pub struct OutputConfig {
    /// Console format (always on).
    pub console: ConsoleFormat,
    pub jsonl: Option<PathBuf>,
    /// acarsdec-JSON UDP targets (host:port).
    pub udp: Vec<String>,
    /// asf-2.0 gRPC ingest URL.
    pub asf2_grpc: Option<String>,
    /// asf-2.0 QUIC ingest host:port.
    pub asf2_quic: Option<String>,
    /// Certificate trust for the QUIC output.
    pub asf2_quic_trust: crate::outputs::asf2_quic::TrustMode,
    /// Prometheus metrics listen address (host:port).
    pub metrics: Option<String>,
    /// SBS-1 (BaseStation, dump1090 port-30003 style) TCP server address.
    pub sbs: Option<String>,
    /// Beast binary TCP server address (Mode S, dump1090 port-30005 style).
    pub beast: Option<String>,
    /// NMEA (AIVDM) TCP server address.
    pub nmea_tcp: Option<String>,
    /// GSMTAP/UDP target for Iridium GSM frames (Wireshark).
    pub gsmtap: Option<String>,
    /// Iridium satellite-name matching TLE source ("auto" or a path).
    pub iridium_satmap: Option<String>,
    /// Web dashboard listen address (live map + message stream).
    pub http: Option<String>,
    /// MQTT broker URL (mqtt://[user:pass@]host[:port]).
    pub mqtt: Option<String>,
    /// MQTT topic prefix (messages publish to `<prefix>/<mode>`).
    pub mqtt_topic: String,
    /// Per-mode Airframes feed router (legacy per-port native-format push;
    /// asf-2.0 multiplexes every mode separately under the canonical ident).
    pub airframes: Option<crate::outputs::airframes::AirframesRouter>,
}

pub struct SessionConfig {
    pub mode: Mode,
    pub center_hz: u64,
    pub channels_hz: Vec<u64>,
    pub station_ident: String,
    pub sdr: Option<SdrInfo>,
    pub outputs: OutputConfig,
    /// Receiver location (lat, lon) — enables ADS-B surface decode.
    pub receiver_pos: Option<(f64, f64)>,
    /// ACARS label filter applied before messages reach the bus.
    pub label_filter: LabelFilter,
    /// Demod effort: Max scans every timing grid (file analysis);
    /// Live trims to a real-time budget for embedded hardware.
    pub demod_effort: DemodEffort,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DemodEffort {
    Live,
    #[default]
    Max,
}

impl std::str::FromStr for DemodEffort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "live" => Ok(Self::Live),
            "max" => Ok(Self::Max),
            other => Err(format!("unknown effort {other:?} (live|max)")),
        }
    }
}

/// Keep/drop filter on the ACARS label. Non-ACARS messages always
/// pass. An empty filter passes everything.
#[derive(Clone, Default)]
pub struct LabelFilter {
    /// When non-empty, only these labels pass.
    pub include: Vec<String>,
    /// Labels dropped even if included.
    pub exclude: Vec<String>,
}

impl LabelFilter {
    pub fn allows(&self, msg: &Message) -> bool {
        let label = match &msg.body {
            xng_types::MessageBody::Acars(a) => &a.label,
            _ => return true,
        };
        if !self.include.is_empty() && !self.include.iter().any(|l| l == label) {
            return false;
        }
        !self.exclude.iter().any(|l| l == label)
    }
}

const READ_CHUNK: usize = 65_536;

/// Live session state shared with the TUI.
pub struct LiveState {
    /// Per channel: (freq_hz, frames, crc_ok, level_dbfs).
    pub stats: std::sync::Mutex<Vec<(u64, u64, u64, f32)>>,
    /// Decoded ACARS message tally keyed by `(freq_hz, label)` — the
    /// dimension the flat `stats` Vec can't carry. Feeds the per-label
    /// Prometheus counter (VERIFY-9 / ACARS-5.2).
    pub acars_labels: std::sync::Mutex<std::collections::HashMap<(u64, String), u64>>,
    pub spectrum: std::sync::Mutex<Option<SpectrumFrame>>,
    pub samples: std::sync::atomic::AtomicU64,
}

pub struct SpectrumFrame {
    pub bins_db: Vec<f32>,
    #[allow(dead_code)]
    pub center_hz: u64,
    #[allow(dead_code)]
    pub span_hz: f64,
}

impl LiveState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stats: std::sync::Mutex::new(Vec::new()),
            acars_labels: std::sync::Mutex::new(std::collections::HashMap::new()),
            spectrum: std::sync::Mutex::new(None),
            samples: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Upsert one channel's cumulative frame stats keyed by frequency. Keying
    /// by freq (not the per-session channel index) lets several station
    /// sessions — each numbering its channels from 0 — share one `LiveState`
    /// without clobbering each other's rows.
    pub fn record_channel(&self, freq: u64, frames: u64, crc_ok: u64, level: f32) {
        let mut s = self.stats.lock().unwrap();
        match s.iter_mut().find(|e| e.0 == freq) {
            Some(e) => *e = (freq, frames, crc_ok, level),
            None => s.push((freq, frames, crc_ok, level)),
        }
    }

    /// Tally one decoded ACARS message under `(freq, label)`.
    pub fn record_acars_label(&self, freq: u64, label: &str) {
        *self.acars_labels.lock().unwrap().entry((freq, label.to_string())).or_insert(0) += 1;
    }
}

/// One mode-specific per-channel decoder.
pub(crate) enum ModeChannel {
    Acars(AcarsChannelDecoder),
    Ais(AisChannelDecoder),
    Adsb(AdsbDecoder),
    Vdl2(Vdl2ChannelDecoder),
    Aero(AeroChannelDecoder),
    AeroBurst(AeroBurstDecoder),
    StdC(StdcChannelDecoder),
    Hfdl(HfdlChannelDecoder),
    Iridium(IridiumChannelDecoder),
    IridiumWide(IridiumWidebandDecoder),
    Uat(UatChannelDecoder),
    Sarsat(SarsatChannelDecoder),
    Dsc(DscChannelDecoder),
    Navtex(NavtexChannelDecoder),
    Aprs(AprsChannelDecoder),
    Pocsag(PocsagChannelDecoder),
    Eot(EotChannelDecoder),
    Flex(FlexChannelDecoder),
    Vdes(VdesChannelDecoder),
    Sonde(SondeChannelDecoder),
    Adsl(AdslChannelDecoder),
    Atcs(AtcsChannelDecoder),
}

impl ModeChannel {
    fn new(
        mode: Mode,
        sample_rate: f64,
        offset: f64,
        freq: u64,
        effort: DemodEffort,
    ) -> Result<Self, String> {
        match mode {
            Mode::AcarsPoa => Ok(Self::Acars(AcarsChannelDecoder::new(sample_rate, offset)?)),
            Mode::Ais => {
                let mut d = AisChannelDecoder::new(sample_rate, offset, freq)?;
                d.set_max_effort(effort == DemodEffort::Max);
                Ok(Self::Ais(d))
            }
            Mode::Vdl2 => Ok(Self::Vdl2(Vdl2ChannelDecoder::new(sample_rate, offset)?)),
            Mode::AeroL => Ok(Self::Aero(AeroChannelDecoder::new(sample_rate, offset)?)),
            Mode::AeroC => Ok(Self::AeroBurst(AeroBurstDecoder::new(sample_rate, offset)?)),
            Mode::StdC => Ok(Self::StdC(StdcChannelDecoder::new(sample_rate, offset)?)),
            Mode::Hfdl => Ok(Self::Hfdl(HfdlChannelDecoder::new(sample_rate, offset)?)),
            Mode::Iridium => {
                // At zero offset (channel == capture center) the wideband
                // burst hunter consumes the whole capture — needed for
                // SBD/ACARS traffic, which hops across duplex channels.
                if offset.abs() < 1e-6 {
                    Ok(Self::IridiumWide(IridiumWidebandDecoder::new(sample_rate)?))
                } else {
                    Ok(Self::Iridium(IridiumChannelDecoder::new(sample_rate, offset)?))
                }
            }
            Mode::Adsb => {
                if offset.abs() > 1e-6 {
                    return Err("Mode S uses the whole capture: tune -c to 1090.000M and pass --channels 1090".into());
                }
                Ok(Self::Adsb(if effort == DemodEffort::Live {
                    AdsbDecoder::new_live(sample_rate)?
                } else {
                    AdsbDecoder::new(sample_rate)?
                }))
            }
            // UAT 978 MHz is wideband like ADS-B: it consumes the whole capture.
            Mode::Uat => {
                if offset.abs() > 1e-6 {
                    return Err("UAT uses the whole capture: tune -c to 978.000M and pass --channels 978".into());
                }
                Ok(Self::Uat(UatChannelDecoder::new(sample_rate)?))
            }
            Mode::Sarsat => Ok(Self::Sarsat(SarsatChannelDecoder::new(sample_rate, offset)?)),
            Mode::Dsc => Ok(Self::Dsc(DscChannelDecoder::new(sample_rate, offset)?)),
            Mode::Navtex => Ok(Self::Navtex(NavtexChannelDecoder::new(sample_rate, offset)?)),
            Mode::Sonde => Ok(Self::Sonde(SondeChannelDecoder::new(sample_rate, offset)?)),
            Mode::AdsL => Ok(Self::Adsl(AdslChannelDecoder::new(sample_rate, offset)?)),
            Mode::Atcs => Ok(Self::Atcs(AtcsChannelDecoder::new(sample_rate, offset)?)),
            Mode::Aprs => Ok(Self::Aprs(AprsChannelDecoder::new(sample_rate, offset)?)),
            // POCSAG transmits at 512/1200/2400 Bd; 1200 is the most common.
            // (Per-session baud selection is a follow-up config knob.)
            Mode::Pocsag => Ok(Self::Pocsag(PocsagChannelDecoder::new(sample_rate, offset, 1200)?)),
            Mode::Eot => Ok(Self::Eot(EotChannelDecoder::new(sample_rate, offset)?)),
            // FLEX: baud 0 = auto-detect the rate from the Sync 1 A-code
            // (1600 2-FSK / 3200 / 6400 4-FSK) — real US paging is 4-level.
            Mode::Flex => Ok(Self::Flex(FlexChannelDecoder::new(sample_rate, offset, 0)?)),
            Mode::Vdes => Ok(Self::Vdes(VdesChannelDecoder::new(sample_rate, offset)?)),
            other => Err(format!("mode {other} has no native core yet")),
        }
    }

    fn passband_hz(mode: Mode) -> f64 {
        match mode {
            Mode::Ais => xng_mode_ais::CHANNEL_PASSBAND_HZ,
            Mode::Vdl2 => xng_mode_vdl2::CHANNEL_PASSBAND_HZ,
            Mode::AeroL | Mode::AeroC => xng_mode_aero::CHANNEL_PASSBAND_HZ,
            Mode::StdC => xng_mode_stdc::CHANNEL_PASSBAND_HZ,
            Mode::Hfdl => xng_mode_hfdl::CHANNEL_PASSBAND_HZ,
            Mode::Iridium => xng_mode_iridium::CHANNEL_PASSBAND_HZ,
            Mode::Adsb | Mode::Uat => 0.0, // wideband: offset must be 0, no DDC
            Mode::Sarsat => xng_mode_sarsat::CHANNEL_PASSBAND_HZ,
            Mode::Dsc => xng_mode_dsc::CHANNEL_PASSBAND_HZ,
            Mode::Navtex => xng_mode_navtex::CHANNEL_PASSBAND_HZ,
            Mode::Sonde => xng_mode_sonde::CHANNEL_PASSBAND_HZ,
            Mode::AdsL => xng_mode_adsl::CHANNEL_PASSBAND_HZ,
            Mode::Atcs => xng_mode_atcs::CHANNEL_PASSBAND_HZ,
            Mode::Aprs => xng_mode_aprs::CHANNEL_PASSBAND_HZ,
            Mode::Pocsag => xng_mode_pocsag::CHANNEL_PASSBAND_HZ,
            Mode::Eot => xng_mode_eot::CHANNEL_PASSBAND_HZ,
            Mode::Flex => xng_mode_flex::CHANNEL_PASSBAND_HZ,
            Mode::Vdes => xng_mode_vdes::CHANNEL_PASSBAND_HZ,
            _ => xng_mode_acars::CHANNEL_PASSBAND_HZ,
        }
    }

    fn channel_rate(&self) -> f64 {
        match self {
            Self::Acars(_) => xng_mode_acars::CHANNEL_RATE,
            Self::Ais(_) => xng_mode_ais::CHANNEL_RATE,
            Self::Vdl2(_) => xng_mode_vdl2::CHANNEL_RATE,
            Self::Aero(_) | Self::AeroBurst(_) => xng_mode_aero::CHANNEL_RATE,
            Self::StdC(_) => xng_mode_stdc::CHANNEL_RATE,
            Self::Hfdl(_) => xng_mode_hfdl::CHANNEL_RATE,
            Self::Iridium(_) => xng_mode_iridium::CHANNEL_RATE,
            Self::IridiumWide(_) => xng_mode_iridium::CHANNEL_RATE,
            Self::Adsb(_) => 2_000_000.0,
            Self::Uat(_) => xng_mode_uat::CHANNEL_RATE,
            Self::Sarsat(_) => xng_mode_sarsat::CHANNEL_RATE,
            Self::Dsc(_) => xng_mode_dsc::CHANNEL_RATE,
            Self::Navtex(_) => xng_mode_navtex::CHANNEL_RATE,
            Self::Sonde(_) => xng_mode_sonde::CHANNEL_RATE,
            Self::Adsl(_) => xng_mode_adsl::CHANNEL_RATE,
            Self::Atcs(_) => xng_mode_atcs::CHANNEL_RATE,
            Self::Aprs(_) => xng_mode_aprs::CHANNEL_RATE,
            Self::Pocsag(_) => xng_mode_pocsag::CHANNEL_RATE,
            Self::Eot(_) => xng_mode_eot::CHANNEL_RATE,
            Self::Flex(_) => xng_mode_flex::CHANNEL_RATE,
            Self::Vdes(_) => xng_mode_vdes::CHANNEL_RATE,
        }
    }

    fn level(&self) -> f32 {
        match self {
            Self::Acars(d) => d.level_dbfs(),
            Self::Ais(d) => d.level_dbfs(),
            Self::Adsb(d) => d.level_dbfs(),
            Self::Vdl2(d) => d.level_dbfs(),
            Self::Aero(d) => d.level_dbfs(),
            Self::AeroBurst(d) => d.level_dbfs(),
            Self::StdC(d) => d.level_dbfs(),
            Self::Hfdl(d) => d.level_dbfs(),
            Self::Iridium(d) => d.level_dbfs(),
            Self::IridiumWide(d) => d.level_dbfs(),
            Self::Uat(d) => d.level_dbfs(),
            Self::Sarsat(d) => d.level_dbfs(),
            Self::Dsc(d) => d.level_dbfs(),
            Self::Navtex(d) => d.level_dbfs(),
            Self::Sonde(d) => d.level_dbfs(),
            Self::Adsl(d) => d.level_dbfs(),
            Self::Atcs(d) => d.level_dbfs(),
            Self::Aprs(d) => d.level_dbfs(),
            Self::Pocsag(d) => d.level_dbfs(),
            Self::Eot(d) => d.level_dbfs(),
            Self::Flex(d) => d.level_dbfs(),
            Self::Vdes(d) => d.level_dbfs(),
        }
    }

    /// Decode a capture chunk into normalized messages.
    /// Returns (messages, frames_seen, frames_crc_ok).
    fn process(&mut self, iq: &[Complex<f32>], freq: u64, prov: &Provenance) -> (Vec<Message>, u64, u64) {
        match self {
            Self::Acars(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let ok = frames.iter().filter(|f| f.crc_ok).count() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_acars::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Ais(dec) => {
                let found = dec.process(iq);
                let seen = found.len() as u64;
                let level = dec.level_dbfs();
                let msgs = found
                    .into_iter()
                    .map(|(f, nmea)| xng_mode_ais::to_message(&f, nmea, freq, level, prov.clone()))
                    .collect();
                // The AIS deframer only surfaces CRC-valid frames.
                (msgs, seen, seen)
            }
            Self::Vdl2(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let ok = frames
                    .iter()
                    .filter(|f| f.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_vdl2::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Aero(dec) => {
                let events = dec.process(iq);
                let seen = events.len() as u64;
                let level = dec.level_dbfs();
                let ok = events
                    .iter()
                    .filter(|e| e.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = events
                    .iter()
                    .map(|e| xng_mode_aero::to_message(e, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::AeroBurst(dec) => {
                let events = dec.process(iq);
                let seen = events.len() as u64;
                let level = dec.level_dbfs();
                let ok = events
                    .iter()
                    .filter(|e| e.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = events
                    .iter()
                    .map(|e| xng_mode_aero::to_message(e, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::StdC(dec) => {
                let packets = dec.process(iq);
                let seen = packets.len() as u64;
                let level = dec.level_dbfs();
                let ok = packets.iter().filter(|p| p.checksum_ok).count() as u64;
                let msgs = packets
                    .iter()
                    .map(|p| xng_mode_stdc::to_message(p, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Iridium(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_iridium::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::IridiumWide(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|(off, f)| {
                        let f_hz = (freq as i64 + off.round() as i64).max(0) as u64;
                        xng_mode_iridium::to_message(f, f_hz, level, prov.clone())
                    })
                    .collect();
                (msgs, seen, seen)
            }
            Self::Hfdl(dec) => {
                let events = dec.process(iq);
                let seen = events.len() as u64;
                let level = dec.level_dbfs();
                let ok = events
                    .iter()
                    .filter(|e| e.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = events
                    .iter()
                    .map(|e| xng_mode_hfdl::to_message(e, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Adsb(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_adsb::to_message(f, freq, prov.clone()))
                    .collect();
                // The validator only surfaces parity-valid frames.
                (msgs, seen, seen)
            }
            // UAT is wideband (whole-capture), like ADS-B: frame carries its own
            // level, RS-FEC gates so every surfaced frame is valid.
            Self::Uat(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_uat::to_message(f, freq, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Sarsat(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_sarsat::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Dsc(dec) => {
                let msgs_dec = dec.process(iq);
                let seen = msgs_dec.len() as u64;
                let level = dec.level_dbfs();
                let msgs = msgs_dec
                    .iter()
                    .map(|f| xng_mode_dsc::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Navtex(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_navtex::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Sonde(dec) => {
                let decoded = dec.process(iq);
                let seen = decoded.len() as u64;
                let level = dec.level_dbfs();
                let msgs = decoded
                    .iter()
                    .map(|d| xng_mode_sonde::to_message(d, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Adsl(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_adsl::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Atcs(dec) => {
                let decoded = dec.process(iq);
                let seen = decoded.len() as u64;
                let level = dec.level_dbfs();
                let msgs = decoded
                    .iter()
                    .map(|d| xng_mode_atcs::to_message(d, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Aprs(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_aprs::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Pocsag(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_pocsag::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Eot(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                // The receive frequency picks the link direction: ~452.9375 MHz
                // carries HOT→EOT commands ("hot"); ~457.9375 MHz carries
                // EOT→HOT telemetry ("eot").
                let is_hot = (freq as i64 - 452_937_500).abs() < (freq as i64 - 457_937_500).abs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_eot::to_message(f, freq, level, is_hot, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Flex(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_flex::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::Vdes(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                // to_message returns None for frames whose ASM payload doesn't
                // decode; count those as seen-but-not-message.
                let msgs: Vec<Message> = frames
                    .iter()
                    .filter_map(|f| xng_mode_vdes::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
        }
    }
}

/// Spawn the configured output sinks on the current tokio runtime.
/// JSON descriptor of one decode session for the dashboard / `xng status`.
fn session_descriptor(
    sdr: &Option<SdrInfo>,
    mode: Mode,
    center_hz: u64,
    channels_hz: &[u64],
    sample_rate: f64,
    receiver_pos: Option<(f64, f64)>,
) -> serde_json::Value {
    let (selector, serial) = match sdr {
        Some(s) => (s.id.clone(), s.serial.clone().or_else(|| parse_serial(&s.id))),
        None => ("file".to_string(), None),
    };
    let mut d = serde_json::json!({
        "sdr": selector,
        "serial": serial,
        "mode": mode.as_str(),
        "center_mhz": center_hz as f64 / 1e6,
        "channels": channels_hz.iter().map(|c| *c as f64 / 1e6).collect::<Vec<_>>(),
        "sample_rate": sample_rate,
    });
    // Receiver position (from `receiver-pos`) so the dashboard can pin the station.
    if let Some((lat, lon)) = receiver_pos {
        d["receiver_pos"] = serde_json::json!([lat, lon]);
    }
    d
}

/// Pull `serial=…` out of a SoapySDR-style selector string.
fn parse_serial(selector: &str) -> Option<String> {
    selector.split(',').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k.trim() == "serial").then(|| v.trim().to_string())
    })
}

fn spawn_outputs(
    bus: &MessageBus,
    outputs: &OutputConfig,
    station: &StationIdentity,
    sessions: &[serde_json::Value],
) -> Vec<tokio::task::JoinHandle<Result<(), std::io::Error>>> {
    let mut output_tasks = Vec::new();
    output_tasks.push(tokio::spawn({
        let rx = bus.subscribe();
        let fmt = outputs.console;
        async move {
            console::run(rx, fmt).await;
            Ok::<(), std::io::Error>(())
        }
    }));
    if let Some(path) = outputs.jsonl.clone() {
        let rx = bus.subscribe();
        output_tasks.push(tokio::spawn(async move { jsonl::run(rx, &path).await }));
    }
    for target in outputs.udp.clone() {
        let rx = bus.subscribe();
        output_tasks.push(tokio::spawn(acarsdec_json::run(rx, target)));
    }
    if let Some(router) = outputs.airframes.clone() {
        if !router.is_empty() {
            let rx = bus.subscribe();
            output_tasks.push(tokio::spawn(crate::outputs::airframes::run(rx, router)));
        }
    }
    if let Some(addr) = outputs.sbs.clone() {
        let rx = bus.subscribe();
        output_tasks.push(tokio::spawn(crate::outputs::sbs::run(rx, addr)));
    }
    if let Some(addr) = outputs.beast.clone() {
        let rx = bus.subscribe();
        output_tasks.push(tokio::spawn(crate::outputs::beast::run(rx, addr)));
    }
    if let Some(addr) = outputs.nmea_tcp.clone() {
        let rx = bus.subscribe();
        output_tasks.push(tokio::spawn(crate::outputs::nmea_tcp::run(rx, addr)));
    }
    if let Some(addr) = outputs.gsmtap.clone() {
        let rx = bus.subscribe();
        output_tasks.push(tokio::spawn(crate::outputs::gsmtap::run(rx, addr)));
    }
    if let Some(addr) = outputs.http.clone() {
        let rx = bus.subscribe();
        let ident = station.ident.clone();
        let sessions = sessions.to_vec();
        output_tasks.push(tokio::spawn(crate::outputs::http::run(rx, addr, ident, sessions)));
    }
    if let Some(url) = outputs.mqtt.clone() {
        let rx = bus.subscribe();
        let topic = outputs.mqtt_topic.clone();
        let ident = station.ident.clone();
        output_tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::outputs::mqtt::run(rx, url, topic, ident).await {
                tracing::error!("mqtt output: {e}");
            }
            Ok(())
        }));
    }
    if let Some(url) = outputs.asf2_grpc.clone() {
        let rx = bus.subscribe();
        let (id, ident) = (station.id.to_string(), station.ident.clone());
        output_tasks.push(tokio::spawn(crate::outputs::asf2_grpc::run(rx, url, id, ident)));
    }
    if let Some(target) = outputs.asf2_quic.clone() {
        let rx = bus.subscribe();
        let trust = outputs.asf2_quic_trust.clone();
        let (id, ident) = (station.id.to_string(), station.ident.clone());
        output_tasks.push(tokio::spawn(crate::outputs::asf2_quic::run(rx, target, trust, id, ident)));
    }
    output_tasks
}

/// Resolve when the process is asked to stop — Ctrl-C (SIGINT) or SIGTERM.
/// Handling SIGTERM matters for SDR sources: `pkill`/service stop send
/// SIGTERM, and without this the process dies before `Drop` runs, leaving
/// e.g. the Airspy still streaming so the next open finds a wedged device
/// (needs an external reset). A graceful stop lets the source close cleanly.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Spawn the interrupt handler: the first signal sets `stop` for a graceful
/// drain (sources close cleanly so the device isn't left wedged); a second
/// signal forces an immediate exit. The escape hatch matters when the decode
/// loop is blocked in a device read that never observes `stop` — without it a
/// wedged SDR traps the process until SIGKILL/SIGQUIT.
fn spawn_interrupt_handler(stop: Arc<AtomicBool>, what: &'static str) {
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("interrupt received, stopping {what} (signal again to force quit)");
        stop.store(true, Ordering::Relaxed);
        shutdown_signal().await;
        eprintln!("forced quit");
        std::process::exit(130);
    });
}

/// Run a decode session until the source ends or `stop` is set.
pub fn run_session(mut source: Box<dyn IqSource>, cfg: SessionConfig) -> anyhow::Result<()> {
    let sample_rate = source.sample_rate();
    let capture_center = if cfg.center_hz > 0 { cfg.center_hz } else { source.center_freq_hz() };

    // Build one decoder per channel up front so config errors surface early.
    let mut decoders = Vec::new();
    for &freq in &cfg.channels_hz {
        let offset = freq as f64 - capture_center as f64;
        if 2.0 * (offset.abs() + ModeChannel::passband_hz(cfg.mode)) > sample_rate {
            anyhow::bail!(
                "channel {:.3} MHz is outside the capture (center {:.3} MHz, rate {} S/s)",
                freq as f64 / 1e6,
                capture_center as f64 / 1e6,
                sample_rate
            );
        }
        let mut dec = ModeChannel::new(cfg.mode, sample_rate, offset, freq, cfg.demod_effort)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        if let (ModeChannel::Adsb(d), Some((lat, lon))) = (&mut dec, cfg.receiver_pos) {
            d.set_receiver_position(lat, lon);
        }
        decoders.push((freq, dec));
    }
    tracing::info!(
        "{} session: {} channel(s) from a {:.0} S/s capture centered at {:.3} MHz",
        cfg.mode,
        decoders.len(),
        sample_rate,
        capture_center as f64 / 1e6
    );

    let station = StationIdentity::new(cfg.station_ident.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let live = LiveState::new();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        if let Some(addr) = cfg.outputs.metrics.clone() {
            let live = live.clone();
            let mode = cfg.mode.as_str().to_string();
            tokio::spawn(async move {
                if let Err(e) = crate::outputs::metrics::serve(addr, live, mode).await {
                    tracing::warn!("metrics endpoint failed: {e}");
                }
            });
        }
        let desc = vec![session_descriptor(
            &cfg.sdr,
            cfg.mode,
            capture_center,
            &cfg.channels_hz,
            sample_rate,
            cfg.receiver_pos,
        )];
        let output_tasks = spawn_outputs(&bus, &cfg.outputs, &station, &desc);

        // Ctrl-C / SIGTERM → graceful stop, second signal forces quit.
        spawn_interrupt_handler(stop.clone(), "session");

        // DSP loop on a blocking thread.
        let decode = tokio::task::spawn_blocking({
            let bus = bus.clone();
            let stop = stop.clone();
            {
                let live = live.clone();
                // Satellite/HF ACARS blocks arrive minutes apart; VHF
                // bearers are quick (libacars timeout profiles).
                let reasm_timeout = match cfg.mode {
                    Mode::AcarsPoa | Mode::Vdl2 => 120.0,
                    _ => 660.0,
                };
                move || {
                    let mut reasm = (
                        xng_acars::reasm::Reassembler::new(reasm_timeout),
                        xng_acars::miam::FileReassembler::new(),
                    );
                    decode_loop(
                        &mut *source,
                        decoders,
                        station,
                        cfg.sdr,
                        bus,
                        stop,
                        Some((live, capture_center, sample_rate)),
                        Some(&mut reasm),
                        cfg.label_filter,
                    )
                }
            }
        });
        let stats = decode.await??;

        drop(bus); // close the channel so outputs drain and exit
        for t in output_tasks {
            if let Err(e) = t.await? {
                tracing::warn!("output error: {e}");
            }
        }

        for (freq, count, crc_ok) in &stats {
            tracing::info!(
                "channel {:.3} MHz: {count} frame(s), {crc_ok} with valid CRC",
                *freq as f64 / 1e6
            );
        }
        let total: u64 = stats.iter().map(|s| s.1).sum();
        tracing::info!("session complete: {total} frame(s) decoded");
        Ok(())
    })
}

pub(crate) fn decode_loop(
    source: &mut dyn IqSource,
    mut decoders: Vec<(u64, ModeChannel)>,
    station: StationIdentity,
    sdr: Option<SdrInfo>,
    bus: MessageBus,
    stop: Arc<AtomicBool>,
    live: Option<(Arc<LiveState>, u64, f64)>,
    mut reasm: Option<&mut (xng_acars::reasm::Reassembler, xng_acars::miam::FileReassembler)>,
    label_filter: LabelFilter,
) -> anyhow::Result<Vec<(u64, u64, u64)>> {
    use std::sync::atomic::Ordering as AtomOrd;
    let mut spectrum_fft: Option<std::sync::Arc<dyn rustfft::Fft<f32>>> = None;
    let mut chunk_count: u64 = 0;
    let mut buf = vec![Complex::new(0.0f32, 0.0f32); READ_CHUNK];
    let mut stats: Vec<(u64, u64, u64)> = decoders.iter().map(|(f, _)| (*f, 0, 0)).collect();
    let mut consecutive_errors: u32 = 0;
    let mut dedup = DedupFilter::new();

    while !stop.load(Ordering::Relaxed) {
        let n = match source.read(&mut buf) {
            Ok(n) => {
                consecutive_errors = 0;
                n
            }
            Err(SdrError::EndOfStream) => break,
            Err(e) => {
                // Transient device hiccups (overflows, timeouts) are routine
                // on live streams; only give up if they persist.
                consecutive_errors += 1;
                if consecutive_errors >= 10 {
                    return Err(anyhow::anyhow!("giving up after {consecutive_errors} consecutive read errors: {e}"));
                }
                tracing::warn!("read error ({consecutive_errors}/10): {e}");
                continue;
            }
        };
        if let Some((state, center_hz, sample_rate)) = &live {
            state.samples.fetch_add(n as u64, AtomOrd::Relaxed);
            chunk_count += 1;
            if chunk_count % 4 == 1 && n >= 512 {
                let fft = spectrum_fft
                    .get_or_insert_with(|| {
                        rustfft::FftPlanner::new().plan_fft_forward(512)
                    })
                    .clone();
                let mut fbuf: Vec<Complex<f32>> = buf[..512]
                    .iter()
                    .enumerate()
                    .map(|(k, &x)| {
                        let w = 0.54
                            - 0.46
                                * (std::f32::consts::TAU * k as f32 / 511.0).cos();
                        x * w
                    })
                    .collect();
                fft.process(&mut fbuf);
                // FFT-shift and convert to dB.
                let bins_db: Vec<f32> = (0..512)
                    .map(|k| {
                        let idx = (k + 256) % 512;
                        10.0 * (fbuf[idx].norm_sqr() / (512.0 * 512.0)).max(1e-12).log10()
                    })
                    .collect();
                *state.spectrum.lock().unwrap() = Some(SpectrumFrame {
                    bins_db,
                    center_hz: *center_hz,
                    span_hz: *sample_rate,
                });
            }
        }
        for (i, (freq, dec)) in decoders.iter_mut().enumerate() {
            let prov = Provenance {
                station: station.clone(),
                app: AppInfo::xng(),
                sdr: sdr.clone(),
                channel: Some(ChannelInfo {
                    index: i as u32,
                    frequency_hz: *freq,
                    sample_rate: dec.channel_rate(),
                }),
            };
            let (msgs, seen, ok) = dec.process(&buf[..n], *freq, &prov);
            stats[i].1 += seen;
            stats[i].2 += ok;
            for mut msg in msgs {
                if dedup.is_duplicate(&msg) {
                    continue;
                }
                if let Some((r, files)) = reasm.as_deref_mut() {
                    apply_reassembly(&mut msg, r, files);
                }
                // Label Iridium ring alerts with the broadcasting satellite
                // (no-op unless a TLE satellite map was loaded at startup).
                crate::satmap::enrich(&mut msg);
                // Attribute space-based APRS (145.825 / ISS digipeat) to the
                // satellite(s) overhead (no-op unless init_aprs ran).
                crate::satmap::enrich_aprs(&mut msg);
                if !label_filter.allows(&msg) {
                    continue;
                }
                if let (Some((state, _, _)), xng_types::MessageBody::Acars(a)) =
                    (&live, &msg.body)
                {
                    state.record_acars_label(*freq, &a.label);
                }
                bus.publish(msg);
            }
            if let Some((state, _, _)) = &live {
                state.record_channel(stats[i].0, stats[i].1, stats[i].2, dec_level_after(dec));
            }
        }
    }
    Ok(stats)
}

/// Run several decode sessions (different modes/SDRs) in one process,
/// sharing one message bus and one set of outputs — the whole station
/// as a single unit.
pub fn run_station(sessions: Vec<(Box<dyn IqSource>, SessionConfig)>) -> anyhow::Result<()> {
    anyhow::ensure!(!sessions.is_empty(), "station config has no sessions");

    struct Prepared {
        source: Box<dyn IqSource>,
        decoders: Vec<(u64, ModeChannel)>,
        cfg: SessionConfig,
        capture_center: u64,
    }
    let mut prepared = Vec::new();
    let mut sessions_desc = Vec::new();
    for (source, cfg) in sessions {
        let sample_rate = source.sample_rate();
        let capture_center =
            if cfg.center_hz > 0 { cfg.center_hz } else { source.center_freq_hz() };
        let mut decoders = Vec::new();
        for &freq in &cfg.channels_hz {
            let offset = freq as f64 - capture_center as f64;
            if 2.0 * (offset.abs() + ModeChannel::passband_hz(cfg.mode)) > sample_rate {
                anyhow::bail!(
                    "[{}] channel {:.3} MHz is outside the capture (center {:.3} MHz, rate {} S/s)",
                    cfg.mode,
                    freq as f64 / 1e6,
                    capture_center as f64 / 1e6,
                    sample_rate
                );
            }
            let mut dec =
                ModeChannel::new(cfg.mode, sample_rate, offset, freq, cfg.demod_effort)
                    .map_err(|e| anyhow::anyhow!("[{}] {:.3} MHz: {e}", cfg.mode, freq as f64 / 1e6))?;
            if let (ModeChannel::Adsb(d), Some((lat, lon))) = (&mut dec, cfg.receiver_pos) {
                d.set_receiver_position(lat, lon);
            }
            decoders.push((freq, dec));
        }
        tracing::info!(
            "station session: {} with {} channel(s) at {:.0} S/s centered {:.3} MHz",
            cfg.mode,
            decoders.len(),
            sample_rate,
            capture_center as f64 / 1e6
        );
        sessions_desc.push(session_descriptor(
            &cfg.sdr,
            cfg.mode,
            capture_center,
            &cfg.channels_hz,
            sample_rate,
            cfg.receiver_pos,
        ));
        prepared.push(Prepared { source, decoders, cfg, capture_center });
    }

    let station = StationIdentity::new(prepared[0].cfg.station_ident.clone());
    let stop = Arc::new(AtomicBool::new(false));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        // One shared LiveState fed by every session's decode loop, so the
        // station's /metrics reflects real per-channel frame + per-label ACARS
        // counts (previously the served state was never updated → all zeros).
        // Only built when metrics is enabled, to keep the no-metrics path free
        // of the per-channel locking + spectrum FFT.
        let live = prepared[0].cfg.outputs.metrics.clone().map(|addr| {
            let live = LiveState::new();
            let served = live.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    crate::outputs::metrics::serve(addr, served, "station".to_string()).await
                {
                    tracing::warn!("metrics endpoint failed: {e}");
                }
            });
            live
        });
        let output_tasks = spawn_outputs(&bus, &prepared[0].cfg.outputs, &station, &sessions_desc);

        spawn_interrupt_handler(stop.clone(), "station");

        let mut decode_tasks = Vec::new();
        for mut prep in prepared {
            let bus = bus.clone();
            let stop = stop.clone();
            let station = station.clone();
            let live = live.clone();
            let capture_center = prep.capture_center;
            decode_tasks.push(tokio::task::spawn_blocking(move || {
                let reasm_timeout = match prep.cfg.mode {
                    Mode::AcarsPoa | Mode::Vdl2 => 120.0,
                    _ => 660.0,
                };
                let mut reasm = (
                    xng_acars::reasm::Reassembler::new(reasm_timeout),
                    xng_acars::miam::FileReassembler::new(),
                );
                let sample_rate = prep.source.sample_rate();
                let live = live.map(|l| (l, capture_center, sample_rate));
                decode_loop(
                    &mut *prep.source,
                    std::mem::take(&mut prep.decoders),
                    station,
                    prep.cfg.sdr.clone(),
                    bus,
                    stop,
                    live,
                    Some(&mut reasm),
                    prep.cfg.label_filter.clone(),
                )
            }));
        }
        for t in decode_tasks {
            match t.await? {
                Ok(stats) => {
                    for (freq, seen, ok) in stats {
                        tracing::info!(
                            "channel {:.3} MHz: {} frame(s), {} with valid CRC",
                            freq as f64 / 1e6,
                            seen,
                            ok
                        );
                    }
                }
                Err(e) => tracing::warn!("session ended with error: {e}"),
            }
        }

        drop(bus);
        for t in output_tasks {
            if let Err(e) = t.await? {
                tracing::warn!("output error: {e}");
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// Suppresses cross-channel duplicates: the same transmission decoded
/// on two frequencies (ACARS uplinks especially) produces byte-identical
/// raw payloads within a short window. Keyed on the raw frame bytes.
pub(crate) struct DedupFilter {
    seen: std::collections::HashMap<u64, std::time::Instant>,
}

const DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_millis(2500);

impl DedupFilter {
    pub(crate) fn new() -> Self {
        Self { seen: std::collections::HashMap::new() }
    }

    /// True when this message is a duplicate of one just published.
    pub(crate) fn is_duplicate(&mut self, msg: &Message) -> bool {
        let Some(raw) = &msg.raw else { return false };
        if raw.is_empty() {
            return false;
        }
        let now = std::time::Instant::now();
        if self.seen.len() > 1024 {
            self.seen.retain(|_, t| now.duration_since(*t) < DEDUP_WINDOW);
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        raw.hash(&mut h);
        msg.mode.as_str().hash(&mut h);
        let key = h.finish();
        match self.seen.get(&key) {
            Some(t) if now.duration_since(*t) < DEDUP_WINDOW => true,
            _ => {
                self.seen.insert(key, now);
                false
            }
        }
    }
}

/// Offer ACARS bodies to the multi-block reassembler; when a message
/// completes, replace the text with the full assembly and re-run the
/// application layer over it (long CPDLC/OHMA/MIAM payloads only decode
/// from complete text).
fn apply_reassembly(
    msg: &mut Message,
    r: &mut xng_acars::reasm::Reassembler,
    files: &mut xng_acars::miam::FileReassembler,
) {
    use xng_acars::reasm::Reasm;
    let MessageBody::Acars(core) = &mut msg.body else { return };
    if !msg.decode.crc_ok {
        return;
    }
    let now = msg.timestamp.timestamp_millis() as f64 / 1e3;
    // MIAM file transfers span many label-MA messages; on completion
    // the combined CORE PDU is attached to the closing segment.
    if core.label == "MA" {
        if let Some(tail) = core.tail.clone() {
            if let Some(pdu) = files.push(&tail, &core.text, now) {
                let mut app = core.app.take().unwrap_or_else(|| serde_json::json!({}));
                app["miam_file_complete"] = serde_json::to_value(&pdu).unwrap_or_default();
                core.app = Some(app);
            }
        }
    }
    if let Reasm::Complete(full) = r.push(core, now) {
        let downlink = core.block_id.is_some_and(|b| b.is_ascii_digit());
        let dec = xng_acars::decode(&core.label, &full, downlink);
        core.text = full;
        core.reassembled = true;
        if let Some(app) = dec.app {
            core.app = serde_json::to_value(&app).ok();
        }
    }
}

fn dec_level_after(dec: &ModeChannel) -> f32 {
    dec.level()
}

/// Build the per-channel decoders for a session config (shared between
/// the console runtime and the TUI).
pub(crate) fn build_decoders(
    sample_rate: f64,
    capture_center: u64,
    cfg: &SessionConfig,
) -> anyhow::Result<Vec<(u64, ModeChannel)>> {
    let mut decoders = Vec::new();
    for &freq in &cfg.channels_hz {
        let offset = freq as f64 - capture_center as f64;
        if 2.0 * (offset.abs() + ModeChannel::passband_hz(cfg.mode)) > sample_rate {
            anyhow::bail!(
                "channel {:.3} MHz is outside the capture",
                freq as f64 / 1e6
            );
        }
        let mut dec = ModeChannel::new(cfg.mode, sample_rate, offset, freq, cfg.demod_effort)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        if let (ModeChannel::Adsb(d), Some((lat, lon))) = (&mut dec, cfg.receiver_pos) {
            d.set_receiver_position(lat, lon);
        }
        decoders.push((freq, dec));
    }
    Ok(decoders)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acars_msg(label: &str) -> Message {
        Message {
            mode: Mode::AcarsPoa,
            timestamp: chrono::Utc::now(),
            frequency_hz: 131_550_000,
            signal: Default::default(),
            decode: Default::default(),
            body: xng_types::MessageBody::Acars(xng_types::AcarsCore {
                label: label.into(),
                ..Default::default()
            }),
            raw: None,
            source: Provenance {
                station: StationIdentity::new("XX-TEST"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn label_filter_include_exclude() {
        let empty = LabelFilter::default();
        assert!(empty.allows(&acars_msg("H1")));

        let only_h1 = LabelFilter { include: vec!["H1".into()], exclude: vec![] };
        assert!(only_h1.allows(&acars_msg("H1")));
        assert!(!only_h1.allows(&acars_msg("Q0")));

        let no_q0 = LabelFilter { include: vec![], exclude: vec!["Q0".into()] };
        assert!(no_q0.allows(&acars_msg("H1")));
        assert!(!no_q0.allows(&acars_msg("Q0")));

        // exclude wins over include
        let both = LabelFilter { include: vec!["Q0".into()], exclude: vec!["Q0".into()] };
        assert!(!both.allows(&acars_msg("Q0")));
    }
}
