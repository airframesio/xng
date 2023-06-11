use crate::common::formats::EntityType;
use crate::common::frame::{self as cff, Indexed, HFDLGSEntry};
use crate::common::wkt::WKTPolyline;
use crate::modules::hfdl::airframes::{AIRFRAMESIO_HOST, AIRFRAMESIO_DUMPHFDL_TCP_PORT, get_airframes_gs_status};
use crate::modules::hfdl::utils::{freq_bands_by_sample_rate, first_freq_above_eq};
use crate::utils::normalize_tail;
use crate::utils::timestamp::{split_unix_time_to_utc_datetime, nearest_time_in_past, unix_time_to_utc_datetime};

use self::frame::Frame;
use self::method::valid_session_method;
use self::schedule::valid_session_schedule;
use self::session::DumpHFDLSession;
use self::systable::SystemTable;
use self::validators::validate_listening_bands;
use super::settings::ModuleSettings;
use super::XngModule;
use actix_web::web::Data;
use async_trait::async_trait;
use chrono::{Utc, SecondsFormat};
use chrono_tz::UTC;
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use log::*;
use serde_json::json;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::{io::BufReader, process, sync::RwLock};

mod airframes;
mod frame;
mod method;
mod module;
mod schedule;
mod session;
mod systable;
mod utils;
mod validators;

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumphfdl";
const DEFAULT_SYSTABLE_PATH: &'static str = "/etc/systable.conf";

const DEFAULT_STALE_TIMEOUT_SECS: u64 = 1800;
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 600;

const HFDL_COMMAND: &'static str = "hfdl";

const PROP_STALE_TIMEOUT_SEC: &'static str = "stale_timeout_sec";
const PROP_USE_AIRFRAMES_GS: &'static str = "use_airframes_gs";
const PROP_SAMPLE_RATE: &'static str = "sample_rate";
const PROP_LISTENING_BAND: &'static str = "listening_band";
const PROP_NEXT_SESSION_BAND: &'static str = "next_session_band";
const PROP_SESSION_SCHEDULE: &'static str = "session_schedule";
const PROP_SESSION_METHOD: &'static str = "session_method";
const PROP_ACTIVE_ONLY: &'static str = "active_only";

#[derive(Default)]
pub struct HfdlModule {
    name: &'static str,
    settings: Option<Data<RwLock<ModuleSettings>>>,

    // TODO: have a field to store all valid sample rates for the driver
    
    bin: PathBuf,
    systable: SystemTable,

    args: Vec<String>,
    driver: String,

    feed_airframes: bool,
    
    sample_rate: u64,
    stale_timeout_secs: u64,
    use_airframes_gs: bool,
    next_session_band: u64,
    schedule: String,
    method: String,
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
                arg!(--"sample-rate" <HERTZ> "Initial sample rate to use for splitting HFDL spectrum into bands of coverage"),
                arg!(--"use-airframes-gs-map" "Use airframes.io's live HFDL ground station frequency map"),
                arg!(--"start-band-contains" <HERTZ> "Initial starting band to listen on. Overrides --schedule if both are configured"),
                arg!(--schedule <SCHEDULE_FMT> "Session switch schedule in the format of: hour=<HOUR_0_TO_23>,band_contains=<FREQ_HZ>;..."),
                arg!(--method <METHOD_TYPE> "Session switching methods to use. Default method is random. Valid methods: random, inc, dec, static")
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

        // TODO: Populate sample rates from rust-soapy into a field
        
        self.sample_rate = args
            .get_one::<String>("sample-rate")
            .unwrap_or(&String::from("default"))
            .parse::<u64>()
            .unwrap_or(512000);

        self.stale_timeout_secs = args
            .get_one::<String>("stale-timeout")
            .unwrap_or(&String::from("default"))
            .parse::<u64>()
            .unwrap_or(DEFAULT_STALE_TIMEOUT_SECS);

        self.use_airframes_gs = args.get_flag("use-airframes-gs-map");
        
        // TODO: parse schedule and determine if we need to populate initial next_band
        let schedule = args.get_one::<String>("schedule");
        
        self.next_session_band = args
            .get_one::<String>("start-band-contains")
            .unwrap_or(&String::from("default"))
            .parse::<u16>()
            .unwrap_or(0) as u64;

