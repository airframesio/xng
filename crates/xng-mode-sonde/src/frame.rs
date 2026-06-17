//! RS41 frame / sub-block decoder.
//!
//! Decodes a de-whitened, post-FEC RS41 frame into structured fields. The
//! frame is a fixed 8-byte sync header followed by a chain of
//! `ID | LEN | DATA[LEN] | CRC16` sub-blocks. The sub-block IDs and the
//! field offsets within them are from rs1729/RS `rs41mod.c` (the `pos_*`
//! and `pck_*` defines) and the layout notes in `rs41.txt`.
//!
//! Sub-blocks (fixed positions; the chain is positionally fixed in the
//! standard frame):
//! - 0x039 `7928` STATUS:  frame#, sonde ID, battery, calibration sub-frame
//! - 0x065 `7A2A` PTU:     12 x 24-bit raw temperature/humidity/pressure
//! - 0x093 `7C1E` GPS-INFO: GPS week, time-of-week (RXM-RAW)
//! - 0x112 `7B15` GPS-POS:  ECEF position + velocity, #SVs (NAV-SOL)
//!
//! All multi-byte integers are little-endian.

use crate::crc::crc16;
use serde::{Deserialize, Serialize};

// --- sub-block positions (de-whitened frame offsets, rs1729/RS rs41mod.c) ---

/// STATUS sub-block start (pck `0x7928`).
pub const POS_STATUS: usize = 0x039;
/// Frame number (u16).
pub const POS_FRAME_NB: usize = 0x03B;
/// Sonde serial ID (8 ASCII bytes).
pub const POS_SONDE_ID: usize = 0x03D;
/// Battery voltage byte.
pub const POS_BATT: usize = 0x045;
/// Calibration sub-frame counter (0x00..0x32) then 16 config bytes.
pub const POS_CAL_DATA: usize = 0x052;

/// PTU sub-block start (pck `0x7A2A`).
pub const POS_PTU: usize = 0x065;
/// First of the 12 x 24-bit PTU raw measurements (after the 2-byte sub-header).
pub const POS_PTU_MEAS: usize = 0x067;
/// Signed-16 pressure auxiliary value (PTU offset +38 after the sub-header).
pub const POS_PTU_PAUX: usize = POS_PTU_MEAS + 38; // 0x08D

/// GPS-INFO sub-block start (pck `0x7C1E`, RXM-RAW).
pub const POS_GPS_INFO: usize = 0x093;
/// GPS full week number (u16).
pub const POS_GPS_WEEK: usize = 0x095;
/// GPS time-of-week in milliseconds (u32).
pub const POS_GPS_ITOW: usize = 0x097;

/// GPS-POS sub-block start (pck `0x7B15`, NAV-SOL).
pub const POS_GPS_POS: usize = 0x112;
/// ECEF X coordinate (i32, centimetres).
pub const POS_ECEF_X: usize = 0x114;
/// ECEF velocity (3 x i16, centimetres/second).
pub const POS_ECEF_V: usize = 0x120;
/// Number of satellites used in the navigation solution.
pub const POS_NUM_SV: usize = 0x126;

// --- sub-block packet IDs (rs1729/RS rs41mod.c pck_*) ---
pub const PCK_STATUS: u8 = 0x79;
pub const PCK_PTU: u8 = 0x7A;
pub const PCK_GPS_INFO: u8 = 0x7C;
pub const PCK_GPS_POS: u8 = 0x7B;

/// Standard (non-extended) RS41 frame length.
pub const STD_FRAME_LEN: usize = 320;
/// Maximum (extended / aux-xdata) RS41 frame length.
pub const MAX_FRAME_LEN: usize = 518;

// --- little-endian readers ---

fn u16le(f: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*f.get(p)?, *f.get(p + 1)?]))
}
fn u24le(f: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *f.get(p)?,
        *f.get(p + 1)?,
        *f.get(p + 2)?,
        0,
    ]))
}
fn u32le(f: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *f.get(p)?,
        *f.get(p + 1)?,
        *f.get(p + 2)?,
        *f.get(p + 3)?,
    ]))
}
fn i32le(f: &[u8], p: usize) -> Option<i32> {
    u32le(f, p).map(|v| v as i32)
}
fn i16le(f: &[u8], p: usize) -> Option<i16> {
    u16le(f, p).map(|v| v as i16)
}

/// Earth ellipsoid (WGS-84) constants used by [`ecef_to_geodetic`].
const EARTH_A: f64 = 6_378_137.0;
const EARTH_B: f64 = 6_356_752.314_245_18;

