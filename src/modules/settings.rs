use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Serialize)]
pub struct ModuleSettings {
    pub props: HashMap<String, Value>,

    #[serde(skip_serializing)]
    pub disable_api_control: bool,

    #[serde(skip_serializing)]
    pub api_token: Option<String>,

    #[serde(skip_serializing)]
    pub reload_signaler: UnboundedSender<()>,

    #[serde(skip_serializing)]
    pub end_session_signaler: UnboundedSender<()>,
}

impl ModuleSettings {
    pub fn new(
        reload_signaler: UnboundedSender<()>,
        end_session_signaler: UnboundedSender<()>,
        disable_api_control: bool,
        api_token: Option<&String>,
        settings: Vec<(&'static str, Value)>,
    ) -> ModuleSettings {
        ModuleSettings {
            props: settings
                .into_iter()
                .map(|(x, y)| (x.to_string(), y))
                .collect(),
            disable_api_control,
            api_token: api_token.map(|v| v.clone()),
            reload_signaler,
            end_session_signaler,
        }
    }
}
