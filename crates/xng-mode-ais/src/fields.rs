//! Field-level AIS message decoding (ITU-R M.1371-5): positions,
//! kinematics, static/voyage data, binary and safety messages.
//! Validated against pyais (MIT) as a decode oracle — see the vendored
//! vectors below.

use serde_json::{Value, json};

fn u(bits: &[u8], s: usize, n: usize) -> Option<u64> {
    if s + n > bits.len() {
        return None;
    }
    Some(bits[s..s + n].iter().fold(0u64, |v, &b| (v << 1) | b as u64))
}

fn i(bits: &[u8], s: usize, n: usize) -> Option<i64> {
    let v = u(bits, s, n)?;
    let sign = 1u64 << (n - 1);
    Some(if v & sign != 0 { v as i64 - (1i64 << n) } else { v as i64 })
}

/// 6-bit ASCII: 0..31 → '@'..'_', 32..63 → ' '..'?'. Trims '@' padding.
fn sixbit(bits: &[u8], s: usize, chars: usize) -> Option<String> {
    let mut out = String::new();
    for k in 0..chars {
        let v = u(bits, s + 6 * k, 6)? as u8;
        out.push(if v < 32 { (v + 64) as char } else { v as char });
    }
    Some(out.trim_end_matches(['@', ' ']).to_string())
}

fn position(bits: &[u8], lon_start: usize) -> Option<(f64, f64)> {
    let lon = i(bits, lon_start, 28)? as f64 / 600_000.0;
    let lat = i(bits, lon_start + 28, 27)? as f64 / 600_000.0;
    if lon.abs() > 180.0 || lat.abs() > 90.0 {
        return None; // 181/91 = not available
    }
    Some((lat, lon))
}

fn sog(bits: &[u8], s: usize) -> Option<f64> {
    match u(bits, s, 10)? {
        1023 => None,
        v => Some(v as f64 / 10.0),
    }
}

fn cog(bits: &[u8], s: usize) -> Option<f64> {
    match u(bits, s, 12)? {
        3600.. => None,
        v => Some(v as f64 / 10.0),
    }
}

fn heading(bits: &[u8], s: usize) -> Option<u64> {
    match u(bits, s, 9)? {
        511 => None,
        v => Some(v),
    }
}

fn data_hex(bits: &[u8], s: usize) -> String {
    bits[s..]
        .chunks(8)
        .map(|c| format!("{:02x}", c.iter().fold(0u8, |v, &b| (v << 1) | b)))
        .collect()
}

const NAV_STATUS: [&str; 16] = [
    "under way (engine)",
    "at anchor",
    "not under command",
    "restricted manoeuvrability",
    "constrained by draught",
    "moored",
    "aground",
    "engaged in fishing",
    "under way (sailing)",
    "reserved (HSC)",
    "reserved (WIG)",
    "reserved",
    "reserved",
    "reserved",
    "AIS-SART",
    "undefined",
];

/// Classify a distress/safety transmitter by its MMSI prefix (ITU-R M.1371 /
/// the MID allocation for device MMSIs): 970 = AIS-SART (search & rescue
/// transmitter), 972 = AIS-MOB (man-overboard), 974 = EPIRB-AIS. These
/// devices send ordinary AIS messages; the prefix is what marks them as a
/// distress class. `None` for a normal MMSI.
pub fn distress_class(mmsi: u32) -> Option<&'static str> {
    match mmsi / 1_000_000 {
        970 => Some("AIS-SART"),
        972 => Some("AIS-MOB"),
        974 => Some("EPIRB-AIS"),
        _ => None,
    }
}

