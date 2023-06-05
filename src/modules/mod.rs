use actix_web::web::Data;
use actix_web::{HttpServer, App, middleware};
use async_trait::async_trait;
use clap::{ArgMatches, Command, arg};
use log::*;
use reqwest::Url;
use serde_json::json;
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
use crate::common::batcher::create_es_batch_task;
use crate::common::frame::CommonFrame;
use crate::modules::session::EndSessionReason;

use self::session::Session;
use self::settings::ModuleSettings;

mod hfdl;
mod services;
mod session;
mod settings;

const DEFAULT_SESSION_INTERMISSION_SECS: u64 = 0;
const DEFAULT_BATCH_WAIT_MS: u64 = 200;

const DEFAULT_LISTEN_HOST: &'static str = "127.0.0.1";
const DEFAULT_LISTEN_PORT: u16 = 7871;

const DEFAULT_CHANNEL_BUFFER: usize = 2048;

const PROP_SESSION_TIMEOUT_SEC: &'static str = "session_timeout_sec";
const PROP_SESSION_INTERMISSION_SEC: &'static str = "session_intermission_sec";

#[async_trait]
pub trait XngModule {
    fn id(&self) -> &'static str;

    fn default_session_timeout_secs(&self) -> u64;
    
    fn get_arguments(&self) -> Command;
    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error>;

    async fn load_module_settings(&self, settings: Data<RwLock<ModuleSettings>>);

    fn process_message(&self, msg: &str) -> Result<CommonFrame, io::Error>;
    fn start_session(&self) -> Result<Box<dyn Session>, io::Error>;
}

pub struct ModuleManager {
    modules: HashMap<&'static str, Box<dyn XngModule>>,
}

impl ModuleManager {
    pub fn init() -> ModuleManager {
        ModuleManager {
            modules: HashMap::from_iter(
                [hfdl::HfdlModule::new()]
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

        let api_token: Option<&String> = args.get_one("api-token");
        let disable_cross_site = args.get_flag("disable-cross-site");
        let listen_host = args.get_one::<String>("listen-host").unwrap_or(&DEFAULT_LISTEN_HOST.to_string()).to_owned();
        let listen_port = args
            .get_one::<String>("listen-port")
            .unwrap_or(&String::from("default"))
            .parse::<u16>()
            .unwrap_or(DEFAULT_LISTEN_PORT);
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
        let elastic_url = if let Some(raw_url) = args.get_one::<String>("elastic") {
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
                
        if swarm_url.is_some() && elastic_url.is_some() {
            error!("Swarm mode and importing to Elasticsearch are mutually exclusive options");
            error!("Please choose either swarm mode or importing to Elasticsearch.");
            return;    
        }
        
        let (reload_signaler, mut reload_signal) = mpsc::unbounded_channel::<()>();
        let (end_session_signaler, mut end_session_signal) = mpsc::unbounded_channel::<()>();
        let Ok(mut interrupt_signal) = signal(SignalKind::interrupt()) else {
            error!("Failed to register interrupt signal");
            return;
        };

        if let Err(e) = module.parse_arguments(args) {
            error!("Failed to parse arguments: {}", e.to_string());
            return;    
        }

        let module_settings = Data::new(
            RwLock::new(
                ModuleSettings::new(
                    reload_signaler,
                    end_session_signaler,
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
        module.load_module_settings(module_settings.clone()).await;
        
        let cancel_token = CancellationToken::new();
        let http_cancel_token = cancel_token.clone();
        let http_module_settings = module_settings.clone();
        
        let http_thread = tokio::spawn(async move {
            let restricted_origin = format!("http://{}:{}", listen_host, listen_port);
            
            let server = HttpServer::new(move || {
                App::new()
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

            // TODO: set up TCP connection for swarm
            
            loop {
                select! {
                    Some(frame) = rx.recv() => {
                        // TODO: process frame by parsing ACARS content
                    
                        // TODO: ship frame to swarm target if swarm mode

                        if let Some(ref es_url) = elastic_url {
                            let mut batch = frames_batch.lock().await;
                            let es_url = es_url.clone();

                            if batch.len() == 0 {
                                let frames_batch = frames_batch.clone();

                                batcher = Some(
                                    create_es_batch_task(
                                        es_url, 
                                        frames_batch, 
                                        Duration::from_millis(DEFAULT_BATCH_WAIT_MS)
                                    )
                                );
                            }

                            batch.push(frame);
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
        });
        
        let mut should_run = true;

        while should_run {
            let mut reason = EndSessionReason::None;

            let mut session = match module.start_session() {
                Ok(v) => v,
                Err(e) => {
                    error!("Failed to start session: {}", e.to_string());
                    reason = EndSessionReason::ProcessStartError;
                    break;
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
                                
                                reason = EndSessionReason::SessionTimeout;
                                break;
                            }
                        };
                        
                        match result {
                            Ok(read_size) => {
                                if read_size == 0 {
                                    error!("Encountered bad read size of 0, ending session");
                                    debug!("Session Error Messages");
                                    debug!("======================");
                                    debug!("{}", session.get_errors().await);
                                    debug!("======================");
                                    
                                    reason = EndSessionReason::ReadEOF;
                                    break    
                                }

                                if !disable_print_frame {
                                    println!("{}", raw_msg.trim());
                                }                        
                                
                                let frame = match module.process_message(&raw_msg) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        error!("Malformed frame, could not convert to common frame format: {}", e.to_string());
                                        continue;
                                    }
                                };

                                let out = serde_json::to_string(&frame).unwrap_or(String::from(""));
                                println!("{}", out);
                                
                                if let Err(e) = tx.send(frame).await {
                                    error!("Failed to send common frame to processing thread: {}", e.to_string());                                    
                                }
                                
                                since_last_msg = Instant::now();
                            }
                            Err(e) => {
                                error!("Read failure: {}", e.to_string());
                                
                                reason = EndSessionReason::ReadError;
                                break;
                            }
                        };
                    }
                    _ = end_session_signal.recv() => {
                        debug!("Got request to end current session");
                        reason = EndSessionReason::UserAPIControl;
                        break;
                    }
                    _ = interrupt_signal.recv() => {
                        warn!("Got interrupt, exiting session cleanly...");
                        
                        should_run = false;
                        reason = EndSessionReason::UserInterrupt;
                        break;
                    }
                    _ = reload_signal.recv() => {
                        let settings = module_settings.read().await;
                        
                        match settings.props.get(PROP_SESSION_TIMEOUT_SEC) {
                            Some(v) => session_timeout_secs = v.as_u64().unwrap_or(module.default_session_timeout_secs()),
                            None => warn!("Failed to find session_timeout_secs key in module settings")
                        }

                        match settings.props.get(PROP_SESSION_INTERMISSION_SEC) {
                            Some(v) => session_intermission_secs = v.as_u64().unwrap_or(DEFAULT_SESSION_INTERMISSION_SECS),
                            None => warn!("Failed to find session_intermission_secs key in module settings")
                        }

                        info!("Module session timeout and intermission props reloaded");
                    }
                } 
            }

            session.end(reason).await;
            
            if should_run && session_intermission_secs > 0 {
                debug!("Session ended, waiting for {} seconds before continuing", session_intermission_secs);
                sleep(Duration::from_secs(session_intermission_secs)).await;
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
