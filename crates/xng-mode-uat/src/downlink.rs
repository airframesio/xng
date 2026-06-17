//! UAT downlink ADS-B message (MDB) decoder — DO-282B §2.2.4.5.
//!
//! Field offsets, scaling, and the payload-type → element-set mapping
//! follow FlightAware dump978's `uat_message.cc` (`AdsbMessage`), which is
//! the maintained reference and whose JSON this module reproduces. The
//! 1-based `(byte, bit)` addressing matches DO-282B's field tables.
//!
//! A short payload is 18 bytes (HDR + SV); a long payload is 34 bytes
//! (HDR + SV + the optional MS / AUX SV / TS elements selected by the
//! MDB/payload type).

use crate::bits::BitReader;
use serde::Serialize;

fn round_n(v: f64, n: i32) -> f64 {
    let f = 10f64.powi(n);
    (v * f).round() / f
}

/// 2.2.4.5.1.2 ADDRESS QUALIFIER.
pub fn address_qualifier_name(q: u32) -> &'static str {
    match q {
        0 => "adsb_icao",
        1 => "adsb_other",
        2 => "tisb_icao",
        3 => "tisb_trackfile",
        4 => "vehicle",
        5 => "fixed_beacon",
        6 => "adsr_other",
        7 => "reserved",
        _ => "invalid",
    }
}

/// 2.2.4.5.2.5 A/G STATE.
pub fn airground_name(s: u32) -> &'static str {
    match s {
        0 => "airborne",
        1 => "supersonic",
        2 => "ground",
        3 => "reserved",
        _ => "invalid",
    }
}

fn vv_src_name(s: u32) -> &'static str {
    match s {
        0 => "geometric",
        1 => "barometric",
        _ => "invalid",
    }
}

fn emergency_name(e: u32) -> &'static str {
    match e {
        0 => "none",
        1 => "general",
        2 => "medical",
        3 => "minfuel",
        4 => "nordo",
        5 => "unlawful",
        6 => "downed",
        7 => "reserved",
        _ => "invalid",
    }
}

fn sil_supplement_name(s: u32) -> &'static str {
    match s {
        0 => "per_hour",
        1 => "per_sample",
        _ => "invalid",
    }
}

