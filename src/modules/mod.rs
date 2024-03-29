use actix_web::web::Data;
use actix_web::{HttpServer, App, middleware};
use async_trait::async_trait;
use clap::{ArgMatches, Command, arg};
use ::elasticsearch::Elasticsearch;
use log::*;
use reqwest::Url;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::select;
use tokio::sync::{RwLock, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, sleep, Instant};
use tokio::io;
use tokio_util::sync::CancellationToken;
use std::collections::HashMap;
use std::process::exit;
use std::time::Duration;

use crate::common;
use crate::common::arguments::{parse_api_token, parse_disable_cross_site, parse_listen_host, parse_listen_port, parse_elastic_url, parse_state_db_url, parse_disable_state_db, parse_elastic_index};
use crate::common::batcher::create_es_batch_task;
use crate::common::es_utils::create_es_client;
use crate::common::events::GroundStationChangeEvent;
use crate::common::frame::CommonFrame;
use crate::modules::session::{EndSessionReason, SESSION_SCHEDULED_END};
use crate::modules::validators::validate_listening_bands;
use crate::server::db::StateDB;
use crate::server::services as server_services;

use self::session::Session;
use self::settings::ModuleSettings;

mod aoa;
mod hfdl;
mod services;
mod session;
mod validators;

pub mod elasticsearch;
pub mod settings;

const DEFAULT_INITIAL_SWARM_CONNECT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_SESSION_INTERMISSION_SECS: u64 = 0;
const DEFAULT_FAILED_SESSION_START_WAIT_SECS: u64 = 60;
const DEFAULT_BATCH_WAIT_MS: u64 = 200;
const DEFAULT_STATE_DB_URL: &'static str = "sqlite://state.sqlite3";
const DEFAULT_LISTEN_HOST: &'static str = "127.0.0.1";
const DEFAULT_LISTEN_PORT: u16 = 7871;

const DEFAULT_CHANNEL_BUFFER: usize = 2048;

const PROP_SESSION_TIMEOUT_SEC: &'static str = "session_timeout_sec";
const PROP_SESSION_INTERMISSION_SEC: &'static str = "session_intermission_sec";
const PROP_LISTENING_BAND: &'static str = "listening_band";

#[async_trait]
pub trait XngModule {
    fn id(&self) -> &'static str;

    fn default_session_timeout_secs(&self) -> u64;
    
    fn get_arguments(&self) -> Command;
    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error>;

    async fn init(&mut self, settings: Data<RwLock<ModuleSettings>>, state_db: Data<RwLock<StateDB>>);

    async fn process_message(&mut self, current_band: &Vec<u64>, msg: &str) -> Result<CommonFrame, io::Error>;
    async fn start_session(&mut self, last_end_reason: EndSessionReason) -> Result<Box<dyn Session>, io::Error>;

    async fn reload(&mut self) -> Result<(), io::Error>;
}

pub struct ModuleManager {
    modules: HashMap<&'static str, Box<dyn XngModule>>,
}

