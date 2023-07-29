use log::*;
use tokio::io::BufReader;
use std::path::PathBuf;
use std::process::Stdio;

use actix_web::web::Data;
use async_trait::async_trait;
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use serde_json::json;
use tokio::sync::RwLock;
use tokio::{io, process};

use self::frame::Frame;
use self::session::DumpVDL2Session;
use self::validators::validate_next_session_band;

use super::session::EndSessionReason;
use super::settings::ModuleSettings;
use super::XngModule;
use crate::common::arguments::{extract_soapysdr_driver, parse_bin_path};
use crate::common::{AIRFRAMESIO_DUMPVDL2_UDP_PORT, AIRFRAMESIO_HOST};
use crate::modules::PROP_LISTENING_BAND;
use crate::server::db::StateDB;

mod frame;
mod ground_station_db;
mod module;
mod session;
mod validators;

const AOA_COMMAND: &'static str = "aoa";

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumpvdl2";
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 900;
const DEFAULT_VDL2_FREQ: u64 = 136975;

const PROP_NEXT_SESSION_BAND: &'static str = "next_session_band";

#[derive(Default)]
pub struct AoaModule {
    name: &'static str,
    settings: Option<Data<RwLock<ModuleSettings>>>,

    bin: PathBuf,
    // TODO: ground station data struct
    args: Vec<String>,
    driver: String,

    feed_airframes: bool,

    next_session_band: Vec<u64>,
}

#[async_trait]
impl XngModule for AoaModule {
    fn id(&self) -> &'static str {
        self.name
    }

    fn default_session_timeout_secs(&self) -> u64 {
        DEFAULT_SESSION_TIMEOUT_SECS
    }

    fn get_arguments(&self) -> Command {
        Command::new(AOA_COMMAND)
            .about("Listen to ACARS-Over-AVLC messages using dumpvdl2")
            .args(&[
                arg!(--bin <FILE> "Path to dumpvdl2 binary"),
                arg!(--"ground-stations" <FILE> "Path to VDL2 Ground Stations CSV file from Airframes data repository (geo-region specific)"),
                Arg::new("start-bands").long("start-bands").value_delimiter(',').help("Starting VDL2 frequencies in kHz to listen to (default: 136975)")
            ])
            .arg(Arg::new("aoa-args").action(ArgAction::Append))
    }

    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error> {
        self.feed_airframes = args.get_flag("feed-airframes");

        let bin_path = parse_bin_path(args, DEFAULT_BIN_PATH);
        if !bin_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "Provided dumpvdl2 path is not a file: {}",
                    bin_path.to_string_lossy()
                ),
            ));
        }
        self.bin = bin_path;

        let Some(aoa_args) = args.get_many("aoa-args") else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Missing required VDL2 positional arguments"));  
        };
        self.args = aoa_args
            .clone()
            .map(|x: &String| x.to_string())
            .collect::<Vec<String>>();

        let Some(driver) = extract_soapysdr_driver(&self.args) else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Missing --soapysdr argument with driver= specification"));              
        };
        self.driver = driver.clone();

        if self.feed_airframes
            && !self
                .args
                .iter()
                .any(|x| x.eq_ignore_ascii_case("--station-id"))
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Missing required --station-id <name> argument when feed airframes.io option is enabled"));
        }

        let start_bands: Vec<u64> = match args.get_many::<String>("start-bands") {
            Some(bands) => {
                let mut freqs: Vec<u64> = bands
                    .map(|x| x.as_str().parse::<u64>().unwrap_or(0))
                    .collect();
                freqs.sort_unstable();
                freqs
            }
            None => vec![DEFAULT_VDL2_FREQ],
        };
        if start_bands.iter().any(|&x| x < 118000 || x > 137000) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Starting bands contains invalid kHz frequency: {:?}",
                    start_bands
                ),
            ));
        }
        self.next_session_band = start_bands;

        Ok(())
    }

    async fn init(
        &mut self,
        settings: Data<RwLock<ModuleSettings>>,
        _state_db: Data<RwLock<StateDB>>,
    ) {
        self.settings = Some(settings.clone());

        let mut settings = settings.write().await;

        settings.add_prop_with_validator(
            PROP_NEXT_SESSION_BAND.to_string(),
            json!(self.next_session_band),
            validate_next_session_band,
        );
    }

    async fn start_session(
        &mut self,
        _last_end_reason: EndSessionReason,
    ) -> Result<Box<dyn super::session::Session>, io::Error> {
        let settings = self.get_settings()?;

        let mut extra_args = self.args.clone();
        let output_arg = format!(
            "decoded:json:udp:address={},port={}",
            AIRFRAMESIO_HOST, AIRFRAMESIO_DUMPVDL2_UDP_PORT
        );

        if self.feed_airframes {
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
        }

        let listening_bands: Vec<u64>;
        
        let mut proc;
        {
            let mut settings = settings.write().await;

            let next_session_band: Vec<u64>;
            {
                let Some(value) = settings.props.get(&PROP_NEXT_SESSION_BAND.to_string()) else {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_NEXT_SESSION_BAND prop")
                    );    
                };

                // NOTE: next_session_band's validator should prevent malformed entries from sneaking in
                match value.as_array() {
                    Some(x) => next_session_band = x.into_iter().map(|y| y.as_u64().unwrap_or(0)).collect(),
                    None => {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "PROP_NEXT_SESSION_BAND expects an array of numbers"))
                    }
                }
            }
        
            proc = match process::Command::new(self.bin.clone())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .arg("--output")
                .arg("decoded:json:file:path=-")
                .args(extra_args)
                .args(next_session_band.iter().map(|x| (x * 1000).to_string()).collect::<Vec<String>>())
                .spawn()
            {
                Ok(v) => v,
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("Failed to spawn process: {}", e.to_string()),
                    ))
                }
            };

            {
                let Some(value) = settings.props.get_mut(&PROP_LISTENING_BAND.to_string()) else {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_LISTENING_BAND prop")
                    );    
                };
                *value = json!(next_session_band);
            }

            listening_bands = next_session_band.clone();
            
            {
                let Some(value) = settings.props.get_mut(&PROP_NEXT_SESSION_BAND.to_string()) else {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_NEXT_SESSION_BAND prop")
                    );    
                };
                *value = json!([]);
            }

            debug!("New AoA session started, listening: {:?}", next_session_band);            
        }
        
        let Some(stdout) = proc.stdout.take() else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unable to take stdout from child process"));
        };
        let Some(stderr) = proc.stderr.take() else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unable to take stderr from child process"));
        };

        Ok(Box::new(DumpVDL2Session::new(proc, BufReader::new(stdout), stderr, listening_bands)))
    }

    async fn process_message(
        &mut self,
        current_band: &Vec<u64>,
        msg: &str,
    ) -> Result<crate::common::frame::CommonFrame, io::Error> {
        let raw_frame = serde_json::from_str::<Frame>(msg)?;

        info!("{:?}", raw_frame);
        
        Err(io::Error::new(io::ErrorKind::Other, "not implemented yet"))
    }

    async fn reload(&mut self) -> Result<(), io::Error> {
        Ok(())
    }
}
