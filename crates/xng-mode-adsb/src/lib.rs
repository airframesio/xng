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

pub mod decode;
pub mod demod;
pub mod frame;
pub mod modulate;

use chrono::Utc;
use decode::Cpr;
use num_complex::Complex;
use std::collections::HashMap;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// CPR pairing window (Annex 10: even/odd frames within 10 s).
const CPR_PAIR_SECS: f64 = 10.0;
/// Reference-position freshness for locally unambiguous decode.
const CPR_LOCAL_SECS: f64 = 180.0;
const TRACK_MAX: usize = 4096;

#[derive(Default, Clone)]
struct AcState {
    even: Option<(Cpr, f64)>,
    odd: Option<(Cpr, f64)>,
    last_pos: Option<(f64, f64, f64)>,
}

/// Decodes Mode S from a capture centered on 1090 MHz.
pub struct AdsbDecoder {
    demod: demod::PpmDemod,
    input_rate: f64,
    samples_seen: u64,
    track: HashMap<u32, AcState>,
}

impl AdsbDecoder {
    /// `input_rate` must give an even integer number of samples per µs
    /// (2.0, 4.0, 8.0 MS/s ...). Use `-r 2000000` on an RTL-SDR.
    pub fn new(input_rate: f64) -> Result<Self, String> {
        Ok(Self {
            demod: demod::PpmDemod::new(input_rate)?,
            input_rate,
            samples_seen: 0,
            track: HashMap::new(),
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<frame::AdsbFrame> {
        self.samples_seen += input.len() as u64;
        let now = self.samples_seen as f64 / self.input_rate;
        let mut frames = self.demod.process(input);
        for f in &mut frames {
            if let Some(cpr) = f.cpr {
                f.position = self.resolve_position(f.icao, cpr, now);
            }
        }
        frames
    }

    /// Resolve a CPR report: locally against the aircraft's fresh last
    /// fix when available, else globally from a fresh even/odd pair
    /// (airborne only — surface global needs a receiver reference).
    fn resolve_position(&mut self, icao: u32, cpr: Cpr, now: f64) -> Option<(f64, f64)> {
        if self.track.len() >= TRACK_MAX {
            self.track.retain(|_, s| {
                s.last_pos.map_or(false, |(_, _, t)| now - t < CPR_LOCAL_SECS)
            });
        }
        let st = self.track.entry(icao).or_default();
        if cpr.odd {
            st.odd = Some((cpr, now));
        } else {
            st.even = Some((cpr, now));
        }

        let pos = match st.last_pos {
            Some((rlat, rlon, t)) if now - t < CPR_LOCAL_SECS => {
                Some(decode::cpr_local(cpr, rlat, rlon))
            }
            _ if !cpr.surface => match (st.even, st.odd) {
                (Some((e, te)), Some((o, to))) if (te - to).abs() < CPR_PAIR_SECS => {
                    decode::cpr_global_airborne(e, o, to >= te)
                }
                _ => None,
            },
            _ => None,
        };
        let pos = pos.filter(|(lat, lon)| (-90.0..=90.0).contains(lat) && (-180.0..=180.0).contains(lon));
        if let Some((lat, lon)) = pos {
            st.last_pos = Some((lat, lon, now));
        }
        pos
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
            squawk: f.squawk.clone(),
            lat: f.position.map(|p| p.0),
            lon: f.position.map(|p| p.1),
            speed_kt: f.velocity.map(|v| v.speed_kt),
            speed_type: f.velocity.map(|v| if v.airspeed { "AS".into() } else { "GS".into() }),
            track_deg: f.velocity.map(|v| v.track_deg),
            vertical_rate_fpm: f.velocity.and_then(|v| v.vertical_rate_fpm),
        },
        raw: Some(f.bytes.clone()),
        source,
    }
}

#[cfg(test)]
mod tracker_tests {
    use super::*;

    // CPR pair from "The 1090 Megahertz Riddle" (ICAO 40621D).
    const EVEN: Cpr = Cpr { odd: false, lat: 93000, lon: 51372, surface: false };
    const ODD: Cpr = Cpr { odd: true, lat: 74158, lon: 50194, surface: false };

    #[test]
    fn tracker_resolves_globally_then_locally() {
        let mut dec = AdsbDecoder::new(2_000_000.0).unwrap();
        // Even alone: no position yet.
        assert_eq!(dec.resolve_position(0x40621D, EVEN, 0.0), None);
        // Odd within the pairing window: global decode. The newest
        // (odd) frame's own fix is reported — the aircraft moved a
        // little between the pair, so it sits ~0.01° from the even fix
        // the decode.rs vector test pins exactly.
        let (lat, lon) = dec.resolve_position(0x40621D, ODD, 1.0).unwrap();
        assert!((lat - 52.266).abs() < 0.01, "{lat}");
        assert!((lon - 3.939).abs() < 0.01, "{lon}");
        // A later lone frame resolves locally off the cached fix.
        let (lat2, lon2) = dec.resolve_position(0x40621D, EVEN, 30.0).unwrap();
        assert!((lat2 - 52.2572).abs() < 1e-3, "{lat2}");
        assert!((lon2 - 3.91937).abs() < 1e-3, "{lon2}");
    }

    #[test]
    fn stale_pairs_do_not_resolve() {
        let mut dec = AdsbDecoder::new(2_000_000.0).unwrap();
        assert_eq!(dec.resolve_position(0x40621D, EVEN, 0.0), None);
        // 60 s later: outside the 10 s pairing window, no reference.
        assert_eq!(dec.resolve_position(0x40621D, ODD, 60.0), None);
    }
}