/// Convert ECEF metres to geodetic (lat°, lon°, alt m) on WGS-84.
///
/// Bowring-style closed-form solution, identical to rs1729/RS
/// `ecef2elli()`.
pub fn ecef_to_geodetic(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let a = EARTH_A;
    let b = EARTH_B;
    let a2_b2 = a * a - b * b;
    let e2 = a2_b2 / (a * a);
    let ee2 = a2_b2 / (b * b);

    let lam = y.atan2(x);
    let p = (x * x + y * y).sqrt();
    let t = (z * a).atan2(p * b);

    let phi = (z + ee2 * b * t.sin().powi(3)).atan2(p - e2 * a * t.cos().powi(3));

    let r = a / (1.0 - e2 * phi.sin().powi(2)).sqrt();
    let alt = p / phi.cos() - r;

    (phi.to_degrees(), lam.to_degrees(), alt)
}

/// Decoded GPS position (from the NAV-SOL sub-block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpsPos {
    /// Latitude, degrees (WGS-84).
    pub lat: f64,
    /// Longitude, degrees (WGS-84).
    pub lon: f64,
    /// Altitude above the ellipsoid, metres.
    pub alt_m: f64,
    /// Horizontal speed, m/s.
    pub speed_ms: f64,
    /// Course over ground, degrees (0 = north, clockwise).
    pub course_deg: f64,
    /// Vertical speed (climb positive), m/s.
    pub climb_ms: f64,
    /// Number of satellites used in the navigation solution.
    pub num_sv: u8,
}

/// Decoded GPS time (from the RXM-RAW sub-block).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpsTime {
    /// Full GPS week number (weeks since 1980-01-06).
    pub week: u16,
    /// Time of week, milliseconds since Sunday 00:00 GPS.
    pub tow_ms: u32,
}

/// Decoded PTU sub-block: the 12 raw 24-bit measurement channels plus the
/// per-frame calibration sub-frame.
///
/// The RS41's calibrated temperature/humidity/pressure require the full
/// 51-sub-frame calibration table (counter 0x00..0x32) reassembled over
/// ~51 consecutive frames; from a single frame only the raw channel
/// readings and this frame's one calibration sub-frame are available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ptu {
    /// 12 raw 24-bit channels: [0..3] main temperature ratio, [3..6]
    /// humidity, [6..9] humidity-sensor temperature, [9..12] pressure
    /// (sensor-dependent).
    pub raw: [u32; 12],
    /// Signed-16 pressure auxiliary value (sensor temperature for the
    /// pressure transducer on RS41-SGP).
    pub p_aux: i16,
    /// Calibration sub-frame counter carried in this frame (0x00..0x32).
    pub cal_index: u8,
    /// The 16 calibration/config bytes carried in this frame's sub-frame.
    pub cal_bytes: [u8; 16],
}

/// A fully decoded RS41 frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rs41Frame {
    /// Sonde serial number (e.g. "K1930293").
    pub serial: String,
    /// Frame counter.
    pub frame_num: u16,
    /// Battery voltage, volts.
    pub battery_v: f32,
    /// GPS time, when the GPS-INFO sub-block CRC checks.
    pub gps_time: Option<GpsTime>,
    /// GPS position, when the GPS-POS sub-block CRC checks.
    pub gps_pos: Option<GpsPos>,
    /// PTU measurements, when the PTU sub-block CRC checks.
    pub ptu: Option<Ptu>,
    /// Per-sub-block CRC results, for diagnostics.
    pub crc: CrcStatus,
}

/// Which sub-blocks passed their CRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CrcStatus {
    pub status: bool,
    pub ptu: bool,
    pub gps_info: bool,
    pub gps_pos: bool,
}

