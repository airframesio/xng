use crate::modules::XngModule;

use super::{HfdlModule, HFDL_COMMAND};

impl HfdlModule {
    pub fn new() -> Box<dyn XngModule> {
        Box::new(HfdlModule {
            name: HFDL_COMMAND,
            ..Default::default()
        })
    }
}
