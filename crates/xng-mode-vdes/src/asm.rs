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
//! destination MMSI for Message 6) and the spec-citable DAC/FID ASM
//! application payloads below, citing IMO SN.1/Circ.289 (DAC=1, IMO
//! international) or the UNECE Inland-AIS catalogue (DAC=200) per arm:
//!
//!   DAC=1  FID=11  Meteorological & hydrological data (IMO236 layout)
//!   DAC=1  FID=16  Number of persons on board
//!   DAC=1  FID=17  VTS-generated / synthetic targets (first target)
//!   DAC=1  FID=18  Clearance time to enter port (addressed)
//!   DAC=1  FID=31  Meteorological & hydrological data (IMO289 layout)
//!   DAC=200 FID=10 Inland ship static & voyage related data
//!   DAC=200 FID=55 Inland number of persons on board (addressed)
//!
//! Unrecognised DAC/FID fall back to a hex dump of the application data —
//! nothing is fabricated. The deeper IALA / Inland DAC/FID catalogue and the
//! variable-length repeated-block payloads (FID 22 area notice, FID 14 tidal
//! window, the FID 17 second-and-later targets) are deferred (PROVENANCE.md).

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

/// AIS 6-bit ASCII (ITU-R M.1371-5 Table 47): values 0..31 map to '@'..'_',
/// 32..63 map to ' '..'?'. Reads `chars` six-bit characters starting at bit
/// offset `s`, trimming the '@' / trailing-space padding. Returns `None` if
/// the field runs off the end of `bits`. Used by the text/identifier fields
/// of several DAC/FID application payloads.
fn sixbit(bits: &[u8], s: usize, chars: usize) -> Option<String> {
    if s + 6 * chars > bits.len() {
        return None;
    }
    let mut out = String::new();
    for k in 0..chars {
        let v = u(bits, s + 6 * k, 6)? as u8;
        out.push(if v < 32 { (v + 64) as char } else { v as char });
    }
    Some(out.trim_end_matches(['@', ' ']).to_string())
}

