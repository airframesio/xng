//! `xng airframes`: inspect the Airframes feed gateway.

use crate::outputs::airframes;
use crate::outputs::airframes_quic::{self, QuicSettings};
use std::path::PathBuf;
use xng_types::Mode;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum AirframesCommand {
    /// Show what the feed gateway takes now, and how each mode this xng
    /// decodes would be sent with `transport = "quic"`
    Ingests {
        /// Feed gateway host:port
        #[arg(long, default_value = airframes_quic::DEFAULT_ENDPOINT)]
        endpoint: String,
        /// quic (then TLS over TCP), quic-only or tls
        #[arg(long, default_value = "quic")]
        transport: String,
        /// PEM certificate/CA to trust (self-hosted gateway)
        #[arg(long)]
        ca: Option<PathBuf>,
        /// DANGEROUS: no certificate check. Lab use only.
        #[arg(long)]
        insecure: bool,
        /// Print the gateway's WELCOME as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cmd: AirframesCommand) -> anyhow::Result<()> {
    match cmd {
        AirframesCommand::Ingests {
            endpoint,
            transport,
            ca,
            insecure,
            json,
        } => {
            let s = QuicSettings::for_transport(Some(&transport), Some(endpoint), ca, insecure)?
                .ok_or_else(|| anyhow::anyhow!("--transport must be quic, quic-only or tls"))?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let (welcome, via) = rt.block_on(airframes_quic::probe(&s))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&welcome)?);
                return Ok(());
            }
            let ingests = airframes_quic::parse_ingests(&welcome);
            println!("{} over {via}", s.endpoint);
            match &welcome["user"]["username"] {
                serde_json::Value::String(u) => println!("signed in as {u}"),
                _ => println!("anonymous (sign in with `xng atc login`)"),
            }
            match &ingests {
                None => {
                    println!("this gateway does not list its ingests (it takes the native formats)")
                }
                Some(list) => {
                    println!("\nThe gateway takes:");
                    for (f, m) in list {
                        println!("  {m:<12} {f}");
                    }
                }
            }
            println!("\nThis xng would send:");
            let listed = |f: &str, m: Mode| match &ingests {
                Some(list) => list.iter().any(|(lf, lm)| lf == f && lm == m.as_str()),
                // No list: the native formats only (what the uplink assumes).
                None => f != airframes_quic::XNG_FORMAT,
            };
            for &mode in Mode::native_modes() {
                let how = if !airframes_quic::gateway_eligible(mode) {
                    match mode {
                        Mode::Adsb | Mode::Uat => {
                            "never (Airframes does not ingest ADS-B; feed ADS-B networks)"
                        }
                        Mode::Airband => "ATC feed (xng atc)",
                        Mode::Sonde => "per-port sonde intake",
                        _ => "not sent",
                    }
                    .to_string()
                } else if let Some(f) = airframes_quic::format_for(mode).filter(|f| listed(f, mode))
                {
                    f.to_string()
                } else if listed(airframes_quic::XNG_FORMAT, mode) {
                    "xng".to_string()
                } else if airframes::default_endpoint(mode).is_some() {
                    "per-port ingest (the gateway does not take it yet)".to_string()
                } else {
                    "not yet (no Airframes ingest)".to_string()
                };
                println!("  {:<12} {how}", mode.as_str());
            }
            Ok(())
        }
    }
}
