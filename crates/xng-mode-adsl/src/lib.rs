//! Native ADS-L (EASA SRD860 i-Conspicuity) message decoder.
//!
//! ADS-L is the open, low-power direct-broadcast electronic-conspicuity
//! standard published by EASA (ED Decision 2022/024/R, "Technical
//! Specification for ADS-L transmissions using SRD860"), the
//! FLARM/OGN-adjacent format carried on the 868 MHz SRD860 band at 100 kbps
//! 2-FSK. This crate is the **message/frame decoder**: it takes a received
//! ADS-L packet (the on-wire bytes after Manchester/de-whitening and sync
//! detection) and decodes the iConspicuity payload — address, position,
//! altitude, velocity, track, aircraft category and the integrity/source
//! fields — into structured JSON.
//!
//! Pipeline:
//!
//! ```text
//! packet bytes → [Frame::parse] (length, CRC-24, XXTEA descramble)
//!              → [IConspicuity::decode] (bit/field decode)
//!              → serde_json::Value
//! ```
//!
//! The 868 MHz IQ → bits demodulator (2-FSK, Manchester whitening, IEEE
//! sync word) is a documented TODO; this crate ships the verified decode
//! layer. See PROVENANCE.md for the clean-room sourcing of every protocol
//! fact (the EASA spec field layout + the OGN/SoftRF reference encoder).
//!
//! Note: in the xng mode roadmap this is the item tracked as "ADS-K",
//! interpreted as ADS-L (EASA SRD860 i-Conspicuity).

pub mod crc;
pub mod vr;
pub mod xxtea;

use serde::{Deserialize, Serialize};

/// Bytes of the scrambled/payload section (5 × 32-bit words).
pub const PAYLOAD_BYTES: usize = 20;
/// XXTEA mixing rounds used for the ADS-L payload scramble.
pub const XXTEA_LOOPS: u32 = 6;
/// `Length` byte value the OGN/SoftRF framing prepends: 24 = 0x18 (Version
/// + 20 payload bytes + 3 CRC bytes, excluding the length byte itself).
pub const LENGTH_FIELD: u8 = 24;
/// Payload-Type-Identifier value for the iConspicuity payload (§F.2.1).
pub const TYPE_ICONSPICUITY: u8 = 0x02;

/// Errors from frame parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Not enough bytes for Version + 20 payload + 3 CRC.
    TooShort,
    /// CRC-24 residue was non-zero.
    BadCrc,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooShort => write!(f, "frame too short"),
            FrameError::BadCrc => write!(f, "CRC-24 mismatch"),
        }
    }
}

impl std::error::Error for FrameError {}

/// A parsed ADS-L frame: the Version byte plus the descrambled 20-byte
/// payload. The 24-bit CRC has already been verified.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Version byte (Version[4]/Signature[1]/Key[2]/Reserved[1]).
    pub version: u8,
    /// Descrambled 20-byte payload (Type, Address, Meta, Position,
    /// Integrity), still in wire byte order.
    pub payload: [u8; PAYLOAD_BYTES],
}

/// Read four bytes little-endian into a 32-bit word (OGN `get4bytes`).
#[inline]
pub fn word_from_le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

/// Write a 32-bit word as four little-endian bytes (OGN `set4bytes`).
#[inline]
pub fn word_to_le(w: u32, out: &mut [u8]) {
    out[0] = w as u8;
    out[1] = (w >> 8) as u8;
    out[2] = (w >> 16) as u8;
    out[3] = (w >> 24) as u8;
}

/// Pack the 20 payload bytes into the five little-endian 32-bit words that
/// XXTEA operates on.
pub fn words_from_le(payload: &[u8; PAYLOAD_BYTES]) -> [u32; 5] {
    let mut w = [0u32; 5];
    for (i, word) in w.iter_mut().enumerate() {
        *word = word_from_le(&payload[i * 4..]);
    }
    w
}

/// Unpack five little-endian words back into 20 payload bytes.
pub fn words_to_le(words: &[u32; 5]) -> [u8; PAYLOAD_BYTES] {
    let mut p = [0u8; PAYLOAD_BYTES];
    for (i, &word) in words.iter().enumerate() {
        word_to_le(word, &mut p[i * 4..]);
    }
    p
}