/// Errors that prevent decoding a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Frame is shorter than the standard 320 bytes.
    TooShort(usize),
    /// The 8-byte sync header does not match the RS41 constant.
    BadHeader,
    /// The STATUS sub-block CRC failed (serial / frame# unreliable).
    StatusCrcFailed,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort(n) => write!(f, "frame too short: {n} bytes"),
            DecodeError::BadHeader => write!(f, "bad RS41 sync header"),
            DecodeError::StatusCrcFailed => write!(f, "STATUS sub-block CRC failed"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Check the CRC of the sub-block whose header (`ID | LEN`) starts at
/// `pos`, against the expected packet ID. Returns true when the trailing
/// little-endian CRC matches the body.
fn check_subblock(frame: &[u8], pos: usize, pck: u8) -> bool {
    if frame.get(pos) != Some(&pck) {
        return false;
    }
    let Some(&len) = frame.get(pos + 1) else {
        return false;
    };
    let len = len as usize;
    let body_start = pos + 2;
    let crc_pos = body_start + len;
    if crc_pos + 2 > frame.len() {
        return false;
    }
    let Some(stored) = u16le(frame, crc_pos) else {
        return false;
    };
    stored == crc16(&frame[body_start..crc_pos])
}

/// Decode a de-whitened, post-FEC RS41 frame.
///
/// The STATUS sub-block (serial + frame#) must CRC-check, otherwise the
/// frame is rejected. The GPS and PTU sub-blocks are decoded only when
/// their own CRC checks; a failing one leaves the corresponding field
/// `None` rather than emitting garbage.
pub fn decode_frame(frame: &[u8]) -> Result<Rs41Frame, DecodeError> {
    if frame.len() < STD_FRAME_LEN {
        return Err(DecodeError::TooShort(frame.len()));
    }
    if frame[..8] != crate::whitening::HEADER {
        return Err(DecodeError::BadHeader);
    }

    let crc = CrcStatus {
        status: check_subblock(frame, POS_STATUS, PCK_STATUS),
        ptu: check_subblock(frame, POS_PTU, PCK_PTU),
        gps_info: check_subblock(frame, POS_GPS_INFO, PCK_GPS_INFO),
        gps_pos: check_subblock(frame, POS_GPS_POS, PCK_GPS_POS),
    };

    if !crc.status {
        return Err(DecodeError::StatusCrcFailed);
    }

    // STATUS: serial, frame#, battery, calibration index.
    let serial = decode_serial(frame);
    let frame_num = u16le(frame, POS_FRAME_NB).unwrap_or(0);
    let battery_v = frame[POS_BATT] as f32 / 10.0;

    // GPS-INFO.
    let gps_time = if crc.gps_info {
        Some(GpsTime {
            week: u16le(frame, POS_GPS_WEEK).unwrap_or(0),
            tow_ms: u32le(frame, POS_GPS_ITOW).unwrap_or(0),
        })
    } else {
        None
    };

    // GPS-POS.
    let gps_pos = if crc.gps_pos {
        decode_gps_pos(frame)
    } else {
        None
    };

    // PTU.
    let ptu = if crc.ptu { decode_ptu(frame) } else { None };

    Ok(Rs41Frame {
        serial,
        frame_num,
        battery_v,
        gps_time,
        gps_pos,
        ptu,
        crc,
    })
}

/// Sonde serial: 8 bytes at POS_SONDE_ID, ASCII, trailing NULs trimmed.
fn decode_serial(frame: &[u8]) -> String {
    let raw = &frame[POS_SONDE_ID..POS_SONDE_ID + 8];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    raw[..end].iter().map(|&b| b as char).collect()
}

fn decode_gps_pos(frame: &[u8]) -> Option<GpsPos> {
    // ECEF position: 3 x i32 centimetres -> metres.
    let x = i32le(frame, POS_ECEF_X)? as f64 / 100.0;
    let y = i32le(frame, POS_ECEF_X + 4)? as f64 / 100.0;
    let z = i32le(frame, POS_ECEF_X + 8)? as f64 / 100.0;
    let (lat, lon, alt_m) = ecef_to_geodetic(x, y, z);

    // ECEF velocity: 3 x i16 centimetres/second -> metres/second.
    let vx = i16le(frame, POS_ECEF_V)? as f64 / 100.0;
    let vy = i16le(frame, POS_ECEF_V + 2)? as f64 / 100.0;
    let vz = i16le(frame, POS_ECEF_V + 4)? as f64 / 100.0;

    // ECEF velocity -> local North/East/Up.
    let phi = lat.to_radians();
    let lam = lon.to_radians();
    let vn = -vx * phi.sin() * lam.cos() - vy * phi.sin() * lam.sin() + vz * phi.cos();
    let ve = -vx * lam.sin() + vy * lam.cos();
    let vu = vx * phi.cos() * lam.cos() + vy * phi.cos() * lam.sin() + vz * phi.sin();

    let speed_ms = (vn * vn + ve * ve).sqrt();
    let mut course_deg = ve.atan2(vn).to_degrees();
    if course_deg < 0.0 {
        course_deg += 360.0;
    }

    let num_sv = *frame.get(POS_NUM_SV)?;

    Some(GpsPos {
        lat,
        lon,
        alt_m,
        speed_ms,
        course_deg,
        climb_ms: vu,
        num_sv,
    })
}

fn decode_ptu(frame: &[u8]) -> Option<Ptu> {
    let mut raw = [0u32; 12];
    for (i, r) in raw.iter_mut().enumerate() {
        *r = u24le(frame, POS_PTU_MEAS + 3 * i)?;
    }
    let p_aux = i16le(frame, POS_PTU_PAUX)?;
    let cal_index = *frame.get(POS_CAL_DATA)?;
    let mut cal_bytes = [0u8; 16];
    cal_bytes.copy_from_slice(frame.get(POS_CAL_DATA + 1..POS_CAL_DATA + 17)?);
    Some(Ptu {
        raw,
        p_aux,
        cal_index,
        cal_bytes,
    })
}
