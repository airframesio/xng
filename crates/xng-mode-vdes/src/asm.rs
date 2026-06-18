//! VDES ASM (Application-Specific Message) payload decode.
//!
//! ITU-R M.2092-1 ("Technical characteristics for a VHF data exchange
//! system in the maritime mobile band") carries Application-Specific
//! Messages on the dedicated ASM channels using the AIS binary-message
//! transport: the addressed-binary (AIS Message 6) and broadcast-binary
//! (AIS Message 8) structures of ITU-R M.1371, and the SAME
//! application-identifier catalogue — a 10-bit Designated Area Code (DAC)
//! plus a 6-bit Function Identifier (FID). The ASM application data is the
//! identical DAC/FID-keyed binary payload defined for AIS Message 6/8 and
//! catalogued by IMO SN.1/Circ.289 (DAC=1, IMO international) and the IALA
//! ASM registry.
//!
//! Bit layout of the transport (ITU-R M.1371-5, Message 6 / Message 8;
//! reused verbatim by M.2092-1 for ASM):
//!
//!   Message 8 (broadcast ASM):
//!     bits 0..6   message ID (= 8)
//!     bits 6..8   repeat indicator
//!     bits 8..38  source MMSI
//!     bits 38..40 spare
//!     bits 40..50 DAC (10)
//!     bits 50..56 FID (6)
//!     bits 56..   application data
//!
//!   Message 6 (addressed ASM):
//!     bits 0..6   message ID (= 6)
//!     bits 6..8   repeat indicator
//!     bits 8..38  source MMSI
//!     bits 38..40 sequence number
//!     bits 40..70 destination MMSI
//!     bit  70     retransmit flag
//!     bit  71     spare
//!     bits 72..82 DAC (10)
//!     bits 82..88 FID (6)
//!     bits 88..   application data
//!
//! We decode the transport header (message ID, source MMSI, DAC/FID,
//! destination MMSI for Message 6) and a couple of well-documented DAC=1
//! ASM application payloads, citing IMO SN.1/Circ.289 per arm. Unrecognised
//! DAC/FID fall back to a hex dump of the application data — nothing is
//! fabricated.

use serde_json::{json, Map, Value};

/// Read `n` MSB-first bits at offset `s` as an unsigned integer.
fn u(bits: &[u8], s: usize, n: usize) -> Option<u64> {
    if s + n > bits.len() {
        return None;
    }
    Some(bits[s..s + n].iter().fold(0u64, |v, &b| (v << 1) | b as u64))
}

/// Read `n` MSB-first bits at offset `s` as a two's-complement signed value.
fn i(bits: &[u8], s: usize, n: usize) -> Option<i64> {
    let v = u(bits, s, n)?;
    let sign = 1u64 << (n - 1);
    Some(if v & sign != 0 { v as i64 - (1i64 << n) } else { v as i64 })
}

/// Hex dump of the bits from offset `s` to the end (MSB-first per octet).
fn data_hex(bits: &[u8], s: usize) -> String {
    bits[s..]
        .chunks(8)
        .map(|c| format!("{:02x}", c.iter().fold(0u8, |v, &b| (v << 1) | b)))
        .collect()
}

/// The decoded transport header + application fields of one ASM.
#[derive(Debug, Clone, PartialEq)]
pub struct Asm {
    /// AIS-format message ID: 6 = addressed ASM, 8 = broadcast ASM.
    pub msg_id: u8,
    /// Source MMSI (the transmitting station).
    pub source_mmsi: u32,
    /// Destination MMSI for an addressed (Message 6) ASM.
    pub dest_mmsi: Option<u32>,
    /// Designated Area Code (10 bits).
    pub dac: u16,
    /// Function Identifier (6 bits).
    pub fid: u8,
    /// Decoded application fields when the DAC/FID is recognised; otherwise
    /// just `{"data_hex": ...}` carrying the raw application payload.
    pub app: Value,
}

impl Asm {
    /// `kind` string for the bus message body.
    pub fn kind(&self) -> &'static str {
        match self.msg_id {
            6 => "asm-addressed",
            8 => "asm-broadcast",
            _ => "asm",
        }
    }

    /// Flatten to a single JSON object: header fields + application fields.
    pub fn details(&self) -> Value {
        let mut d = Map::new();
        d.insert("msg_id".into(), json!(self.msg_id));
        d.insert("source_mmsi".into(), json!(self.source_mmsi));
        if let Some(dm) = self.dest_mmsi {
            d.insert("dest_mmsi".into(), json!(dm));
        }
        d.insert("dac".into(), json!(self.dac));
        d.insert("fid".into(), json!(self.fid));
        if let Value::Object(app) = &self.app {
            if !app.is_empty() {
                d.insert("app".into(), Value::Object(app.clone()));
            }
        }
        Value::Object(d)
    }
}

