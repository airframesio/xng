//! xng — next-generation multi-mode SDR decoder.
//!
//! M0 foundation: CLI skeleton, message bus, console/JSONL outputs, IQ file
//! tooling. Decode cores land per `docs/ARCHITECTURE.md` §5.

mod bus;
mod commands;
mod outputs;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "xng", version, about = "Next-generation multi-mode SDR decoder (ACARS, VDL2, HFDL, satcom, AIS, ...)")]
struct Cli {
    /// Increase log verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List available SDR devices (SoapySDR)
    Devices {
        /// SoapySDR filter args, e.g. "driver=rtlsdr"
        #[arg(default_value = "")]
        filter: String,
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
    /// Run the built-in pipeline self-test (bus + outputs + DSP sanity)
    Selftest {
        /// Also write messages to this JSONL file
        #[arg(long)]
        jsonl: Option<PathBuf>,
        /// Print messages as raw JSON instead of pretty one-liners
        #[arg(long)]
        json: bool,
    },
    /// Start decode sessions (native cores land in M1+)
    Listen,
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match cli.command {
        Command::Devices { filter } => commands::devices::run(&filter),
        Command::IqInfo { file, sample_rate, format, center_freq, fft_size } => {
            commands::iq_info::run(&file, sample_rate, format.as_deref(), center_freq, fft_size)
        }
        Command::Selftest { jsonl, json } => commands::selftest::run(jsonl.as_deref(), json),
        Command::Listen => {
            anyhow::bail!(
                "native decode cores land starting with M1 (ACARS); see docs/ARCHITECTURE.md §5. \
                 The legacy dumphfdl wrapper lives in legacy/ until the extern module returns in M10."
            )
        }
    }
}
