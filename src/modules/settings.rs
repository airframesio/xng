use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::{Sender, UnboundedSender};

use crate::common::events::GroundStationChangeEvent;

use super::session::EndSessionReason;

pub type ValidatorCallback = fn(&Value) -> Result<(), String>;

#[derive(Clone, Debug, Serialize)]
pub struct FreqInfo {
    pub khz: u64,
    pub last_updated: DateTime<Utc>,
}

impl Hash for FreqInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.khz.hash(state);
    }
}
impl PartialEq for FreqInfo {
    fn eq(&self, other: &Self) -> bool {
        self.khz == other.khz
    }
}
impl Eq for FreqInfo {}

#[derive(Clone, Serialize)]
pub struct GroundStation {
    #[serde(skip_serializing_if = "Value::is_null")]
    pub id: Value,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    pub active_frequencies: HashSet<FreqInfo>,
}

impl GroundStation {
    pub fn invalidate(&mut self, stale_after: Duration) {
        let now = Utc::now();
        self.active_frequencies
            .retain(|x| (now - x.last_updated) < stale_after);
    }
}

impl PartialEq for GroundStation {
    fn eq(&self, other: &Self) -> bool {
        match (&self.id, &other.id) {
            (Value::Null, Value::Null) => true,
            (Value::Number(x), Value::Number(y)) => *x == *y,
            (Value::String(x), Value::String(y)) => {
                x.to_lowercase().trim() == y.to_lowercase().trim()
            }
            _ => false,
        }
    }
}
impl Eq for GroundStation {}

pub fn update_station_by_frequencies(
    settings: &mut ModuleSettings,
    arrival_time: Option<String>,
    stale_timeout_secs: i64,
    station_id: Value,
    station_name: Option<String>,
    freqs: &Vec<u64>,
) -> Option<GroundStationChangeEvent> {
    let station: &mut GroundStation;
    {
        match settings
            .stations
            .iter()
            .position(|x| match (&x.id, &station_id) {
                (Value::Null, Value::Null) => true,
                (Value::String(x), Value::String(y)) => *x == *y,
                (Value::Number(x), Value::Number(y)) => *x == *y,
                _ => false,
            }) {
            Some(idx) => station = &mut settings.stations[idx],
            None => {
                settings.stations.push(GroundStation {
                    id: station_id,
                    name: station_name,
                    active_frequencies: HashSet::new(),
                });
                station = settings.stations.last_mut().unwrap();
            }
        }
        station.invalidate(Duration::seconds(stale_timeout_secs));
    }

    let now = Utc::now();
    let new_freq_set: HashSet<FreqInfo> = freqs
        .iter()
        .map(|x| FreqInfo {
            khz: *x,
            last_updated: now,
        })
        .collect();
    let mut event: Option<GroundStationChangeEvent> = None;

    let changed = station.active_frequencies != new_freq_set;
    if changed {
        event = Some(GroundStationChangeEvent {
            ts: arrival_time.unwrap_or(now.to_rfc3339_opts(SecondsFormat::Micros, true)),
            id: station.id.clone(),
            name: station.name.clone(),
            old: format!(
                "{:?}",
                station
                    .active_frequencies
                    .iter()
                    .map(|x| x.khz)
                    .collect::<Vec<u64>>()
            ),
            new: format!("{:?}", freqs),
        });
    }
    station.active_frequencies.clear();
    station.active_frequencies.extend(new_freq_set);

    event
}

#[derive(Serialize)]
pub struct ModuleSettings {
    pub props: HashMap<String, Value>,
    pub stations: Vec<GroundStation>,

    #[serde(skip_serializing)]
    pub swarm_mode: bool,

    #[serde(skip_serializing)]
    pub disable_api_control: bool,

    #[serde(skip_serializing)]
    pub api_token: Option<String>,

    #[serde(skip_serializing)]
    pub reload_signaler: UnboundedSender<()>,

    #[serde(skip_serializing)]
    pub end_session_signaler: UnboundedSender<EndSessionReason>,

    #[serde(skip_serializing)]
    pub change_event_tx: Sender<GroundStationChangeEvent>,

    #[serde(skip_serializing)]
    validators: HashMap<String, ValidatorCallback>,
}

impl ModuleSettings {
    pub fn new(
        reload_signaler: UnboundedSender<()>,
        end_session_signaler: UnboundedSender<EndSessionReason>,
        change_event_tx: Sender<GroundStationChangeEvent>,
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
            stations: Vec::new(),
            disable_api_control,
            swarm_mode,
            api_token: api_token.map(|v| v.clone()),
            reload_signaler,
            end_session_signaler,
            change_event_tx,
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