/// Decode the fields of an AIS message; `None` when the type is not
/// (yet) field-decoded. Positions in degrees, speeds in knots.
pub fn decode(msg_type: u8, bits: &[u8]) -> Option<Value> {
    let mut d = serde_json::Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            d.insert(k.into(), v);
        }
    };
    match msg_type {
        // Class A position reports.
        1..=3 => {
            let status = u(bits, 38, 4)? as usize;
            put("nav_status", json!(NAV_STATUS[status]));
            put("sog_kt", json!(sog(bits, 50)));
            if let Some((lat, lon)) = position(bits, 61) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            put("cog_deg", json!(cog(bits, 116)));
            put("heading_deg", json!(heading(bits, 128)));
        }
        // Base station report.
        4 => {
            if let (Some(y), Some(mo), Some(da), Some(h), Some(mi), Some(s)) = (
                u(bits, 38, 14),
                u(bits, 52, 4),
                u(bits, 56, 5),
                u(bits, 61, 5),
                u(bits, 66, 6),
                u(bits, 72, 6),
            ) {
                if y > 0 {
                    put("utc", json!(format!("{y:04}-{mo:02}-{da:02}T{h:02}:{mi:02}:{s:02}Z")));
                }
            }
            if let Some((lat, lon)) = position(bits, 79) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
        }
        // Static and voyage data.
        5 => {
            put("imo", json!(u(bits, 40, 30)));
            put("callsign", json!(sixbit(bits, 70, 7)));
            put("name", json!(sixbit(bits, 112, 20)));
            put("ship_type", json!(u(bits, 232, 8)));
            put("draught_m", json!(u(bits, 294, 8)? as f64 / 10.0));
            put("destination", json!(sixbit(bits, 302, 20)));
        }
        // Addressed binary message.
        6 => {
            put("seqno", json!(u(bits, 38, 2)));
            put("dest_mmsi", json!(u(bits, 40, 30)));
            put("retransmit", json!(u(bits, 70, 1)? == 1));
            put("dac", json!(u(bits, 72, 10)));
            put("fid", json!(u(bits, 82, 6)));
            if bits.len() > 88 {
                put("data_hex", json!(data_hex(bits, 88)));
            }
        }
        // Binary / safety acknowledgements.
        7 | 13 => {
            put("dest_mmsi", json!(u(bits, 40, 30)));
        }
        // Broadcast binary message.
        8 => {
            put("dac", json!(u(bits, 40, 10)));
            put("fid", json!(u(bits, 50, 6)));
            if bits.len() > 56 {
                put("data_hex", json!(data_hex(bits, 56)));
            }
        }
        // SAR aircraft.
        9 => {
            match u(bits, 38, 12)? {
                4095 => {}
                alt => put("altitude_m", json!(alt)),
            }
            put("sog_kt", json!(sog(bits, 50)));
            if let Some((lat, lon)) = position(bits, 61) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
        }
        // UTC response: position part of the type-4 shape.
        11 => {
            if let Some((lat, lon)) = position(bits, 79) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
        }
        // Addressed safety-related text.
        12 => {
            put("seqno", json!(u(bits, 38, 2)));
            put("dest_mmsi", json!(u(bits, 40, 30)));
            put("retransmit", json!(u(bits, 70, 1)? == 1));
            put("text", json!(sixbit(bits, 72, (bits.len().saturating_sub(72)) / 6)));
        }
        // Broadcast safety-related text.
        14 => {
            put("text", json!(sixbit(bits, 40, (bits.len().saturating_sub(40)) / 6)));
        }
        // Class B position reports (19 adds name/type).
        18 | 19 => {
            put("sog_kt", json!(sog(bits, 46)));
            if let Some((lat, lon)) = position(bits, 57) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            put("cog_deg", json!(cog(bits, 112)));
            put("heading_deg", json!(heading(bits, 124)));
            if msg_type == 19 {
                put("name", json!(sixbit(bits, 143, 20)));
                put("ship_type", json!(u(bits, 263, 8)));
            }
        }
        // Aids to navigation.
        21 => {
            put("aton_type", json!(u(bits, 38, 5)));
            put("name", json!(sixbit(bits, 43, 20)));
            if let Some((lat, lon)) = position(bits, 164) {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
        }
        // Static data report (part A: name; part B: type + callsign).
        24 => match u(bits, 38, 2)? {
            0 => put("name", json!(sixbit(bits, 40, 20))),
            1 => {
                put("ship_type", json!(u(bits, 40, 8)));
                put("callsign", json!(sixbit(bits, 90, 7)));
            }
            _ => return None,
        },
        // DGNSS broadcast binary message.
        17 => {
            // Raw field is in 1/10-minute units; reported /10 to match
            // the pyais oracle convention.
            let lon = i(bits, 40, 18)? as f64 / 10.0;
            let lat = i(bits, 58, 17)? as f64 / 10.0;
            {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            if bits.len() > 80 {
                put("data_hex", json!(data_hex(bits, 80)));
            }
        }
        // Data link management: up to four slot-reservation blocks.
        20 => {
            let mut blocks = Vec::new();
            for k in 0..4 {
                let s0 = 40 + k * 30;
                let (Some(offset), Some(number), Some(timeout), Some(increment)) = (
                    u(bits, s0, 12),
                    u(bits, s0 + 12, 4),
                    u(bits, s0 + 16, 3),
                    u(bits, s0 + 19, 11),
                ) else {
                    break;
                };
                if offset == 0 && number == 0 {
                    continue;
                }
                blocks.push(json!({
                    "offset": offset,
                    "slots": number,
                    "timeout_min": timeout,
                    "increment": increment,
                }));
            }
            if !blocks.is_empty() {
                put("reservations", json!(blocks));
            }
        }
        // Channel management (regional channel assignment).
        22 => {
            put("channel_a", json!(u(bits, 40, 12)));
            put("channel_b", json!(u(bits, 52, 12)));
            put("txrx", json!(u(bits, 64, 4)));
            put("high_power", json!(u(bits, 68, 1)? == 1));
            let addressed = u(bits, 139, 1)? == 1;
            put("addressed", json!(addressed));
            if !addressed {
                // Region corners in 1/10-minute units.
                put("ne_lon", json!(i(bits, 69, 18)? as f64 / 10.0));
                put("ne_lat", json!(i(bits, 87, 17)? as f64 / 10.0));
                put("sw_lon", json!(i(bits, 104, 18)? as f64 / 10.0));
                put("sw_lat", json!(i(bits, 122, 17)? as f64 / 10.0));
            }
            put("band_a", json!(u(bits, 140, 1)? == 1));
            put("band_b", json!(u(bits, 141, 1)? == 1));
            put("zone_size", json!(u(bits, 142, 3)));
        }
        // Group assignment command.
        23 => {
            put("ne_lon", json!(i(bits, 40, 18)? as f64 / 10.0));
            put("ne_lat", json!(i(bits, 58, 17)? as f64 / 10.0));
            put("sw_lon", json!(i(bits, 75, 18)? as f64 / 10.0));
            put("sw_lat", json!(i(bits, 93, 17)? as f64 / 10.0));
            put("station_type", json!(u(bits, 110, 4)));
            put("ship_type", json!(u(bits, 114, 8)));
            put("txrx", json!(u(bits, 144, 2)));
            put("interval", json!(u(bits, 146, 4)));
            put("quiet_min", json!(u(bits, 150, 4)));
        }
        // Long-range position report.
        27 => {
            let lon = i(bits, 44, 18)? as f64 / 600.0;
            let lat = i(bits, 62, 17)? as f64 / 600.0;
            if lon.abs() <= 180.0 && lat.abs() <= 90.0 {
                put("lat", json!(lat));
                put("lon", json!(lon));
            }
            match u(bits, 79, 6)? {
                63 => {}
                v => put("sog_kt", json!(v)),
            }
        }
        _ => return None,
    }
    if d.is_empty() { None } else { Some(Value::Object(d)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AIVDM armored payload → bit vector.
    pub(super) fn bits_of(payload: &str, fill: usize) -> Vec<u8> {
        let mut bits = Vec::new();
        for c in payload.bytes() {
            let mut v = c as i32 - 48;
            if v > 40 {
                v -= 8;
            }
            for k in (0..6).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        }
        bits.truncate(bits.len() - fill);
        bits
    }

    fn typed(payload: &str, fill: usize) -> (u8, Vec<u8>) {
        let bits = bits_of(payload, fill);
        let t = bits[..6].iter().fold(0u8, |v, &b| (v << 1) | b);
        (t, bits)
    }

    // Oracle: pyais 2.x decode of the same sentences (2026-06-10/11).

    #[test]
    fn dgnss_t17_matches_pyais() {
        let (t, bits) = typed("A02R5Ph0E81:p7h5Ed1h=h", 4);
        assert_eq!(t, 17);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["lon"], 33.8);
        assert_eq!(d["lat"], 59.9);
        assert_eq!(d["data_hex"], "7c0556c07037");
    }

    #[test]
    fn link_mgmt_t20_matches_pyais() {
        let (t, bits) = typed("D028rqP2tN?b<`I6D0000000000", 2);
        assert_eq!(t, 20);
        let d = decode(t, &bits).unwrap();
        let r = &d["reservations"];
        assert_eq!(r[0]["offset"], 47);
        assert_eq!(r[0]["slots"], 1);
        assert_eq!(r[0]["timeout_min"], 7);
        assert_eq!(r[0]["increment"], 250);
        assert_eq!(r[1]["offset"], 2250);
        assert_eq!(r[1]["increment"], 1125);
        assert!(r.get(2).is_none());
    }

    #[test]
    fn channel_mgmt_t22_matches_pyais() {
        let (t, bits) = typed("F030p8B2N2PMaJR0r;6f3rj20000", 0);
        assert_eq!(t, 22);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["channel_a"], 2087);
        assert_eq!(d["channel_b"], 2088);
        assert_eq!(d["txrx"], 1);
        assert_eq!(d["high_power"], true);
        assert_eq!(d["ne_lon"], -7710.0);
        assert_eq!(d["ne_lat"], 3300.0);
        assert_eq!(d["sw_lon"], -8020.0);
        assert_eq!(d["sw_lat"], 3210.0);
        assert_eq!(d["addressed"], false);
        assert_eq!(d["zone_size"], 4);
    }

    #[test]
    fn group_assign_t23_matches_pyais() {
        let (t, bits) = typed("G02:Kn01R`sn@291nj600000900", 2);
        assert_eq!(t, 23);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["ne_lon"], 157.8);
        assert_eq!(d["ne_lat"], 3064.2);
        assert_eq!(d["sw_lon"], 109.6);
        assert_eq!(d["sw_lat"], 3040.8);
        assert_eq!(d["station_type"], 6);
        assert_eq!(d["ship_type"], 0);
        assert_eq!(d["txrx"], 0);
        assert_eq!(d["interval"], 9);
        assert_eq!(d["quiet_min"], 0);
    }

    #[test]
    fn class_a_position_matches_pyais() {
        let (t, bits) = typed("177KQJ5000G?tO`K>RA1wUbN0TKH", 0);
        assert_eq!(t, 1);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["nav_status"], "moored");
        assert!((d["lat"].as_f64().unwrap() - 47.582833).abs() < 1e-5);
        assert!((d["lon"].as_f64().unwrap() - -122.345833).abs() < 1e-5);
        assert_eq!(d["sog_kt"], 0.0);
        assert_eq!(d["cog_deg"], 51.0);
        assert_eq!(d["heading_deg"], 181);
    }

    #[test]
    fn static_voyage_matches_pyais() {
        let payload = concat!(
            "55P5TL01VIaAL@7WKO@mBplU@<PDhh000000001S;AJ::4A80?4i@E53",
            "1@0000000000000"
        );
        let (t, bits) = typed(payload, 2);
        assert_eq!(t, 5);
        let d = decode(t, &bits).unwrap();
        assert_eq!(d["imo"], 6710932);
        assert_eq!(d["callsign"], "WDA9674");
        assert_eq!(d["name"], "MT.MITCHELL");
        assert_eq!(d["ship_type"], 99);
        assert_eq!(d["destination"], "SEATTLE");
        assert_eq!(d["draught_m"], 6.0);
    }

    #[test]
    fn class_b_position_matches_pyais() {
        let (t, bits) = typed("B52K>;h00Fc>jpUlNV@ikwpUoP06", 0);
        assert_eq!(t, 18);
        let d = decode(t, &bits).unwrap();
        assert!((d["lat"].as_f64().unwrap() - 40.68454).abs() < 1e-5);
        assert!((d["lon"].as_f64().unwrap() - -74.072132).abs() < 1e-5);
        assert_eq!(d["sog_kt"], 0.1);
    }

    #[test]
    fn type8_binary_broadcast() {
        let bits = bits_of("83HOI:00Gh420h@", 2);
        let d = decode(8, &bits).unwrap();
        assert_eq!(d["dac"], 1);
        assert_eq!(d["fid"], 31);
        assert_eq!(d["data_hex"], "01020304");
    }

    #[test]
    fn type12_addressed_safety_text() {
        let bits = bits_of("<42Lati0W:Ot=C7P6B?=Pjoihhjhqq", 0);
        let d = decode(12, &bits).unwrap();
        assert_eq!(d["dest_mmsi"], 271002111);
        assert_eq!(d["retransmit"], false);
        assert_eq!(d["text"], "MSG FROM 271002099");
    }

    #[test]
    fn type14_broadcast_safety_text() {
        let bits = bits_of(">5?Per18=HB1U:1@E=B0m<L", 2);
        let d = decode(14, &bits).unwrap();
        assert_eq!(d["text"], "RCVD YR TEST MSG");
    }

    #[test]
    fn type6_addressed_binary() {
        let bits = bits_of("62?n;bQ:cbapaleEbP", 4);
        let d = decode(6, &bits).unwrap();
        assert_eq!(d["dest_mmsi"], 313240222);
        assert_eq!(d["dac"], 669);
        assert_eq!(d["fid"], 11);
        assert_eq!(d["data_hex"], "55aa");
    }

    #[test]
    fn distress_class_by_mmsi_prefix() {
        assert_eq!(distress_class(970_12_3456), Some("AIS-SART"));
        assert_eq!(distress_class(972_00_0001), Some("AIS-MOB"));
        assert_eq!(distress_class(974_99_9999), Some("EPIRB-AIS"));
        assert_eq!(distress_class(366_123_456), None); // ordinary US ship
        assert_eq!(distress_class(0), None);
    }
}
