use serde::Deserialize;
use serde_json::Value;
use serde_valid::Validate;

use crate::common::formats::{validate_entity_type, Application, Timestamp};

#[derive(Debug, Deserialize, Validate)]
pub struct Entity {
    pub addr: String,

    #[serde(rename = "type")]
    #[validate(custom(validate_entity_type))]
    pub entity_type: String,

    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct ACARS {
    pub err: bool,
    pub crc_ok: bool,
    pub more: bool,

    #[validate(max_length = 8)]
    pub reg: String,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub mode: String,

    #[validate(min_length = 2)]
    #[validate(max_length = 2)]
    pub label: String,

    pub sublabel: Option<String>,
    pub cfi: Option<String>,
    pub mfi: Option<String>,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub blk_id: String,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub ack: String,

    #[validate(max_length = 8)]
    pub flight: Option<String>,

    #[validate(min_length = 3)]
    #[validate(max_length = 3)]
    pub msg_num: Option<String>,

    #[validate(min_length = 1)]
    #[validate(max_length = 1)]
    pub msg_num_seq: Option<String>,

    pub msg_text: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct GPSCoord {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Deserialize, Validate)]
pub struct VDLParamLocation {
    pub loc: GPSCoord,
    pub alt: u32,
}
// TODO: to-WKTPoint

#[derive(Debug, Deserialize, Validate)]
pub struct VDLParam {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Deserialize, Validate)]
pub struct XID {
    pub err: bool,

    #[serde(rename = "type")]
    pub xid_type: String,

    #[serde(rename = "type_descr")]
    pub xid_type_desc: String,

    pub vdl_params: Vec<VDLParam>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CLNP {
    pub err: bool,
    // pub adsc_v2
}

#[derive(Debug, Deserialize, Validate)]
pub struct X25 {
    pub err: bool,
    pub pkt_type: u32,
    pub pkt_type_name: String,
    pub chan_group: u32,
    pub chan_num: u16,
    pub more: bool,
    pub clnp: Option<CLNP>,
}

// "adsc_v2": {"adsc_report": {"choice": "demand-report", "data": {"on_demand_report": {"report_data": {"position": {"lat": {"deg": 50, "min": 7, "sec": 53.9, "dir": "north"}, "lon": {"deg": 8, "min": 8, "sec":50.7, "dir": "east"}, "alt": {"val": 35980.0, "unit": "ft"}}, "timestamp": {"date": {"year": 2022, "month": 7, "day": 12}, "time": {"hour": 22, "min": 25, "sec": 53}}, ... }

#[derive(Debug, Deserialize, Validate)]
pub struct AVLC {
    pub src: Entity,
    pub dst: Entity,

    pub cr: String,

    pub rseq: Option<u32>,
    pub sseq: Option<u32>,

    pub cmd: Option<String>,

    pub pf: Option<bool>,

    pub acars: Option<ACARS>,
    pub xid: Option<XID>,
    pub x25: Option<X25>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct VDL2 {
    pub app: Application,

    #[serde(rename = "t")]
    #[validate]
    pub ts: Timestamp,

    #[validate(minimum = 118000000)]
    #[validate(maximum = 137000000)]
    pub freq: u64,

    pub idx: u64,
    pub sig_level: f64,
    pub noise_level: f64,
    pub freq_skew: f64,

    #[validate]
    pub avlc: Option<AVLC>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct Frame {
    #[validate]
    pub vdl2: VDL2,
}
