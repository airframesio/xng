use crate::common::arguments::{extract_soapysdr_driver, parse_bin_path};
use crate::common::formats::EntityType;
use crate::common::frame::{self as cff, HFDLGSEntry, Indexed};
use crate::common::wkt::WKTPolyline;
use crate::common::{AIRFRAMESIO_DUMPHFDL_TCP_PORT, AIRFRAMESIO_HOST};
use crate::modules::hfdl::airframes::get_airframes_gs_status;
use crate::modules::hfdl::schedule::parse_session_schedule;
use crate::modules::hfdl::utils::{
    first_freq_above_eq, freq_bands_by_sample_rate, get_max_dist_khz_by_sample_rate,
};
use crate::modules::PROP_LISTENING_BAND;
use crate::server::db::StateDB;
use crate::utils::normalize_tail;
use crate::utils::timestamp::{
    nearest_time_in_past, split_unix_time_to_utc_datetime, unix_time_to_utc_datetime,
};

use self::frame::Frame;
use self::schedule::validate_session_schedule;
use self::session::DumpHFDLSession;
use self::systable::SystemTable;
use self::validators::{validate_next_session_band, validate_session_method};
use super::session::EndSessionReason;
use super::settings::{update_station_by_frequencies, ModuleSettings};
use super::XngModule;
use actix_web::web::Data;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Local, SecondsFormat, Utc};
use chrono_tz::UTC;
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use log::*;
use rand::Rng;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::ops::DerefMut;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{self, BufReader};
use tokio::process;
use tokio::sync::RwLock;

mod airframes;
mod frame;
mod module;
mod schedule;
mod session;
mod systable;
mod utils;
mod validators;

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumphfdl";
const DEFAULT_SYSTABLE_PATH: &'static str = "/etc/systable.conf";

const DEFAULT_STALE_TIMEOUT_SECS: u64 = 2700;
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 600;
const DEFAULT_SESSION_METHOD: &'static str = "random";

const HFDL_COMMAND: &'static str = "hfdl";

const PROP_STALE_TIMEOUT_SEC: &'static str = "stale_timeout_sec";
const PROP_USE_AIRFRAMES_GS: &'static str = "use_airframes_gs";
const PROP_SAMPLE_RATE: &'static str = "sample_rate";
const PROP_NEXT_SESSION_BAND: &'static str = "next_session_band";
const PROP_SESSION_SCHEDULE: &'static str = "session_schedule";
const PROP_SESSION_METHOD: &'static str = "session_method";
const PROP_ONLY_USE_ACTIVE: &'static str = "only_use_active";

#[derive(Default)]
pub struct HfdlModule {
    name: &'static str,
    settings: Option<Data<RwLock<ModuleSettings>>>,

    sample_rates: Vec<u64>,

    bin: PathBuf,
    systable: SystemTable,

    args: Vec<String>,
    driver: String,

    feed_airframes: bool,

    sample_rate: u64,
    stale_timeout_secs: u64,
    use_airframes_gs: bool,
    only_use_active: bool,
    next_session_band: u64,
    schedule: String,
    method: String,

