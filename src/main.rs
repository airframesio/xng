//! xng — next-generation multi-mode SDR decoder.
//!
//! M1: native ACARS decoding from SDR hardware or IQ recordings, with
//! console/JSONL outputs and acarsdec-compatible Airframes feeding.

mod bus;
mod commands;
mod tui;
mod freq;
mod outputs;
mod runtime;
mod sdr_args;

use clap::{Args, Parser, Subcommand};
use outputs::console::ConsoleFormat;
use std::path::PathBuf;
use xng_sdr::{FileIqSource, IqFormat};

const AIRFRAMES_ACARS_UDP: &str = "feed.airframes.io:5550";

#[derive(Parser)]
#[command(name = "xng", version, about = "Next-generation multi-mode SDR decoder (ACARS, VDL2, HFDL, satcom, AIS, ...)")]
struct Cli {
    /// Increase log verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct TuneOpts {
    /// Decode mode: acars, vdl2, hfdl, aero, aero-c, std-c, ais, adsb, or iridium
    #[arg(short, long, default_value = "acars")]
    mode: String,
    /// Capture sample rate in Hz (must be an integer multiple of the
    /// mode's channel rate: 24 kHz for ACARS, 48 kHz for AIS; 2400000
    /// works for both). The tui derives it from the mode's plan when
    /// omitted; decode/listen require it.
    #[arg(short = 'r', long)]
    sample_rate: Option<f64>,
    /// Capture center frequency (e.g. 131.500M). The tui derives it from
    /// the channel set when omitted; decode/listen require it.
    #[arg(short = 'c', long)]
    center_freq: Option<String>,
    /// Channel frequencies, comma separated (e.g. 131.550,131.725). The
    /// tui falls back to the mode's built-in plan (as many channels as
    /// fit the capture width and CPU budget); decode/listen require it.
    #[arg(long, value_delimiter = ',')]
    channels: Vec<String>,
    /// Receiver location as lat,lon (e.g. 38.69,-121.59) — enables
    /// ADS-B surface-position decoding
    #[arg(long)]
    receiver_pos: Option<String>,
    /// Only pass ACARS messages with these labels (comma separated,
    /// e.g. H1,Q0). Non-ACARS messages always pass.
    #[arg(long, value_delimiter = ',')]
    filter_labels: Vec<String>,
    /// Drop ACARS messages with these labels (comma separated)
    #[arg(long, value_delimiter = ',')]
    exclude_labels: Vec<String>,
    /// Demod effort: 'max' scans every timing grid (default for file
    /// decode), 'live' trims to a real-time budget (default for SDR
    /// commands; matters on Pi-class hardware)
    #[arg(long)]
    demod_effort: Option<runtime::DemodEffort>,
}

fn parse_receiver_pos(s: &Option<String>) -> anyhow::Result<Option<(f64, f64)>> {
    match s {
        None => Ok(None),
        Some(v) => {
            let (a, b) = v
                .split_once(',')
                .ok_or_else(|| anyhow::anyhow!("--receiver-pos wants lat,lon"))?;
            Ok(Some((a.trim().parse()?, b.trim().parse()?)))
        }
    }
}

