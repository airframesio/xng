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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack: Option<String>,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blk_id: Option<String>,

    #[validate(min_length = 3)]
    #[validate(max_length = 3)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_num: Option<String>,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_num_seq: Option<String>,

    #[validate(max_length = 8)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,

    #[validate(max_length = 8)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sublabel: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfi: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfi: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Validate)]
pub struct Entity {
    #[serde(rename = "type")]
    #[validate(custom(validate_entity_type))]
    pub kind: String,

    #[validate(min_length = 6)]
    #[validate(max_length = 6)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icao: Option<String>,

    pub gs: Option<String>,
    pub id: Option<u8>,

    #[validate(max_length = 8)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    #[validate(max_length = 8)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<WKTPoint>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct PropagationPath {
    #[validate(minimum = 2.0)]
    #[validate(maximum = 1630.0)]
    pub freqs: Vec<f64>,

    pub path: WKTPolyline,

    #[validate]
    pub party: Entity,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct Indexed {
    #[validate(
        pattern = r"^20[1-4][0-9]-(0[0-9]|1[0-2])-([0-2][0-9]|3[0-1])T([0-1][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]\.[0-9]{3,6}Z$"
    )]
    pub timestamp: String,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct HFDLGSEntry {
    pub kind: String,
    pub id: u8,
    pub gs: String,

    #[validate(minimum = 2.0)]
    #[validate(maximum = 1630.0)]
    pub freqs: Vec<f64>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct HFDLMetadata {
    pub kind: String,

    #[validate]
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub heard_on: Vec<HFDLGSEntry>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct Metadata {
    #[validate]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hfdl: Option<HFDLMetadata>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CommonFrame {
    #[validate(
        pattern = r"^20[1-4][0-9]-(0[0-9]|1[0-2])-([0-2][0-9]|3[0-1])T([0-1][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]\.[0-9]{3,6}Z$"
    )]
    pub timestamp: String,

    #[validate(minimum = 2.0)]
    #[validate(maximum = 1630.0)]
    pub freq: f64,
    pub signal: f32,
    pub err: bool,

    #[validate]
    pub paths: Vec<PropagationPath>,

    pub app: AppInfo,

    #[validate]
    pub src: Entity,

    #[validate]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst: Option<Entity>,

    #[validate]
    pub indexed: Indexed,

    #[validate]
    pub metadata: Metadata,

    #[validate]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acars: Option<ACARS>,
}
