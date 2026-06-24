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
use xng_mode_acars::{AcarsChannelDecoder, AcarsMultiChannelDecoder};
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
    /// NMEA (AIVDM) UDP push target (host:port).
    pub nmea_udp: Option<String>,
    /// Prefix NMEA output with a tag-block (`\s:<station>,c:<ts>*HH\`).
    pub nmea_tag_blocks: bool,
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
    /// Own-ship MMSI: when set (with a station `receiver-pos`), an AIVDO Type 1
    /// position report is emitted periodically so chart plotters show the
    /// station (AIS-5c).
    pub own_ship_mmsi: Option<u32>,
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
    /// AIS type/MMSI filter + rate downsample + content dedup (AIS-5h).
    pub ais_filter: AisFilter,
    /// Demod effort: Max scans every timing grid (file analysis);
    /// Live trims to a real-time budget for embedded hardware.
    pub demod_effort: DemodEffort,
    /// VDL2 CFO reject (ppm); `None` disables it (VDL2-7).
    pub max_ppm: Option<f64>,
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

/// Output-side AIS shaping (AIS-5h): keep/drop by message type and MMSI,
/// rate-downsample dynamic position reports, and drop content-duplicate
/// re-reports. Non-AIS messages always pass. The static keep/drop lives here
/// (pure, testable); the time-stateful rate + dedup parts run in [`AisGate`].
#[derive(Clone, Default)]
pub struct AisFilter {
    /// When non-empty, only these AIS message types pass.
    pub include_types: Vec<u8>,
    /// Types dropped even if included.
    pub exclude_types: Vec<u8>,
    /// When non-empty, only these MMSIs pass.
    pub include_mmsi: Vec<u32>,
    /// MMSIs dropped even if included.
    pub exclude_mmsi: Vec<u32>,
    /// Minimum seconds between dynamic position reports per MMSI; `None` off.
    pub min_interval_s: Option<f64>,
    /// Drop a `(mmsi, content)` that repeats within this many seconds; `None` off.
    pub dedup_window_s: Option<f64>,
}

impl AisFilter {
    /// Static keep/drop on message type + MMSI (include-then-exclude). Non-AIS
    /// bodies always pass.
    pub fn allows(&self, msg: &Message) -> bool {
        let (mt, mmsi) = match &msg.body {
            MessageBody::Ais { msg_type, mmsi, .. } => (*msg_type, *mmsi),
            _ => return true,
        };
        if let Some(t) = mt {
            if !self.include_types.is_empty() && !self.include_types.contains(&t) {
                return false;
            }
            if self.exclude_types.contains(&t) {
                return false;
            }
        }
        if let Some(m) = mmsi {
            if !self.include_mmsi.is_empty() && !self.include_mmsi.contains(&m) {
                return false;
            }
            if self.exclude_mmsi.contains(&m) {
                return false;
            }
        }
        true
    }
}

/// Per-session time-stateful half of [`AisFilter`]: rate downsample + content
/// dedup. Keyed on MMSI / decoded content so it generalizes to the cross-mode
/// dedup (XM-5) later.
#[derive(Default)]
struct AisGate {
    /// MMSI → time (s) of the last *kept* dynamic position report.
    last_pos: std::collections::HashMap<u32, f64>,
    /// content hash → time (s) last seen.
    seen: std::collections::HashMap<u64, f64>,
    /// Messages since the last `seen` sweep, for amortized stale-entry pruning.
    sweeps: u32,
}

