use std::collections::HashMap;

use clap::{ArgMatches, Command};

pub trait XngModule {
    fn get_arguments(&self) -> Command;
}

pub struct ModuleManager {
    modules: HashMap<&'static str, Box<dyn XngModule>>,
}

impl ModuleManager {
    pub fn init() -> ModuleManager {
        ModuleManager {
            modules: HashMap::new(),
        }
    }

    pub fn register_arguments(&self, cmd: Command) -> Command {
        cmd.subcommands(
            self.modules
                .values()
                .map(|m| m.get_arguments())
                .collect::<Vec<Command>>(),
        )
    }

    // TODO: register API endpoints

    pub async fn start(&self, cmd: &str, args: &ArgMatches) {
        todo!()
    }
}
