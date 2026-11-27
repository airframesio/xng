//! xng — next-generation multi-mode SDR decoder.
//!
//! M1: native ACARS decoding from SDR hardware or IQ recordings, with
//! console/JSONL outputs and acarsdec-compatible Airframes feeding.


use clap::{Args, Parser, Subcommand};
use xng::sdr_open::open_sdr;
use xng::tuning::{self, parse_receiver_pos, parse_tune, resolve_tune_auto};
use xng::{commands, freq, outputs, runtime, satmap, sdr_args, tui};
use xng::outputs::console::ConsoleFormat;
use std::path::PathBuf;
use xng_sdr::{FileIqSource, IqFormat};

#[derive(Parser)]
#[command(name = "xng", version, about = "Next-generation multi-mode SDR decoder (ACARS, VDL2, HFDL, satcom, AIS, ...)")]
struct Cli {
    /// Increase log verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Decode worker threads for parallel channelization / burst demod
    /// (Iridium wideband FFT + per-burst extract, and other parallel decode
    /// paths). Default: auto — all available CPU cores.
    #[arg(long, global = true)]
    decode_threads: Option<usize>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct TuneOpts {
    /// Decode mode: acars, vdl2, hfdl, aero, aero-c, std-c, ais, adsb, iridium,
    /// uat, sarsat, dsc, navtex, sonde, ads-l, atcs, aprs, pocsag, eot, flex,
    /// vdes, time, meshtastic, or meshcore
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
    /// VDL2 only: reject bursts whose carrier offset exceeds this many ppm
    #[arg(long)]
    max_ppm: Option<f64>,
    /// Meshtastic/MeshCore: LoRa preset (LongFast, MediumFast, ShortTurbo, ...
    /// or meshcore-us, meshcore-eu, meshcore-anz). Default: LongFast for
    /// meshtastic; meshcore-us, or meshcore-eu in the 868 MHz band
    #[arg(long)]
    lora_preset: Option<String>,
    /// Meshtastic/MeshCore: spreading factor(s) to listen for, comma
    /// separated (overrides the preset; each adds one demodulator)
    #[arg(long, value_delimiter = ',')]
    lora_sf: Vec<u8>,
    /// Meshtastic/MeshCore: LoRa bandwidth in kHz (overrides the preset)
    #[arg(long)]
    lora_bw: Option<f64>,
    /// Meshtastic: extra channel to decrypt, as NAME=BASE64KEY (repeatable;
    /// the default key on every preset's channel is always tried)
    #[arg(long)]
    meshtastic_key: Vec<String>,
    /// MeshCore: extra group channel, as #hashtag or NAME=BASE64KEY
    /// (repeatable; the Public channel is always tried)
    #[arg(long)]
    meshcore_channel: Vec<String>,
}

impl TuneOpts {
    /// Install the session's LoRa mesh settings (first call wins).
    fn install_lora(&self) {
        let mut cfg = xng_mode_lora::LoraConfig {
            preset: self.lora_preset.clone(),
            sfs: self.lora_sf.clone(),
            bw_hz: self.lora_bw.map(|k| k * 1e3),
            ..Default::default()
        };
        if let Some(p) = &cfg.preset {
            if xng_mode_lora::named_preset(p).is_none() {
                eprintln!("xng: unknown --lora-preset {p}");
                std::process::exit(2);
            }
        }
        for k in &self.meshtastic_key {
            match xng_mode_lora::parse_meshtastic_key(k) {
                Ok(c) => cfg.meshtastic_channels.push(c),
                Err(e) => {
                    // The value may hold a private key: never echo it.
                    eprintln!("xng: --meshtastic-key: {e}");
                    std::process::exit(2);
                }
            }
        }
        for c in &self.meshcore_channel {
            match xng_mode_lora::parse_meshcore_channel(c) {
                Ok(c) => cfg.meshcore_channels.push(c),
                Err(e) => {
                    eprintln!("xng: --meshcore-channel: {e}");
                    std::process::exit(2);
                }
            }
        }
        xng_mode_lora::set_config(cfg);
    }

