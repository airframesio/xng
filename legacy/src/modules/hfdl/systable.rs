use lazy_static::lazy_static;
use log::*;
use regex::Regex;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;

#[derive(Debug)]
pub struct GroundStation {
    pub id: u8,
    pub name: String,
    pub short: String,
    pub position: (f64, f64),
    pub frequencies: Vec<u16>,
}

impl GroundStation {
    pub fn new(
        id: u8,
        name: &str,
        lat: f64,
        lon: f64,
        frequencies: Vec<f64>,
    ) -> Option<GroundStation> {
        lazy_static! {
            static ref SHORT_NAMES: HashMap<u8, &'static str> = HashMap::from([
                (1, "SFO"),
                (2, "MKK"),
                (3, "RKV"),
                (4, "FOK"),
                (5, "AKL"),
                (6, "HDY"),
                (7, "SNN"),
                (8, "JNB"),
                (9, "BRW"),
                (10, "MWX"),
                (11, "PTY"),
                (13, "VVI"),
                (14, "KJA"),
                (15, "BAH"),
                (16, "GUM"),
                (17, "LPA")
            ]);
        }

        if id == 0 {
            debug!("Ground station ID is not valid");
            return None;
        }

        if name.len() == 0 {
            debug!("Ground station name is not valid");
            return None;
        }

        if lat >= 90.0 || lat <= -90.0 {
            debug!("Ground station latitude is not valid: {}", lat);
            return None;
        }

        if lon >= 180.0 || lon <= -180.0 {
            debug!("Ground station longitude is not valid: {}", lon);
            return None;
        }

        if frequencies.is_empty() {
            debug!("Ground station frequencies are not valid");
            return None;
        }

        if frequencies.iter().any(|&x| x == 0.0) {
            debug!("Ground station frequencies contain invalid data");
            return None;
        }

        Some(GroundStation {
            id,
            name: name.to_string(),
            short: SHORT_NAMES.get(&id).unwrap_or(&"???").to_string(),
            position: (lat, lon),
            frequencies: frequencies.into_iter().map(|x| x as u16).collect(),
        })
    }
}

#[derive(Debug, Default)]
pub struct SystemTable {
    pub path: PathBuf,

    pub version: u8,
    pub stations: Vec<GroundStation>,
}

impl SystemTable {
    pub fn get_version(&self) -> u8 {
        self.version
    }

    pub fn by_id(&self, id: u8) -> Option<&GroundStation> {
        self.stations.iter().find(|x| x.id == id)
    }

    pub fn by_name(&self, name: &str) -> Option<&GroundStation> {
        self.stations
            .iter()
            .find(|x| x.name.eq_ignore_ascii_case(name))
    }

    pub fn all_freqs(&self) -> Vec<u16> {
        self.stations
            .iter()
            .flat_map(|x| x.frequencies.clone())
            .collect()
    }

    pub fn load(path: &PathBuf) -> io::Result<Self> {
        lazy_static! {
            static ref NEWLINES_FMT: Regex = Regex::new(r"[\r\n]").unwrap();
            static ref SYSTABLE_FMT: Regex = Regex::new(
                r"(?x)
                \s*version\s*=\s*([0-9]+)\s*;
                \s*stations\s*=\s*\(
                    (.+)
                \)\s*;
                "
            )
            .unwrap();
            static ref STATIONS_FMT: Regex = Regex::new(
                r#"(?x)
                \{
                    \s*id\s*=\s*([0-9]+)\s*;
                    \s*name\s*:\s*"([\w\s,-]+?)"
                    \s*lat\s*=\s*(-{0,1}[0-9]{1,2}(?:\.[0-9]{1,6}){0,1})\s*;
                    \s*lon\s*=\s*(-{0,1}[0-9]{1,3}(?:\.[0-9]{1,6}){0,1})\s*;
                    \s*frequencies\s*=\s*\(\s*((?:[0-9]{4,5}(?:\.0){0,1}\s*,{0,1}\s*)+)\)\s*;
                    \s*
                \}
                "#
            )
            .unwrap();
        }

        let mut raw_content = String::new();
        {
            let Ok(mut fd) = File::open(path) else {
                return Err(
                    io::Error::new(
                        io::ErrorKind::NotFound, 
                        format!("Could not load {} for parsing", path.to_string_lossy())
                    )
                );
            };
            
            match fd.read_to_string(&mut raw_content) {
                Err(e) => return Err(
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Unable to read {} for parsing: {}", path.to_string_lossy(), e.to_string())
                    )
                ),
                _ => {}
            };
        }

        let content = NEWLINES_FMT.replace_all(&raw_content, "");
        let Some(m) = SYSTABLE_FMT.captures(&content) else {
            debug!(
                "SYSTABLE_FMT regex has no captures when processing {}",
                path.to_string_lossy()
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Malformed systable configuration: {}",
                    path.to_string_lossy()
                ),
            ));
        };

        let version: u8 = m.get(1).map_or("", |x| x.as_str()).parse().unwrap_or(0);
        if version < 51 {
            debug!(
                "System table version number too old: expected >=51, got {}",
                version
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid version number: {}", path.to_string_lossy()),
            ));
        }

        debug!("Processing ground stations from systable.conf");

        let mut stations: Vec<GroundStation> = Vec::new();
        let station_content = m.get(2).map_or("", |x| x.as_str());
        for c in STATIONS_FMT.captures_iter(&station_content) {
            if let Some(station) = GroundStation::new(
                c.get(1)
                    .map_or("", |x| x.as_str())
                    .parse::<u8>()
                    .unwrap_or(0),
                c.get(2).map_or("", |x| x.as_str()),
                c.get(3)
                    .map_or("", |x| x.as_str())
                    .parse::<f64>()
                    .unwrap_or(180.0),
                c.get(4)
                    .map_or("", |x| x.as_str())
                    .parse::<f64>()
                    .unwrap_or(180.0),
                c.get(5)
                    .map_or("", |x| x.as_str())
                    .replace(" ", "")
                    .split(",")
                    .into_iter()
                    .map(|x| x.parse::<f64>().unwrap_or(0.0))
                    .collect(),
            ) {
                trace!("  Station = {:#?}", station);
                stations.push(station);
            } else {
                trace!("Invalid station");
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Malformed ground station: {}", path.to_string_lossy()),
                ));
            }
        }

        debug!(
            "Parsed {} HFDL ground stations from system table {}",
            stations.len(),
            version
        );

        Ok(SystemTable {
            path: path.clone(),
            version,
            stations,
        })
    }
}
