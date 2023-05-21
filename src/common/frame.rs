use serde::{Deserialize, Serialize};

use super::wkt::{WKTPoint, WKTPolyline};

#[derive(Debug, Deserialize, Serialize)]
pub struct ACARS {
    pub mode: String,
    pub more: bool,
    pub label: String,

    pub ack: Option<String>,
    pub blk_id: Option<String>,
    pub msg_num: Option<String>,
    pub msg_num_seq: Option<String>,
    pub tail: Option<String>,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct Entity {
    pub coords: Option<WKTPoint>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PropagationPath {
    pub freq: f64,
    pub path: WKTPolyline,
    pub party: Entity,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CommonFrame {
    pub timestamp: f64,
    pub freq: f64,
    pub signal: f32,
    pub err: bool,

    pub paths: Option<PropagationPath>,
    pub app: AppInfo,
    pub src: Entity,
    pub dst: Option<Entity>,
    pub acars: Option<ACARS>,
}
