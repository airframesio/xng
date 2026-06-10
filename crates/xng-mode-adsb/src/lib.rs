//! Native Mode S / ADS-B (1090 MHz) decode core.
//!
//! Unlike the channelized modes, Mode S is a single wideband signal
//! processed in the magnitude domain: capture at 1090 MHz → PPM pulse
//! demod ([`demod::PpmDemod`]) → CRC-24 validation with an ICAO cache for
//! address-overlaid parity ([`frame`]) → basic extended-squitter decode
//! (ident, altitude) → [`xng_types::Message`]. Deep BDS/position decoding
//! layers on later.
//!
//! See PROVENANCE.md for the clean-room sourcing of every protocol fact.

pub mod demod;
pub mod frame;
pub mod modulate;

use chrono::Utc;
use num_complex::Complex;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Decodes Mode S from a capture centered on 1090 MHz.
pub struct AdsbDecoder {
    demod: demod::PpmDemod,
}

impl AdsbDecoder {
    /// `input_rate` must give an even integer number of samples per µs
    /// (2.0, 4.0, 8.0 MS/s ...). Use `-r 2000000` on an RTL-SDR.
    pub fn new(input_rate: f64) -> Result<Self, String> {
        Ok(Self { demod: demod::PpmDemod::new(input_rate)? })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<frame::AdsbFrame> {
        self.demod.process(input)
    }

    /// Smoothed noise-floor estimate in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.noise_dbfs()
    }
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &frame::AdsbFrame,
    frequency_hz: u64,
    source: Provenance,
) -> Message {
    Message {
        mode: Mode::Adsb,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(f.level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
        body: MessageBody::ModeS {
            df: f.df,
            icao: Some(format!("{:06X}", f.icao)),
            callsign: f.callsign.clone(),
            altitude_ft: f.altitude_ft,
        },
        raw: Some(f.bytes.clone()),
        source,
    }
}
