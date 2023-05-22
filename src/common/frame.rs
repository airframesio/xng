use super::{
    formats::validate_entity_type,
    wkt::{WKTPoint, WKTPolyline},
};
use serde::{Deserialize, Serialize};
use serde_valid::Validate;

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct ACARS {
    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub mode: String,

    pub more: bool,

    #[validate(min_length = 2)]
    #[validate(max_length = 2)]
    pub label: String,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub ack: Option<String>,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub blk_id: Option<String>,

    #[validate(min_length = 3)]
    #[validate(max_length = 3)]
    pub msg_num: Option<String>,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub msg_num_seq: Option<String>,

    #[validate(max_length = 8)]
    pub tail: Option<String>,

    #[validate(max_length = 8)]
    pub flight: Option<String>,

    pub sublabel: Option<String>,
    pub mfi: Option<String>,
    pub cfi: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct Entity {
    #[serde(rename = "type")]
    #[validate(custom(validate_entity_type))]
    pub kind: String,

    #[validate(min_length = 6)]
    #[validate(max_length = 6)]
    pub icao: Option<String>,

    pub gs: Option<String>,
    pub id: Option<u8>,

    #[validate(max_length = 8)]
    pub callsign: Option<String>,

    pub coords: Option<WKTPoint>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct PropagationPath {
    #[validate(minimum = 2.0)]
    #[validate(maximum = 1630.0)]
    pub freq: f64,

    pub path: WKTPolyline,

    #[validate]
    pub party: Entity,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CommonFrame {
    pub timestamp: f64,

    #[validate(minimum = 2.0)]
    #[validate(maximum = 1630.0)]
    pub freq: f64,
    pub signal: f32,
    pub err: bool,

    #[validate]
    pub paths: Option<PropagationPath>,

    pub app: AppInfo,

    #[validate]
    pub src: Entity,

    #[validate]
    pub dst: Option<Entity>,

    #[validate]
    pub acars: Option<ACARS>,
}