        // TODO: parse method
        let method = args.get_one::<String>("method");

        Ok(())
    }

    async fn init(&mut self, settings: Data<RwLock<ModuleSettings>>) {
        self.settings = Some(settings.clone());
        
        let mut settings = settings.write().await;

        settings.props.insert(
            PROP_STALE_TIMEOUT_SEC.to_string(),
            json!(self.stale_timeout_secs),
        );
        settings.props.insert(
            PROP_USE_AIRFRAMES_GS.to_string(),
            json!(self.use_airframes_gs),
        );
        settings.props.insert(
            PROP_SAMPLE_RATE.to_string(),
            json!(self.sample_rate),
        );

        settings.add_prop_with_validator(
            PROP_LISTENING_BAND.to_string(), 
            json!(Vec::new() as Vec<u64>), 
            validate_listening_bands
        );
        settings.add_prop_with_validator(
            PROP_NEXT_SESSION_BAND.to_string(), 
            json!(self.next_session_band), 
            valid_session_method
        );
        settings.add_prop_with_validator(
            PROP_SESSION_SCHEDULE.to_string(), 
            json!(self.schedule), 
            valid_session_schedule
        );
        settings.add_prop_with_validator(
            PROP_SESSION_METHOD.to_string(),
            json!(self.method),
            valid_session_method
        );
    }

    async fn start_session(&mut self) -> Result<Box<dyn super::session::Session>, io::Error> {
        let settings = self.get_settings()?;
        
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

        let mut proc;
        {
            let mut settings = settings.write().await;

            let use_airframes_gs;
            {
                let value = settings.props.get(&PROP_USE_AIRFRAMES_GS.to_string()).unwrap_or(&json!(false));
                use_airframes_gs = value.as_bool().unwrap_or(false);
            }
            
            let sample_rate;
            {
                let Some(suggested_sample_rate) = settings.props.get(&PROP_SAMPLE_RATE.to_string()) else {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_SAMPLE_RATE prop"));    
                };
                match self.nearest_sample_rate(suggested_sample_rate.as_u64().unwrap_or(0)) {
                    Some(rate) => sample_rate = rate,
                    None => {
                        return Err(
                            io::Error::new(io::ErrorKind::InvalidData, 
                            format!("Failed to find nearest sample rate to {:?}", suggested_sample_rate))
                        );
                    }
                };
            }
            
            let next_session_band;
            {
                let Some(value) = settings.props.get(&PROP_NEXT_SESSION_BAND.to_string()) else {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_NEXT_SESSION_BAND prop")
                    );    
                };
                next_session_band = value.as_u64().unwrap_or(0);
            } 

            if next_session_band == 0 {
                // TODO: use method + sample rate to calculate next_session_band
            }

            let mut all_freqs: Vec<u16> = Vec::new();
            if use_airframes_gs {
                match get_airframes_gs_status().await {
                    Ok(gs_status) => {
                        all_freqs.extend(gs_status.all_freqs());
                    }
                    Err(e) => debug!("Failed to get Airframes HFDL map: {}", e.to_string())
                }
            }

            if all_freqs.is_empty() {
                all_freqs.extend_from_slice(&self.systable.all_freqs());
            }
            all_freqs.sort_unstable();

            let bands_for_rate = freq_bands_by_sample_rate(&all_freqs, sample_rate as u32);

            debug!("Available Bands: {:?}", bands_for_rate);
            
            let Some(target_freq) = first_freq_above_eq(&all_freqs, next_session_band as u16) else {
                return Err(
                    io::Error::new(
                        io::ErrorKind::InvalidData, 
                        format!("Failed to find first freq above {:?}", next_session_band)
                    )
                );
            };
            let Some((_, bands, _)) = bands_for_rate
                .iter()
                .map(|(k, v)| (k, v, v.iter().position(|&x| x == target_freq)))
                .filter(|(_, _, i)| i.is_some())
                .next() else {
                return Err(
                    io::Error::new(
                        io::ErrorKind::InvalidData, 
                        format!("Failed to find band in which target frequency belongs in: {:?}", target_freq)
                    )
                );
            };

            proc = match process::Command::new(self.bin.clone())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .arg("--system-table")
                .arg(self.systable.path.to_path_buf())
                .arg("--sample-rate")
                .arg(format!("{}", sample_rate))
                .arg("--output")
                .arg("decoded:json:file:path=-")
                .args(extra_args)
                .args(bands.iter().map(|x| x.to_string()).collect::<Vec<String>>())
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

            {
                let Some(value) = settings.props.get_mut(&PROP_LISTENING_BAND.to_string()) else {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_LISTENING_BAND prop")
                    );    
                };
                *value = json!(bands);
            }

            {
                let Some(value) = settings.props.get_mut(&PROP_NEXT_SESSION_BAND.to_string()) else {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "Missing PROP_NEXT_SESSION_BAND prop")
                    );    
                };
                *value = json!(0);
            }

            debug!("New HFDL session started, requested freq {}, listening: {:?}", next_session_band, bands);            
        }
        
        let Some(stdout) = proc.stdout.take() else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unable to take stdout from child process"));
        };
        let Some(stderr) = proc.stderr.take() else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unable to take stderr from child process"));
        };

        Ok(Box::new(DumpHFDLSession::new(
            proc,
            BufReader::new(stdout),
            stderr,
            true, // TODO: only false for method="static"
        )))
    }

    fn process_message(&mut self, msg: &str) -> Result<crate::common::frame::CommonFrame, io::Error> {
        let raw_frame = serde_json::from_str::<Frame>(msg)?;
        let mut frame_src: cff::Entity;
        let mut frame_dst: Option<cff::Entity> = None;

        let mut acars_content: Option<cff::ACARS> = None;

        let Some(arrival_time) = split_unix_time_to_utc_datetime(
            raw_frame.hfdl.ts.sec as i64, 
            (raw_frame.hfdl.ts.usec * 1000) as u32
        ) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid arrival time",
            ))
        };

        let mut paths: Vec<cff::PropagationPath> = Vec::new();
        let mut metadata: Option<cff::HFDLMetadata> = None;
        
        let mut indexed = cff::Indexed {
            timestamp: arrival_time.to_rfc3339_opts(SecondsFormat::Micros, true),
        };
        
        if let Some(ref spdu) = raw_frame.hfdl.spdu {
            frame_src = spdu.src.to_common_frame_entity(&self.systable);

            if spdu.systable_version > self.systable.version {
                warn!("System Table from SPDU is newer than provided! Provided version = {}, SPDU version = {}", self.systable.version, spdu.systable_version);
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Provided HFDL system table is out of date"));
            }

            metadata = Some(cff::HFDLMetadata {
                kind: String::from("Squitter"),
                heard_on: spdu.gs_status.iter().map(|x| cff::HFDLGSEntry {
                    kind: x.gs.entity_type.clone(),
                    id: x.gs.id,
                    gs: x.gs.name.clone().unwrap_or(String::from("")),
                    freqs: x.freqs.iter().map(|y| y.freq as f64 / 1000.0).collect(),
                }).collect(),
                reason: None,  
            });
        } else if let Some(ref lpdu) = raw_frame.hfdl.lpdu {
            frame_src = lpdu.src.to_common_frame_entity(&self.systable);
            frame_dst = Some(lpdu.dst.to_common_frame_entity(&self.systable));
            
            if let Some(ref hfnpdu) = lpdu.hfnpdu {
                if let Some(ref flight_id) = hfnpdu.flight_id {
                    frame_src.callsign = Some(flight_id.clone());
                }

                if let Some(ref pos) = hfnpdu.pos {
                    let pt = pos.as_wkt();
                    if pt.valid() {
                        frame_src.coords = Some(pt.clone());

                        if let Some(ref gs_pt) = frame_dst.as_ref().unwrap().coords {
                            paths.push(cff::PropagationPath {
                                freqs: vec![raw_frame.hfdl.freq_as_mhz()],
                                path: WKTPolyline {
                                    points: vec![pt.as_tuple(), gs_pt.as_tuple()],
                                },
                                party: frame_dst.clone().unwrap(),
                            });
                        }
                    }
                }

                if let Some(ref msg_time) = hfnpdu.time {
                    let Some(frame_time) = nearest_time_in_past(&arrival_time, msg_time.hour, msg_time.min, msg_time.sec) else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Failed to calculate frame timestamp",
                        ))
                    };
                    indexed.timestamp = frame_time.to_rfc3339_opts(SecondsFormat::Micros, true);
                }

                if let Some(ref acars) = hfnpdu.acars {
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

                    if lpdu.from_ground_station() {
                        match frame_dst {
                            Some(ref mut entity) => {
                                entity.tail = Some(normalize_tail(acars.reg.clone()))
                            }
                            _ => {},
                        }
                    } else {
                        frame_src.tail = Some(normalize_tail(acars.reg.clone()));
                    }
                }

                let mut reason: Option<String> = None;
                if let Some(ref last_change) = hfnpdu.last_freq_change_cause {
                    reason = Some(last_change.descr.clone());
                }

                let mut heard_on: Vec<HFDLGSEntry> = vec![];
                if let Some(ref freq_data) = hfnpdu.freq_data {
                    heard_on = freq_data.iter().map(|x| cff::HFDLGSEntry {
                        kind: x.gs.entity_type.clone(),
                        id: x.gs.id,
                        gs: x.gs.name.clone().unwrap_or(String::from("")),
                        freqs: x.heard_on_freqs.iter().map(|y| y.freq as f64 / 1000.0).collect(),
                    }).collect();

                    if frame_src.coords.is_some() && matches!(lpdu.dst.kind(), EntityType::GroundStation) {
                        let pt = frame_src.coords.as_ref().unwrap();
                        let dst = frame_dst.as_ref().unwrap();
                        for entry in heard_on.iter() {
                            if dst.id.is_some() && dst.id.unwrap() != entry.id && !entry.freqs.is_empty() {
                                let Some(gs) = self.systable.by_id(entry.id) else {
                                    continue
                                };
                                paths.push(cff::PropagationPath {
                                    freqs: entry.freqs.clone(),
                                    path: WKTPolyline { points: vec![pt.as_tuple(), (gs.position.1, gs.position.0, 0.0)] },
                                    party: cff::Entity {
                                        kind: String::from("Ground station"),
                                        id: Some(gs.id),
                                        gs: Some(gs.name.clone()),
                                        icao: None,
                                        callsign: None,
                                        tail: None,
                                        coords: Some(crate::common::wkt::WKTPoint { x: gs.position.1, y: gs.position.0, z: 0.0 }), 
                                    }, 
                                });
                            }
                        }
                    }
                }
                
                metadata = Some(cff::HFDLMetadata {
                    kind: hfnpdu.kind.name.clone(),
                    heard_on,
                    reason, 
                });
            } else {
                if let Some(ref ac_info) = lpdu.ac_info {
                    if lpdu.from_ground_station() {
                        match frame_dst {
                            Some(ref mut entity) => entity.icao = Some(ac_info.icao.clone()),
                            _ => {},
                        }
                    }                        
                }

                if let Some(ref ac_id) = lpdu.assigned_ac_id {
                    if lpdu.from_ground_station() {
                        match frame_dst {
                            Some(ref mut entity) => entity.id = Some(*ac_id),
                            _ => {},
                        }
                    }
                }

                let mut reason: Option<String> = None;
                if let Some(ref r) = lpdu.reason {
                    reason = Some(r.descr.clone());
                }
                metadata = Some(cff::HFDLMetadata { kind: lpdu.kind.name.clone(), heard_on: vec![], reason});
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HFDL frame missing LPDU/SPDU block",
            ));
        }

        Ok(cff::CommonFrame {
            timestamp: unix_time_to_utc_datetime(
                raw_frame.hfdl.ts.to_f64()
            ).unwrap_or(Utc::now().with_timezone(&UTC)).to_rfc3339_opts(SecondsFormat::Micros, true),
            freq: raw_frame.hfdl.freq_as_mhz(),
            signal: raw_frame.hfdl.sig_level as f32,

            err: false,

            paths,

            app: cff::AppInfo {
                name: raw_frame.hfdl.app.name,
                version: raw_frame.hfdl.app.version,
            },

            indexed,
            metadata: cff::Metadata {
                hfdl: metadata,  
            },
            
            src: frame_src,
            dst: frame_dst,
            acars: acars_content,
        })
    }
}