impl Frame {
    /// Read 3 bytes little-endian (OGN `get3bytes`) and sign-extend the
    /// 24-bit value.
    #[inline]
    fn signed24(b: &[u8]) -> i32 {
        let raw = (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16);
        (raw << 8) >> 8 // sign-extend from bit 23
    }

    /// Parse a received ADS-L packet.
    ///
    /// `bytes` is the de-whitened on-wire content **after** the sync word:
    /// the Version byte, the (scrambled) 20-byte payload, then 3 CRC bytes.
    /// Some framings (OGN/SoftRF) prepend a Length byte; if `bytes` begins
    /// with the fixed `LENGTH_FIELD` (0x18) and is one byte longer than
    /// expected, that leading Length byte is skipped automatically.
    pub fn parse(bytes: &[u8]) -> Result<Frame, FrameError> {
        // Expected minimum: Version(1) + payload(20) + CRC(3) = 24 bytes.
        const FRAME_LEN: usize = 1 + PAYLOAD_BYTES + 3;
        let body = if bytes.len() == FRAME_LEN + 1 && bytes[0] == LENGTH_FIELD {
            &bytes[1..]
        } else {
            bytes
        };
        if body.len() < FRAME_LEN {
            return Err(FrameError::TooShort);
        }
        let body = &body[..FRAME_LEN];

        // CRC-24 covers Version + payload + the 3 CRC bytes; residue == 0.
        if crc::check(body) != 0 {
            return Err(FrameError::BadCrc);
        }

        let version = body[0];
        let mut scrambled = [0u8; PAYLOAD_BYTES];
        scrambled.copy_from_slice(&body[1..1 + PAYLOAD_BYTES]);

        let mut words = words_from_le(&scrambled);
        xxtea::decrypt_key0(&mut words, XXTEA_LOOPS);
        let payload = words_to_le(&words);

        Ok(Frame { version, payload })
    }

    /// Payload Type Identifier (§F.2.1). 0x02 = iConspicuity broadcast;
    /// bit 7 set marks a unicast payload.
    pub fn payload_type(&self) -> u8 {
        self.payload[0]
    }

    /// 6-bit Address Mapping Table value (§F.2.2).
    pub fn address_table(&self) -> u8 {
        self.payload[1] & 0x3F
    }

    /// 24-bit sender address (§F.2.2). The 30-bit "Sender Address" field is
    /// `AMT(6) | Address(24)`; this returns the 24-bit Address.
    pub fn address(&self) -> u32 {
        (word_from_le(&self.payload[1..5]) >> 6) & 0x00FF_FFFF
    }

    /// Relay/forward flag (§F.2, bit 39): packet retransmitted on behalf of
    /// the sender.
    pub fn relay(&self) -> bool {
        self.payload[4] & 0x80 != 0
    }

    /// Decode the iConspicuity payload, if this frame carries one.
    pub fn iconspicuity(&self) -> Option<IConspicuity> {
        if self.payload_type() & 0x7F != TYPE_ICONSPICUITY {
            return None;
        }
        Some(IConspicuity::decode(self))
    }
}

/// Human-readable name for an Address Mapping Table value (§F.2.2).
pub fn address_type_name(amt: u8) -> &'static str {
    match amt {
        0 => "random/privacy",
        5 => "icao",
        6 => "flarm",
        7 => "ogn",
        8 => "fanet",
        9..=63 => "manufacturer",
        _ => "reserved",
    }
}

/// Flight-state enumeration (§G.1.2).
pub fn flight_state_name(v: u8) -> &'static str {
    match v {
        0 => "undefined",
        1 => "on_ground",
        2 => "airborne",
        _ => "reserved",
    }
}

/// Aircraft-category enumeration (§G.1.3).
pub fn aircraft_category_name(v: u8) -> &'static str {
    match v {
        0 => "none",
        1 => "light_fixed_wing",
        2 => "small_to_heavy_fixed_wing",
        3 => "rotorcraft",
        4 => "glider",
        5 => "lighter_than_air",
        6 => "ultralight",
        7 => "hang_glider_paraglider",
        8 => "skydiver",
        9 => "evtol_uam",
        10 => "gyrocopter",
        11 => "uas_open",
        12 => "uas_specific",
        13 => "uas_certified",
        _ => "reserved",
    }
}