    /// The library-level tune request these flags describe.
    fn tune(&self) -> tuning::Tune {
        self.install_lora();
        tuning::Tune {
            mode: self.mode.clone(),
            sample_rate: self.sample_rate,
            center_freq: self.center_freq.clone(),
            channels: self.channels.clone(),
            receiver_pos: self.receiver_pos.clone(),
            filter_labels: self.filter_labels.clone(),
            exclude_labels: self.exclude_labels.clone(),
            demod_effort: self.demod_effort,
            max_ppm: self.max_ppm,
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
    /// Push raw NMEA AIVDM as UDP datagrams to this target (e.g. host:10110)
    #[arg(long)]
    nmea_udp: Option<String>,
    /// Prefix NMEA output with a tag-block (\s:<station>,c:<unix_ts>*HH\)
    #[arg(long)]
    nmea_tag_blocks: bool,
    /// Send Iridium GSM (CC/MM/SMS) frames to Wireshark via GSMTAP/UDP
    /// (default 127.0.0.1:4729 when given without an address)
    #[arg(long, num_args = 0..=1, default_missing_value = "127.0.0.1:4729")]
    gsmtap: Option<String>,
    /// Label Iridium ring alerts with the broadcasting satellite via SGP4
    /// (default: auto-fetch Iridium-NEXT TLEs from Celestrak; or give a
    /// local TLE file path)
    #[arg(long, num_args = 0..=1, default_missing_value = "auto")]
    iridium_satmap: Option<String>,
    /// JSON file mapping hex VDL2 ground-station addresses to names
    /// (shown in console output)
    #[arg(long)]
    gs_file: Option<PathBuf>,
    /// Aircraft database CSV (tar1090/Mictronics format: icao;reg;type)
    /// enriching the web dashboard with registrations and types
    #[arg(long)]
    aircraft_db: Option<PathBuf>,
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
    /// Publish messages as [mode, json] over a ZMQ PUB socket (repeatable; one
    /// socket fans out to all). Each binds by default (e.g. tcp://0.0.0.0:5555
    /// or ipc:///tmp/xng.sock); prefix with connect: to connect to a collector
    #[arg(long)]
    zmq: Vec<String>,
}

impl OutputOpts {
    /// Turn the CLI output flags into an output config (default basemap) and
    /// the station identity they name.
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
        let udp = self.udp.clone();
        if self.feed_airframes {
            anyhow::ensure!(
                self.station_id.is_some(),
                "--feed-airframes requires --station-id (e.g. XX-KSEA-ACARS1)"
            );
            // Airframes feeding itself is wired per-session by the caller via
            // `outputs::airframes::cli_router` (the mode isn't known here).
        }
        if let Some(p) = &self.gs_file {
            outputs::console::load_gs_names(p)?;
        }
        if let Some(p) = &self.aircraft_db {
            let n = outputs::dbinfo::AircraftDb::load(p)?;
            tracing::info!("aircraft db: {n} entries");
        }
        if let Some(src) = &self.iridium_satmap {
            match satmap::init(src) {
                Ok(n) => {
                    tracing::info!("iridium satmap: {n} satellites ({src})");
                    // Receiver location from ring-alert Doppler needs the TLEs.
                    xng::doppler_locate::enable(0.0);
                }
                Err(e) => tracing::warn!("iridium satmap disabled: {e}"),
            }
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
                nmea_udp: self.nmea_udp.clone(),
                nmea_tag_blocks: self.nmea_tag_blocks,
                gsmtap: self.gsmtap.clone(),
                iridium_satmap: self.iridium_satmap.clone(),
                http: self.http.clone(),
                map: Default::default(),
                mqtt: self.mqtt.clone(),
                mqtt_topic: self.mqtt_topic.clone(),
                zmq: self.zmq.clone(),
                // Set per-session by the caller once the mode is known.
                airframes: None,
                own_ship_mmsi: None,
                airband: Default::default(),
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
        #[command(flatten)]
        airband: xng::airband::cli::AirbandOpts,
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
        #[command(flatten)]
        airband: xng::airband::cli::AirbandOpts,
    },
    /// Run a whole station from a config file: several decode
    /// sessions (modes + SDRs) in one process with shared outputs
    Station {
        /// Path to the station TOML config
        config: PathBuf,
    },
    /// Show a running station's sessions and live decode status (queries
    /// its web dashboard endpoint)
    Status {
        /// Dashboard address of the running station (host:port)
        #[arg(long, default_value = "127.0.0.1:8080")]
        http: String,
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
        /// Mode to survey: acars, vdl2, hfdl, aero, std-c, ais, adsb, iridium,
        /// uat, sarsat, dsc, navtex, sonde, ads-l, atcs, aprs, pocsag, eot,
        /// flex, vdes, time, meshtastic, meshcore
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
    /// Airframes ATC network: sign this feeder in or out (`xng atc login`)
    Atc {
        #[command(subcommand)]
        cmd: commands::atc::AtcCommand,
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

/// Parse the command line, set up logging and the decode pool, and run the
/// chosen subcommand.
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    tracing::info!("xng v{}", env!("CARGO_PKG_VERSION"));

    // Size the decode worker pool (parallel channelization / burst demod).
    // Left unset, rayon lazily uses all logical cores ("auto"); an explicit
    // --decode-threads pins the count. build_global is one-shot, so only
    // call it when overriding.
    if let Some(n) = cli.decode_threads {
        if runtime::set_decode_threads(n) {
            tracing::info!("decode worker threads: {}", n.max(1));
        }
    } else {
        let auto = std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1);
        tracing::info!("decode worker threads: auto ({auto} cores)");
    }

    match cli.command {
        Command::Devices { filter } => commands::devices::run(&filter),
        Command::Decode { file, format, tune, output, airband } => {
            // "-" reads IQ from stdin (ECO-15): KiwiSDR/GNU Radio/rx_sdr pipes.
            let is_stdin = file.as_os_str() == "-";
            let fmt = match format.as_deref() {
                Some(f) => f.parse().map_err(|e: String| anyhow::anyhow!(e))?,
                None if is_stdin => anyhow::bail!(
                    "reading IQ from stdin (-) requires an explicit --format (cf32|cs16|cs8|cu8)"
                ),
                None => IqFormat::from_extension(&file).ok_or_else(|| {
                    anyhow::anyhow!("cannot guess IQ format; pass --format (cf32|cs16|cs8|cu8)")
                })?,
            };
            let (mode, rate, center_hz, channels_hz) = parse_tune(&tune.tune())?;
            let (mut outputs, station_ident) = output.build()?;
            outputs.airframes =
                Some(outputs::airframes::cli_router(output.feed_airframes, &station_ident, &[mode]));
            outputs.airband = airband.outputs()?;
            let airband_session = airband.session()?;
            let source = if is_stdin {
                FileIqSource::open_stdin(fmt, rate, center_hz)
            } else {
                FileIqSource::open(&file, fmt, rate, center_hz)?
            };
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
                    ais_filter: Default::default(),
                    demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Max),
                    max_ppm: tune.max_ppm,
                    airband: airband_session,
                },
            )
        }
        Command::Listen { sdr, gain, tune, output, airband } => {
            listen(&sdr, gain, &tune, &output, &airband)
        }
        Command::Station { config } => run_station_cmd(&config),
        Command::Status { http } => run_status_cmd(&http),
        Command::IqInfo { file, sample_rate, format, center_freq, fft_size } => {
            commands::iq_info::run(&file, sample_rate, format.as_deref(), center_freq, fft_size)
        }
        Command::Extern { format, output, command } => {
            let fmt: commands::extern_cmd::ExternFormat =
                format.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let (mut outputs, station_ident) = output.build()?;
            outputs.airframes = Some(outputs::airframes::cli_router(
                output.feed_airframes,
                &station_ident,
                &[
                    xng_types::Mode::AcarsPoa,
                    xng_types::Mode::Vdl2,
                    xng_types::Mode::Hfdl,
                    xng_types::Mode::Ais,
                ],
            ));
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
            let (mode, rate, center_hz, channels_hz) = resolve_tune_auto(&tune.tune(), &sdr)?;
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
                    ais_filter: Default::default(),
                    demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Live),
                    max_ppm: tune.max_ppm,
                    airband: Default::default(),
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
                        nmea_udp: None,
                        nmea_tag_blocks: false,
                        gsmtap: None,
                        iridium_satmap: None,
                        http: None,
                        map: Default::default(),
                        mqtt: None,
                        mqtt_topic: "xng".into(),
                        zmq: Vec::new(),
                        airframes: None,
                        own_ship_mmsi: None,
                        airband: Default::default(),
                    },
                },
            )
        }
        Command::Ingest { grpc, quic, quic_cert_out } => {
            commands::ingest::run(grpc, quic, quic_cert_out.as_deref())
        }
        Command::Atc { cmd } => commands::atc::run(cmd),
        Command::Selftest { jsonl, json } => commands::selftest::run(jsonl.as_deref(), json),
    }
}