/// Insert a non-empty string field, omitting blank/all-padding text.
fn put_str(d: &mut Map<String, Value>, key: &str, s: Option<String>) {
    if let Some(s) = s {
        if !s.is_empty() {
            d.insert(key.into(), json!(s));
        }
    }
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
            // Wind direction (9-bit deg, 360 = N/A) and gust direction
            // (9-bit deg, 360 = N/A).
            if let Some(wd) = u(bits, p + 80, 9) {
                if wd != 360 {
                    d.insert("wind_dir_deg".into(), json!(wd));
                }
            }
            if let Some(wgd) = u(bits, p + 89, 9) {
                if wgd != 360 {
                    d.insert("wind_gust_dir_deg".into(), json!(wgd));
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
            // Dew point: signed 10-bit, 0.1 °C, raw 501 = N/A.
            if let Some(dp) = i(bits, p + 116, 10) {
                if dp != 501 {
                    d.insert("dew_point_c".into(), json!(dp as f64 / 10.0));
                }
            }
            // Air pressure: 9-bit, hPa = raw + 799, raw 511 = N/A
            // (IMO289: 800–1201 hPa, 402 = pressure > 1201).
            if let Some(pr) = u(bits, p + 126, 9) {
                if pr != 511 {
                    d.insert("pressure_hpa".into(), json!(pr as i64 + 799));
                }
            }
            // Pressure tendency: 2-bit (0 steady, 1 decreasing, 2 increasing,
            // 3 = N/A).
            if let Some(pt) = u(bits, p + 135, 2) {
                if pt != 3 {
                    d.insert("pressure_tendency".into(), json!(pt));
                }
            }
            // Horizontal visibility: 1-bit ">" flag + 7-bit value, 0.1 NM,
            // raw 127 = N/A.
            if let Some(vis) = u(bits, p + 138, 7) {
                if vis != 127 {
                    d.insert("visibility_nm".into(), json!(vis as f64 / 10.0));
                    if u(bits, p + 137, 1) == Some(1) {
                        d.insert("visibility_greater".into(), json!(true));
                    }
                }
            }
            // Water level (incl. tide): 12-bit, metres = (raw - 1000)/100,
            // raw 4001 = N/A (IMO289: −10.0 to +30.0 m).
            if let Some(wl) = u(bits, p + 145, 12) {
                if wl != 4001 {
                    d.insert("water_level_m".into(), json!((wl as f64 - 1000.0) / 100.0));
                }
            }
            // Water level trend: 2-bit (3 = N/A).
            if let Some(wlt) = u(bits, p + 157, 2) {
                if wlt != 3 {
                    d.insert("water_level_trend".into(), json!(wlt));
                }
            }
            // Surface current: speed 8-bit 0.1 kt (255 = N/A), direction
            // 9-bit deg (360 = N/A).
            if let Some(cs) = u(bits, p + 159, 8) {
                if cs != 255 {
                    d.insert("surface_current_speed_kt".into(), json!(cs as f64 / 10.0));
                }
            }
            if let Some(cd) = u(bits, p + 167, 9) {
                if cd != 360 {
                    d.insert("surface_current_dir_deg".into(), json!(cd));
                }
            }
            // Significant wave height: 8-bit 0.1 m (255 = N/A), wave period
            // 6-bit s (63 = N/A), wave direction 9-bit deg (360 = N/A).
            if let Some(wh) = u(bits, p + 220, 8) {
                if wh != 255 {
                    d.insert("wave_height_m".into(), json!(wh as f64 / 10.0));
                }
            }
            if let Some(wp) = u(bits, p + 228, 6) {
                if wp != 63 {
                    d.insert("wave_period_s".into(), json!(wp));
                }
            }
            if let Some(wvd) = u(bits, p + 234, 9) {
                if wvd != 360 {
                    d.insert("wave_dir_deg".into(), json!(wvd));
                }
            }
            // Sea state (Beaufort 0..12, raw 15 = N/A).
            if let Some(ss) = u(bits, p + 266, 4) {
                if ss != 15 {
                    d.insert("sea_state".into(), json!(ss));
                }
            }
            // Water temperature: signed 10-bit 0.1 °C, raw 601 = N/A.
            if let Some(wt) = i(bits, p + 270, 10) {
                if wt != 601 {
                    d.insert("water_temp_c".into(), json!(wt as f64 / 10.0));
                }
            }
            // Salinity: 9-bit 0.1 ‰, raw 510 = N/A.
            if let Some(sal) = u(bits, p + 283, 9) {
                if sal != 510 {
                    d.insert("salinity_permille".into(), json!(sal as f64 / 10.0));
                }
            }
            // Ice: 2-bit (0 No, 1 Yes, 3 = N/A).
            if let Some(ice) = u(bits, p + 292, 2) {
                if ice != 3 {
                    d.insert("ice".into(), json!(ice == 1));
                }
            }
        }
        // DAC=1, FID=11 — "Meteorological and hydrological data" (IMO236;
        // IMO SN.1/Circ.289 Annex 1 Table 1). This is the OLDER met/hydro
        // layout, structurally distinct from FID 31: position is LATITUDE
        // (24-bit) FIRST then LONGITUDE (25-bit), date/time is a packed
        // 16-bit ddhhmm field (NO position-accuracy bit), and air
        // temperature / dew point are UNSIGNED with offsets. Layout per the
        // 56-bit Message-8 header (data start `p` = 56):
        //   lat 24 (1/1000 min), lon 25 (1/1000 min),
        //   day 5 / hour 5 / minute 6, avg wind 7, gust 7, wind dir 9,
        //   wind gust dir 9, air temp 11 (raw-600, /10 °C), humidity 7,
        //   dew point 10 (raw-200, /10 °C), pressure 9 (raw+800 hPa), ...
        // Sentinels: lat 0x7FFFFF, lon 0xFFFFFF (= N/A); air temp 2047,
        // dew point 1023, pressure 511, humidity 127, wind 127, dir 511.
        (1, 11) => {
            let lat = i(bits, p, 24);
            let lon = i(bits, p + 24, 25);
            if let (Some(lat), Some(lon)) = (lat, lon) {
                let lat = lat as f64 / 60_000.0;
                let lon = lon as f64 / 60_000.0;
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    d.insert("lon".into(), json!(lon));
                    d.insert("lat".into(), json!(lat));
                }
            }
            // Packed date/time (bits 49..65): day 5 / hour 5 / minute 6.
            if let Some(day) = u(bits, p + 49, 5) {
                if day != 0 {
                    d.insert("day".into(), json!(day));
                }
            }
            if let Some(hour) = u(bits, p + 54, 5) {
                if hour != 24 {
                    d.insert("hour".into(), json!(hour));
                }
            }
            if let Some(minute) = u(bits, p + 59, 6) {
                if minute != 60 {
                    d.insert("minute".into(), json!(minute));
                }
            }
            if let Some(ws) = u(bits, p + 65, 7) {
                if ws != 127 {
                    d.insert("wind_speed_kt".into(), json!(ws));
                }
            }
            if let Some(wg) = u(bits, p + 72, 7) {
                if wg != 127 {
                    d.insert("wind_gust_kt".into(), json!(wg));
                }
            }
            if let Some(wd) = u(bits, p + 79, 9) {
                if wd != 511 {
                    d.insert("wind_dir_deg".into(), json!(wd));
                }
            }
            if let Some(wgd) = u(bits, p + 88, 9) {
                if wgd != 511 {
                    d.insert("wind_gust_dir_deg".into(), json!(wgd));
                }
            }
            // Air temperature: UNSIGNED 11-bit, °C = (raw - 600)/10,
            // raw 2047 = N/A.
            if let Some(at) = u(bits, p + 97, 11) {
                if at != 2047 {
                    d.insert("air_temp_c".into(), json!((at as f64 - 600.0) / 10.0));
                }
            }
            if let Some(rh) = u(bits, p + 108, 7) {
                if rh != 127 {
                    d.insert("humidity_pct".into(), json!(rh));
                }
            }
            // Dew point: UNSIGNED 10-bit, °C = (raw - 200)/10, raw 1023 = N/A.
            if let Some(dp) = u(bits, p + 115, 10) {
                if dp != 1023 {
                    d.insert("dew_point_c".into(), json!((dp as f64 - 200.0) / 10.0));
                }
            }
            // Air pressure: 9-bit, hPa = raw + 800, raw 511 = N/A.
            if let Some(pr) = u(bits, p + 125, 9) {
                if pr != 511 {
                    d.insert("pressure_hpa".into(), json!(pr as i64 + 800));
                }
            }
        }
        // DAC=1, FID=17 — "VTS-generated/synthetic targets" (IMO289;
        // IMO SN.1/Circ.289 Annex). Message 8 broadcast. After the 56-bit
        // header the body is a sequence of 122-bit target reports. We decode
        // the FIRST report (the common case) per gpsd driver_ais.c layout:
        //   idtype 2 (0=MMSI,1=IMO,2=callsign,3=other),
        //   id 42 (MMSI/IMO number) or 7×6-bit ASCII (callsign/other),
        //   spare 4, lat 24 (1/1000 min), lon 25 (1/1000 min),
        //   COG 9 (deg, 360=N/A), timestamp 6 (UTC second),
        //   SOG 10 (kt, 1023=N/A, 1022=>=102.2).
        (1, 17) => {
            if let Some(idtype) = u(bits, p, 2) {
                match idtype {
                    0 => {
                        if let Some(id) = u(bits, p + 2, 42) {
                            if id != 0 {
                                d.insert("target_mmsi".into(), json!(id));
                            }
                        }
                    }
                    1 => {
                        if let Some(id) = u(bits, p + 2, 42) {
                            if id != 0 {
                                d.insert("target_imo".into(), json!(id));
                            }
                        }
                    }
                    2 => put_str(&mut d, "target_callsign", sixbit(bits, p + 2, 7)),
                    _ => put_str(&mut d, "target_id", sixbit(bits, p + 2, 7)),
                }
            }
            let lat = i(bits, p + 48, 24);
            let lon = i(bits, p + 72, 25);
            if let (Some(lat), Some(lon)) = (lat, lon) {
                let lat = lat as f64 / 60_000.0;
                let lon = lon as f64 / 60_000.0;
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    d.insert("lon".into(), json!(lon));
                    d.insert("lat".into(), json!(lat));
                }
            }
            if let Some(cog) = u(bits, p + 97, 9) {
                if cog != 360 {
                    d.insert("cog_deg".into(), json!(cog));
                }
            }
            if let Some(sec) = u(bits, p + 106, 6) {
                if sec < 60 {
                    d.insert("timestamp_sec".into(), json!(sec));
                }
            }
            if let Some(sog) = u(bits, p + 112, 10) {
                if sog != 1023 {
                    d.insert("sog_kt".into(), json!(sog as f64 / 10.0));
                }
            }
        }
        // DAC=1, FID=18 — "Clearance time to enter port" (IMO289; IMO
        // SN.1/Circ.289 Annex). Message 6 addressed; data start `p` = 88.
        // Layout per gpsd driver_ais.c (offsets relative to `p`):
        //   linkage 10, month 4, day 5, hour 5, minute 6,
        //   port name 120 (20×6-bit), destination 30 (5×6-bit UN/LOCODE),
        //   lon 25 (1/1000 min), lat 24 (1/1000 min).
        (1, 18) => {
            if let Some(linkage) = u(bits, p, 10) {
                if linkage != 0 {
                    d.insert("linkage_id".into(), json!(linkage));
                }
            }
            if let (Some(month), Some(day), Some(hour), Some(minute)) =
                (u(bits, p + 10, 4), u(bits, p + 14, 5), u(bits, p + 19, 5), u(bits, p + 24, 6))
            {
                if month != 0 {
                    d.insert("month".into(), json!(month));
                }
                if day != 0 {
                    d.insert("day".into(), json!(day));
                }
                if hour != 24 {
                    d.insert("hour".into(), json!(hour));
                }
                if minute != 60 {
                    d.insert("minute".into(), json!(minute));
                }
            }
            put_str(&mut d, "port_name", sixbit(bits, p + 30, 20));
            put_str(&mut d, "destination", sixbit(bits, p + 150, 5));
            let lon = i(bits, p + 180, 25);
            let lat = i(bits, p + 205, 24);
            if let (Some(lon), Some(lat)) = (lon, lat) {
                let lon = lon as f64 / 60_000.0;
                let lat = lat as f64 / 60_000.0;
                if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                    d.insert("lon".into(), json!(lon));
                    d.insert("lat".into(), json!(lat));
                }
            }
        }
        // DAC=200 (UNECE Inland AIS, "Inland-AIS" application catalogue),
        // FID=10 — "Inland ship static and voyage related data" (UNECE
        // Inland AIS / RIS; ES-TRIN; gpsd driver_ais.c dac200fid10).
        // Message 8 broadcast; data start `p` = 56. Layout (offsets rel. `p`):
        //   ENI/VIN 48 (8×6-bit ASCII European Vessel ID),
        //   length 13 (0.1 m), beam 10 (0.1 m), ship type 14 (ERI code),
        //   hazard 3 (0..3 blue cones, 5 = unknown), draught 11 (0.01 m),
        //   loaded 2 (1 loaded, 2 unloaded, 0 = N/A),
        //   speed_q 1, course_q 1, heading_q 1 (data-quality flags).
        (200, 10) => {
            put_str(&mut d, "eni", sixbit(bits, p, 8));
            if let Some(len) = u(bits, p + 48, 13) {
                if len != 0 {
                    d.insert("length_m".into(), json!(len as f64 / 10.0));
                }
            }
            if let Some(beam) = u(bits, p + 61, 10) {
                if beam != 0 {
                    d.insert("beam_m".into(), json!(beam as f64 / 10.0));
                }
            }
            if let Some(st) = u(bits, p + 71, 14) {
                if st != 0 {
                    d.insert("eri_ship_type".into(), json!(st));
                }
            }
            if let Some(hz) = u(bits, p + 85, 3) {
                if hz != 5 {
                    d.insert("hazard_cones".into(), json!(hz));
                }
            }
            if let Some(dr) = u(bits, p + 88, 11) {
                if dr != 0 {
                    d.insert("draught_m".into(), json!(dr as f64 / 100.0));
                }
            }
            // Loaded status: 1 = loaded, 2 = unloaded, 0 = not available.
            if let Some(ld) = u(bits, p + 99, 2) {
                match ld {
                    1 => d.insert("loaded".into(), json!("loaded")),
                    2 => d.insert("loaded".into(), json!("unloaded")),
                    _ => None,
                };
            }
            if let Some(q) = u(bits, p + 101, 1) {
                d.insert("speed_quality_high".into(), json!(q == 1));
            }
            if let Some(q) = u(bits, p + 102, 1) {
                d.insert("course_quality_high".into(), json!(q == 1));
            }
            if let Some(q) = u(bits, p + 103, 1) {
                d.insert("heading_quality_high".into(), json!(q == 1));
            }
        }
        // DAC=200 (UNECE Inland AIS), FID=55 — "Number of persons on board"
        // (Inland-AIS; gpsd driver_ais.c dac200fid55). Message 6 addressed;
        // data start `p` = 88. Layout (offsets rel. `p`):
        //   crew 8, passengers 13, personnel (shipboard) 8.
        // 0xFF / 0x1FFF = unknown (omit).
        (200, 55) => {
            if let Some(crew) = u(bits, p, 8) {
                if crew != 0xFF {
                    d.insert("crew".into(), json!(crew));
                }
            }
            if let Some(pax) = u(bits, p + 8, 13) {
                if pax != 0x1FFF {
                    d.insert("passengers".into(), json!(pax));
                }
            }
            if let Some(personnel) = u(bits, p + 21, 8) {
                if personnel != 0xFF {
                    d.insert("personnel".into(), json!(personnel));
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
