use clap::{ArgMatches, Command};
use std::collections::HashMap;

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
                .map(|m| m.get_arguments())
                .collect::<Vec<Command>>(),
        )
    }

    // TODO: register API endpoints

    pub async fn start(&self, cmd: &str, args: &ArgMatches) {
        todo!()
    }
}