/// `xng station`: load a station TOML and run every session it names.
fn run_station_cmd(config: &std::path::Path) -> anyhow::Result<()> {
    let st = commands::station::load(config)?;
    let console = if st.outputs.json { ConsoleFormat::Json } else { ConsoleFormat::Pretty };
    runtime::run_station(commands::station::build(&st, console)?)
}

/// `xng status`: query a running station's dashboard JSON and print a
/// per-session table with live decode status.
fn run_status_cmd(http: &str) -> anyhow::Result<()> {
    let body = http_get(http, "/api/state").map_err(|e| {
        anyhow::anyhow!(
            "could not reach a station at http://{http}/ ({e}). Is one running with `http = \"{http}\"` in its config (or --http {http})?"
        )
    })?;
    let s: serde_json::Value = serde_json::from_str(&body)?;

    let now = s["now"].as_u64().unwrap_or(0);
    let started = s["started"].as_u64().unwrap_or(now);
    let station = s["station"].as_str().unwrap_or("xng station");
    let totals = &s["totals"];
    let last_seen = &s["last_seen"];

    let up = now.saturating_sub(started);
    let (h, m, sec) = (up / 3600, (up % 3600) / 60, up % 60);
    let uptime = if h > 0 { format!("{h}h{m:02}m") } else if m > 0 { format!("{m}m{sec:02}s") } else { format!("{sec}s") };
    println!("\n  {station}   up {uptime}   {} aircraft · {} vessels",
        s["aircraft"].as_array().map_or(0, |a| a.len()),
        s["vessels"].as_array().map_or(0, |a| a.len()));

    // Build rows from the session list.
    let empty = vec![];
    let sessions = s["sessions"].as_array().unwrap_or(&empty);
    let mut rows: Vec<[String; 5]> = Vec::new();
    for sess in sessions {
        let sdr = sess["sdr"].as_str().unwrap_or("?");
        let serial = sess["serial"].as_str().unwrap_or("—");
        let mode = sess["mode"].as_str().unwrap_or("?");
        let chans = sess["channels"].as_array().map_or(0, |c| c.len());
        let center = sess["center_mhz"].as_f64().unwrap_or(0.0);
        let driver = sdr.split(',').next().unwrap_or(sdr)
            .strip_prefix("driver=").unwrap_or(sdr);
        let count = totals[mode].as_u64().unwrap_or(0);
        let seen = last_seen[mode].as_u64();
        let status = match (count, seen) {
            (0, _) => "awaiting traffic".to_string(),
            (n, Some(t)) => {
                let ago = now.saturating_sub(t);
                let when = if ago < 5 { "now".to_string() }
                    else if ago < 90 { format!("{ago}s ago") }
                    else { format!("{}m ago", ago / 60) };
                let live = if ago < 60 { "decoding" } else { "idle" };
                format!("{live} · {n} msgs · last {when}")
            }
            (n, None) => format!("{n} msgs"),
        };
        rows.push([
            driver.to_string(),
            serial.to_string(),
            mode.to_uppercase(),
            format!("{chans} ch @ {center:.3} MHz"),
            status,
        ]);
    }
    if rows.is_empty() {
        println!("\n  (station reports no sessions)\n");
        return Ok(());
    }
    print_table(&["SDR", "Serial", "Mode", "Tuning", "Status"], &rows);
    println!();
    Ok(())
}

