use actix_web::web::Data;
use log::*;
use soapysdr::Device;
use std::env;
use tokio::io;
use tokio::sync::RwLock;

use super::{HfdlModule, HFDL_COMMAND};
use crate::modules::{settings::ModuleSettings, XngModule};

const ENV_XNG_TEST_RATES: &'static str = "XNG_TEST_SAMPLERATES";

impl HfdlModule {
    pub fn new() -> Box<dyn XngModule> {
        Box::new(HfdlModule {
            name: HFDL_COMMAND,

            ..Default::default()
        })
    }

    pub fn get_settings(&self) -> Result<Data<RwLock<ModuleSettings>>, io::Error> {
        let Some(ref settings) = self.settings else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ModuleSettings is None",
            ));
        };
        Ok(settings.clone())
    }

    pub fn load_sample_rates(&mut self, driver: &String) -> Result<(), io::Error> {
        if let Ok(value) = env::var(ENV_XNG_TEST_RATES) {
            let mut test_rates: Vec<u64> = value
                .split(",")
                .map(|x| x.parse::<u64>().unwrap_or(0))
                .filter(|&x| x > 0)
                .collect();
            test_rates.sort_unstable();
            test_rates.dedup();

            debug!(
                "Found {} env var, using provided samples: {:?}",
                ENV_XNG_TEST_RATES, test_rates
            );
            self.sample_rates = test_rates;

            Ok(())
        } else {
            let dev = match Device::new(driver.as_str()) {
                Ok(x) => x,
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "Failed to open SoapySDR device {} ({}): is it plugged in?",
                            driver,
                            e.to_string()
                        ),
                    ))
                }
            };

            let chan_count = match dev.num_channels(soapysdr::Direction::Rx) {
                Ok(x) => x,
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "Failed to enumerate channels for Rx for SoapySDR device {}: {}",
                            driver,
                            e.to_string()
                        ),
                    ))
                }
            };
            if chan_count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("Unsupported SoapySDR device {} with no channels", driver),
                ));
            } else if chan_count > 1 {
                warn!(
                    "SoapySDR device {} has more than one receive channel!",
                    driver
                );
            }

            // TODO: support more channels but for now, only grab data for the first one
            let mut sample_rates = match dev.get_sample_rate_range(soapysdr::Direction::Rx, 0) {
                Ok(x) => x
                    .iter()
                    .map(|x| {
                        if x.minimum == x.maximum {
                            x.maximum as u64
                        } else {
                            0 as u64
                        }
                    })
                    .collect::<Vec<u64>>(),
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!(
                            "Failed to get sample rates for SoapySDR device {} on channel 0: {}",
                            driver,
                            e.to_string()
                        ),
                    ))
                }
            };

            if sample_rates.iter().any(|&x| x == 0) {
                warn!(
                    "SoapySDR device {} has different minimum/maximum sample rate range entry!",
                    driver
                );

                sample_rates = sample_rates.into_iter().filter(|&x| x > 0).collect();
            }

            sample_rates.sort_unstable();
            sample_rates.dedup();

            self.sample_rates = sample_rates;

            Ok(())
        }
    }

    pub fn nearest_sample_rate(&self, sample_rate: u64) -> Option<u64> {
        if let Some(idx) = self.sample_rates.iter().position(|&x| x >= sample_rate) {
            Some(self.sample_rates[idx])
        } else {
            None
        }
    }

    pub fn calculate_actual_sample_rate(&self, bands: &Vec<u16>) -> Option<u64> {
        let mut bands = bands.clone();
        bands.sort_unstable();

        match (bands.first(), bands.last()) {
            (Some(min_freq), Some(max_freq)) => {
                self.nearest_sample_rate(((max_freq - min_freq) as f64 * 1.2) as u64 * 1000)
            }
            _ => None,
        }
    }
}
