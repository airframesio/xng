use std::io;

use serde::Deserialize;
use serde_json;

pub const AIRFRAMESIO_HOST: &'static str = "feed.acars.io";

pub const AIRFRAMESIO_DUMPHFDL_TCP_PORT: u16 = 5556;
pub const AIRFRAMESIO_DUMPVDL2_UDP_PORT: u16 = 5552;

#[derive(Debug, Deserialize)]
pub struct GroundStationFreqInfo {
    pub active: Vec<u16>,
}

#[derive(Debug, Deserialize)]
pub struct GroundStationStatus {
    pub id: u8,
    pub name: String,
    pub frequencies: GroundStationFreqInfo,
    pub last_updated: f64,
}

#[derive(Debug, Deserialize)]
pub struct HFDLGroundStationStatus {
    pub ground_stations: Vec<GroundStationStatus>,
}

pub async fn get_airframes_gs_status() -> io::Result<HFDLGroundStationStatus> {
    let response = match reqwest::get("https://api.airframes.io/hfdl/ground-stations").await {
        Ok(r) => r,
        Err(e) => {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                e.to_string(),
            ))
        }
    };

    let body = match response.text().await {
        Ok(v) => v,
        Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
    };

    serde_json::from_str(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
}