/// Decode the ASM transport header + application payload from a CRC-valid
/// frame's MSB-first message bit string.
pub fn decode(bits: &[u8]) -> Option<Asm> {
    let msg_id = u(bits, 0, 6)? as u8;
    let source_mmsi = u(bits, 8, 30)? as u32;
    let (dest_mmsi, dac, fid, data_off) = match msg_id {
        8 => {
            // Broadcast binary: DAC at 40, FID at 50, data at 56.
            (None, u(bits, 40, 10)? as u16, u(bits, 50, 6)? as u8, 56usize)
        }
        6 => {
            // Addressed binary: dest MMSI at 40, DAC at 72, FID at 82, data at 88.
            let dest = u(bits, 40, 30)? as u32;
            (Some(dest), u(bits, 72, 10)? as u16, u(bits, 82, 6)? as u8, 88usize)
        }
        _ => return None,
    };
    let app = app_decode(dac, fid, bits, data_off);
    Some(Asm { msg_id, source_mmsi, dest_mmsi, dac, fid, app })
}

/// Decode the DAC/FID-keyed application payload. `p` is the bit offset where
/// the application data begins. Recognised layouts are spec-cited; anything
/// else returns `{"data_hex": ...}` so the binary payload is preserved.
fn app_decode(dac: u16, fid: u8, bits: &[u8], p: usize) -> Value {
    let mut d = Map::new();
    match (dac, fid) {
        // DAC=1 (IMO international), FID=16 — "Number of persons on board"
        // (IMO SN.1/Circ.289 Annex, §"Number of persons on board"; ITU-R
        // M.1371-5 Annex 5 §3.10). 13-bit unsigned count, 0 = not available.
        (1, 16) => {
            if let Some(n) = u(bits, p, 13) {
                if n != 0 {
                    d.insert("persons_on_board".into(), json!(n));
                }
            }
        }
        // DAC=1, FID=31 — "Meteorological and hydrological data" (IMO
        // SN.1/Circ.289 Annex, §"Meteorological and Hydrological Data";
        // ITU-R M.1371-5 Annex 8). 360-bit application block. Position is
        // longitude 25 / latitude 24 FIRST, in units of 1/1000 minute
        // (raw / 60000 degrees). Sentinels: longitude 181°, latitude 91° =
        // not available. We decode the leading grounded scalar fields and
        // defer the WMO-coded weather tail.
        (1, 31) => {
            let lon = i(bits, p, 25);
            let lat = i(bits, p + 25, 24);
            if let (Some(lon), Some(lat)) = (lon, lat) {
                let lon = lon as f64 / 60_000.0;
                let lat = lat as f64 / 60_000.0;
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    d.insert("lon".into(), json!(lon));
                    d.insert("lat".into(), json!(lat));
                }
            }
            // position accuracy flag (1), UTC day 5 / hour 5 / minute 6.
            if let Some(pa) = u(bits, p + 49, 1) {
                d.insert("position_accuracy".into(), json!(pa == 1));
            }
            if let Some(day) = u(bits, p + 50, 5) {
                if day != 0 {
                    d.insert("day".into(), json!(day));
                }
            }
            if let Some(hour) = u(bits, p + 55, 5) {
                if hour != 24 {
                    d.insert("hour".into(), json!(hour));
                }
            }
            if let Some(minute) = u(bits, p + 60, 6) {
                if minute != 60 {
                    d.insert("minute".into(), json!(minute));
                }
            }
            // Average wind speed (7-bit kt, 127 = N/A) and gust (7-bit kt).
            if let Some(ws) = u(bits, p + 66, 7) {
                if ws != 127 {
                    d.insert("wind_speed_kt".into(), json!(ws));
                }
            }
            if let Some(wg) = u(bits, p + 73, 7) {
                if wg != 127 {
                    d.insert("wind_gust_kt".into(), json!(wg));
                }
            }
            // Wind direction (9-bit deg, 360 = N/A).
            if let Some(wd) = u(bits, p + 80, 9) {
                if wd != 360 {
                    d.insert("wind_dir_deg".into(), json!(wd));
                }
            }
            // Air temperature: 0.1 °C signed (11-bit), raw -1024 = N/A.
            if let Some(at) = i(bits, p + 98, 11) {
                if at != -1024 {
                    d.insert("air_temp_c".into(), json!(at as f64 / 10.0));
                }
            }
            // Relative humidity %: 7-bit, 101 = N/A.
            if let Some(rh) = u(bits, p + 109, 7) {
                if rh != 101 {
                    d.insert("humidity_pct".into(), json!(rh));
                }
            }
        }
        _ => {}
    }
    // Preserve the raw application payload for anything not (fully) decoded.
    if d.is_empty() && bits.len() > p {
        d.insert("data_hex".into(), json!(data_hex(bits, p)));
    }
    Value::Object(d)
}
