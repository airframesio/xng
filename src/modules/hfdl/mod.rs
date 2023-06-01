use crate::common::frame as cff;
use crate::utils::airframes::{AIRFRAMESIO_DUMPHFDL_TCP_PORT, AIRFRAMESIO_HOST};

use self::frame::Frame;
use self::session::DumpHFDLSession;
use self::systable::SystemTable;
use super::settings::ModuleSettings;
use super::XngModule;
use actix_web::web::Data;
use async_trait::async_trait;
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use log::*;
use serde_json::json;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::{io::BufReader, process, sync::RwLock};

mod frame;
mod module;
mod session;
mod systable;

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumphfdl";
const DEFAULT_SYSTABLE_PATH: &'static str = "/etc/systable.conf";

const DEFAULT_STALE_TIMEOUT_SECS: u64 = 1800;
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 600;

const HFDL_COMMAND: &'static str = "hfdl";

const PROP_STALE_TIMEOUT_SEC: &'static str = "stale_timeout_sec";
const PROP_USE_AIRFRAMES_GS: &'static str = "use_airframes_gs";

#[derive(Default)]
pub struct HfdlModule {
    name: &'static str,

    bin: PathBuf,
    systable: SystemTable,

    args: Vec<String>,
    bandwidth: u64,
    driver: String,

    stale_timeout_secs: u64,

    use_airframes_gs: bool,
    feed_airframes: bool,
}

fn extract_soapysdr_driver(args: &Vec<String>) -> Option<String> {
    let Some(soapy_idx) = args.iter().position(|x| x.eq_ignore_ascii_case("--soapysdr")) else {
        return None;
    };
    if soapy_idx + 1 >= args.len() {
        return None;
    }
    args[soapy_idx + 1]
        .split(",")
        .map(|x| x.to_string())
        .collect::<Vec<String>>()
        .into_iter()
        .find(|x| x.to_ascii_lowercase().starts_with("driver="))
}

