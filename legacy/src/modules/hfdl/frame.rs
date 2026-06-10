use serde::Deserialize;
use serde_valid::Validate;

use crate::common::formats::{validate_entity_type, Application, EntityType, Timestamp};
use crate::common::frame;
use crate::common::wkt::WKTPoint;

use super::systable::SystemTable;

#[derive(Debug, Deserialize, Validate)]
pub struct AircraftInfo {
    #[validate(min_length = 6)]
    #[validate(max_length = 6)]
    pub icao: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct Entity {
    #[serde(rename = "type")]
    #[validate(custom(validate_entity_type))]
    pub entity_type: String,

    pub id: u8,
    pub name: Option<String>,

    #[validate]
    pub ac_info: Option<AircraftInfo>,
}

impl Entity {
    pub fn kind(&self) -> EntityType {
        let normalized_type = self.entity_type.to_lowercase();
        if normalized_type == "aircraft" {
            return EntityType::Aircraft;
        } else if normalized_type == "ground station" {
            return EntityType::GroundStation;
        }

        unreachable!("Validation failed: encountered {}", normalized_type)
    }

    pub fn to_common_frame_entity(&self, systable: &SystemTable) -> frame::Entity {
        frame::Entity {
            kind: self.entity_type.clone(),
            icao: if let Some(ref ac_info) = self.ac_info {
                Some(ac_info.icao.clone())
            } else {
                None
            },
            gs: match (self.kind(), &self.name) {
                (EntityType::GroundStation, Some(name)) => Some(name.clone()),
                _ => None,
            },
            id: Some(self.id.into()),
            callsign: None,
            tail: None,
            coords: match (self.kind(), systable.by_id(self.id)) {
                (EntityType::GroundStation, Some(station)) => Some(WKTPoint {
                    x: station.position.1,
                    y: station.position.0,
                    z: 0.0,
                }),
                _ => None,
            },
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct FrequencyInfo {
    pub id: u8,

    #[validate(minimum = 200.0)]
    #[validate(maximum = 22000.0)]
    pub freq: f64,
}

#[derive(Debug, Deserialize, Validate)]
pub struct GroundStation {
    #[validate]
    pub gs: Entity,

    pub utc_sync: bool,

    #[validate]
    pub freqs: Vec<FrequencyInfo>,
}

#[derive(Debug, Deserialize)]
pub struct PDUType {
    pub id: u16,
    pub name: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct Reason {
    pub code: u32,
    pub descr: String,
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
pub struct Position {
    #[validate(minimum = -180.0)]
    #[validate(maximum = 180.0)]
    pub lat: f64,

    #[validate(minimum = -180.0)]
    #[validate(maximum = 180.0)]
    pub lon: f64,
}

impl Position {
    pub fn as_wkt(&self) -> WKTPoint {
        WKTPoint {
            x: self.lon,
            y: self.lat,
            z: 0.0,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SystablePartial {
    pub part_num: u8,
    pub parts_cnt: u8,
}

#[derive(Debug, Deserialize, Validate)]
pub struct FreqData {
    pub gs: Entity,
    pub listening_on_freqs: Vec<FrequencyInfo>,
    pub heard_on_freqs: Vec<FrequencyInfo>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct PerfDataFreq {
    pub id: u32,

    #[validate(minimum = 2000.0)]
    #[validate(maximum = 22000.0)]
    pub freq: Option<f64>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct HFNPDUTime {
    #[validate(maximum = 23)]
    pub hour: u8,

    #[validate(maximum = 59)]
    pub min: u8,

    #[validate(maximum = 59)]
    pub sec: u8,
}

#[derive(Debug, Deserialize, Validate)]
pub struct HFNPDU {
    pub err: bool,

    #[serde(rename = "type")]
    pub kind: PDUType,

    #[validate(min_length = 6)]
    #[validate(max_length = 6)]
    pub flight_id: Option<String>,

    #[validate]
    pub pos: Option<Position>,

    #[validate]
    pub acars: Option<ACARS>,

    pub version: Option<u8>,
    pub systable_partial: Option<SystablePartial>,

    pub flight_leg_num: Option<u32>,

    #[validate]
    pub frequency: Option<PerfDataFreq>,

    #[validate]
    pub time: Option<HFNPDUTime>,

    #[validate]
    pub freq_data: Option<Vec<FreqData>>,

    pub last_freq_change_cause: Option<Reason>,

    pub request_data: Option<u16>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct LPDU {
    pub err: bool,

    #[validate]
    pub src: Entity,

    #[validate]
    pub dst: Entity,

    #[serde(rename = "type")]
    pub kind: PDUType,

    #[validate]
    pub hfnpdu: Option<HFNPDU>,

    #[validate]
    pub ac_info: Option<AircraftInfo>,

    pub assigned_ac_id: Option<u8>,
    pub reason: Option<Reason>,
}

impl LPDU {
    pub fn from_ground_station(&self) -> bool {
        match self.src.kind() {
            EntityType::GroundStation => true,
            _ => false,
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct SPDU {
    pub err: bool,

    #[validate]
    pub src: Entity,

    pub spdu_version: u8,
    pub change_note: String,
    pub systable_version: u8,

    #[validate]
    pub gs_status: Vec<GroundStation>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct HFDL {
    pub app: Application,

    #[serde(rename = "t")]
    #[validate]
    pub ts: Timestamp,

    #[validate(minimum = 2000)]
    #[validate(maximum = 22000)]
    pub freq: u64,

    pub bit_rate: u16,
    pub sig_level: f64,
    pub noise_level: f64,
    pub freq_skew: f64,

    pub slot: String,

    #[validate]
    pub spdu: Option<SPDU>,

    #[validate]
    pub lpdu: Option<LPDU>,
}

impl HFDL {
    pub fn freq_as_mhz(&self) -> f64 {
        self.freq as f64 / 1000000.0
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct Frame {
    #[validate]
    pub hfdl: HFDL,
}
