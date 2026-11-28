//! `xng atc login | logout | status`: sign this feeder in to the Airframes
//! ATC network. `xng atc match`: show which ATC catalog entries channels
//! would be fed under. `xng atc nearby`: the channels `--channels auto`
//! would pick.

use crate::airband::atc_auth::{self, DEFAULT_API};
use crate::airband::atc_catalog::{self, Resolution};
use crate::airband::atc_nearby;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum AtcCommand {
    /// Sign in (opens a browser approval by default; --email for a password)
    Login {
        /// Sign in with this email and a password instead of a browser approval
        #[arg(long)]
        email: Option<String>,
        /// Read the password from stdin (no prompt), for scripts
        #[arg(long)]
        password_stdin: bool,
        /// Airframes API base URL
        #[arg(long, default_value = DEFAULT_API)]
        api: String,
        /// Credentials file (default ~/.config/xng/atc.json)
        #[arg(long)]
        credentials: Option<PathBuf>,
    },
    /// Forget the saved sign-in (keeps the feeder id)
    Logout {
        #[arg(long)]
        credentials: Option<PathBuf>,
    },
    /// Show who this feeder is signed in as and check the sign-in works
    Status {
        #[arg(long)]
        credentials: Option<PathBuf>,
    },
    /// Show the ATC catalog entry each frequency would be fed under at a
    /// position (what the feeder does for channels without atc-frequency)
    Match {
        /// Frequencies in MHz, optionally with the channel label that breaks
        /// ties: 120.5 "121.9=SJC Ground"
        #[arg(required = true)]
        frequencies: Vec<String>,
        /// Station latitude
        #[arg(long, allow_hyphen_values = true)]
        lat: f64,
        /// Station longitude
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        /// Airframes API base URL
        #[arg(long, default_value = DEFAULT_API)]
        api: String,
    },
    /// Show the ATC channels `listen --mode airband --channels auto` would
    /// pick near a position, and the command that feeds them
    Nearby {
        /// Station latitude
        #[arg(long, allow_hyphen_values = true)]
        lat: f64,
        /// Station longitude
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        /// Search radius (nautical miles)
        #[arg(long, default_value_t = 30)]
        radius_nm: u32,
        /// SDR sample rate (Hz); sets how much of the band one capture covers
        #[arg(short = 'r', long, default_value_t = 2_400_000.0)]
        sample_rate: f64,
        /// Center frequency to stay on (default: picked)
        #[arg(short = 'c', long)]
        center_freq: Option<String>,
        /// Pick at most this many channels
        #[arg(long, default_value_t = 8)]
        max_channels: usize,
        /// Only these services, e.g. TWR,GND,APP
        #[arg(long, value_delimiter = ',')]
        services: Vec<String>,
        /// Airframes API base URL (default: the sign-in's, like `listen`,
        /// else https://api.airframes.io)
        #[arg(long)]
        api: Option<String>,
        /// Credentials file from `xng atc login` (default ~/.config/xng/atc.json)
        #[arg(long)]
        credentials: Option<PathBuf>,
    },
}

pub fn run(cmd: AtcCommand) -> anyhow::Result<()> {
    match cmd {
        AtcCommand::Login { email, password_stdin, api, credentials } => {
            let path = credentials.unwrap_or_else(atc_auth::default_path);
            let c = match email {
                Some(email) => {
                    let pw = if password_stdin { read_line_stdin()? } else { prompt_password("Password: ")? };
                    atc_auth::login_password(&path, &api, &email, &pw)?
                }
                None => {
                    let dc = atc_auth::device_start(&api)?;
                    println!("To sign this feeder in to the Airframes ATC network, open");
                    println!();
                    println!("    {}", dc.verification_uri_complete.as_deref().unwrap_or(&dc.verification_uri));
                    println!();
                    println!("and confirm the code {}", dc.user_code);
                    println!("Waiting for approval...");
                    atc_auth::device_wait(&path, &api, &dc)?
                }
            };
            println!("Signed in as {}. Saved to {}.", c.username.as_deref().unwrap_or("your account"), path.display());
            println!("Start feeding with --atc-feed (CLI) or [outputs.airband.atc] (station file).");
            Ok(())
        }
        AtcCommand::Logout { credentials } => {
            let path = credentials.unwrap_or_else(atc_auth::default_path);
            if atc_auth::logout(&path)? {
                println!("Signed out. The feeder id in {} is kept.", path.display());
            } else {
                println!("Not signed in.");
            }
            Ok(())
        }
        AtcCommand::Status { credentials } => status(&credentials.unwrap_or_else(atc_auth::default_path)),
        AtcCommand::Match { frequencies, lat, lon, api } => match_frequencies(&frequencies, lat, lon, &api),
        AtcCommand::Nearby { lat, lon, radius_nm, sample_rate, center_freq, max_channels, services, api, credentials } => {
            let center_hz = center_freq.as_deref().map(crate::freq::parse_hz).transpose()?;
            let api = api.unwrap_or_else(|| {
                let path = credentials.unwrap_or_else(atc_auth::default_path);
                atc_auth::load(&path).ok().and_then(|c| c.api).unwrap_or_else(|| DEFAULT_API.to_string())
            });
            let services = services.iter().map(|s| s.trim().to_ascii_uppercase()).collect();
            nearby(&atc_nearby::Query { api, lat, lon, radius_nm, sample_rate, max_channels, services, center_hz })
        }
    }
}