    last_req_session_band: u64,
    last_random_freq_band: u64,
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
        Command::new(HFDL_COMMAND)
            .about("Listen to HFDL messages using dumphfdl")
            .args(&[
                arg!(--bin <FILE> "Path to dumphfdl binary"),
                arg!(--systable <FILE> "Path to dumphfdl system table configuration"),
                arg!(--"stale-timeout" <SECONDS> "Elapsed time since last update before an aircraft and ground station frequency data is considered stale"),
                arg!(--"sample-rate" <HERTZ> "Initial sample rate to use for splitting HFDL spectrum into bands of coverage"),
                arg!(--"use-airframes-gs-map" "Use airframes.io's live HFDL ground station frequency map"),
                arg!(--"only-listen-on-active" "Only listen on active HFDL frequencies (NOTE: use --use-airframes-gs-map to avoid rapid initial session ends on new SPDUs)"),
                arg!(--"start-band-contains" <HERTZ> "Initial starting band to listen on. Overrides --schedule if both are configured"),
                arg!(--schedule <SCHEDULE_FMT> "Session switch schedule in the format of: time=<HOUR_0_TO_23>,band_contains=<FREQ_HZ>;..."),
                arg!(--method <METHOD_TYPE> "Session switching methods to use. Default method is random. Valid methods: random, inc, dec, static, track:GS_ID")
            ])
            .arg(Arg::new("hfdl-args").action(ArgAction::Append))
    }

    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error> {
        self.feed_airframes = args.get_flag("feed-airframes");

        let bin_path = parse_bin_path(args, DEFAULT_BIN_PATH);
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Missing required HFDL positional arguments",
            ));
        };
        self.args = hfdl_args
            .clone()
            .map(|x: &String| x.to_string())
            .collect::<Vec<String>>();

        let Some(driver) = extract_soapysdr_driver(&self.args) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Missing --soapysdr argument with driver= specification",
            ));
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

        if let Err(e) = self.load_sample_rates(&driver) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Unable to obtain sample rates for SoapySDR device {}: {}",
                    self.driver,
                    e.to_string()
                ),
            ));
        }

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
        self.only_use_active = args.get_flag("only-listen-on-active");

        let schedule = args
            .get_one::<String>("schedule")
            .map(|x| x.clone())
            .unwrap_or(String::from(""));
        if !schedule.is_empty() {
            match validate_session_schedule(&json!(schedule)) {
                Ok(_) => {}
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid schedule format: {}", e.to_string()),
                    ))
                }
            };
        }
        self.schedule = schedule;

        self.next_session_band = args
            .get_one::<String>("start-band-contains")
            .unwrap_or(&String::from("default"))
            .parse::<u16>()
            .unwrap_or(0) as u64;

        let method = args
            .get_one::<String>("method")
            .unwrap_or(&String::from(DEFAULT_SESSION_METHOD))
            .clone();
        if let Err(e) = validate_session_method(&json!(method)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid session method, {}: {}", method, e.to_string()),
            ));
        }
        self.method = method.to_lowercase();

        Ok(())
    }

    async fn init(
        &mut self,
        settings: Data<RwLock<ModuleSettings>>,
        state_db: Data<RwLock<StateDB>>,
    ) {
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
            PROP_ONLY_USE_ACTIVE.to_string(),
            json!(self.only_use_active),
        );
        settings
            .props
            .insert(PROP_SAMPLE_RATE.to_string(), json!(self.sample_rate));
        settings.add_prop_with_validator(
            PROP_NEXT_SESSION_BAND.to_string(),
            json!(self.next_session_band),
            validate_next_session_band,
        );
        settings.add_prop_with_validator(
            PROP_SESSION_SCHEDULE.to_string(),
            json!(self.schedule),
            validate_session_schedule,
        );
        settings.add_prop_with_validator(
            PROP_SESSION_METHOD.to_string(),
            json!(self.method),
            validate_session_method,
        );

        {
            let state_db = state_db.write().await;
            for gs in self.systable.stations.iter() {
                if let Err(e) = state_db
                    .create_ground_station(gs.id as u32, &gs.name, gs.position.0, gs.position.1)
                    .await
                {
                    warn!(
                        "Failed to populate initial ground stations, id={} name={}: {}",
                        gs.id,
                        gs.name,
                        e.to_string()
                    );
                }
            }
        }
    }

    // NOTE: not gonna lie, this code below is pretty gnarly and could use some refactoring...
    async fn start_session(
        &mut self,
        last_end_reason: EndSessionReason,
    ) -> Result<Box<dyn super::session::Session>, io::Error> {
        let settings = self.get_settings()?;
        let mut next_session_begin: Option<DateTime<Local>> = None;

        let mut end_session_on_timeout = true;

        let mut extra_args = self.args.clone();
        let output_arg = format!(
            "decoded:json:tcp:address={},port={}",
            AIRFRAMESIO_HOST, AIRFRAMESIO_DUMPHFDL_TCP_PORT
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

        let listening_bands: Vec<u16>;

        let mut proc;
        {
            let mut settings = settings.write().await;

            let use_airframes_gs;
            {
                let value = settings
                    .props
                    .get(&PROP_USE_AIRFRAMES_GS.to_string())
                    .unwrap_or(&json!(false));
                use_airframes_gs = value.as_bool().unwrap_or(false);
            }

            let only_use_active;
            {
                let value = settings
                    .props
                    .get(&PROP_ONLY_USE_ACTIVE.to_string())
                    .unwrap_or(&json!(false));
                only_use_active = value.as_bool().unwrap_or(false);
            }

            let session_method;
            {
                let Some(value) = settings.props.get(&PROP_SESSION_METHOD.to_string()) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Missing PROP_SESSION_METHOD prop",
                    ));
                };
                session_method = value.as_str().unwrap_or(DEFAULT_SESSION_METHOD).to_string();
            }
            if session_method == "static" {
                end_session_on_timeout = false;
            }

            let sample_rate;
            {
                let Some(suggested_sample_rate) = settings.props.get(&PROP_SAMPLE_RATE.to_string())
                else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Missing PROP_SAMPLE_RATE prop",
                    ));
                };
                match self.nearest_sample_rate(suggested_sample_rate.as_u64().unwrap_or(0)) {
                    Some(rate) => sample_rate = rate,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "Failed to find nearest sample rate to {:?}",
                                suggested_sample_rate
                            ),
                        ));
                    }
                };
            }

            let stale_timeout_sec;
            {
                let Some(value) = settings.props.get(&PROP_STALE_TIMEOUT_SEC.to_string()) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Missing PROP_STALE_TIMEOUT_SEC prop",
                    ));
                };
                match value.as_u64() {
                    Some(x) => stale_timeout_sec = x,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Stale timeout is not a number",
                        ));
                    }
                }
            }

            let mut next_session_band;
            {
                let Some(value) = settings.props.get(&PROP_NEXT_SESSION_BAND.to_string()) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Missing PROP_NEXT_SESSION_BAND prop",
                    ));
                };
                next_session_band = value.as_u64().unwrap_or(0);
            }

            // NOTE: always respect user requests to change frequency bands
            if next_session_band == 0 && matches!(last_end_reason, EndSessionReason::SessionUpdate)
            {
                next_session_band = self.last_req_session_band;
            }

            if !self.schedule.is_empty() {
                let schedule = match parse_session_schedule(&self.schedule) {
                    Ok(x) => x,
                    Err(e) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Failed to parse schedule: {}", e.to_string()),
                        ))
                    }
                };

                debug!("Parsed schedule: {:?}", schedule);

                if let Some((dt, _)) = schedule.first() {
                    next_session_begin = Some(*dt);

                    if let Some((_, target_freq)) = schedule.last() {
                        match (last_end_reason, next_session_band) {
                            (EndSessionReason::SessionTimeout, 0) => info!("Scheduled session band timed out, trying others until next scheduled switch"),
                            (_, 0) | (EndSessionReason::SessionEnd, _) => next_session_band = *target_freq as u64,
                            _ => {}
                        }
                    }
                }
            }

            let mut all_freqs: Vec<u16> = Vec::new();

            if only_use_active {
                all_freqs.extend(
                    settings
                        .stations
                        .iter_mut()
                        .flat_map(|x| {
                            x.invalidate(Duration::seconds(stale_timeout_sec as i64));
                            x.active_frequencies.iter().map(|y| y.khz as u16)
                        })
                        .collect::<Vec<u16>>(),
                );
            }

            // NOTE: avoid doubly updating the stations since SessionUpdate would've just updated
            if use_airframes_gs && !matches!(last_end_reason, EndSessionReason::SessionUpdate) {
                match get_airframes_gs_status().await {
                    Ok(gs_status) => {
                        trace!("Populating stations with Airframes HFDL map data");

                        for station in gs_status.ground_stations.iter() {
                            let station_freq_set: Vec<u64> = station
                                .frequencies
                                .active
                                .iter()
                                .map(|&x| x as u64)
                                .collect();

                            if let Some(change_event) = update_station_by_frequencies(
                                settings.deref_mut(),
                                None,
                                stale_timeout_sec as i64,
                                json!(station.id),
                                Some(station.name.clone()),
                                &station_freq_set,
                            ) {
                                trace!(
                                    "Ground station ID {} [{}] changed frequency set: {} -> {}",
                                    change_event.pretty_id(),
                                    change_event.pretty_name(),
                                    change_event.old,
                                    change_event.new,
                                );

                                if let Err(e) = settings.change_event_tx.send(change_event).await {
                                    warn!(
                                        "Failed to send ground station change event: {}",
                                        e.to_string()
                                    );
                                }
                            }
                        }

                        all_freqs.extend(gs_status.all_freqs());
                    }
                    Err(e) => warn!("Failed to get Airframes HFDL map: {}", e.to_string()),
                }
            }

            if all_freqs.is_empty() {
                all_freqs.extend_from_slice(&self.systable.all_freqs());
            }
            all_freqs.sort_unstable();
            all_freqs.dedup();

            let bands_for_rate = freq_bands_by_sample_rate(&all_freqs, sample_rate as u32);

            debug!("Available Bands: {:?}", bands_for_rate);

            if next_session_band == 0 {
                let mut candidates = bands_for_rate
                    .iter()
                    .map(|(_, v)| v.first())
                    .filter(|x| x.is_some())
                    .map(|x| x.unwrap())
                    .collect::<Vec<&u16>>();
                candidates.sort_unstable();

                let listening_band = settings
                    .props
                    .get(&PROP_LISTENING_BAND.to_string())
                    .unwrap_or(&Value::Null);
                let mut last_listening_freq: Option<u64> = None;

                if !candidates.is_empty() && listening_band.is_array() && session_method != "static"
                {
                    let last_band = listening_band
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_u64())
                        .filter(|x| x.is_some())
                        .map(|x| x.unwrap())
                        .collect::<Vec<u64>>();
                    if let Some(first_freq) = last_band.first() {
                        last_listening_freq = Some(*first_freq as u64);

                        if let Some(last_idx) =
                            candidates.iter().position(|&x| *x as u64 >= *first_freq)
                        {
                            let max_idx = candidates.len() - 1;
                            match session_method.as_str() {
                                "inc" => {
                                    if last_idx + 1 > max_idx {
                                        next_session_band = *candidates[0] as u64;
                                    } else {
                                        next_session_band = *candidates[last_idx + 1] as u64;
                                    }
                                }
                                "dec" => {
                                    if last_idx == 0 {
                                        next_session_band = *candidates[max_idx] as u64;
                                    } else {
                                        next_session_band = *candidates[last_idx - 1] as u64;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }

                if next_session_band == 0 && session_method == "random" {
                    let mut rng = rand::thread_rng();
                    if let Some(first_freq) = last_listening_freq {
                        candidates = candidates
                            .into_iter()
                            .filter(|&x| {
                                *x as u64 != first_freq && *x as u64 != self.last_random_freq_band
                            })
                            .collect();
                        self.last_random_freq_band = first_freq;
                    }

                    if !candidates.is_empty() {
                        let new_idx = rng.gen_range(0..(candidates.len() - 1));
                        next_session_band = *candidates[new_idx] as u64;
                    } else {
                        warn!("Candidate bands pool is empty!");
                    }
                }

                if next_session_band == 0
                    && listening_band.is_array()
                    && session_method.starts_with("track:")
                {
                    let last_band = listening_band
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_u64())
                        .filter(|x| x.is_some())
                        .map(|x| x.unwrap())
                        .collect::<Vec<u64>>();

                    if let Ok(gs_id) = &session_method[6..].parse::<u32>() {
                        if let Some(station) =
                            settings.stations.iter().find(|&x| x.id == json!(gs_id))
                        {
                            let mut rng = rand::thread_rng();
                            let new_idx = rng.gen_range(0..(station.active_frequencies.len() - 1));
                            next_session_band = station
                                .active_frequencies
                                .iter()
                                .filter(|x| !last_band.iter().any(|y| *y == x.khz))
                                .map(|x| x.khz)
                                .collect::<Vec<u64>>()[new_idx];
                        } else {
                            warn!(
                                "Invalid target ground station ID ({} is not within range)",
                                gs_id
                            );
                        }
                    } else {
                        warn!("Malformed target ground station ID: expecting positive integer");
                    }
                }
            }

            let Some(target_freq) = first_freq_above_eq(&all_freqs, next_session_band as u16)
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to find first freq above {:?}", next_session_band),
                ));
            };
            let Some((_, bands, _)) = bands_for_rate
                .iter()
                .map(|(k, v)| (k, v, v.iter().position(|&x| x == target_freq)))
                .filter(|(_, _, i)| i.is_some())
                .next()
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Failed to find band in which target frequency belongs in: {:?}",
                        target_freq
                    ),
                ));
            };

            let used_sample_rate = self
                .calculate_actual_sample_rate(bands)
                .unwrap_or(sample_rate);
            debug!("Using sample rate of {} for listening", used_sample_rate);

            self.last_req_session_band = next_session_band;

            proc = match process::Command::new(self.bin.clone())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .arg("--system-table")
                .arg(self.systable.path.to_path_buf())
                .arg("--sample-rate")
                .arg(format!("{}", used_sample_rate))
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
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Missing PROP_LISTENING_BAND prop",
                    ));
                };
                *value = json!(bands);
            }

            listening_bands = bands.clone();

            {
                let Some(value) = settings.props.get_mut(&PROP_NEXT_SESSION_BAND.to_string())
                else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Missing PROP_NEXT_SESSION_BAND prop",
                    ));
                };
                *value = json!(0);
            }

            debug!(
                "New HFDL session started, requested freq {}, listening: {:?}",
                next_session_band, bands
            );
        }

        let Some(stdout) = proc.stdout.take() else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Unable to take stdout from child process",
            ));
        };
        let Some(stderr) = proc.stderr.take() else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Unable to take stderr from child process",
            ));
        };

        Ok(Box::new(DumpHFDLSession::new(
            proc,
            BufReader::new(stdout),
            stderr,
            listening_bands,
            next_session_begin,
            end_session_on_timeout,
        )))
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
            raw_frame.hfdl.ts.sec as i64,
            (raw_frame.hfdl.ts.usec * 1000) as u32,
        ) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid arrival time",
            ));
        };

        let mut paths: Vec<cff::PropagationPath> = Vec::new();
        let metadata: Option<cff::HFDLMetadata>;

        let mut indexed = cff::Indexed {
            timestamp: arrival_time.to_rfc3339_opts(SecondsFormat::Micros, true),

            ..Default::default()
        };
        let mut has_err = false;

        if let Some(ref spdu) = raw_frame.hfdl.spdu {
            frame_src = spdu.src.to_common_frame_entity(&self.systable);

            if spdu.systable_version > self.systable.version {
                warn!("System Table from SPDU is newer than provided! Provided version = {}, SPDU version = {}", self.systable.version, spdu.systable_version);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Provided HFDL system table is out of date",
                ));
            }

            {
                let settings = self.get_settings()?;
                let mut settings = settings.write().await;

                let use_airframes_gs;
                {
                    let value = settings
                        .props
                        .get(&PROP_USE_AIRFRAMES_GS.to_string())
                        .unwrap_or(&json!(false));
                    use_airframes_gs = value.as_bool().unwrap_or(false);
                }

                let only_use_active;
                {
                    let value = settings
                        .props
                        .get(&PROP_ONLY_USE_ACTIVE.to_string())
                        .unwrap_or(&json!(false));
                    only_use_active = value.as_bool().unwrap_or(false);
                }

                let sample_rate;
                {
                    let value = settings
                        .props
                        .get(&PROP_SAMPLE_RATE.to_string())
                        .unwrap_or(&json!(false));
                    sample_rate = value.as_u64().unwrap_or(0);
                }

                let stale_timeout_sec;
                {
                    let Some(value) = settings.props.get(&PROP_STALE_TIMEOUT_SEC.to_string())
                    else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Missing PROP_STALE_TIMEOUT_SEC prop",
                        ));
                    };
                    match value.as_u64() {
                        Some(x) => stale_timeout_sec = x,
                        None => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Stale timeout is not a number",
                            ));
                        }
                    }
                }

                let mut changed = false;

                for station in spdu.gs_status.iter() {
                    let freq_set: Vec<u64> = station.freqs.iter().map(|x| x.freq as u64).collect();
                    if let Some(change_event) = update_station_by_frequencies(
                        settings.deref_mut(),
                        Some(arrival_time.to_rfc3339_opts(SecondsFormat::Micros, true)),
                        stale_timeout_sec as i64,
                        json!(station.gs.id),
                        station.gs.name.clone(),
                        &freq_set,
                    ) {
                        debug!(
                            "Ground station ID {} [{}] changed frequency set: {} -> {}",
                            change_event.pretty_id(),
                            change_event.pretty_name(),
                            change_event.old,
                            change_event.new,
                        );

                        if let Err(e) = settings.change_event_tx.send(change_event.clone()).await {
                            warn!(
                                "Failed to send ground station change event: {}",
                                e.to_string()
                            );
                        }

                        match (
                            serde_json::from_str::<Vec<u16>>(change_event.old.as_str()),
                            serde_json::from_str::<Vec<u16>>(change_event.new.as_str()),
                        ) {
                            (Ok(old_band), Ok(new_band)) => {
                                let old_band_set: HashSet<&u16> =
                                    HashSet::from_iter(old_band.iter());
                                let new_band_set: HashSet<&u16> =
                                    HashSet::from_iter(new_band.iter());

                                let added_or_removed: HashSet<_> =
                                    old_band_set.symmetric_difference(&new_band_set).collect();
                                let max_dist_khz =
                                    get_max_dist_khz_by_sample_rate(sample_rate as u32) as u64;

                                trace!(
                                    "  changed_freqs = {:?}; max_dist_khz = {}",
                                    added_or_removed,
                                    max_dist_khz
                                );

                                if added_or_removed.iter().any(|&&x| {
                                    let Some(first_freq) = current_band.first() else {
                                        debug!(
                                            "Current band does not have a first freq: {:?}",
                                            current_band
                                        );
                                        return true;
                                    };
                                    let Some(last_freq) = current_band.last() else {
                                        debug!(
                                            "Current band does not have a last freq: {:?}",
                                            current_band
                                        );
                                        return true;
                                    };

                                    if ((*x as u64) >= *first_freq && (*x as u64) <= *last_freq)
                                        || ((*x as u64) < *first_freq
                                            && (*last_freq - (*x as u64)) <= max_dist_khz)
                                        || ((*x as u64) > *last_freq
                                            && ((*x as u64) - *last_freq) <= max_dist_khz)
                                    {
                                        return true;
                                    }

                                    false
                                }) {
                                    changed = true;
                                }
                            }
                            (Err(e), _) => {
                                debug!(
                                    "old_band is not valid JSON: {:?} ; error = {}",
                                    change_event.old,
                                    e.to_string()
                                );
                                changed = true;
                            }
                            (_, Err(e)) => {
                                debug!(
                                    "new_band is not valid JSON: {:?} ; error = {}",
                                    change_event.new,
                                    e.to_string()
                                );
                                changed = true;
                            }
                        }
                    }
                }

                if (only_use_active || use_airframes_gs) && changed {
                    // TODO: consider forcing session end if target track GS_ID changes

                    if let Err(e) = settings
                        .end_session_signaler
                        .send(EndSessionReason::SessionUpdate)
                    {
                        warn!("Failed to signal end session after : {}", e.to_string());
                    } else {
                        debug!("Latest SPDU changed frequencies, reloading session to make sure only active frequencies are listened to");
                    }
                }
            }

            metadata = Some(cff::HFDLMetadata {
                kind: String::from("Squitter"),
                heard_on: spdu
                    .gs_status
                    .iter()
                    .map(|x| cff::HFDLGSEntry {
                        kind: x.gs.entity_type.clone(),
                        id: x.gs.id,
                        gs: x.gs.name.clone().unwrap_or(String::from("")),
                        freqs: x.freqs.iter().map(|y| y.freq as f64 / 1000.0).collect(),
                    })
                    .collect(),
                reason: None,
            });
        } else if let Some(ref lpdu) = raw_frame.hfdl.lpdu {
            frame_src = lpdu.src.to_common_frame_entity(&self.systable);
            frame_dst = Some(lpdu.dst.to_common_frame_entity(&self.systable));

            if let Some(ref ac_info) = lpdu.ac_info {
                match (lpdu.from_ground_station(), &mut frame_src, &mut frame_dst) {
                    (true, _, Some(ref mut dst)) => dst.icao = Some(ac_info.icao.clone()),
                    (false, src, _) => src.icao = Some(ac_info.icao.clone()),
                    (_, _, _) => {}
                }
            }

            if let Some(ref hfnpdu) = lpdu.hfnpdu {
                if let Some(ref flight_id) = hfnpdu.flight_id {
                    frame_src.callsign = Some(flight_id.trim().to_string());
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
                    let Some(frame_time) = nearest_time_in_past(
                        &arrival_time,
                        msg_time.hour,
                        msg_time.min,
                        msg_time.sec,
                    ) else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Failed to calculate frame timestamp",
                        ));
                    };
                    indexed.timestamp = frame_time.to_rfc3339_opts(SecondsFormat::Micros, true);
                }

                if let Some(ref acars) = hfnpdu.acars {
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

                    if lpdu.from_ground_station() {
                        match frame_dst {
                            Some(ref mut entity) => {
                                entity.tail = Some(normalize_tail(&acars.reg));
                            }
                            _ => {}
                        }
                    } else {
                        frame_src.tail = Some(normalize_tail(&acars.reg));
                    }
                }

                let mut reason: Option<String> = None;
                if let Some(ref last_change) = hfnpdu.last_freq_change_cause {
                    reason = Some(last_change.descr.clone());
                }

                let mut heard_on: Vec<HFDLGSEntry> = vec![];
                if let Some(ref freq_data) = hfnpdu.freq_data {
                    heard_on = freq_data
                        .iter()
                        .map(|x| cff::HFDLGSEntry {
                            kind: x.gs.entity_type.clone(),
                            id: x.gs.id,
                            gs: x.gs.name.clone().unwrap_or(String::from("")),
                            freqs: x
                                .heard_on_freqs
                                .iter()
                                .map(|y| y.freq as f64 / 1000.0)
                                .collect(),
                        })
                        .collect();

                    if frame_src.coords.is_some()
                        && matches!(lpdu.dst.kind(), EntityType::GroundStation)
                    {
                        let pt = frame_src.coords.as_ref().unwrap();
                        let dst = frame_dst.as_ref().unwrap();
                        for entry in heard_on.iter() {
                            if dst.id.is_some()
                                && dst.id.unwrap() != (entry.id as u32)
                                && !entry.freqs.is_empty()
                            {
                                let Some(gs) = self.systable.by_id(entry.id) else {
                                    continue;
                                };
                                paths.push(cff::PropagationPath {
                                    freqs: entry.freqs.clone(),
                                    path: WKTPolyline {
                                        points: vec![
                                            pt.as_tuple(),
                                            (gs.position.1, gs.position.0, 0.0),
                                        ],
                                    },
                                    party: cff::Entity {
                                        kind: String::from("Ground station"),
                                        id: Some(gs.id.into()),
                                        gs: Some(gs.name.clone()),
                                        icao: None,
                                        callsign: None,
                                        tail: None,
                                        coords: Some(crate::common::wkt::WKTPoint {
                                            x: gs.position.1,
                                            y: gs.position.0,
                                            z: 0.0,
                                        }),
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
                if let Some(ref ac_id) = lpdu.assigned_ac_id {
                    if lpdu.from_ground_station() {
                        match frame_dst {
                            Some(ref mut entity) => entity.id = Some((*ac_id).into()),
                            _ => {}
                        }
                    }
                }

                let mut reason: Option<String> = None;
                if let Some(ref r) = lpdu.reason {
                    reason = Some(r.descr.clone());
                }
                metadata = Some(cff::HFDLMetadata {
                    kind: lpdu.kind.name.clone(),
                    heard_on: vec![],
                    reason,
                });
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HFDL frame missing LPDU/SPDU block",
            ));
        }

        Ok(cff::CommonFrame {
            timestamp: unix_time_to_utc_datetime(raw_frame.hfdl.ts.to_f64())
                .unwrap_or(Utc::now().with_timezone(&UTC))
                .to_rfc3339_opts(SecondsFormat::Nanos, true),
            freq: raw_frame.hfdl.freq_as_mhz(),
            signal: raw_frame.hfdl.sig_level as f32,

            err: has_err,

            paths,

            app: cff::AppInfo {
                name: raw_frame.hfdl.app.name,
                version: raw_frame.hfdl.app.version,
            },

            indexed,
            metadata: cff::Metadata { hfdl: metadata },

            src: frame_src,
            dst: frame_dst,
            acars: acars_content,
        })
    }

    async fn reload(&mut self) -> Result<(), io::Error> {
        let settings = self.get_settings()?;
        {
            let settings = settings.read().await;

            let Some(schedule) = settings.props.get(PROP_SESSION_SCHEDULE) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Session schedule prop value does not exist"),
                ));
            };

            if let Some(schedule) = schedule.as_str() {
                // NOTE: schedule is already validated on being set
                self.schedule = schedule.to_string();
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Session schedule prop value is not a string"),
                ));
            }
        }

        Ok(())
    }
}