/// Emergency-status enumeration (§G.1.4).
pub fn emergency_status_name(v: u8) -> &'static str {
    match v {
        0 => "undefined",
        1 => "no_emergency",
        2 => "general_emergency",
        3 => "lifeguard_medical",
        4 => "no_communications",
        5 => "unlawful_interference",
        6 => "downed_aircraft",
        _ => "reserved",
    }
}

/// Latitude LSB in degrees (§G.1.5): 1° / 93206.
pub const LAT_LSB_DEG: f64 = 1.0 / 93206.0;
/// Longitude LSB in degrees (§G.1.5): 1° / 46603.
pub const LON_LSB_DEG: f64 = 1.0 / 46603.0;
/// Ground-speed LSB in m/s (§G.1.8).
pub const SPEED_LSB_MPS: f64 = 0.25;
/// Altitude offset in metres (§G.1.7): −320 m floor.
pub const ALT_OFFSET_M: i32 = 320;
/// Vertical-rate LSB in m/s (§G.1.9).
pub const CLIMB_LSB_MPS: f64 = 0.125;
/// Ground-track LSB in degrees (§G.1.10): 360 / 512.
pub const TRACK_LSB_DEG: f64 = 360.0 / 512.0;
/// Sentinel raw 24-bit lat/lon value for "no position fix" (§G.1.5).
pub const NO_FIX_RAW: u32 = 0x00FF_FFFF;

/// A fully decoded ADS-L iConspicuity payload (§G.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IConspicuity {
    /// 24-bit sender address.
    pub address: u32,
    /// Address Mapping Table value (§F.2.2).
    pub address_table: u8,
    /// Address-type name (icao / flarm / ogn / fanet / random / …).
    pub address_type: &'static str,
    /// Packet was relayed/forwarded.
    pub relay: bool,
    /// Timestamp in quarter-seconds since the full hour, modulo 60 s
    /// (§G.1.1); values 60..63 are invalid.
    pub timestamp_q: u8,
    /// Timestamp in seconds (`timestamp_q × 0.25`); `None` if invalid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_s: Option<f64>,
    /// Flight state value (§G.1.2).
    pub flight_state: u8,
    /// Flight-state name.
    pub flight_state_name: &'static str,
    /// Aircraft category value (§G.1.3).
    pub aircraft_category: u8,
    /// Aircraft-category name.
    pub aircraft_category_name: &'static str,
    /// Emergency status value (§G.1.4).
    pub emergency: u8,
    /// Emergency-status name.
    pub emergency_name: &'static str,
    /// Latitude in degrees (WGS-84), or `None` if no fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude_deg: Option<f64>,
    /// Longitude in degrees (WGS-84), or `None` if no fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude_deg: Option<f64>,
    /// Ground speed in m/s (§G.1.8).
    pub ground_speed_mps: f64,
    /// Geometric altitude above the WGS-84 ellipsoid, in metres (§G.1.7).
    pub altitude_hae_m: i32,
    /// Vertical rate in m/s, positive up (§G.1.9); `None` if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_rate_mps: Option<f64>,
    /// Ground track in degrees clockwise from north (§G.1.10).
    pub ground_track_deg: f64,
    /// Source Integrity Level (SIL), 0..3 (§G.1.11).
    pub source_integrity: u8,
    /// Design Assurance Level (SDA), 0..3 (§G.1.12).
    pub design_assurance: u8,
    /// Navigation Integrity Category (NIC), 0..12 (§G.1.13).
    pub navigation_integrity: u8,
    /// Horizontal Position Accuracy (NACp), 0..7 (§G.1.14).
    pub horizontal_accuracy: u8,
    /// Vertical Position Accuracy (GVA), 0..3 (§G.1.15).
    pub vertical_accuracy: u8,
    /// Velocity Accuracy (NACv), 0..3 (§G.1.16).
    pub velocity_accuracy: u8,
}

