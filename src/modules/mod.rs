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

    pub async fn start(&self, cmd: &str, args: &ArgMatches) {
        let Some(module) = self.modules.get(cmd) else {
            error!("Invalid module '{}', please choose a valid module.", cmd);
            exit(exitcode::CONFIG);   
        };

    }
}
