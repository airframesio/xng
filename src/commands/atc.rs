//! `xng atc login | logout | status`: sign this feeder in to the Airframes
//! ATC network.

use crate::airband::atc_auth::{self, DEFAULT_API};
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
    }
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
        Err(e) => println!("signed in:   saved, but refreshing failed: {e:#}"),
    }
    Ok(())
}

fn read_line_stdin() -> anyhow::Result<String> {
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

/// Prompt without echo where the terminal allows it.
fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    #[cfg(unix)]
    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .stdin(std::process::Stdio::inherit())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let r = read_line_stdin();
    #[cfg(unix)]
    if echo_off {
        let _ = std::process::Command::new("stty").arg("echo").stdin(std::process::Stdio::inherit()).status();
        println!();
    }
    r
}
