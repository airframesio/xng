use crate::modules::XngModule;

use super::{AoaModule, AOA_COMMAND};

impl AoaModule {
    pub fn new() -> Box<dyn XngModule> {
        Box::new(AoaModule {
            name: AOA_COMMAND,

            ..Default::default()
        })
    }
}
