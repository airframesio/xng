use clap::{ArgMatches, Command, arg};
use log::*;
use reqwest::Url;
use std::collections::HashMap;
use std::process::exit;

use crate::common;

mod hfdl;

pub trait XngModule {
    fn id(&self) -> &'static str;
    fn get_arguments(&self) -> Command;
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
                            arg!(--"feed-airframes" "Feed JSON frames to airframes.io"),
                            arg!(--"station-name" <NAME> "Sets up a station name for feeding to airframes.io"),
                            arg!(--"session-intermission" <SECONDS> "Time to wait between sessions"),
                            arg!(--"disable-print-frame" "Disable printing JSON frames to STDOUT"), 
                        ])
                )
                .collect::<Vec<Command>>(),
        )
    }

    // TODO: register API endpoints

    pub async fn start(&self, cmd: &str, args: &ArgMatches) {
        stderrlog::new()
            .module(module_path!())
            .quiet(args.get_flag("quiet"))
            .verbosity(if args.get_flag("debug") { 3 } else if args.get_flag("verbose") { 2 } else { 1 })
            .timestamp(if args.get_flag("debug") || args.get_flag("verbose") { stderrlog::Timestamp::Second } else { stderrlog::Timestamp::Off })
            .init()
            .unwrap();

        if let Some(swarm_url) = args.get_one::<Url>("swarm") {
            // TODO: don't start or init server
        } else {
            // TODO: init actix-web server
        }
        
        let Some(module) = self.modules.get(cmd) else {
            exit(exitcode::CONFIG);   
        };
    }
}