#[derive(Args)]
struct OutputOpts {
    /// Print messages as raw JSON instead of pretty one-liners
    #[arg(long)]
    json: bool,
    /// Append normalized messages to a JSONL file
    #[arg(long)]
    jsonl: Option<PathBuf>,
    /// Send acarsdec-compatible JSON datagrams to host:port (repeatable)
    #[arg(long)]
    udp: Vec<String>,
    /// Feed Airframes (acarsdec JSON to feed.airframes.io:5550); requires --station-id
    #[arg(long)]
    feed_airframes: bool,
    /// Station ident (e.g. XX-KSEA-ACARS1)
    #[arg(long)]
    station_id: Option<String>,
    /// Stream asf-2.0 over gRPC to this ingest URL (e.g. http://127.0.0.1:6001)
    #[arg(long)]
    asf2_grpc: Option<String>,
    /// Stream asf-2.0 over QUIC to host:port (TLS verified against
    /// system roots by default)
    #[arg(long)]
    asf2_quic: Option<String>,
    /// PEM file with the ingest's certificate/CA to trust for
    /// --asf2-quic (see `xng ingest --quic-cert-out`)
    #[arg(long)]
    asf2_quic_ca: Option<PathBuf>,
    /// DANGEROUS: disable TLS certificate verification for --asf2-quic.
    /// The feed can be intercepted or spoofed. Lab use only.
    #[arg(long)]
    asf2_quic_insecure: bool,
    /// Serve Prometheus metrics on this address (e.g. 0.0.0.0:9090)
    #[arg(long)]
    metrics: Option<String>,
    /// Serve SBS-1/BaseStation output on this address (e.g. 0.0.0.0:30003;
    /// Mode S/ADS-B messages only)
    #[arg(long)]
    sbs: Option<String>,
    /// Serve Beast binary frames (Mode S) over TCP, dump1090-style
    /// (e.g. 0.0.0.0:30005)
    #[arg(long)]
    beast: Option<String>,
    /// Serve raw NMEA AIVDM over TCP (e.g. 0.0.0.0:10110)
    #[arg(long)]
    nmea_tcp: Option<String>,
    /// JSON file mapping hex VDL2 ground-station addresses to names
    /// (shown in console output)
    #[arg(long)]
    gs_file: Option<PathBuf>,
    /// Serve the live web dashboard (map of decoded aircraft/vessels
    /// + message stream) on this address (e.g. 0.0.0.0:8080)
    #[arg(long)]
    http: Option<String>,
    /// Publish messages as JSON to an MQTT broker
    /// (mqtt://[user:pass@]host[:port])
    #[arg(long)]
    mqtt: Option<String>,
    /// MQTT topic prefix; messages publish to <prefix>/<mode>
    #[arg(long, default_value = "xng")]
    mqtt_topic: String,
}

impl OutputOpts {
    fn build(&self) -> anyhow::Result<(runtime::OutputConfig, String)> {
        anyhow::ensure!(
            !(self.asf2_quic_insecure && self.asf2_quic_ca.is_some()),
            "--asf2-quic-insecure and --asf2-quic-ca are mutually exclusive"
        );
        let quic_trust = if self.asf2_quic_insecure {
            outputs::asf2_quic::TrustMode::Insecure
        } else if let Some(ca) = &self.asf2_quic_ca {
            outputs::asf2_quic::TrustMode::CaFile(ca.clone())
        } else {
            outputs::asf2_quic::TrustMode::SystemRoots
        };
        let mut udp = self.udp.clone();
        if self.feed_airframes {
            anyhow::ensure!(
                self.station_id.is_some(),
                "--feed-airframes requires --station-id (e.g. XX-KSEA-ACARS1)"
            );
            udp.push(AIRFRAMES_ACARS_UDP.to_owned());
        }
        if let Some(p) = &self.gs_file {
            outputs::console::load_gs_names(p)?;
        }
        let ident = self.station_id.clone().unwrap_or_else(|| "XNG-DEV".to_owned());
        Ok((
            runtime::OutputConfig {
                console: if self.json { ConsoleFormat::Json } else { ConsoleFormat::Pretty },
                jsonl: self.jsonl.clone(),
                udp,
                asf2_grpc: self.asf2_grpc.clone(),
                asf2_quic: self.asf2_quic.clone(),
                asf2_quic_trust: quic_trust,
                metrics: self.metrics.clone(),
                sbs: self.sbs.clone(),
                beast: self.beast.clone(),
                nmea_tcp: self.nmea_tcp.clone(),
                http: self.http.clone(),
                mqtt: self.mqtt.clone(),
                mqtt_topic: self.mqtt_topic.clone(),
            },
            ident,
        ))
    }
}