impl AisGate {
    /// True to keep `msg`; `now` is the message time in seconds. Applies the
    /// rate downsample (dynamic position-report types) and content dedup from
    /// `cfg`. Non-AIS messages always pass.
    fn pass(&mut self, msg: &Message, cfg: &AisFilter, now: f64) -> bool {
        /// Prune stale `seen`/`last_pos` entries every N kept messages rather
        /// than on every one — the lookups already treat expired entries as
        /// absent, so `retain` is only for memory reclamation, not correctness.
        const SWEEP_EVERY: u32 = 256;
        let MessageBody::Ais { msg_type, mmsi, details, .. } = &msg.body else {
            return true;
        };
        // Rate downsample: thin frequent dynamic position reports per vessel.
        // Decide here, but DON'T advance the per-MMSI clock yet — a message
        // dropped by the dedup stage below was never emitted, so it must not
        // reset the rate window (else a genuinely new fix arriving shortly
        // after a dedup-dropped duplicate would be wrongly throttled).
        let rate_mmsi = match (cfg.min_interval_s, *mmsi) {
            (Some(min), Some(m)) if matches!(msg_type, Some(1 | 2 | 3 | 18 | 19 | 27)) => {
                if self.last_pos.get(&m).is_some_and(|&last| now - last < min) {
                    return false;
                }
                Some(m)
            }
            _ => None,
        };
        // Content dedup: collapse identical (mmsi, type, decoded content)
        // within the window (multi-receiver echoes, repeated static reports).
        if let Some(win) = cfg.dedup_window_s {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            mmsi.hash(&mut h);
            msg_type.hash(&mut h);
            details.as_ref().map(|d| d.to_string()).unwrap_or_default().hash(&mut h);
            let key = h.finish();
            if self.seen.get(&key).is_some_and(|&t| now - t < win) {
                return false;
            }
            self.seen.insert(key, now);
        }
        // Both gates passed → record the kept position for the rate window.
        if let Some(m) = rate_mmsi {
            self.last_pos.insert(m, now);
        }
        // Amortized housekeeping: both maps only grow on kept messages, and the
        // lookups above already treat an expired entry as absent, so `retain`
        // is purely memory reclamation — run it every SWEEP_EVERY kept messages,
        // not per message, and for whichever map's gate is enabled.
        self.sweeps += 1;
        if self.sweeps >= SWEEP_EVERY {
            self.sweeps = 0;
            if let Some(win) = cfg.dedup_window_s {
                self.seen.retain(|_, t| now - *t < win);
            }
            if let Some(min) = cfg.min_interval_s {
                // An entry older than `min` would clear the rate gate anyway, so
                // dropping it is behaviour-preserving; this bounds last_pos to
                // recently-active MMSIs instead of leaking one entry per vessel
                // ever heard for the session's life.
                self.last_pos.retain(|_, t| now - *t < min);
            }
        }
        true
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
    /// Cumulative FEC-corrected octets/bits per channel freq (ECO-7).
    pub fec: std::sync::Mutex<std::collections::HashMap<u64, u64>>,
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
            fec: std::sync::Mutex::new(std::collections::HashMap::new()),
            spectrum: std::sync::Mutex::new(None),
            samples: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Add `n` FEC-corrected units (octets/bits, mode-specific) for `freq`.
    pub fn record_fec(&self, freq: u64, n: u64) {
        if n > 0 {
            *self.fec.lock().unwrap().entry(freq).or_insert(0) += n;
        }
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
    /// Several ACARS channels sharing one downconverter front end (the CPU
    /// optimization). Carries the per-channel frequencies so each decoded
    /// frame is tagged with the right channel. Unlike the single-channel
    /// variants this one spans MANY channels, so it is special-cased in the
    /// decode loop (see `process_shared`).
    //
    // TODO(perf): the decode loop currently assumes one (freq, ModeChannel)
    // per channel and tags every message from a ModeChannel with one
    // Provenance. This shared variant is special-cased in `decode_loop` to
    // emit its own per-channel-tagged messages. When the shared front end is
    // rolled out to vdl2/ais/aero/stdc (all use the same per-channel offset
    // DDC), generalize the loop to natively drive multi-output decoders
    // instead of special-casing ACARS here.
    AcarsShared { dec: AcarsMultiChannelDecoder, freqs: Vec<u64> },
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
        max_ppm: Option<f64>,
    ) -> Result<Self, String> {
        match mode {
            Mode::AcarsPoa => Ok(Self::Acars(AcarsChannelDecoder::new(sample_rate, offset)?)),
            Mode::Ais => {
                let mut d = AisChannelDecoder::new(sample_rate, offset, freq)?;
                d.set_max_effort(effort == DemodEffort::Max);
                Ok(Self::Ais(d))
            }
            Mode::Vdl2 => {
                let mut d = Vdl2ChannelDecoder::new(sample_rate, offset)?;
                d.set_max_ppm(max_ppm);
                Ok(Self::Vdl2(d))
            }
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
            Self::Acars(_) | Self::AcarsShared { .. } => xng_mode_acars::CHANNEL_RATE,
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
            // Shared ACARS is driven via `process_shared`; this scalar level
            // accessor reports channel 0 (used only for single-channel
            // displays, which the shared path does not take).
            Self::AcarsShared { dec, .. } => {
                if dec.num_channels() > 0 { dec.level_dbfs(0) } else { f32::NEG_INFINITY }
            }
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
            // Shared ACARS spans many channels and cannot be tagged with one
            // freq/provenance; the decode loop drives it via `process_shared`.
            Self::AcarsShared { .. } => (Vec::new(), 0, 0),
            Self::Acars(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let ok = frames.iter().filter(|f| f.crc_ok).count() as u64;
                let (level, noise) = (dec.level_dbfs(), dec.noise_dbfs());
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_acars::to_message(f, freq, level, noise, prov.clone()))
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

    /// Decode a capture chunk for a SHARED multi-channel decoder, producing one
    /// result per channel that actually decoded a frame. Each result is tagged
    /// with its own channel index, frequency, level, and provenance — the
    /// per-channel work the decode loop does for single-channel decoders, done
    /// here because one shared decoder spans many channels.
    ///
    /// Returns `(channel_index, freq, level, msgs, seen, ok)` per channel.
    /// Only `Self::AcarsShared` produces output; other variants return empty.
    fn process_shared(
        &mut self,
        iq: &[Complex<f32>],
        base_prov: &SharedProvenance,
    ) -> Vec<(usize, u64, f32, Vec<Message>, u64, u64)> {
        let Self::AcarsShared { dec, freqs } = self else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, frames) in dec.process(iq) {
            let freq = freqs[i];
            let (level, noise) = (dec.level_dbfs(i), dec.noise_dbfs(i));
            let prov = base_prov.for_channel(i, freq, xng_mode_acars::CHANNEL_RATE);
            let seen = frames.len() as u64;
            let ok = frames.iter().filter(|f| f.crc_ok).count() as u64;
            let msgs: Vec<Message> = frames
                .iter()
                .map(|f| xng_mode_acars::to_message(f, freq, level, noise, prov.clone()))
                .collect();
            out.push((i, freq, level, msgs, seen, ok));
        }
        out
    }
}

/// The provenance fields common to every channel of a session; the per-channel
/// `ChannelInfo` is filled in per frame for shared decoders.
pub(crate) struct SharedProvenance {
    station: StationIdentity,
    app: AppInfo,
    sdr: Option<SdrInfo>,
}

impl SharedProvenance {
    fn for_channel(&self, index: usize, freq: u64, channel_rate: f64) -> Provenance {
        Provenance {
            station: self.station.clone(),
            app: self.app.clone(),
            sdr: self.sdr.clone(),
            channel: Some(ChannelInfo {
                index: index as u32,
                frequency_hz: freq,
                sample_rate: channel_rate,
            }),
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
        let tags = outputs.nmea_tag_blocks;
        output_tasks.push(tokio::spawn(crate::outputs::nmea_tcp::run(rx, addr, tags)));
    }
    if let Some(target) = outputs.nmea_udp.clone() {
        let rx = bus.subscribe();
        let tags = outputs.nmea_tag_blocks;
        output_tasks.push(tokio::spawn(crate::outputs::nmea_udp::run(rx, target, tags)));
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
        let mut dec = ModeChannel::new(cfg.mode, sample_rate, offset, freq, cfg.demod_effort, cfg.max_ppm)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        if let (ModeChannel::Adsb(d), Some((lat, lon))) = (&mut dec, cfg.receiver_pos) {
            d.set_receiver_position(lat, lon);
        }
        decoders.push((freq, dec));
    }
    let n_channels = decoders.len();
    decoders = collapse_shared_acars(decoders, sample_rate, capture_center, cfg.mode);
    tracing::info!(
        "{} session: {} channel(s) from a {:.0} S/s capture centered at {:.3} MHz",
        cfg.mode,
        n_channels,
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
                        cfg.ais_filter,
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
    ais_filter: AisFilter,
) -> anyhow::Result<Vec<(u64, u64, u64)>> {
    use std::sync::atomic::Ordering as AtomOrd;
    let mut spectrum_fft: Option<std::sync::Arc<dyn rustfft::Fft<f32>>> = None;
    let mut chunk_count: u64 = 0;
    let mut buf = vec![Complex::new(0.0f32, 0.0f32); READ_CHUNK];
    // Per-index stats for single-channel decoders. Shared multi-channel
    // decoders (one index spanning many channels) are excluded here and
    // tracked per-frequency in `shared_stats` instead.
    let mut stats: Vec<(u64, u64, u64)> = decoders
        .iter()
        .filter(|(_, d)| !matches!(d, ModeChannel::AcarsShared { .. }))
        .map(|(f, _)| (*f, 0, 0))
        .collect();
    // Per-frequency stats for shared multi-channel decoders, seeded with every
    // channel so even zero-frame channels appear in the summary.
    let mut shared_stats: std::collections::BTreeMap<u64, (u64, u64)> =
        std::collections::BTreeMap::new();
    for (_, d) in &decoders {
        if let ModeChannel::AcarsShared { freqs, .. } = d {
            for &f in freqs {
                shared_stats.insert(f, (0, 0));
            }
        }
    }
    let mut consecutive_errors: u32 = 0;
    let mut dedup = DedupFilter::new();
    let mut ais_gate = AisGate::default();

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
            // Shared multi-channel decoders (ACARS) emit per-channel results
            // already tagged with the right freq/provenance; drive them
            // separately so the single-channel path below stays unchanged.
            //
            // TODO(perf): this special-case exists because the shared front
            // end spans many channels but the loop's stats/provenance are
            // per-decoder-index. When the shared front end is generalized to
            // vdl2/ais/aero/stdc, fold this into the main path (a decoder
            // yielding per-channel (freq, msgs, …) results) rather than
            // branching on the ACARS variant here.
            if let ModeChannel::AcarsShared { .. } = dec {
                let base = SharedProvenance {
                    station: station.clone(),
                    app: AppInfo::xng(),
                    sdr: sdr.clone(),
                };
                for (_ci, freq, level, msgs, seen, ok) in dec.process_shared(&buf[..n], &base) {
                    let entry = shared_stats.entry(freq).or_insert((0, 0));
                    entry.0 += seen;
                    entry.1 += ok;
                    if let Some((state, _, _)) = &live {
                        let fec: u64 =
                            msgs.iter().map(|m| m.decode.fec_corrected.unwrap_or(0) as u64).sum();
                        state.record_fec(freq, fec);
                    }
                    for msg in msgs {
                        publish_msg(
                            msg, freq, &mut dedup, reasm.as_deref_mut(), &label_filter,
                            &ais_filter, &mut ais_gate, &live, &bus,
                        );
                    }
                    if let Some((state, _, _)) = &live {
                        let (s, o) = *entry;
                        state.record_channel(freq, s, o, level);
                    }
                }
                continue;
            }

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
            if let Some((state, _, _)) = &live {
                let fec: u64 = msgs.iter().map(|m| m.decode.fec_corrected.unwrap_or(0) as u64).sum();
                state.record_fec(*freq, fec);
            }
            for msg in msgs {
                publish_msg(
                    msg, *freq, &mut dedup, reasm.as_deref_mut(), &label_filter,
                    &ais_filter, &mut ais_gate, &live, &bus,
                );
            }
            if let Some((state, _, _)) = &live {
                state.record_channel(stats[i].0, stats[i].1, stats[i].2, dec_level_after(dec));
            }
        }
    }
    // Fold any shared per-channel stats into the returned summary.
    for (freq, (seen, ok)) in shared_stats {
        stats.push((freq, seen, ok));
    }
    Ok(stats)
}

/// Apply the full publish pipeline to one decoded message: cross-channel
/// dedup, multi-block reassembly, satellite enrichment, label/AIS filtering,
/// the per-label live tally, then publish to the bus. Shared by the
/// single-channel and shared-ACARS paths in `decode_loop`.
#[allow(clippy::too_many_arguments)]
// The publish pipeline genuinely needs all of this session state; threading
// it as args keeps decode_loop's borrow checker happy without a context
// struct (a future cleanup when the loop is generalized — see the perf TODO).
fn publish_msg(
    mut msg: Message,
    freq: u64,
    dedup: &mut DedupFilter,
    reasm: Option<&mut (xng_acars::reasm::Reassembler, xng_acars::miam::FileReassembler)>,
    label_filter: &LabelFilter,
    ais_filter: &AisFilter,
    ais_gate: &mut AisGate,
    live: &Option<(Arc<LiveState>, u64, f64)>,
    bus: &MessageBus,
) {
    if dedup.is_duplicate(&msg) {
        return;
    }
    if let Some((r, files)) = reasm {
        apply_reassembly(&mut msg, r, files);
    }
    // Label Iridium ring alerts with the broadcasting satellite
    // (no-op unless a TLE satellite map was loaded at startup).
    crate::satmap::enrich(&mut msg);
    // Attribute space-based APRS (145.825 / ISS digipeat) to the
    // satellite(s) overhead (no-op unless init_aprs ran).
    crate::satmap::enrich_aprs(&mut msg);
    if !label_filter.allows(&msg) {
        return;
    }
    // AIS output shaping (AIS-5h): type/MMSI filter, then rate
    // downsample + content dedup keyed on the message time.
    if !ais_filter.allows(&msg)
        || !ais_gate.pass(&msg, ais_filter, msg.timestamp.timestamp_millis() as f64 / 1e3)
    {
        return;
    }
    if let (Some((state, _, _)), xng_types::MessageBody::Acars(a)) = (live, &msg.body) {
        // ACARS-5.2 per-label tally — CRC-valid frames only. A bad-CRC frame
        // carries a garbled 2-byte label (raw 7-bit bytes); tallying it would
        // spawn an unbounded set of junk `label="…"` series and blow up
        // Prometheus cardinality.
        if msg.decode.crc_ok {
            state.record_acars_label(freq, &a.label);
        }
    }
    bus.publish(msg);
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
                ModeChannel::new(cfg.mode, sample_rate, offset, freq, cfg.demod_effort, cfg.max_ppm)
                    .map_err(|e| anyhow::anyhow!("[{}] {:.3} MHz: {e}", cfg.mode, freq as f64 / 1e6))?;
            if let (ModeChannel::Adsb(d), Some((lat, lon))) = (&mut dec, cfg.receiver_pos) {
                d.set_receiver_position(lat, lon);
            }
            decoders.push((freq, dec));
        }
        let n_channels = decoders.len();
        decoders = collapse_shared_acars(decoders, sample_rate, capture_center, cfg.mode);
        tracing::info!(
            "station session: {} with {} channel(s) at {:.0} S/s centered {:.3} MHz",
            cfg.mode,
            n_channels,
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

        // Own-ship AIVDO beacon (AIS-5c): when an own-ship MMSI is configured
        // and some session has a receiver-pos, inject a position report every
        // 30 s so it reaches the NMEA sinks / map like any AIS fix. Polls the
        // stop flag each second so it never keeps the bus open past shutdown.
        if let (Some(mmsi), Some((lat, lon))) = (
            prepared[0].cfg.outputs.own_ship_mmsi,
            prepared.iter().find_map(|p| p.cfg.receiver_pos),
        ) {
            let (bus, stop, station) = (bus.clone(), stop.clone(), station.clone());
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                let mut n: u64 = 0;
                loop {
                    tick.tick().await;
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if n % 30 == 0 {
                        bus.publish(own_ship_message(mmsi, lat, lon, &station));
                    }
                    n += 1;
                }
            });
        }

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
                    prep.cfg.ais_filter.clone(),
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
    let status = r.push(core, now);
    // Record the reassembler's verdict (libacars `assstat`) on every message
    // that passed through it, not just completed ones (ACARS-5.1).
    core.assstat = Some(status.assstat().to_string());
    if let Reasm::Complete(full) = status {
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

/// Build the station's own-ship AIS message (AIS-5c): an AIVDO Type 1 position
/// report carrying the receiver's own MMSI + location, so it flows to every
/// output (NMEA sinks, the map, JSONL) like any other AIS fix.
fn own_ship_message(mmsi: u32, lat: f64, lon: f64, station: &StationIdentity) -> Message {
    Message {
        mode: Mode::Ais,
        timestamp: chrono::Utc::now(),
        frequency_hz: 161_975_000,
        signal: Default::default(),
        decode: xng_types::DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
        body: MessageBody::Ais {
            nmea: vec![xng_mode_ais::own_ship_position(mmsi, lat, lon)],
            msg_type: Some(1),
            mmsi: Some(mmsi),
            details: Some(serde_json::json!({
                "mmsi": mmsi, "lat": lat, "lon": lon, "own_ship": true,
            })),
        },
        raw: None,
        source: Provenance {
            station: station.clone(),
            app: AppInfo::xng(),
            sdr: None,
            channel: None,
        },
    }
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
        let mut dec = ModeChannel::new(cfg.mode, sample_rate, offset, freq, cfg.demod_effort, cfg.max_ppm)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        if let (ModeChannel::Adsb(d), Some((lat, lon))) = (&mut dec, cfg.receiver_pos) {
            d.set_receiver_position(lat, lon);
        }
        decoders.push((freq, dec));
    }
    Ok(collapse_shared_acars(decoders, sample_rate, capture_center, cfg.mode))
}

/// When an ACARS session has more than one channel, replace the N independent
/// per-channel `Acars` decoders with a single `AcarsShared` decoder that uses
/// one shared downconverter front end (the CPU optimization). A single ACARS
/// channel, or any other mode, is returned unchanged.
///
/// On failure to build the shared decoder (e.g. a capture too narrow to span
/// every channel), the original per-channel decoders are kept — the shared
/// front end is a pure optimization, never a correctness requirement.
fn collapse_shared_acars(
    decoders: Vec<(u64, ModeChannel)>,
    sample_rate: f64,
    capture_center: u64,
    mode: Mode,
) -> Vec<(u64, ModeChannel)> {
    if mode != Mode::AcarsPoa || decoders.len() < 2 {
        return decoders;
    }
    // Every entry is a single-channel ACARS decoder at this point.
    if !decoders.iter().all(|(_, d)| matches!(d, ModeChannel::Acars(_))) {
        return decoders;
    }
    let freqs: Vec<u64> = decoders.iter().map(|(f, _)| *f).collect();
    let offsets: Vec<f64> = freqs.iter().map(|&f| f as f64 - capture_center as f64).collect();
    match AcarsMultiChannelDecoder::new(sample_rate, &offsets) {
        Ok(dec) => {
            tracing::info!(
                "ACARS: {} channels share one downconverter front end",
                freqs.len()
            );
            // The shared entry is keyed on the first channel's freq; its own
            // per-channel freqs live in the variant (the loop reads those).
            vec![(freqs[0], ModeChannel::AcarsShared { dec, freqs })]
        }
        Err(e) => {
            tracing::warn!("shared ACARS front end unavailable ({e}); using per-channel DDCs");
            decoders
        }
    }
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

    fn ais_msg(msg_type: u8, mmsi: u32, details: Option<serde_json::Value>, ts_s: i64) -> Message {
        Message {
            mode: Mode::Ais,
            timestamp: chrono::DateTime::from_timestamp(ts_s, 0).unwrap(),
            frequency_hz: 161_975_000,
            signal: Default::default(),
            decode: Default::default(),
            body: xng_types::MessageBody::Ais {
                nmea: vec![],
                msg_type: Some(msg_type),
                mmsi: Some(mmsi),
                details,
            },
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
    fn own_ship_message_carries_aivdo() {
        let m = own_ship_message(366_123_456, 37.5, -122.3, &StationIdentity::new("XX-TEST"));
        assert_eq!(m.mode, Mode::Ais);
        assert!(m.decode.crc_ok);
        let MessageBody::Ais { nmea, mmsi, msg_type, .. } = &m.body else {
            panic!("expected AIS body");
        };
        assert_eq!(*mmsi, Some(366_123_456));
        assert_eq!(*msg_type, Some(1));
        assert!(nmea[0].starts_with("!AIVDO,"), "{nmea:?}");
    }

    #[test]
    fn ais_filter_type_and_mmsi_keep_drop() {
        // Include types 1 & 5 → type 3 dropped; non-AIS always passes.
        let f = AisFilter { include_types: vec![1, 5], ..Default::default() };
        assert!(f.allows(&ais_msg(1, 100, None, 0)));
        assert!(!f.allows(&ais_msg(3, 100, None, 0)));
        assert!(f.allows(&acars_msg("H1")));
        // Exclude one MMSI.
        let f2 = AisFilter { exclude_mmsi: vec![999], ..Default::default() };
        assert!(!f2.allows(&ais_msg(1, 999, None, 0)));
        assert!(f2.allows(&ais_msg(1, 100, None, 0)));
    }

    #[test]
    fn ais_gate_rate_downsamples_dynamic_reports() {
        let cfg = AisFilter { min_interval_s: Some(10.0), ..Default::default() };
        let mut g = AisGate::default();
        // Type-1 from MMSI 7: first kept, +5 s dropped, +12 s kept (from last kept).
        assert!(g.pass(&ais_msg(1, 7, None, 1000), &cfg, 1000.0));
        assert!(!g.pass(&ais_msg(1, 7, None, 1005), &cfg, 1005.0));
        assert!(g.pass(&ais_msg(1, 7, None, 1012), &cfg, 1012.0));
        // A different MMSI is independent; static type 5 is never throttled.
        assert!(g.pass(&ais_msg(1, 8, None, 1005), &cfg, 1005.0));
        assert!(g.pass(&ais_msg(5, 7, None, 1006), &cfg, 1006.0));
        assert!(g.pass(&ais_msg(5, 7, None, 1007), &cfg, 1007.0));
    }

    #[test]
    fn ais_gate_content_dedup_collapses_repeats() {
        let cfg = AisFilter { dedup_window_s: Some(10.0), ..Default::default() };
        let mut g = AisGate::default();
        let same = || Some(serde_json::json!({ "lat": 1.0, "lon": 2.0 }));
        // Identical content within the window → second dropped.
        assert!(g.pass(&ais_msg(1, 7, same(), 1000), &cfg, 1000.0));
        assert!(!g.pass(&ais_msg(1, 7, same(), 1003), &cfg, 1003.0));
        // Different content from the same MMSI is kept.
        assert!(g.pass(&ais_msg(1, 7, Some(serde_json::json!({ "lat": 9.0 })), 1004), &cfg, 1004.0));
        // Past the window, the original content is allowed again.
        assert!(g.pass(&ais_msg(1, 7, same(), 1011), &cfg, 1011.0));
    }

    #[test]
    fn ais_gate_dedup_drop_does_not_advance_rate_clock() {
        // Both knobs on, dedup window wider than the rate interval. A
        // content-duplicate that clears the rate gate but is then dropped by
        // dedup must NOT advance the per-MMSI rate clock — otherwise a
        // genuinely new fix arriving shortly after is wrongly throttled even
        // though nothing was emitted at the duplicate's time.
        let cfg = AisFilter { min_interval_s: Some(10.0), dedup_window_s: Some(30.0), ..Default::default() };
        let mut g = AisGate::default();
        let a = || Some(serde_json::json!({ "lat": 1.0, "lon": 2.0 }));
        let b = || Some(serde_json::json!({ "lat": 3.0, "lon": 4.0 }));
        // t=1000 content A → kept (rate clock = 1000).
        assert!(g.pass(&ais_msg(1, 7, a(), 1000), &cfg, 1000.0));
        // t=1012 same content A → passes rate (12 ≥ 10) but dropped by dedup
        // (12 < 30); the rate clock must stay at 1000, not advance to 1012.
        assert!(!g.pass(&ais_msg(1, 7, a(), 1012), &cfg, 1012.0));
        // t=1015 NEW content B → 1015-1000 = 15 ≥ 10, so it must be kept.
        // (With the pre-fix last_pos pollution it would read 1015-1012 = 3 and
        // be wrongly throttled.)
        assert!(g.pass(&ais_msg(1, 7, b(), 1015), &cfg, 1015.0), "new fix must not be throttled");
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

    #[test]
    fn reassembly_stamps_assstat() {
        let mut r = xng_acars::reasm::Reassembler::new(660.0);
        let mut files = xng_acars::miam::FileReassembler::new();

        // A plain single block (no block_id) is "skipped" by the reassembler,
        // but the verdict is still stamped on the message (ACARS-5.1).
        let mut msg = acars_msg("H1");
        msg.decode.crc_ok = true;
        apply_reassembly(&mut msg, &mut r, &mut files);
        let MessageBody::Acars(c) = &msg.body else { unreachable!() };
        assert_eq!(c.assstat.as_deref(), Some("skipped"));

        // A bad-CRC frame never reaches the reassembler → no verdict.
        let mut bad = acars_msg("H1");
        bad.decode.crc_ok = false;
        apply_reassembly(&mut bad, &mut r, &mut files);
        let MessageBody::Acars(c) = &bad.body else { unreachable!() };
        assert_eq!(c.assstat, None);
    }
}