impl IConspicuity {
    /// Decode the iConspicuity fields from a parsed [`Frame`]. Byte offsets
    /// follow the OGN/SoftRF `ADSL_Packet` struct: payload byte 0 = Type,
    /// 1..4 = Address, 5..6 = Meta, 7..17 = Position, 18..19 = Integrity.
    pub fn decode(frame: &Frame) -> IConspicuity {
        let p = &frame.payload;

        // Meta (bytes 5..6): TimeStamp[6]/FlightState[2] then AcftCat[5]/Emergency[3].
        let timestamp_q = p[5] & 0x3F;
        let flight_state = p[5] >> 6;
        let aircraft_category = p[6] & 0x1F;
        let emergency = p[6] >> 5;

        let timestamp_s = if timestamp_q < 60 {
            Some(timestamp_q as f64 * 0.25)
        } else {
            None
        };

        // Position (bytes 7..17): Lat[24] Lon[24] Speed[8] Alt[14] Climb[9] Track[9].
        let pos = &p[7..18];
        let lat_raw = Self::raw24(&pos[0..3]);
        let lon_raw = Self::raw24(&pos[3..6]);
        let (latitude_deg, longitude_deg) = if lat_raw == NO_FIX_RAW || lon_raw == NO_FIX_RAW {
            (None, None)
        } else {
            (
                Some(Frame::signed24(&pos[0..3]) as f64 * LAT_LSB_DEG),
                Some(Frame::signed24(&pos[3..6]) as f64 * LON_LSB_DEG),
            )
        };

        let ground_speed_mps = vr::uns_decode(pos[6] as u32, 6) as f64 * SPEED_LSB_MPS;

        // Altitude: 14-bit field split (Position[8] low 6 bits) << 8 | Position[7].
        let alt_word = (((pos[8] & 0x3F) as u32) << 8) | pos[7] as u32;
        let altitude_hae_m = vr::uns_decode(alt_word, 12) as i32 - ALT_OFFSET_M;

        // Climb: 9-bit field (Position[9] low 7 bits) << 2 | Position[8] >> 6.
        let climb_word = (((pos[9] & 0x7F) as u32) << 2) | (pos[8] as u32 >> 6);
        let vertical_rate_mps = if climb_word == 0x100 {
            None // declared absent (§G.1.9 special value)
        } else {
            Some(vr::sign_decode(climb_word, 6) as f64 * CLIMB_LSB_MPS)
        };

        // Track: 9-bit field Position[10] << 1 | Position[9] >> 7.
        let track_word = ((pos[10] as u32) << 1) | (pos[9] as u32 >> 7);
        let ground_track_deg = track_word as f64 * TRACK_LSB_DEG;

        // Integrity (bytes 18..19): SIL[2] DA[2] NIC[4] / NACp[3] GVA[2] NACv[2] Rsvd[1].
        let i0 = p[18];
        let i1 = p[19];
        let source_integrity = i0 & 0x3;
        let design_assurance = (i0 >> 2) & 0x3;
        let navigation_integrity = i0 >> 4;
        let horizontal_accuracy = i1 & 0x7;
        let vertical_accuracy = (i1 >> 3) & 0x3;
        let velocity_accuracy = (i1 >> 5) & 0x3;

        IConspicuity {
            address: frame.address(),
            address_table: frame.address_table(),
            address_type: address_type_name(frame.address_table()),
            relay: frame.relay(),
            timestamp_q,
            timestamp_s,
            flight_state,
            flight_state_name: flight_state_name(flight_state),
            aircraft_category,
            aircraft_category_name: aircraft_category_name(aircraft_category),
            emergency,
            emergency_name: emergency_status_name(emergency),
            latitude_deg,
            longitude_deg,
            ground_speed_mps,
            altitude_hae_m,
            vertical_rate_mps,
            ground_track_deg,
            source_integrity,
            design_assurance,
            navigation_integrity,
            horizontal_accuracy,
            vertical_accuracy,
            velocity_accuracy,
        }
    }

    /// Serialize the decoded payload to a JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("IConspicuity serializes")
    }

    /// Read the raw (unsigned) 24-bit little-endian value, used for the
    /// "no fix" sentinel test.
    #[inline]
    fn raw24(b: &[u8]) -> u32 {
        (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_type_names() {
        assert_eq!(address_type_name(5), "icao");
        assert_eq!(address_type_name(6), "flarm");
        assert_eq!(address_type_name(7), "ogn");
        assert_eq!(address_type_name(8), "fanet");
        assert_eq!(address_type_name(0), "random/privacy");
        assert_eq!(address_type_name(20), "manufacturer");
    }

    #[test]
    fn too_short_is_error() {
        assert!(matches!(
            Frame::parse(&[0u8; 10]),
            Err(FrameError::TooShort)
        ));
    }
}
