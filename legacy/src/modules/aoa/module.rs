use actix_web::web::Data;
use tokio::io;
use tokio::sync::RwLock;

use crate::modules::{settings::ModuleSettings, XngModule};

use super::{AoaModule, AOA_COMMAND};

impl AoaModule {
    pub fn new() -> Box<dyn XngModule> {
        Box::new(AoaModule {
            name: AOA_COMMAND,

            ..Default::default()
        })
    }

    pub fn get_settings(&self) -> Result<Data<RwLock<ModuleSettings>>, io::Error> {
        let Some(ref settings) = self.settings else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ModuleSettings is None"));
        };
        Ok(settings.clone())
    }
}