#[async_trait]
impl XngModule for HfdlModule {
    fn id(&self) -> &'static str {
        self.name
    }

    fn default_session_timeout_secs(&self) -> u64 {
        DEFAULT_SESSION_TIMEOUT_SECS
    }

    fn get_arguments(&self) -> Command {
        Command::new("hfdl")
            .args(&[
                arg!(--bin <FILE> "Path to dumphfdl binary"),
                arg!(--systable <FILE> "Path to dumphfdl system table configuration"),
                arg!(--"stale-timeout" <SECONDS> "Elapsed time since last update before an aircraft and ground station frequency data is considered stale"),
                arg!(--bandwidth <HERTZ> "Initial bandwidth to use for splitting HFDL spectrum into bands of coverage"),
                arg!(--"use-airframes-gs-map" "Use airframes.io's live HFDL ground station frequency map"),
            ])
            .arg(Arg::new("hfdl-args").action(ArgAction::Append))
    }

    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error> {
        self.feed_airframes = args.get_flag("feed-airframes");

        let bin_path = PathBuf::from(
            args.get_one::<String>("bin")
                .unwrap_or(&DEFAULT_BIN_PATH.to_string()),
        );
        if !bin_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "Provided dumphfdl path is not a file: {}",
                    bin_path.to_string_lossy()
                ),
            ));
        }
        self.bin = bin_path;

        self.systable = SystemTable::load(&PathBuf::from(
            args.get_one::<String>("systable")
                .unwrap_or(&DEFAULT_SYSTABLE_PATH.to_string()),
        ))?;

        let Some(hfdl_args) = args.get_many("hfdl-args") else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Missing required HFDL arguments arguments"));
        };
        self.args = hfdl_args
            .clone()
            .map(|x: &String| x.to_string())
            .collect::<Vec<String>>();

        let Some(driver) = extract_soapysdr_driver(&self.args) else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Missing --soapysdr argument with driver= specification"));              
        };
        self.driver = driver;

        if self.feed_airframes
            && !self
                .args
                .iter()
                .any(|x| x.eq_ignore_ascii_case("--station-id"))
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Missing required --station-id <name> argument when feed airframes.io option is enabled"));
        }

        // TODO: replace 1000 with a default value obtained via rust-soapy

        self.bandwidth = args
            .get_one("bandwidth")
            .unwrap_or(&"default")
            .parse::<u64>()
            .unwrap_or(1000);

        self.stale_timeout_secs = args
            .get_one("stale-timeout")
            .unwrap_or(&"default")
            .parse::<u64>()
            .unwrap_or(DEFAULT_STALE_TIMEOUT_SECS);

        self.use_airframes_gs = args.get_flag("use-airframes-gs-map");

        Ok(())
    }

    async fn load_module_settings(&self, settings: Data<RwLock<ModuleSettings>>) {
        let mut settings = settings.write().await;

        settings.props.insert(
            PROP_STALE_TIMEOUT_SEC.to_string(),
            json!(self.stale_timeout_secs),
        );

        settings.props.insert(
            PROP_USE_AIRFRAMES_GS.to_string(),
            json!(self.use_airframes_gs),
        );
    }

    fn start_session(&self) -> Result<Box<dyn super::session::Session>, io::Error> {
        let mut extra_args = self.args.clone();
        let output_arg = format!(
            "decoded:json:tcp:address={},port={}",
            AIRFRAMESIO_HOST, AIRFRAMESIO_DUMPHFDL_TCP_PORT
        );

        if let Some(idx) = extra_args
            .iter()
            .position(|x| x.eq_ignore_ascii_case(&output_arg))
        {
            if idx == 0 || !extra_args[idx - 1].eq_ignore_ascii_case("--output") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Invalid addition arguments found at index {}: {:?}",
                        idx, extra_args
                    ),
                ));
            }
        } else {
            extra_args.extend_from_slice(&[String::from("--output"), output_arg]);
        }

        let mut proc = match process::Command::new(self.bin.clone())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("--system-table")
            .arg(self.systable.path.to_path_buf())
            .arg("--sample-rate")
            .arg(format!("{}", self.bandwidth))
            .arg("--output")
            .arg("decoded:json:file:path=-")
            .args(extra_args)
            .spawn()
        {
            Ok(v) => v,
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to spawn process: {}", e.to_string()),
                ));
            }
        };

        let Some(stdout) = proc.stdout.take() else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unable to take stdout from child process"));
        };
        let Some(stderr) = proc.stderr.take() else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unable to take stderr from child process"));
        };

        debug!("New HFDL session started");

        Ok(Box::new(DumpHFDLSession::new(
            proc,
            BufReader::new(stdout),
            stderr,
        )))
    }

    fn process_message(&self, msg: &str) -> Result<crate::common::frame::CommonFrame, io::Error> {
        let raw_frame = serde_json::from_str::<Frame>(msg)?;
        let frame_src: cff::Entity;
        let mut frame_dst: Option<cff::Entity> = None;

        if let Some(ref spdu) = raw_frame.hfdl.spdu {
            frame_src = spdu.src.to_common_frame_entity(&self.systable);

            if spdu.systable_version > self.systable.version {
                warn!("System Table from SPDU is newer than provided! Provided version = {}, SPDU version = {}", self.systable.version, spdu.systable_version);
            }
            // TODO: parse ground stations list into metadata?
            println!("{:?}", spdu);
        } else if let Some(ref lpdu) = raw_frame.hfdl.lpdu {
            frame_src = lpdu.src.to_common_frame_entity(&self.systable);
            frame_dst = Some(lpdu.dst.to_common_frame_entity(&self.systable));

            // TODO: parse HFNPDU
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HFDL frame missing LPDU/SPDU block",
            ));
        }

        Ok(cff::CommonFrame {
            timestamp: raw_frame.hfdl.ts.to_f64(),
            freq: raw_frame.hfdl.freq_as_mhz(),
            signal: raw_frame.hfdl.sig_level as f32,

            err: false,

            paths: None,

            app: cff::AppInfo {
                name: raw_frame.hfdl.app.name,
                version: raw_frame.hfdl.app.version,
            },

            src: frame_src,
            dst: frame_dst,
            acars: None,
        })
    }
}
