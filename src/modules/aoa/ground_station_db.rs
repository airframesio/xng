use std::collections::HashMap;

use csv::ReaderBuilder;
use serde::Deserialize;
use tokio::io;

use crate::common::wkt::WKTPoint;

#[derive(Debug, Deserialize)]
struct GSRawRecord {
    #[serde(rename = "GS-ID")]
    gs_id: String,

    #[serde(rename = "Airport-ICAO")]
    icao: String,

    #[serde(rename = "Airport-IATA")]
    iata: String,

    #[serde(rename = "AirportName")]
    name: String,

    #[serde(rename = "AirportLat")]
    lat: String,

    #[serde(rename = "AirportLon")]
    lon: String,
}

#[derive(Debug)]
pub struct GSRecord {
    pub icao_addr: String,
    pub airport_icao: String,
    pub airport_iata: String,
    pub airport_name: String,
    pub coords: WKTPoint,
}

impl GSRawRecord {
    pub fn coords_as_wkt(&self) -> Option<WKTPoint> {
        let mut val = self.lat.clone().trim().to_string();
        let mut dir = val.pop()?;
        let y = val.parse::<f64>().ok()? * (if dir == 'S' { -1.0 } else { 1.0 });

        val = self.lon.clone().trim().to_string();
        dir = val.pop()?;
        let x = val.parse::<f64>().ok()? * (if dir == 'W' { -1.0 } else { 1.0 });

        Some(WKTPoint { x, y, z: 0.0 })
    }
}

#[derive(Debug)]
pub struct GroundStationDB {
    db: HashMap<String, GSRecord>,
}

impl GroundStationDB {
    pub fn from_csv(path: &String) -> Result<GroundStationDB, io::Error> {
        let mut rdr = ReaderBuilder::new().delimiter(b',').from_path(path)?;
        let mut db: HashMap<String, GSRecord> = HashMap::new();

        for entry in rdr.deserialize() {
            let record: GSRawRecord = entry?;
            let Some(coords) = record.coords_as_wkt() else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("Bad GPS coordinates for {}: icao={}", record.icao, record.gs_id)));    
            };

            db.insert(
                record.gs_id.to_uppercase(),
                GSRecord {
                    icao_addr: record.gs_id,
                    airport_icao: record.icao,
                    airport_iata: record.iata,
                    airport_name: record.name,
                    coords,
                },
            );
        }

        Ok(GroundStationDB { db })
    }

    pub fn get(&self, addr: &String) -> Option<&GSRecord> {
        self.db.get(addr)
    }
}