/// Minimal blocking HTTP/1.0 GET for the local dashboard (small JSON;
/// avoids pulling in an HTTP client crate).
fn http_get(addr: &str, path: &str) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    // One write_all, not write!: the latter issues a syscall per format
    // fragment, and the server reads the request only once — a split
    // request leaves it parsing no path and serving the HTML page.
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    // Accumulate until EOF. A minimal server may RST rather than send a
    // clean FIN; tolerate a reset once we already have the response.
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset && !raw.is_empty() => break,
            Err(e) => return Err(e),
        }
    }
    let raw = String::from_utf8_lossy(&raw);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no HTTP body"))?;
    Ok(body.to_string())
}

/// Render a box-drawing table sized to its content.
fn print_table(headers: &[&str], rows: &[[String; 5]]) {
    let mut w: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            w[i] = w[i].max(cell.chars().count());
        }
    }
    let line = |l: &str, mid: &str, r: &str| {
        let segs: Vec<String> = w.iter().map(|&n| "─".repeat(n + 2)).collect();
        format!("{l}{}{r}", segs.join(mid))
    };
    let fmt_row = |cells: &[String]| {
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!(" {c:<width$} ", width = w[i]))
            .collect();
        format!("│{}│", parts.join("│"))
    };
    println!("  {}", line("┌", "┬", "┐"));
    let hdr: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    println!("  {}", fmt_row(&hdr));
    println!("  {}", line("├", "┼", "┤"));
    for r in rows {
        println!("  {}", fmt_row(r));
    }
    println!("  {}", line("└", "┴", "┘"));
}

fn listen(
    sdr: &str,
    gain: Option<f64>,
    tune: &TuneOpts,
    output: &OutputOpts,
    airband: &xng::airband::cli::AirbandOpts,
) -> anyhow::Result<()> {
    let (mode, rate, center_hz, channels_hz) = parse_tune(&tune.tune())?;
    let (mut outputs, station_ident) = output.build()?;
    outputs.airframes =
        Some(outputs::airframes::cli_router(output.feed_airframes, &station_ident, &[mode]));
    outputs.airband = airband.outputs()?;
    let airband_session = airband.session()?;
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
                    ais_filter: Default::default(),
                    demod_effort: tune.demod_effort.unwrap_or(runtime::DemodEffort::Live),
                    max_ppm: tune.max_ppm,
                    airband: airband_session,
        },
    )
}
