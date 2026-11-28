//! `xng atc login | logout | status`: sign this feeder in to the Airframes
//! ATC network. `xng atc match`: show which ATC catalog entries channels
//! would be fed under.

use crate::airband::atc_auth::{self, DEFAULT_API};
use crate::airband::atc_catalog::{self, Resolution};
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
