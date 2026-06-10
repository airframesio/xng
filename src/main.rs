//! xng — next-generation multi-mode SDR decoder.
//!
//! M1: native ACARS decoding from SDR hardware or IQ recordings, with
//! console/JSONL outputs and acarsdec-compatible Airframes feeding.

mod bus;
mod commands;
mod freq;
mod outputs;
mod runtime;

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
    /// Decode mode: acars, vdl2, hfdl, aero, aero-c, std-c, ais, or adsb
    #[arg(short, long, default_value = "acars")]
    mode: String,
    /// Capture sample rate in Hz (must be an integer multiple of the
    /// mode's channel rate: 24 kHz for ACARS, 48 kHz for AIS; 2400000
    /// works for both)
    #[arg(short = 'r', long)]
    sample_rate: f64,
    /// Capture center frequency (e.g. 131.500M)
    #[arg(short = 'c', long)]
    center_freq: String,
    /// Channel frequencies, comma separated (e.g. 131.550,131.725)
    #[arg(long, value_delimiter = ',', required = true)]
    channels: Vec<String>,
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
        let ident = self.station_id.clone().unwrap_or_else(|| "XNG-DEV".to_owned());
        Ok((
            runtime::OutputConfig {
                console: if self.json { ConsoleFormat::Json } else { ConsoleFormat::Pretty },
                jsonl: self.jsonl.clone(),
                udp,
                asf2_grpc: self.asf2_grpc.clone(),
                asf2_quic: self.asf2_quic.clone(),
                asf2_quic_trust: quic_trust,
            },
            ident,
        ))
    }
}

#[derive(Subcommand)]
enum Command {
    /// List available SDR devices (SoapySDR)
    Devices {
        /// SoapySDR filter args, e.g. "driver=rtlsdr"
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
        /// SoapySDR device args, e.g. "driver=rtlsdr" or "driver=airspy,serial=..."
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

fn parse_tune(tune: &TuneOpts) -> anyhow::Result<(xng_types::Mode, u64, Vec<u64>)> {
    let mode: xng_types::Mode = tune.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let center = freq::parse_hz(&tune.center_freq)?;
    let channels = tune
        .channels
        .iter()
        .map(|c| freq::parse_hz(c))
        .collect::<anyhow::Result<Vec<u64>>>()?;
    Ok((mode, center, channels))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match cli.command {
        Command::Devices { filter } => commands::devices::run(&filter),
        Command::Decode { file, format, tune, output } => {
            let fmt = match format.as_deref() {
                Some(f) => f.parse().map_err(|e: String| anyhow::anyhow!(e))?,
                None => IqFormat::from_extension(&file).ok_or_else(|| {
                    anyhow::anyhow!("cannot guess IQ format; pass --format (cf32|cs16|cs8|cu8)")
                })?,
            };
            let (mode, center_hz, channels_hz) = parse_tune(&tune)?;
            let (outputs, station_ident) = output.build()?;
            let source = FileIqSource::open(&file, fmt, tune.sample_rate, center_hz)?;
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
                },
            )
        }
        Command::Listen { sdr, gain, tune, output } => {
            listen(&sdr, gain, &tune, &output)
        }
        Command::IqInfo { file, sample_rate, format, center_freq, fft_size } => {
            commands::iq_info::run(&file, sample_rate, format.as_deref(), center_freq, fft_size)
        }
        Command::Ingest { grpc, quic, quic_cert_out } => {
            commands::ingest::run(grpc, quic, quic_cert_out.as_deref())
        }
        Command::Selftest { jsonl, json } => commands::selftest::run(jsonl.as_deref(), json),
    }
}

#[cfg(feature = "soapy")]
fn listen(sdr: &str, gain: Option<f64>, tune: &TuneOpts, output: &OutputOpts) -> anyhow::Result<()> {
    let (mode, center_hz, channels_hz) = parse_tune(tune)?;
    let (outputs, station_ident) = output.build()?;
    let source = xng_sdr::soapy::SoapyIqSource::open(sdr, tune.sample_rate, center_hz, gain)?;
    runtime::run_session(
        Box::new(source),
        runtime::SessionConfig {
            mode,
            center_hz,
            channels_hz,
            station_ident,
            sdr: Some(xng_types::SdrInfo {
                id: sdr.to_owned(),
                driver: "soapysdr".into(),
                serial: None,
            }),
            outputs,
        },
    )
}

#[cfg(not(feature = "soapy"))]
fn listen(_sdr: &str, _gain: Option<f64>, _tune: &TuneOpts, _output: &OutputOpts) -> anyhow::Result<()> {
    anyhow::bail!("xng was built without SDR device support; rebuild with --features soapy")
}