fn selected_altitude_type_name(t: u32) -> &'static str {
    match t {
        0 => "mcp_fcu",
        1 => "fms",
        _ => "invalid",
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AircraftSize {
    pub length: f64,
    pub width: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CapabilityCodes {
    pub uat_in: bool,
    pub es_in: bool,
    pub tcas_operational: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OperationalModes {
    pub tcas_ra_active: bool,
    pub ident_active: bool,
    pub atc_services: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModeIndicators {
    pub autopilot: bool,
    pub vnav: bool,
    pub altitude_hold: bool,
    pub approach: bool,
    pub lnav: bool,
}

/// A decoded UAT downlink ADS-B message. Optional fields are present only
/// when the corresponding element is present in the payload and carries
/// data (mirroring dump978's "emit if present" JSON).
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct UatDownlink {
    pub address_qualifier: String,
    /// ICAO/UAT address, 6 hex digits.
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressure_altitude: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geometric_altitude: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nic: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub airground_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub north_velocity: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub east_velocity: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vv_src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_velocity_barometric: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_velocity_geometric: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_speed: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnetic_heading: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_heading: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_track: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_size: Option<AircraftSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_lateral_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_longitudinal_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_position_offset_applied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utc_coupled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uplink_feedback: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tisb_site_id: Option<u32>,
    // Mode Status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitter_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flightplan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emergency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mops_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sil: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transmit_mso: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sda: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nac_p: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nac_v: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nic_baro: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_codes: Option<CapabilityCodes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operational_modes: Option<OperationalModes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sil_supplement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gva: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_antenna: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nic_supplement: Option<bool>,
    // Target State
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_altitude_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_altitude_mcp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_altitude_fms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barometric_pressure_setting: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_heading: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_indicators: Option<ModeIndicators>,
    /// MDB / payload type (HDR bits 1..5).
    pub payload_type: u32,
}

const BASE40: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ *??";

impl UatDownlink {
    /// Decode an 18- or 34-byte downlink payload (parity already stripped).
    pub fn decode(payload: &[u8]) -> Result<UatDownlink, &'static str> {
        if payload.len() != 18 && payload.len() != 34 {
            return Err("downlink payload must be 18 or 34 bytes");
        }
        let r = BitReader::new(payload);
        let mut m = UatDownlink::default();

        // HDR (§2.2.4.5.1)
        let payload_type = r.bits(1, 1, 1, 5);
        let address_qualifier = r.bits(1, 6, 1, 8);
        let address = r.bits(2, 1, 4, 8);
        m.payload_type = payload_type;
        m.address_qualifier = address_qualifier_name(address_qualifier).to_string();
        m.address = format!("{address:06x}");

        // DO-282B Table 2-10 "Composition of the ADS-B Payload".
        match payload_type {
            0 => m.decode_sv(&r, address_qualifier),
            1 => {
                m.decode_sv(&r, address_qualifier);
                m.decode_ms(&r);
                m.decode_auxsv(&r);
            }
            2 => {
                m.decode_sv(&r, address_qualifier);
                m.decode_auxsv(&r);
            }
            3 => {
                m.decode_sv(&r, address_qualifier);
                m.decode_ms(&r);
                m.decode_ts(&r, 30);
            }
            4 => {
                m.decode_sv(&r, address_qualifier);
                m.decode_ts(&r, 30);
            }
            5 => {
                m.decode_sv(&r, address_qualifier);
                m.decode_auxsv(&r);
            }
            6 => {
                m.decode_sv(&r, address_qualifier);
                m.decode_ts(&r, 25);
                m.decode_auxsv(&r);
            }
            7..=10 => m.decode_sv(&r, address_qualifier),
            _ => { /* 11..31: HDR only */ }
        }
        Ok(m)
    }

    fn decode_sv(&mut self, r: &BitReader, address_qualifier: u32) {
        let raw_lat = r.bits(5, 1, 7, 7);
        let raw_lon = r.bits(7, 8, 10, 7);

        let raw_alt = r.bits(11, 1, 12, 4);
        if raw_alt != 0 {
            let altitude = (raw_alt as i32 - 41) * 25;
            if r.bit(10, 8) {
                self.geometric_altitude = Some(altitude);
            } else {
                self.pressure_altitude = Some(altitude);
            }
        }

        let nic = r.bits(12, 5, 12, 8);
        self.nic = Some(nic);

        if raw_lat != 0 || raw_lon != 0 || nic != 0 {
            let mut lat = raw_lat as f64 * 360.0 / 16_777_216.0;
            if lat > 90.0 {
                lat -= 180.0;
            }
            let mut lon = raw_lon as f64 * 360.0 / 16_777_216.0;
            if lon > 180.0 {
                lon -= 360.0;
            }
            self.position = Some(Position { lat: round_n(lat, 5), lon: round_n(lon, 5) });
        }

        let ag = r.bits(13, 1, 13, 2);
        self.airground_state = Some(airground_name(ag).to_string());

        match ag {
            0 | 1 => {
                // airborne subsonic / supersonic
                let supersonic = if ag == 1 { 4 } else { 1 };
                let ns_sign = if r.bit(13, 4) { -1 } else { 1 };
                let raw_ns = r.bits(13, 5, 14, 6);
                if raw_ns != 0 {
                    self.north_velocity = Some(supersonic * ns_sign * (raw_ns as i32 - 1));
                }
                let ew_sign = if r.bit(14, 7) { -1 } else { 1 };
                let raw_ew = r.bits(14, 8, 16, 1);
                if raw_ew != 0 {
                    self.east_velocity = Some(supersonic * ew_sign * (raw_ew as i32 - 1));
                }
                if let (Some(n), Some(e)) = (self.north_velocity, self.east_velocity) {
                    let gs = round_n(((n * n + e * e) as f64).sqrt(), 1);
                    self.ground_speed = Some(gs as i32);
                    let mut angle = (e as f64).atan2(n as f64) * 180.0 / std::f64::consts::PI;
                    if angle < 0.0 {
                        angle += 360.0;
                    }
                    self.true_track = Some(round_n(angle, 1));
                }

                let vv_src = r.bits(16, 2, 16, 2);
                self.vv_src = Some(vv_src_name(vv_src).to_string());
                let vv_sign = if r.bit(16, 3) { -1 } else { 1 };
                let raw_vv = r.bits(16, 4, 17, 4);
                if raw_vv != 0 {
                    let vv = vv_sign * (raw_vv as i32 - 1) * 64;
                    match vv_src {
                        1 => self.vertical_velocity_barometric = Some(vv),
                        0 => self.vertical_velocity_geometric = Some(vv),
                        _ => {}
                    }
                }
            }
            2 => {
                // on ground
                let raw_gs = r.bits(13, 5, 14, 6);
                if raw_gs != 0 {
                    self.ground_speed = Some(raw_gs as i32 - 1);
                }
                let tah_type = r.bits(14, 7, 14, 8);
                let angle = round_n(r.bits(15, 1, 16, 1) as f64 * 360.0 / 512.0, 1);
                match tah_type {
                    1 => self.true_track = Some(angle),
                    2 => self.magnetic_heading = Some(angle),
                    3 => self.true_heading = Some(angle),
                    _ => {}
                }
                let raw_av_size = r.bits(16, 2, 16, 5);
                if raw_av_size != 0 {
                    // DO-282B Table 2-35.
                    const SIZES: [(f64, f64); 16] = [
                        (0.0, 0.0),
                        (15.0, 23.0),
                        (25.0, 28.5),
                        (25.0, 34.0),
                        (35.0, 33.0),
                        (35.0, 38.0),
                        (45.0, 39.5),
                        (45.0, 45.0),
                        (55.0, 45.0),
                        (55.0, 52.0),
                        (65.0, 59.5),
                        (65.0, 67.0),
                        (75.0, 72.5),
                        (75.0, 80.0),
                        (85.0, 80.0),
                        (85.0, 90.0),
                    ];
                    let (l, w) = SIZES[raw_av_size as usize];
                    self.aircraft_size = Some(AircraftSize { length: l, width: w });
                }
                if r.bit(16, 7) {
                    let raw_gps_long = r.bits(16, 8, 17, 4);
                    if raw_gps_long != 0 {
                        if raw_gps_long == 1 {
                            self.gps_position_offset_applied = Some(true);
                        } else {
                            self.gps_position_offset_applied = Some(false);
                            self.gps_longitudinal_offset = Some((raw_gps_long as f64 - 1.0) * 2.0);
                        }
                    }
                } else {
                    let raw_gps_lat = r.bits(16, 8, 17, 2);
                    if raw_gps_lat != 0 {
                        if raw_gps_lat <= 3 {
                            self.gps_lateral_offset = Some(raw_gps_lat as f64 * -2.0);
                        } else {
                            self.gps_lateral_offset = Some((raw_gps_lat as f64 - 4.0) * 2.0);
                        }
                    }
                }
            }
            _ => {}
        }

        match address_qualifier {
            0 | 1 | 4 | 5 => {
                self.utc_coupled = Some(r.bit(17, 5));
                self.uplink_feedback = Some(r.bits(17, 6, 17, 8));
            }
            2 | 3 | 6 => {
                self.tisb_site_id = Some(r.bits(17, 5, 17, 8));
            }
            _ => {}
        }
    }

    fn decode_ts(&mut self, r: &BitReader, startbyte: usize) {
        let raw_altitude = r.bits(startbyte, 2, startbyte + 1, 4);
        if raw_altitude != 0 {
            let sat = r.bits(startbyte, 1, startbyte, 1);
            self.selected_altitude_type = Some(selected_altitude_type_name(sat).to_string());
            match sat {
                0 => self.selected_altitude_mcp = Some((raw_altitude as i32 - 1) * 32),
                1 => self.selected_altitude_fms = Some((raw_altitude as i32 - 1) * 32),
                _ => {}
            }
        }
        let raw_bps = r.bits(startbyte + 1, 5, startbyte + 2, 5);
        if raw_bps != 0 {
            self.barometric_pressure_setting = Some(800.0 + (raw_bps as f64 - 1.0) * 0.8);
        }
        if r.bit(startbyte + 2, 6) {
            let heading_sign = if r.bit(startbyte + 2, 7) { -1.0 } else { 1.0 };
            let heading = round_n(r.bits(startbyte + 2, 8, startbyte + 3, 7) as f64 * 180.0 / 256.0, 1);
            self.selected_heading = Some(heading_sign * heading);
        }
        if r.bit(startbyte + 3, 8) {
            self.mode_indicators = Some(ModeIndicators {
                autopilot: r.bit(startbyte + 4, 1),
                vnav: r.bit(startbyte + 4, 2),
                altitude_hold: r.bit(startbyte + 4, 3),
                approach: r.bit(startbyte + 4, 4),
                lnav: r.bit(startbyte + 4, 5),
            });
        }
    }

    fn decode_ms(&mut self, r: &BitReader) {
        let raw1 = r.bits(18, 1, 19, 8);
        let raw2 = r.bits(20, 1, 21, 8);
        let raw3 = r.bits(22, 1, 23, 8);

        let emitter = ((raw1 / 1600) % 40) as u8;
        let mut cs: Vec<u8> = Vec::with_capacity(8);
        cs.push(BASE40[((raw1 / 40) % 40) as usize]);
        cs.push(BASE40[(raw1 % 40) as usize]);
        cs.push(BASE40[((raw2 / 1600) % 40) as usize]);
        cs.push(BASE40[((raw2 / 40) % 40) as usize]);
        cs.push(BASE40[(raw2 % 40) as usize]);
        cs.push(BASE40[((raw3 / 1600) % 40) as usize]);
        cs.push(BASE40[((raw3 / 40) % 40) as usize]);
        cs.push(BASE40[(raw3 % 40) as usize]);
        // Trim trailing spaces and code-37 ('*').
        while matches!(cs.last(), Some(b' ') | Some(b'*')) {
            cs.pop();
        }
        if !cs.is_empty() {
            let text = String::from_utf8_lossy(&cs).to_string();
            if r.bit(27, 7) {
                self.callsign = Some(text);
            } else {
                // Flightplan ID (squawk): four octal digits.
                if text.len() == 4 && text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                    self.flightplan_id = Some(text);
                }
            }
        }

        // Emitter category as e.g. "A2" (set << 3 → letter, & 7 → digit).
        self.emitter_category = Some(format!("{}{}", (b'A' + (emitter >> 3)) as char, emitter & 7));

        self.emergency = Some(emergency_name(r.bits(24, 1, 24, 3)).to_string());
        self.mops_version = Some(r.bits(24, 4, 24, 6));
        self.sil = Some(r.bits(24, 7, 24, 8));
        self.transmit_mso = Some(r.bits(25, 1, 25, 6));
        self.sda = Some(r.bits(25, 7, 25, 8));
        self.nac_p = Some(r.bits(26, 1, 26, 4));
        self.nac_v = Some(r.bits(26, 5, 26, 7));
        self.nic_baro = Some(r.bits(26, 8, 26, 8));

        self.capability_codes = Some(CapabilityCodes {
            uat_in: r.bit(27, 1),
            es_in: r.bit(27, 2),
            tcas_operational: r.bit(27, 3),
        });
        self.operational_modes = Some(OperationalModes {
            tcas_ra_active: r.bit(27, 4),
            ident_active: r.bit(27, 5),
            atc_services: r.bit(27, 6),
        });
        self.sil_supplement = Some(sil_supplement_name(r.bits(27, 8, 27, 8)).to_string());
        self.gva = Some(r.bits(28, 1, 28, 2));
        self.single_antenna = Some(r.bit(28, 3));
        self.nic_supplement = Some(r.bit(28, 4));
    }

    fn decode_auxsv(&mut self, r: &BitReader) {
        let raw_alt = r.bits(30, 1, 31, 4);
        if raw_alt != 0 {
            let altitude = (raw_alt as i32 - 41) * 25;
            // The AUX SV altitude is the *other* altitude type vs the SV
            // (which is always present when AUX SV is present).
            if r.bit(10, 8) {
                self.pressure_altitude = Some(altitude);
            } else {
                self.geometric_altitude = Some(altitude);
            }
        }
    }

    /// Serialize to a JSON value matching the structured `UatDownlink`
    /// shape (no transport/metadata wrapper).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("UatDownlink serializes")
    }
}
