use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

pub type ValidatorCallback = fn(&Value) -> Result<(), String>;

#[derive(Serialize)]
pub struct ModuleSettings {
    pub props: HashMap<String, Value>,

    #[serde(skip_serializing)]
    pub swarm_mode: bool,

    #[serde(skip_serializing)]
    pub disable_api_control: bool,

    #[serde(skip_serializing)]
    pub api_token: Option<String>,

    #[serde(skip_serializing)]
    pub reload_signaler: UnboundedSender<()>,

    #[serde(skip_serializing)]
    pub end_session_signaler: UnboundedSender<()>,

    #[serde(skip_serializing)]
    validators: HashMap<String, ValidatorCallback>,
}

impl ModuleSettings {
    pub fn new(
        reload_signaler: UnboundedSender<()>,
        end_session_signaler: UnboundedSender<()>,
        swarm_mode: bool,
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
            swarm_mode,
            api_token: api_token.map(|v| v.clone()),
            reload_signaler,
            end_session_signaler,
            validators: HashMap::new(),
        }
    }

    pub fn add_prop_with_validator(
        &mut self,
        prop: String,
        value: Value,
        validator: ValidatorCallback,
    ) {
        self.props.insert(prop.clone(), value.clone());
        self.validators.insert(prop.clone(), validator);
    }

    pub fn get_validator(&self, prop: &String) -> Option<&ValidatorCallback> {
        self.validators.get(prop)
    }
}