impl ModuleManager {
    pub fn init() -> ModuleManager {
        ModuleManager {
            modules: HashMap::from_iter(
                [aoa::AoaModule::new(), hfdl::HfdlModule::new()]
                    .map(|m| (m.id(), m))
                    .into_iter()
                    .collect::<Vec<(&'static str, Box<dyn XngModule>)>>(),
            ),
        }
    }

    pub fn register_arguments(&self, cmd: Command) -> Command {
        cmd.subcommands(
            self.modules
                .values()
                .map(|m| 
                    common::arguments::register_common_arguments(m.get_arguments())
                        .args(&[
                            arg!(--"disable-api-control" "Disable controlling of session from API server"),
                            arg!(--swarm <URL> "xng server instance to connect to (local API server will be disabled)"),
                            arg!(--"feed-airframes" "Feed JSON frames to airframes.io"),
                            arg!(--"session-timeout" <SECONDS> "Elapsed time since last frame before a session is considered stale and requires switching"),
                            arg!(--"session-intermission" <SECONDS> "Time to wait between sessions"),
                            arg!(--"disable-print-frame" "Disable printing JSON frames to STDOUT"), 
                        ])
                )
                .collect::<Vec<Command>>(),
        )
    }

    pub async fn start(&mut self, cmd: &str, args: &ArgMatches) {
        let Some(module) = self.modules.get_mut(cmd) else {
            error!("Invalid module '{}', please choose a valid module.", cmd);
            exit(exitcode::CONFIG);   
        };

        let api_token = parse_api_token(args);
        let disable_cross_site = parse_disable_cross_site(args);
        let listen_host = parse_listen_host(args, DEFAULT_LISTEN_HOST);
        let listen_port = parse_listen_port(args, DEFAULT_LISTEN_PORT);
        
        let disable_api_control = args.get_flag("disable-api-control");
        let disable_print_frame = args.get_flag("disable-print-frame");
        
        let mut session_intermission_secs = args
            .get_one::<String>("session-intermission")
            .unwrap_or(&String::from("default"))
            .parse::<u64>()
            .unwrap_or(DEFAULT_SESSION_INTERMISSION_SECS);
        let mut session_timeout_secs = args
            .get_one::<String>("session-timeout")
            .unwrap_or(&String::from("default"))
            .parse::<u64>()
            .unwrap_or(module.default_session_timeout_secs());

        let swarm_url: Option<Url> = if let Some(raw_url) = args.get_one::<String>("swarm") {
            match Url::parse(raw_url) {
                Ok(v) => {
                    info!("Swarm enabled: aggregator = {}", raw_url);
                    Some(v)
                },
                Err(e) => {
                    error!("Swarm URL is invalid: {}", e.to_string());
                    return;
                }
            }
        } else {
            None
        };
        let mut elastic_url = if let Some(raw_url) = parse_elastic_url(args) {
            match Url::parse(raw_url) {
                Ok(v) => {
                    info!("Elasticsearch bulk indexing enabled: target = {}", raw_url);
                    Some(v)
                }
                Err(e) => {
                    error!("Elastisearch URL is invalid: {}", e.to_string());
                    return;
                }
            }
        } else {
            None
        };
        let elastic_index = parse_elastic_index(args);
        let validate_es_cert = args.get_flag("validate-es-cert");
        
        let state_db_url = match Url::parse(parse_state_db_url(args, DEFAULT_STATE_DB_URL).as_str()) {
            Ok(v) => {
                info!("State DB location at {}", v.as_str());
                v
            },
            Err(e) => {
                error!("Invalid state DB URL: {}", e.to_string());
                return;
            }   
        };
                
        if swarm_url.is_some() && elastic_url.is_some() {
            error!("Swarm mode and importing to Elasticsearch are mutually exclusive options");
            error!("Please choose either swarm mode or importing to Elasticsearch.");
            return;    
        }

        let disable_state_db = parse_disable_state_db(args);
        
        let (reload_signaler, mut reload_signal) = mpsc::unbounded_channel::<()>();
        let (end_session_signaler, mut end_session_signal) = mpsc::unbounded_channel::<EndSessionReason>();
        let (change_event_tx, mut change_event_rx) = mpsc::channel::<GroundStationChangeEvent>(DEFAULT_CHANNEL_BUFFER);
        
        let Ok(mut interrupt_signal) = signal(SignalKind::interrupt()) else {
            error!("Failed to register interrupt signal");
            return;
        };

        if let Err(e) = module.parse_arguments(args) {
            error!("Failed to parse arguments: {}", e.to_string());
            return;    
        }

        if disable_state_db {
            debug!("State DB disabled");
        }
        
        let state_db = match StateDB::new(
            if disable_state_db { 
                None 
            } else { 
                Some(state_db_url.to_string()) 
            }
        ).await {
            Ok(v) => Data::new(RwLock::new(v)),
            Err(e) => {
                error!("Failed to create state DB: {}", e.to_string());
                return;
            }
        };
             
        let module_settings = Data::new(
            RwLock::new(
                ModuleSettings::new(
                    reload_signaler,
                    end_session_signaler,
                    change_event_tx,
                    swarm_url.is_some(),
                    disable_api_control,
                    api_token,
                    vec![
                        (PROP_SESSION_TIMEOUT_SEC, json!(session_timeout_secs)),
                        (PROP_SESSION_INTERMISSION_SEC, json!(session_intermission_secs))
                    ]    
                )
            )
        );
        module.init(module_settings.clone(), state_db.clone()).await;

        {
            let mut settings = module_settings.write().await;        
            settings.add_prop_with_validator(
                PROP_LISTENING_BAND.to_string(), 
                json!(Vec::new() as Vec<u64>), 
                validate_listening_bands
            );
        }
        
        let cancel_token = CancellationToken::new();
        let http_cancel_token = cancel_token.clone();
        let http_state_db = state_db.clone();
        let http_module_settings = module_settings.clone();
        
        let http_thread = tokio::spawn(async move {
            let restricted_origin = format!("http://{}:{}", listen_host, listen_port);
            
            let server = HttpServer::new(move || {
                App::new()
                    .app_data(http_state_db.clone())
                    .app_data(http_module_settings.clone())
                    .wrap(middleware::DefaultHeaders::new().add(
                        (
                            "Access-Control-Allow-Origin", 
                            if disable_cross_site {
                                restricted_origin.clone()
                            } else {
                                "*".to_string()
                            }
                        )
                    ))
                    .configure(services::config)
                    .configure(server_services::config)
            })
                .bind((listen_host.clone(), listen_port))
                .unwrap()
                .run();

            info!("HTTP thread started and listening on http://{}:{}", listen_host, listen_port);
            
            select! {
                _ = server => {},
                _ = http_cancel_token.cancelled() => {
                    info!("HTTP thread got cancel request");
                    return;
                }
            }
        });

        let (tx, mut rx) = mpsc::channel::<CommonFrame>(DEFAULT_CHANNEL_BUFFER);
        
        let processor_cancel_token = cancel_token.clone();

        let processor_thread = tokio::spawn(async move {
            let frames_batch: Data<Mutex<Vec<CommonFrame>>> = Data::new(Mutex::new(Vec::new()));
            let mut batcher: Option<JoinHandle<()>> = None;

            let mut swarm_target: Option<String> = None;
            let mut swarm_stream: Option<TcpStream> = None;
            
            if let Some(ref url) = swarm_url {
                swarm_target = Some(format!(
                    "{}:{}",
                    url.host_str().unwrap_or("0.0.0.0"),
                    url.port().unwrap_or(0)
                ));
            }

            if let Some(ref target) = swarm_target {
                let start = Instant::now();
                let mut wait_secs = 1;
                
                while start.elapsed() < Duration::from_secs(DEFAULT_INITIAL_SWARM_CONNECT_TIMEOUT_SECS) {
                    debug!("Attempting to connect to Swarm target at {}", target);

                    select! {
                        result = TcpStream::connect(target) => {
                            match result {
                                Ok(stream) => {
                                    swarm_stream = Some(stream);
                                    debug!("Swarm target successfully connected");
                                    break;
                                }
                                Err(e) => {
                                    error!("Failed to connect to swarm target, trying again in {} seconds: {}", wait_secs, e.to_string());
                                }
                            }
                        }
                        _ = processor_cancel_token.cancelled() => {
                            info!("Processor thread got cancel request during initial connect");
                            return;
                        }
                    }

                    if swarm_stream.is_none() {
                        select! {
                            _ = time::sleep(Duration::from_secs(wait_secs)) => {
                                wait_secs *= 2;
                            }
                            _ = processor_cancel_token.cancelled() => {
                                info!("Processor thread got cancel request while waiting for next retry");
                                return;
                            }
                        }
                    }
                }
            }

            let mut es_client: Option<Elasticsearch> = None;
            if let Some(ref mut es_url) = elastic_url {
                match create_es_client(es_url, validate_es_cert) {
                    Ok(client) => es_client = Some(client),
                    Err(e) => warn!("Failed to create ES client to {}: {}", es_url, e.to_string())
                }
            }
            
            loop {
                select! {
                    Some(mut frame) = rx.recv() => {
                        if let Some(ref acars) = frame.acars {
                            // TODO[ACARS]: use acars-decoder-rust to decode ACARS content and save it to frame.indexed
                        }
                        
                        if let Some(ref mut stream) = swarm_stream {
                            let raw_json = match serde_json::to_string(&frame) {
                                Ok(v) => v,
                                Err(e) => {
                                    error!("Failed to serialize CFF: {}", e.to_string());
                                    continue;
                                }
                            };

                            if let Err(e) = stream.write_all(format!("{}\n", raw_json).as_bytes()).await {
                                // NOTE: For now, just skip frames if we fail to write packet to swarm server
                                
                                match e.kind() {
                                    io::ErrorKind::BrokenPipe => {
                                        match TcpStream::connect(swarm_target.as_ref().unwrap()).await {
                                            Ok(v) => swarm_stream = Some(v),
                                            Err(e) => {
                                                warn!("Failed to connect to swarm target: {}", e.to_string());
                                            }  
                                        };
                                    }
                                    _ => warn!("Failed to proxy frame to Swarm target: {}", e.to_string())
                                }
                            }
                        } else {
                            let state_db = state_db.write().await;
                            if let Err(e) = state_db.update(&frame).await {
                                warn!("Failed to update state DB with frame: {}", e.to_string());
                            }
                        }
                        
                        if let Some(ref client) = es_client {
                            let mut batch = frames_batch.lock().await;

                            if batch.len() == 0 {
                                let frames_batch = frames_batch.clone();

                                batcher = Some(
                                    create_es_batch_task(
                                        client,
                                        &elastic_index, 
                                        frames_batch, 
                                        Duration::from_millis(DEFAULT_BATCH_WAIT_MS)
                                    )
                                );
                            }

                            batch.push(frame);
                        }
                    }
                    Some(ref change_event) = change_event_rx.recv() => {
                        let state_db = state_db.write().await;
                        if let Err(e) = state_db.handle_gs_change_event(change_event).await {
                            warn!("Failed to write ground station change even to state DB: {}", e.to_string());
                        }
                    }
                    _ = processor_cancel_token.cancelled() => {
                        info!("Processor thread got cancel request");
                        break;
                    }
                }
            }
            
            if let Some(batcher) = batcher {
                debug!("Batcher is active, waiting for completion before exiting to prevent data loss");
                if let Err(e) = batcher.await {
                    warn!("Error occurred while waiting for batcher to finish: {}", e.to_string());
                }
            }

            if let Some(ref mut stream) = swarm_stream {
                if let Err(e) = stream.shutdown().await {
                    warn!("Failed to shutdown Swarm connection: {}", e.to_string());
                }
            }
        });
        
        let mut should_run = true;
        let mut reason = EndSessionReason::None;

        while should_run {

            let mut session = match module.start_session(reason).await {
                Ok(v) => v,
                Err(e) => {
                    error!("Failed to start session: {}", e.to_string());
                    reason = EndSessionReason::ProcessStartError;

                    select! {
                        _ = sleep(Duration::from_secs(DEFAULT_FAILED_SESSION_START_WAIT_SECS)) => {}
                        _ = interrupt_signal.recv() => {
                            warn!("Got interrupt during failed session start wait, exiting session cleanly...");
                            
                            break;
                        }
                    }
                    continue;
                }    
            };
            
            let mut since_last_msg = Instant::now();
            
            loop {
                let mut raw_msg = String::new();
                
                select! {
                    results = time::timeout_at(
                        since_last_msg + Duration::from_secs(session_timeout_secs),
                        session.read_message(&mut raw_msg)
                    ) => {
                        let result = match results {
                            Ok(v) => v,
                            Err(e) => {
                                debug!("Timeout encountered: {}", e.to_string());

                                if session.on_timeout().await {
                                    reason = EndSessionReason::SessionTimeout;
                                    break;
                                } else {
                                    since_last_msg = Instant::now();
                                    continue;
                                }
                            }
                        };
                        
                        match result {
                            Ok(read_size) => {
                                if read_size == 0 {
                                    error!("Encountered bad read size of 0, ending session");
                                    debug!("Session Error Messages <<<\n{}", session.get_errors().await);
                                    debug!(">>>");
                                    
                                    reason = EndSessionReason::ReadEOF;
                                    break    
                                }

                                if !disable_print_frame {
                                    println!("{}", raw_msg.trim());
                                }                        
                                
                                let frame = match module.process_message(session.get_listening_band(), &raw_msg).await {
                                    Ok(v) => v,
                                    Err(e) => {
                                        error!("Malformed frame, could not convert to common frame format: {}", e.to_string());
                                        continue;
                                    }
                                };
                                info!("{:?}", frame);
                                if let Err(e) = tx.send(frame).await {
                                    error!("Failed to send common frame to processing thread: {}", e.to_string());                                    
                                }
                                
                                since_last_msg = Instant::now();
                            }
                            Err(e) => {
                                if matches!(e.kind(), io::ErrorKind::Other) {
                                    let Some(inner_err) = e.get_ref() else {
                                        debug!("Unable to get inner error for ErrorKind::Other");
                                        reason = EndSessionReason::ReadError;
                                        break;  
                                    };
                                    if inner_err.to_string() == SESSION_SCHEDULED_END {
                                        debug!("HFDL session ended by schedule");
                                        reason = EndSessionReason::SessionEnd;
                                    }
                                }

                                if matches!(reason, EndSessionReason::None) {
                                    error!("Read failure: {}", e.to_string());
                                    reason = EndSessionReason::ReadError;
                                }

                                break;
                            }
                        };
                    }
                    end_session_reason = end_session_signal.recv() => {
                        reason = end_session_reason.unwrap_or(EndSessionReason::UserAPIControl);
                        debug!("Got request to end current session: {:?}", reason);
                        break;
                    }
                    _ = interrupt_signal.recv() => {
                        warn!("Got interrupt, exiting session cleanly...");
                        
                        should_run = false;
                        reason = EndSessionReason::UserInterrupt;
                        break;
                    }
                    _ = reload_signal.recv() => {
                        {
                            let settings = module_settings.read().await;

                            match settings.props.get(PROP_SESSION_TIMEOUT_SEC) {
                                Some(v) => session_timeout_secs = v.as_u64().unwrap_or(module.default_session_timeout_secs()),
                                None => warn!("Failed to find session_timeout_secs key in module settings")
                            }

                            match settings.props.get(PROP_SESSION_INTERMISSION_SEC) {
                                Some(v) => session_intermission_secs = v.as_u64().unwrap_or(DEFAULT_SESSION_INTERMISSION_SECS),
                                None => warn!("Failed to find session_intermission_secs key in module settings")
                            }

                            info!("Module session timeout, intermission wait time props reloaded");
                        }

                        if let Err(e) = module.reload().await {
                            warn!("Failed to reload settings for {} module: {}", module.id(), e.to_string());
                        } else {
                            info!("Settings for module {} reloaded", module.id());
                        }
                    }
                } 
            }

            session.end(reason).await;
            
            if should_run && session_intermission_secs > 0 {
                debug!("Session ended, waiting for {} seconds before continuing", session_intermission_secs);
                
                select! {
                    _ = sleep(Duration::from_secs(session_intermission_secs)) => {}
                    _ = interrupt_signal.recv() => {
                        warn!("Got interrupt during session intermission, exiting session cleanly...");

                        break;
                    }
                }
            }
        }

        info!("Sending cancel request to spawned threads");
        cancel_token.cancel();

        #[allow(unused_must_use)] {
            tokio::join!(http_thread, processor_thread);
        }
        
        info!("Exiting...");
    }
}
