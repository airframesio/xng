use chrono::{SecondsFormat, Utc};
use chrono_tz::UTC;
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

use self::frame::{Frame, ParamACLocation};
use self::ground_station_db::GroundStationDB;
use self::session::DumpVDL2Session;
use self::validators::validate_next_session_band;

use super::session::EndSessionReason;
use super::settings::ModuleSettings;
use super::XngModule;
use crate::common::arguments::{extract_soapysdr_driver, parse_bin_path};
use crate::common::wkt::WKTPolyline;
use crate::common::{AIRFRAMESIO_DUMPVDL2_UDP_PORT, AIRFRAMESIO_HOST};
use crate::common::frame::{self as cff, Indexed};
use crate::modules::PROP_LISTENING_BAND;
use crate::server::db::StateDB;
use crate::utils::normalize_tail;
use crate::utils::timestamp::{split_unix_time_to_utc_datetime, unix_time_to_utc_datetime};

mod frame;
mod ground_station_db;
mod module;
mod session;
mod validators;

const AOA_COMMAND: &'static str = "aoa";
const BROADCAST_ADDR: &'static str = "FFFFFF";

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumpvdl2";
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 900;
const DEFAULT_VDL2_FREQ: u64 = 136975;

const PROP_NEXT_SESSION_BAND: &'static str = "next_session_band";

#[derive(Default)]
pub struct AoaModule {
    name: &'static str,
    settings: Option<Data<RwLock<ModuleSettings>>>,
    state_db: Option<Data<RwLock<StateDB>>>,
    
    bin: PathBuf,
    stations: Option<GroundStationDB>,
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

        if let Some(ground_station_path) = args.get_one::<String>("ground-stations") {
            self.stations = Some(GroundStationDB::from_csv(ground_station_path)?);
        }

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
        state_db: Data<RwLock<StateDB>>,
    ) {
        self.state_db = Some(state_db.clone());
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
        let mut frame_src: cff::Entity;
        let mut frame_dst: Option<cff::Entity> = None;

        let mut acars_content: Option<cff::ACARS> = None;

        let Some(arrival_time) = split_unix_time_to_utc_datetime(
            raw_frame.vdl2.ts.sec as i64, 
            (raw_frame.vdl2.ts.usec * 1000) as u32
        ) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid arrival time",
            ));
        };

        let mut paths: Vec<cff::PropagationPath> = Vec::new();

        let mut indexed = cff::Indexed {
            timestamp: arrival_time.to_rfc3339_opts(SecondsFormat::Micros, true),

            ..Default::default()
        };
        let mut has_err = false;
        
        if let Some(ref avlc) = raw_frame.vdl2.avlc {
            frame_src = avlc.src.to_common_frame_entity(self.stations.as_ref());
            frame_dst = Some(avlc.dst.to_common_frame_entity(self.stations.as_ref()));

            if let Some(state_db) = self.state_db.clone() {
                let db = state_db.write().await;
                let station: &cff::Entity = if avlc.from_ground_station() {
                    &frame_src
                } else {
                    frame_dst.as_ref().unwrap()
                };

                if station.gs.is_some() && station.coords.is_some() {
                    let gs_name = station.gs.as_ref().unwrap();
                    let coords = station.coords.as_ref().unwrap();
                    let Some(ref addr) = station.icao else {
                        return Err(io::Error::new(io::ErrorKind::NotFound, format!("Ground station missing ICAO: {:?}", station)));    
                    };
                
                    match u32::from_str_radix(addr.as_str(), 16) {
                        Ok(x) => if let Err(e) = db.create_ground_station(x, gs_name, coords.y, coords.x).await {
                            return Err(io::Error::new(io::ErrorKind::Other, format!("Failed to create ground station in state DB: {}", e.to_string())));
                        },
                        Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a valid hexadecimal ICAO addr: {}", addr, e.to_string())))
                    }
                } 
            }
            
            if let Some(ref acars) = avlc.acars {
                has_err = acars.err;
                acars_content = Some(cff::ACARS {
                    mode: acars.mode.clone(),
                    more: acars.more.clone(),
                    label: acars.label.clone(),
                    ack: Some(acars.ack.clone()),
                    blk_id: Some(acars.blk_id.clone()),
                    msg_num: acars.msg_num.clone(),
                    msg_num_seq: acars.msg_num_seq.clone(),
                    tail: Some(acars.reg.clone()),
                    flight: acars.flight.clone(),
                    sublabel: acars.sublabel.clone(),
                    mfi: acars.mfi.clone(),
                    cfi: acars.cfi.clone(),
                    text: Some(acars.msg_text.clone()),
                });

                if avlc.from_ground_station() {
                    match frame_dst {
                        Some(ref mut entity) => {
                            entity.tail = Some(normalize_tail(&acars.reg));
                        },
                        _ => {}
                    }
                } else {
                    frame_src.tail = Some(normalize_tail(&acars.reg));
                    frame_src.callsign = acars.flight.clone();
                }
            } else if let Some(ref xid) = avlc.xid {
                if let Some(param) = xid.vdl_params.iter().find(|x| x.name == "ac_location") {
                    match serde_json::from_value::<ParamACLocation>(param.value.clone()) {
                        Ok(x) => {
                            if let Some(ref gs) = frame_dst {
                                if let Some(ref gs_pt) = gs.coords {
                                    paths.push(
                                        cff::PropagationPath { 
                                            freqs: vec![raw_frame.vdl2.freq_as_mhz()], 
                                            path: WKTPolyline{
                                                points: vec![x.wkt().as_tuple(), gs_pt.as_tuple()],
                                            }, 
                                            party: gs.clone(), 
                                        }
                                    );
                                }
                            }
                            
                            frame_src.coords = Some(x.wkt());
                        }
                        Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{:?} is not a valid ac_location VDL2 param: {}", param.value, e.to_string())))
                    }
                }

                if let Some(param) = xid.vdl_params.iter().find(|x| x.name == "dst_airport") {
                    if let Some(dst_airport) = param.value.as_str() {
                        indexed.dst_airport = Some(dst_airport.to_string());
                    }
                }
            } else if let Some(ref x25) = avlc.x25 {
                if let Some(ref clnp) = x25.clnp {
                    // TODO: ADS-C v2
                }
            }
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "VDL2 frame missing AVLC block"));
        }

        Ok(cff::CommonFrame {
            timestamp: unix_time_to_utc_datetime(raw_frame.vdl2.ts.to_f64()).unwrap_or(Utc::now().with_timezone(&UTC)).to_rfc3339_opts(SecondsFormat::Nanos, true),
            freq: raw_frame.vdl2.freq_as_mhz(),
            signal: raw_frame.vdl2.sig_level as f32,

            err: has_err,

            paths,

            app: cff::AppInfo {
                name: raw_frame.vdl2.app.name,
                version: raw_frame.vdl2.app.version,
            },

            indexed,
            metadata: cff::Metadata { hfdl: None },
            
            src: frame_src,
            dst: frame_dst,
            acars: acars_content,
        })   
    }

    async fn reload(&mut self) -> Result<(), io::Error> {
        Ok(())
    }
}