fn match_frequencies(freqs: &[String], lat: f64, lon: f64, api: &str) -> anyhow::Result<()> {
    anyhow::ensure!((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon), "invalid position");
    for arg in freqs {
        let (f, hint) = match arg.split_once('=') {
            Some((f, l)) => (f, Some(l.trim())),
            None => (arg.as_str(), None),
        };
        let hz = crate::freq::parse_hz(f.trim())?;
        let cands = atc_catalog::fetch(api, hz, lat, lon)?;
        let mhz = hz as f64 / 1e6;
        match atc_catalog::pick(&cands, hint) {
            Resolution::Matched(c) => println!("{mhz:.3}  {}  \"{}\"", c.id, c.label()),
            Resolution::NotAllowed(c) => println!(
                "{mhz:.3}  {}  not fed: {}",
                c.id,
                c.policy_reason.as_deref().unwrap_or("streaming is not permitted")
            ),
            Resolution::Ambiguous(_) => {
                println!("{mhz:.3}  ambiguous: label the channel with the airport and service, or set atc-frequency")
            }
            Resolution::NoMatch => println!("{mhz:.3}  no ATC frequency near this position"),
        }
        for c in cands.iter().take(5) {
            println!(
                "         {:<32} {:>6.1} km  confidence {:.2}{}",
                c.id,
                c.distance_km,
                c.confidence,
                if c.streamable { "" } else { "  (not streamable)" }
            );
        }
    }
    Ok(())
}

fn nearby(q: &atc_nearby::Query) -> anyhow::Result<()> {
    let plan = atc_nearby::resolve(q)?;
    let lo = plan.channels.first().map_or(0, |p| p.freq_hz);
    let hi = plan.channels.last().map_or(0, |p| p.freq_hz);
    println!(
        "center {:.3} MHz, {} channels across {:.3} MHz:",
        plan.center_hz as f64 / 1e6,
        plan.channels.len(),
        (hi - lo) as f64 / 1e6
    );
    for p in &plan.channels {
        println!("  {}", atc_nearby::describe(p));
    }
    println!();
    // The same constraints, so `listen` picks what this printed.
    let mut extra = String::new();
    if let Some(c) = q.center_hz {
        extra.push_str(&format!(" -c {c}"));
    }
    if q.radius_nm != 30 {
        extra.push_str(&format!(" --atc-nearby-radius-nm {}", q.radius_nm));
    }
    if q.max_channels != 8 {
        extra.push_str(&format!(" --atc-nearby-max {}", q.max_channels));
    }
    if !q.services.is_empty() {
        extra.push_str(&format!(" --atc-nearby-services {}", q.services.join(",")));
    }
    println!("Feed them (picks again at start, so new catalog entries are used):");
    println!(
        "  xng listen --sdr driver=rtlsdr --mode airband -r {}{extra} --channels auto --receiver-pos {},{} --atc-feed",
        q.sample_rate as u64, q.lat, q.lon
    );
    let mhz = |hz: u64| format!("{:.3}", hz as f64 / 1e6);
    let labels: Vec<String> = plan.channels.iter().map(|p| format!("{}={}", mhz(p.freq_hz), p.label)).collect();
    println!("Or with these channels fixed:");
    println!(
        "  xng listen --sdr driver=rtlsdr --mode airband -r {} -c {} --channels {} --labels {} --receiver-pos {},{} --atc-feed",
        q.sample_rate as u64,
        plan.center_hz,
        plan.channels.iter().map(|p| mhz(p.freq_hz)).collect::<Vec<_>>().join(","),
        shell_quote(&labels.join(",")),
        q.lat,
        q.lon
    );
    Ok(())
}

/// Single-quote for a POSIX shell: catalog names are data, not shell.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn status(path: &Path) -> anyhow::Result<()> {
    let c = atc_auth::load(path)?;
    println!("credentials: {}", path.display());
    println!("feeder id:   {}", c.feeder_id.as_deref().unwrap_or("(none yet)"));
    if c.refresh_token.is_none() {
        println!("signed in:   no (run `xng atc login`)");
        return Ok(());
    }
    println!("api:         {}", c.api.as_deref().unwrap_or(DEFAULT_API));
    match atc_auth::refresh(path) {
        Ok(t) => {
            let c = atc_auth::load(path)?;
            println!("signed in:   yes, as {}", c.username.as_deref().unwrap_or("?"));
            if let Some(s) = t.remaining_s() {
                println!("token:       valid for {} min", s / 60);
            }
        }
        Err(e) => {
            println!("signed in:   saved, but refreshing failed");
            return Err(e.context("the saved sign-in does not work"));
        }
    }
    Ok(())
}

fn read_line_stdin() -> anyhow::Result<String> {
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

/// Prompt for a password with terminal echo off. Refuses to read it when
/// echo cannot be turned off (use --password-stdin there).
fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let echo_off = std::process::Command::new("stty")
            .arg("-echo")
            .stdin(std::process::Stdio::inherit())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !echo_off {
            println!();
            anyhow::bail!("cannot turn off terminal echo; pipe the password with --password-stdin");
        }
        let r = read_line_stdin();
        let _ = std::process::Command::new("stty").arg("echo").stdin(std::process::Stdio::inherit()).status();
        println!();
        r
    }
    #[cfg(not(unix))]
    {
        let _ = prompt;
        anyhow::bail!("reading a password from this terminal is not supported; pipe it with --password-stdin or sign in without --email")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels_are_quoted_for_the_shell() {
        assert_eq!(super::shell_quote("120.500=KSFO TWR"), "'120.500=KSFO TWR'");
        assert_eq!(super::shell_quote("a=O'HARE $(x)"), r"'a=O'\''HARE $(x)'");
    }
}
