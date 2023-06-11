use actix_web::web::Data;
use std::io;
use tokio::sync::RwLock;

use crate::modules::{settings::ModuleSettings, XngModule};

use super::{HfdlModule, HFDL_COMMAND};

impl HfdlModule {
    pub fn new() -> Box<dyn XngModule> {
        Box::new(HfdlModule {
            name: HFDL_COMMAND,
            ..Default::default()
        })
    }

    pub fn get_settings(&self) -> Result<Data<RwLock<ModuleSettings>>, io::Error> {
        let Some(ref settings) = self.settings else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ModuleSettings is None"));
        };
        Ok(settings.clone())
    }

    pub fn nearest_sample_rate(&self, sample_rate: u64) -> Option<u64> {
        todo!()
    }
}
