use actix_web::web::Data;
use actix_web::{HttpServer, App, middleware};
use clap::{ArgMatches, Command, arg};
use log::*;
use reqwest::Url;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::signal::unix::{SignalKind, signal};
use tokio::select;
use tokio::sync::RwLock;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use std::collections::HashMap;
use std::io;
use std::process::exit;
use std::time::Duration;

use crate::common;

mod hfdl;
mod services;

const DEFAULT_SESSION_INTERMISSION_SECS: u64 = 0;

const DEFAULT_LISTEN_HOST: &'static str = "127.0.0.1";
const DEFAULT_LISTEN_PORT: u16 = 7871;

const PROP_SESSION_TIMEOUT_SEC: &'static str = "session_timeout_sec";
const PROP_SESSION_INTERMISSION_SEC: &'static str = "session_intermission_sec";

pub trait XngModule {
    fn id(&self) -> &'static str;

    fn default_session_timeout_secs(&self) -> u64;
    
    fn get_arguments(&self) -> Command;
    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error>;

    fn load_module_settings(&self, settings: &mut ModuleSettings);
}

#[derive(Debug, Serialize)]
pub struct ModuleSettings {
    props: HashMap<String, Value>,

    #[serde(skip_serializing)]
    disable_api_control: bool, 
    
    #[serde(skip_serializing)]
    api_token: Option<String>,
    
    #[serde(skip_serializing)]    
    reload_signaler: UnboundedSender<()>,

    #[serde(skip_serializing)]
    end_session_signaler: UnboundedSender<()>,
}

impl ModuleSettings {
    pub fn new(
        reload_signaler: UnboundedSender<()>, 
        end_session_signaler: UnboundedSender<()>,
        disable_api_control: bool,
        api_token: Option<&String>,
        settings: Vec<(&'static str, Value)>
    ) -> ModuleSettings {
        ModuleSettings { 
            props: settings.into_iter().map(|(x, y)| (x.to_string(), y)).collect(),
            disable_api_control,
            api_token: api_token.map(|v| v.clone()), 
            reload_signaler, 
            end_session_signaler 
        }
    } 
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
                            arg!(--"station-name" <NAME> "Sets up a station name for feeding to airframes.io"),
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
        let listen_host = *args.get_one("listen-host").unwrap_or(&DEFAULT_LISTEN_HOST);
        let listen_port = args.get_one("listen-port").unwrap_or(&"default").parse::<u16>().unwrap_or(DEFAULT_LISTEN_PORT);
        let disable_api_control = args.get_flag("disable-api-control");
        
        let feed_airframes = args.get_flag("feed-airframes");
        let disable_print_frame = args.get_flag("disable-print-frame");
        
        let swarm_url: Option<&Url> = args.get_one("swarm");
        let elastic_url: Option<&Url> = args.get_one("elastic");
        
        let station_name: Option<&String> = args.get_one("station-name");
        
        let mut session_intermission_secs = args.get_one("session-intermission")
            .unwrap_or(&"default").parse::<u64>()
            .unwrap_or(DEFAULT_SESSION_INTERMISSION_SECS);
        let mut session_timeout_secs = args
            .get_one("session-timeout")
            .unwrap_or(&"default")
            .parse::<u64>()
            .unwrap_or(module.default_session_timeout_secs());

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

        let mut module_settings = ModuleSettings::new(
            reload_signaler,
            end_session_signaler,
            disable_api_control,
            api_token,
            vec![
                (PROP_SESSION_TIMEOUT_SEC, json!(session_timeout_secs)),
                (PROP_SESSION_INTERMISSION_SEC, json!(session_intermission_secs))
            ]    
        );
        module.load_module_settings(&mut module_settings);

        let module_settings = Data::new(RwLock::new(module_settings));
        
        let cancel_token = CancellationToken::new();
        let http_cancel_token = cancel_token.clone();
        let http_module_settings = module_settings.clone();
        
        let http_thread = tokio::spawn(async move {
            let server = HttpServer::new(move || {
                App::new()
                    .app_data(http_module_settings.clone())
                    .wrap(middleware::DefaultHeaders::new().add(
                        (
                            "Access-Control-Allow-Origin", 
                            if disable_cross_site {
                                format!("http://{}:{}", listen_host, listen_port)
                            } else {
                                "*".to_string()
                            }
                        )
                    ))
                    .configure(services::config)
            })
                .bind((listen_host, listen_port))
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
        
        let mut should_run = true;

        while should_run {
            loop {
                select! {
                    _ = end_session_signal.recv() => {
                        debug!("Got request to end current session");
                        break;
                    }
                    _ = interrupt_signal.recv() => {
                        error!("Got interrupt, exiting session cleanly...");
                        // TODO
                        should_run = false;
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

            if session_intermission_secs > 0 {
                debug!("Session ended, waiting for {} seconds before continuing", session_intermission_secs);
                sleep(Duration::from_secs(session_intermission_secs)).await;
            }
        }

        info!("Sending cancel request to spawned threads");
        cancel_token.cancel();

        #[allow(unused_must_use)] {
            http_thread.await;
        }

        info!("Exiting...");
    }
}
