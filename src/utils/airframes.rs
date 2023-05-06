use std::io;

use serde::Deserialize;
use serde_json;

const FLAG_OUTPUT: &'static str = "--output";
const AIRFRAMESIO_DUMPHFDL_OUTPUT: &'static str =
    "decoded:json:tcp:address=feed.acars.io,port=5556";

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

pub fn add_airframes_feeder_args(args: &Vec<String>) -> io::Result<Vec<String>> {
    // --station-id "AA-KEWR-HFDL" --output decoded:json:tcp:address=feed.acars.io,port=5556
    if args.len() < 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Too few additional dumphfdl args for feeding airframes.io, cannot fit required '--soapysdr' and '--station-id' arguments: {:?}", args)));
    }

    if !args.iter().any(|x| x.eq_ignore_ascii_case("--station-id")) {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("Additional dumphfdl arguments missing '--station-id' required for airframes.io feeding: {:?}", args)));
    }

    let mut hfdl_args = args.clone();

    match args
        .iter()
        .position(|x| x.eq_ignore_ascii_case(AIRFRAMESIO_DUMPHFDL_OUTPUT))
    {
        Some(idx) => {
            if idx == 0 || !args[idx - 1].eq_ignore_ascii_case(FLAG_OUTPUT) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid additional dumphfdl arguments found: {:?}", args),
                ));
            }
        }
        None => hfdl_args.extend_from_slice(&[
            FLAG_OUTPUT.to_string(),
            AIRFRAMESIO_DUMPHFDL_OUTPUT.to_string(),
        ]),
    }

    Ok(hfdl_args)
}
