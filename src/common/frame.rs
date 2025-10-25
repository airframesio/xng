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
    pub id: Option<u32>,

    #[validate(max_length = 8)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    #[validate(max_length = 8)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<WKTPoint>,
}

impl Entity {
    pub fn is_ground_station(&self) -> bool {
        self.kind.to_lowercase() == "ground station"
    }
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

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Indexed {
    #[validate(
        pattern = r"^20[1-4][0-9]-(0[0-9]|1[0-2])-([0-2][0-9]|3[0-1])T([0-1][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]\.[0-9]{3,6}Z$"
    )]
    pub timestamp: String,

    pub dst_airport: Option<String>,
    pub src_airport: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_is_ground_station_lowercase() {
        let entity = Entity {
            kind: "ground station".to_string(),
            icao: None,
            gs: Some("SFO".to_string()),
            id: Some(1),
            callsign: None,
            tail: None,
            coords: None,
        };
        assert!(entity.is_ground_station());
    }

    #[test]
    fn test_entity_is_ground_station_uppercase() {
        let entity = Entity {
            kind: "Ground Station".to_string(),
            icao: None,
            gs: Some("SFO".to_string()),
            id: Some(1),
            callsign: None,
            tail: None,
            coords: None,
        };
        assert!(entity.is_ground_station());
    }

    #[test]
    fn test_entity_is_ground_station_mixed_case() {
        let entity = Entity {
            kind: "GROUND STATION".to_string(),
            icao: None,
            gs: Some("SFO".to_string()),
            id: Some(1),
            callsign: None,
            tail: None,
            coords: None,
        };
        assert!(entity.is_ground_station());
    }

    #[test]
    fn test_entity_is_not_ground_station_aircraft() {
        let entity = Entity {
            kind: "Aircraft".to_string(),
            icao: Some("ABC123".to_string()),
            gs: None,
            id: None,
            callsign: Some("AAL123".to_string()),
            tail: None,
            coords: None,
        };
        assert!(!entity.is_ground_station());
    }

    #[test]
    fn test_entity_is_not_ground_station_invalid() {
        let entity = Entity {
            kind: "Unknown".to_string(),
            icao: None,
            gs: None,
            id: None,
            callsign: None,
            tail: None,
            coords: None,
        };
        assert!(!entity.is_ground_station());
    }

    #[test]
    fn test_indexed_timestamp_validation() {
        // Valid timestamp
        let indexed = Indexed {
            timestamp: "2025-10-21T15:30:45.123456Z".to_string(),
            dst_airport: None,
            src_airport: None,
        };
        assert!(indexed.validate().is_ok());
    }

    #[test]
    fn test_indexed_timestamp_validation_min_precision() {
        // Valid timestamp with millisecond precision
        let indexed = Indexed {
            timestamp: "2025-10-21T15:30:45.123Z".to_string(),
            dst_airport: None,
            src_airport: None,
        };
        assert!(indexed.validate().is_ok());
    }

    #[test]
    fn test_indexed_timestamp_validation_invalid_format() {
        // Invalid timestamp format
        let indexed = Indexed {
            timestamp: "2025-10-21 15:30:45".to_string(),
            dst_airport: None,
            src_airport: None,
        };
        assert!(indexed.validate().is_err());
    }

    #[test]
    fn test_indexed_timestamp_validation_invalid_year() {
        // Year out of range (2050 is > 2049)
        let indexed = Indexed {
            timestamp: "2050-10-21T15:30:45.123Z".to_string(),
            dst_airport: None,
            src_airport: None,
        };
        assert!(indexed.validate().is_err());
    }
}