#[derive(Subcommand)]
enum Command {
    /// List available SDR devices (native Airspy backends + SoapySDR)
    Devices {
        /// SoapySDR filter args, e.g. "driver=rtlsdr" (native devices
        /// are always listed)
        #[arg(default_value = "")]
        filter: String,
    },
    /// Decode from a recorded IQ file (multi-channel, mode via --mode)
    Decode {
        /// Path to the IQ file
        file: PathBuf,
        /// Sample format (cf32, cs16, cs8, cu8); guessed from extension if omitted
        #[arg(short, long)]
        format: Option<String>,
        #[command(flatten)]
        tune: TuneOpts,
        #[command(flatten)]
        output: OutputOpts,
    },
    /// Decode live from an SDR (multi-channel, mode via --mode)
    Listen {
        /// SDR device args, e.g. "driver=rtlsdr" (SoapySDR) or
        /// "driver=airspyhf,serial=..." (native backend when built with
        /// --features airspy/airspyhf; add backend=soapy to override)
        #[arg(long, default_value = "")]
        sdr: String,
        /// Tuner gain in dB (hardware AGC when omitted)
        #[arg(short, long)]
        gain: Option<f64>,
        #[command(flatten)]
        tune: TuneOpts,
        #[command(flatten)]
        output: OutputOpts,
    },
    /// Run a whole station from a config file: several decode
    /// sessions (modes + SDRs) in one process with shared outputs
    Station {
        /// Path to the station TOML config
        config: PathBuf,
    },
    /// Inspect a recorded IQ file: duration, power, spectral peaks
    IqInfo {
        /// Path to the IQ file
        file: PathBuf,
        /// Sample rate of the recording in Hz
        #[arg(short = 'r', long)]
        sample_rate: f64,
        /// Sample format (cf32, cs16, cs8, cu8); guessed from extension if omitted
        #[arg(short, long)]
        format: Option<String>,
        /// RF center frequency in Hz (for absolute peak frequencies)
        #[arg(short, long, default_value_t = 0)]
        center_freq: u64,
        /// FFT size for the power spectrum
        #[arg(long, default_value_t = 4096)]
        fft_size: usize,
    },
    /// Wrap an external decoder (second-class): normalize its JSON
    /// output onto the xng bus and outputs
    Extern {
        /// Input format: dumphfdl, dumpvdl2, or acarsdec
        #[arg(long)]
        format: String,
        #[command(flatten)]
        output: OutputOpts,
        /// External decoder command line (after --); stdin when omitted
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Soak-test one mode: monitor all channels that fit the SDR bandwidth
    /// for a sustained period, then report per-channel statistics and
    /// reception advice
    Survey {
        /// SDR device args (as for listen)
        #[arg(long, default_value = "")]
        sdr: String,
        /// Tuner gain in dB (hardware AGC when omitted; see --tune-gain)
        #[arg(short, long)]
        gain: Option<f64>,
        /// Mode to survey: acars, vdl2, hfdl, aero, std-c, ais, adsb, iridium
        #[arg(short, long, default_value = "acars")]
        mode: String,
        /// Capture sample rate in Hz (the mode's plan default when omitted)
        #[arg(short = 'r', long)]
        sample_rate: Option<f64>,
        /// Survey only these channels (MHz/k/M suffixes accepted);
        /// the full built-in plan when omitted
        #[arg(long, value_delimiter = ',')]
        channels: Vec<String>,
        /// Total survey duration in seconds
        #[arg(long, default_value_t = 900)]
        duration: u64,
        /// Print an interim statistics table every N seconds
        #[arg(long, default_value_t = 300)]
        interim: u64,
        /// Run a scan pre-pass and keep active channels (plus the mode's
        /// core worldwide channels, which short scans routinely undersell)
        #[arg(long)]
        scan: bool,
        /// Dwell per capture window during the scan pre-pass, seconds
        #[arg(long, default_value_t = 90)]
        scan_dwell: u64,
        /// Sweep gain settings empirically first and use the best
        #[arg(long)]
        tune_gain: bool,
        /// Dwell per gain step during --tune-gain, seconds
        #[arg(long, default_value_t = 20)]
        tune_dwell: u64,
        /// Dwell per visit when rotating between capture windows, seconds
        #[arg(long, default_value_t = 60)]
        rotate_dwell: u64,
        /// Print decoded messages live during the survey
        #[arg(long)]
        show_messages: bool,
        /// Also write decoded messages to this JSONL file
        #[arg(long)]
        jsonl: Option<PathBuf>,
        /// Write the full survey report as JSON
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Auto-scan known frequency plans and propose a configuration
    Scan {
        /// SoapySDR device args
        #[arg(long, default_value = "")]
        sdr: String,
        /// Tuner gain in dB (hardware AGC when omitted)
        #[arg(short, long)]
        gain: Option<f64>,
        /// Modes to scan, comma separated (default: acars,vdl2,ais)
        #[arg(long, value_delimiter = ',', default_values_t = ["acars".to_string(), "vdl2".to_string(), "ais".to_string()])]
        modes: Vec<String>,
        /// Seconds to dwell on each capture group
        #[arg(long, default_value_t = 20)]
        dwell: u64,
        /// Write the full scan report as JSON
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Interactive TUI: live decode with message browser, spectrum, stats
    Tui {
        /// SoapySDR device args (live mode)
        #[arg(long, default_value = "")]
        sdr: String,
        /// Tuner gain in dB (hardware AGC when omitted)
        #[arg(short, long)]
        gain: Option<f64>,
        /// Replay a recorded IQ file instead of live SDR input
        #[arg(long)]
        file: Option<PathBuf>,
        /// Sample format for --file (guessed from extension if omitted)
        #[arg(short, long)]
        format: Option<String>,
        #[command(flatten)]
        tune: TuneOpts,
    },
    /// Run a reference asf-2.0 ingest server (gRPC and/or QUIC)
    Ingest {
        /// gRPC listen address (e.g. 0.0.0.0:6001)
        #[arg(long)]
        grpc: Option<String>,
        /// QUIC listen address (e.g. 0.0.0.0:6011); uses a self-signed
        /// certificate unless one is provided
        #[arg(long)]
        quic: Option<String>,
        /// Write the QUIC certificate (PEM) here so feeders can pin it
        /// via --asf2-quic-ca
        #[arg(long)]
        quic_cert_out: Option<PathBuf>,
    },
    /// Run the built-in pipeline self-test (bus + outputs + DSP sanity)
    Selftest {
        /// Also write messages to this JSONL file
        #[arg(long)]
        jsonl: Option<PathBuf>,
        /// Print messages as raw JSON instead of pretty one-liners
        #[arg(long)]
        json: bool,
    },
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

/// Strict tune parsing for decode/listen: everything must be explicit.
fn parse_tune(tune: &TuneOpts) -> anyhow::Result<(xng_types::Mode, f64, u64, Vec<u64>)> {
    let mode: xng_types::Mode = tune.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let rate = tune
        .sample_rate
        .ok_or_else(|| anyhow::anyhow!("missing -r/--sample-rate"))?;
    let center = tune
        .center_freq
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing -c/--center-freq"))?;
    let center = freq::parse_hz(center)?;
    anyhow::ensure!(!tune.channels.is_empty(), "missing --channels");
    let channels = tune
        .channels
        .iter()
        .map(|c| freq::parse_hz(c))
        .collect::<anyhow::Result<Vec<u64>>>()?;
    Ok((mode, rate, center, channels))
}

/// Zero-config tuning for the tui: anything omitted is derived from the
/// mode's built-in frequency plan — sample rate from the plan (checked
/// against the device's advertised rates where a native backend can
/// tell us), channels from the densest capture window that fits it
/// (trimmed to a CPU budget, core channels first), center from the
/// chosen channels.
fn resolve_tune_auto(
    tune: &TuneOpts,
    sdr: &str,
) -> anyhow::Result<(xng_types::Mode, f64, u64, Vec<u64>)> {
    let mode: xng_types::Mode = tune.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let (plan_rate, plan_channels) = commands::scan::plan(mode);
    let rate = match tune.sample_rate {
        Some(r) => r,
        None => {
            let advertised = probe_device_rates(sdr);
            let r = commands::scan::pick_auto_rate(&advertised, mode, plan_rate);
            if r != plan_rate {
                tracing::info!(
                    "auto-tune: device prefers {} S/s over the plan's {} S/s",
                    r as u64,
                    plan_rate as u64
                );
            }
            r
        }
    };

    // Explicit channels: only the center may need deriving.
    if !tune.channels.is_empty() {
        let channels = tune
            .channels
            .iter()
            .map(|c| freq::parse_hz(c))
            .collect::<anyhow::Result<Vec<u64>>>()?;
        let center = match &tune.center_freq {
            Some(c) => freq::parse_hz(c)?,
            None => centroid_off_dc(&channels),
        };
        return Ok((mode, rate, center, channels));
    }

    // Explicit center, channels from the plan: keep plan channels that
    // fit the capture around that center.
    if let Some(c) = &tune.center_freq {
        let center = freq::parse_hz(c)?;
        let half = (rate * 0.4) as i64; // 80% usable width, half each side
        let channels: Vec<u64> = plan_channels
            .iter()
            .copied()
            .filter(|f| (*f as i64 - center as i64).abs() <= half)
            .collect();
        anyhow::ensure!(
            !channels.is_empty(),
            "no {mode} plan channels within the capture around {:.3} MHz; pass --channels",
            center as f64 / 1e6
        );
        return Ok((mode, rate, center, channels));
    }

    // Fully automatic. DDC-per-channel is cheap, but a small box should
    // not be saddled with a worldwide plan: budget ~4 channels per core.
    let budget = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) * 4;
    let (center, channels) = commands::scan::auto_window(mode, rate, budget)
        .ok_or_else(|| {
            anyhow::anyhow!("mode {mode} has no built-in frequency plan; pass --channels")
        })?;
    tracing::info!(
        "auto-tune: {} channel(s) around {:.3} MHz at {} S/s (budget {budget})",
        channels.len(),
        center as f64 / 1e6,
        rate as u64
    );
    Ok((mode, rate, center, channels))
}

/// Advertised sample rates of the device `--sdr` selects, where a native
/// backend can ask (empty when it can't — SoapySDR devices generally
/// accept the plan rates).
pub(crate) fn probe_device_rates(sdr: &str) -> Vec<u32> {
    let args = sdr_args::SdrArgs::parse(sdr);
    if args.force_soapy {
        return Vec::new();
    }
    let serial = || args.serial.as_deref().and_then(|s| sdr_args::parse_airspy_serial(s).ok());
    let _ = &serial; // used only by the cfg'd arms below
    match args.driver.as_deref() {
        #[cfg(feature = "airspy")]
        Some("airspy") => xng_sdr::airspy::device_rates(serial()).unwrap_or_default(),
        #[cfg(feature = "airspyhf")]
        Some("airspyhf") => xng_sdr::airspyhf::device_rates(serial()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Midpoint of a channel set, nudged off any exact channel so no carrier
/// sits at DC.
fn centroid_off_dc(channels: &[u64]) -> u64 {
    let center =
        (channels.iter().min().unwrap() + channels.iter().max().unwrap()) / 2;
    if channels.contains(&center) { center + 25_000 } else { center }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    tracing::info!("xng v{}", env!("CARGO_PKG_VERSION"));

    match cli.command {
        Command::Devices { filter } => commands::devices::run(&filter),
        Command::Decode { file, format, tune, output } => {
            let fmt = match format.as_deref() {
                Some(f) => f.parse().map_err(|e: String| anyhow::anyhow!(e))?,
                None => IqFormat::from_extension(&file).ok_or_else(|| {
                    anyhow::anyhow!("cannot guess IQ format; pass --format (cf32|cs16|cs8|cu8)")
                })?,
            };
            let (mode, rate, center_hz, channels_hz) = parse_tune(&tune)?;
            let (outputs, station_ident) = output.build()?;
            let source = FileIqSource::open(&file, fmt, rate, center_hz)?;
            runtime::run_session(
                Box::new(source),
                runtime::SessionConfig {
                    mode,
                    center_hz,
                    channels_hz,
                    station_ident,
                    sdr: Some(xng_types::SdrInfo {
                        id: "file".into(),
                        driver: "file".into(),
                        serial: None,
                    }),
                    outputs,
                    receiver_pos: parse_receiver_pos(&tune.receiver_pos)?,
                    label_filter: runtime::LabelFilter {
                        include: tune.filter_labels.clone(),
                        exclude: tune.exclude_labels.clone(),
                    },
                    demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Max),
                },
            )
        }
        Command::Listen { sdr, gain, tune, output } => {
            listen(&sdr, gain, &tune, &output)
        }
        Command::Station { config } => run_station_cmd(&config),
        Command::IqInfo { file, sample_rate, format, center_freq, fft_size } => {
            commands::iq_info::run(&file, sample_rate, format.as_deref(), center_freq, fft_size)
        }
        Command::Extern { format, output, command } => {
            let fmt: commands::extern_cmd::ExternFormat =
                format.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let (outputs, station_ident) = output.build()?;
            commands::extern_cmd::run(fmt, &command, station_ident, outputs)
        }
        Command::Survey {
            sdr,
            gain,
            mode,
            sample_rate,
            channels,
            duration,
            interim,
            scan,
            scan_dwell,
            tune_gain,
            tune_dwell,
            rotate_dwell,
            show_messages,
            jsonl,
            out,
        } => {
            let mode: xng_types::Mode = mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let channels = channels
                .iter()
                .map(|c| freq::parse_hz(c))
                .collect::<anyhow::Result<Vec<u64>>>()?;
            commands::survey::run(commands::survey::SurveyOpts {
                sdr,
                gain,
                mode,
                sample_rate,
                channels,
                duration_secs: duration,
                interim_secs: interim,
                scan_first: scan,
                scan_dwell_secs: scan_dwell,
                tune_gain,
                tune_dwell_secs: tune_dwell,
                rotate_dwell_secs: rotate_dwell,
                show_messages,
                jsonl,
                out_json: out,
            })
        }
        Command::Scan { sdr, gain, modes, dwell, out } => {
            let modes: Vec<xng_types::Mode> = modes
                .iter()
                .map(|m| m.parse().map_err(|e: String| anyhow::anyhow!(e)))
                .collect::<anyhow::Result<_>>()?;
            commands::scan::run(&sdr, gain, &modes, dwell, out.as_deref())
        }
        Command::Tui { sdr, gain, file, format, tune } => {
            let (mode, rate, center_hz, channels_hz) = resolve_tune_auto(&tune, &sdr)?;
            let source: Box<dyn xng_sdr::IqSource> = match file {
                Some(path) => {
                    let fmt = match format.as_deref() {
                        Some(f) => f.parse().map_err(|e: String| anyhow::anyhow!(e))?,
                        None => IqFormat::from_extension(&path).ok_or_else(|| {
                            anyhow::anyhow!("cannot guess IQ format; pass --format")
                        })?,
                    };
                    // A recording's rate can't be derived from a plan.
                    let rate = tune.sample_rate.ok_or_else(|| {
                        anyhow::anyhow!("--file needs an explicit -r/--sample-rate")
                    })?;
                    Box::new(FileIqSource::open(&path, fmt, rate, center_hz)?)
                }
                None => open_sdr(&sdr, rate, center_hz, gain)?.0,
            };
            tui::run(
                source,
                runtime::SessionConfig {
                    mode,
                    center_hz,
                    channels_hz,
                    station_ident: "XNG-TUI".into(),
                    sdr: None,
                    receiver_pos: parse_receiver_pos(&tune.receiver_pos)?,
                    label_filter: runtime::LabelFilter {
                        include: tune.filter_labels.clone(),
                        exclude: tune.exclude_labels.clone(),
                    },
                    demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Live),
                    outputs: runtime::OutputConfig {
                        console: ConsoleFormat::Pretty,
                        jsonl: None,
                        udp: vec![],
                        asf2_grpc: None,
                        asf2_quic: None,
                        asf2_quic_trust: outputs::asf2_quic::TrustMode::SystemRoots,
                        metrics: None,
                        sbs: None,
                        beast: None,
                        nmea_tcp: None,
                        http: None,
                        mqtt: None,
                        mqtt_topic: "xng".into(),
                    },
                },
            )
        }
        Command::Ingest { grpc, quic, quic_cert_out } => {
            commands::ingest::run(grpc, quic, quic_cert_out.as_deref())
        }
        Command::Selftest { jsonl, json } => commands::selftest::run(jsonl.as_deref(), json),
    }
}

/// Open an SDR from `--sdr` args, returning the source and the backend that
/// served it. `driver=airspy` / `driver=airspyhf` use the native backends
/// when compiled in (falling through to SoapySDR otherwise, where a
/// SoapyAirspy module may exist); `backend=soapy` forces the fallthrough.
fn run_station_cmd(config: &std::path::Path) -> anyhow::Result<()> {
    let st = commands::station::load(config)?;

    // Shared outputs (the first session's SessionConfig carries them).
    let mut udp = st.outputs.udp.clone();
    if st.outputs.feed_airframes {
        udp.push(AIRFRAMES_ACARS_UDP.to_owned());
    }
    let outputs = runtime::OutputConfig {
        console: if st.outputs.json { ConsoleFormat::Json } else { ConsoleFormat::Pretty },
        jsonl: st.outputs.jsonl.clone(),
        udp,
        asf2_grpc: st.outputs.asf2_grpc.clone(),
        asf2_quic: st.outputs.asf2_quic.clone(),
        asf2_quic_trust: outputs::asf2_quic::TrustMode::SystemRoots,
        metrics: st.outputs.metrics.clone(),
        sbs: st.outputs.sbs.clone(),
        beast: st.outputs.beast.clone(),
        nmea_tcp: st.outputs.nmea_tcp.clone(),
        http: st.outputs.http.clone(),
        mqtt: st.outputs.mqtt.clone(),
        mqtt_topic: st.outputs.mqtt_topic.clone().unwrap_or_else(|| "xng".into()),
    };

    let mut sessions = Vec::new();
    for (i, sess) in st.sessions.iter().enumerate() {
        let label = format!("session {} ({})", i + 1, sess.mode);
        // Tuning: explicit values, or the mode plan via the same
        // derivation the TUI uses.
        let tune = TuneOpts {
            mode: sess.mode.clone(),
            sample_rate: sess.sample_rate,
            center_freq: sess.center.clone(),
            channels: sess.channels.clone(),
            receiver_pos: sess.receiver_pos.clone(),
            filter_labels: Vec::new(),
            exclude_labels: Vec::new(),
            demod_effort: sess
                .demod_effort
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|e: String| anyhow::anyhow!("{label}: {e}"))?,
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
            runtime::SessionConfig {
                mode,
                center_hz,
                channels_hz: channels,
                station_ident: st.station_id.clone(),
                sdr: sdr_info,
                outputs: outputs.clone(),
                receiver_pos: parse_receiver_pos(&sess.receiver_pos)?,
                label_filter: Default::default(),
                demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Live),
            },
        ));
    }
    runtime::run_station(sessions)
}

fn open_sdr(
    sdr: &str,
    sample_rate: f64,
    center_hz: u64,
    gain: Option<f64>,
) -> anyhow::Result<(Box<dyn xng_sdr::IqSource>, &'static str)> {
    let args = sdr_args::SdrArgs::parse(sdr);
    match args.driver.as_deref() {
        Some("airspy") if !args.force_soapy => {
            #[cfg(feature = "airspy")]
            {
                let serial =
                    args.serial.as_deref().map(sdr_args::parse_airspy_serial).transpose()?;
                let src = xng_sdr::airspy::AirspyIqSource::open(
                    serial,
                    sample_rate,
                    center_hz,
                    gain,
                    args.bias,
                )?;
                return Ok((Box::new(src), "airspy"));
            }
        }
        Some("airspyhf") if !args.force_soapy => {
            #[cfg(feature = "airspyhf")]
            {
                let serial =
                    args.serial.as_deref().map(sdr_args::parse_airspy_serial).transpose()?;
                let src = xng_sdr::airspyhf::AirspyHfIqSource::open(
                    serial,
                    sample_rate,
                    center_hz,
                    gain,
                    args.bias,
                )?;
                return Ok((Box::new(src), "airspyhf"));
            }
        }
        _ => {}
    }
    open_soapy(&args.soapy, sample_rate, center_hz, gain)
}

#[cfg(feature = "soapy")]
fn open_soapy(
    args: &str,
    sample_rate: f64,
    center_hz: u64,
    gain: Option<f64>,
) -> anyhow::Result<(Box<dyn xng_sdr::IqSource>, &'static str)> {
    let src = xng_sdr::soapy::SoapyIqSource::open(args, sample_rate, center_hz, gain)?;
    Ok((Box::new(src), "soapysdr"))
}

#[cfg(not(feature = "soapy"))]
fn open_soapy(
    _args: &str,
    _sample_rate: f64,
    _center_hz: u64,
    _gain: Option<f64>,
) -> anyhow::Result<(Box<dyn xng_sdr::IqSource>, &'static str)> {
    anyhow::bail!("built without SDR support; use --file or rebuild with --features soapy")
}

fn listen(sdr: &str, gain: Option<f64>, tune: &TuneOpts, output: &OutputOpts) -> anyhow::Result<()> {
    let (mode, rate, center_hz, channels_hz) = parse_tune(tune)?;
    let (outputs, station_ident) = output.build()?;
    let serial = sdr_args::SdrArgs::parse(sdr).serial;
    let (source, driver) = open_sdr(sdr, rate, center_hz, gain)?;
    runtime::run_session(
        source,
        runtime::SessionConfig {
            mode,
            center_hz,
            channels_hz,
            station_ident,
            sdr: Some(xng_types::SdrInfo {
                id: sdr.to_owned(),
                driver: driver.into(),
                serial,
            }),
            outputs,
            receiver_pos: parse_receiver_pos(&tune.receiver_pos)?,
                    label_filter: runtime::LabelFilter {
                        include: tune.filter_labels.clone(),
                        exclude: tune.exclude_labels.clone(),
                    },
                    demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Live),
        },
    )
}
