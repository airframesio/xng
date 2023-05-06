use std::collections::HashMap;

use clap::{ArgMatches, Command};

pub trait XngModule {
    fn register_arguments(&self, cmd: &mut Command) -> Result<(), String>;
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

    pub fn register_arguments(&self, cmd: &mut Command) -> Result<(), String> {
        for (_, module) in self.modules.iter() {
            module.register_arguments(cmd)?
        }

        Ok(())
    }

    // TODO: register API endpoints

    pub async fn start(&self, cmd: &str, args: &ArgMatches) {
        todo!()
    }
}
