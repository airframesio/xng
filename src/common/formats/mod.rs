use serde::Deserialize;
use serde_valid::Validate;

pub enum EntityType {
    Aircraft,
    GroundStation,
    Reserved,
}

#[derive(Debug, Deserialize)]
pub struct Application {
    pub name: String,

    #[serde(rename = "ver")]
    pub version: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct Timestamp {
    pub sec: u64,

    #[validate(maximum = 999999)]
    pub usec: u64,
}

impl Timestamp {
    pub fn to_f64(&self) -> f64 {
        (self.sec as f64) + (self.usec as f64 / 1000000.0)
    }
}

pub fn validate_entity_type(val: &String) -> Result<(), serde_valid::validation::Error> {
    if vec!["aircraft", "ground station", "reserved"]
        .iter()
        .any(|&x| x == val.to_lowercase())
    {
        return Ok(());
    }

    Err(serde_valid::validation::Error::Custom(
        "Entity type should be \"Aircraft\" or \"Ground station\"".to_string(),
    ))
}
