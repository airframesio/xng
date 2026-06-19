//! Free-text position-report extraction (ACARS labels `20`/POS, `4J`,
//! `H1` POS) → latitude / longitude.
//!
//! Clean-room port of the coordinate decoders in airframes' own
//! acars-decoder-typescript (`utils/coordinate_utils.ts`,
//! `utils/arinc_702_helper.ts`, `plugins/Label_20_POS.ts`); facts only,
//! reimplemented. Two packed coordinate conventions appear:
//!
//!   - label `20`/POS uses `decodeStringCoordinates`: the digit run is a
//!     plain scaled decimal degree (`38160` → 38.160°).
//!   - `H1` POS and `4J` `PS`/`POS` use
//!     `decodeStringCoordinatesDecimalMinutes`: the digit run is degrees
//!     followed by tenths of a minute (`43312` → 43° 31.2′ → 43.52°).
//!
//! The older `4J` free-text form (`N5043.5E01121.8`, a literal decimal
//! point separating degrees and decimal-minutes) is also handled.

use serde::Serialize;

/// A decoded geographic position in signed decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
}

fn dir_sign(c: u8) -> Option<f64> {
    match c {
        b'N' | b'E' => Some(1.0),
        b'S' | b'W' => Some(-1.0),
        _ => None,
    }
}

/// Split a `N<lat>` `<W/E><lon>` coordinate string into its direction
/// characters and digit runs. Handles both the contiguous
/// (`N12345W123456`) and space-separated (`N12345 W123456`) layouts, per
/// airframes' `CoordinateUtils`. Returns
/// `(lat_dir, lat_digits, lon_dir, lon_digits)`.
fn split_coord(s: &str) -> Option<(u8, &str, u8, &str)> {
    let b = s.as_bytes();
    if b.len() < 13 {
        return None;
    }
    let lat_dir = b[0];
    // The latitude digit run is the 5 chars after the direction.
    let lat_digits = &s[1..6];
    // The longitude direction is at index 6, unless that is a space, in
    // which case it shifts to index 7 (space-separated form).
    let (lon_dir, lon_digits) = if b[6] == b' ' {
        if b.len() < 14 {
            return None;
        }
        (b[7], &s[8..14])
    } else {
        (b[6], &s[7..13])
    };
    Some((lat_dir, lat_digits, lon_dir, lon_digits))
}

/// Decode a coordinate where the digit run is a plain scaled decimal degree
/// (`38160` → 38.160). Used by label `20`/POS.
pub fn decode_scaled(s: &str) -> Option<Position> {
    let (lat_dir, lat_digits, lon_dir, lon_digits) = split_coord(s)?;
    let lat_sign = dir_sign(lat_dir)?;
    let lon_sign = dir_sign(lon_dir)?;
    if !(lat_dir == b'N' || lat_dir == b'S') || !(lon_dir == b'W' || lon_dir == b'E') {
        return None;
    }
    let lat: f64 = lat_digits.parse().ok()?;
    let lon: f64 = lon_digits.parse().ok()?;
    Some(Position {
        latitude: (lat / 1000.0) * lat_sign,
        longitude: (lon / 1000.0) * lon_sign,
    })
}

/// Decode a coordinate where the digit run is degrees followed by tenths of
/// a minute (`43312` → 43° 31.2′ → 43.52°). Used by `H1` POS and `4J`
/// `PS`/`POS`.
pub fn decode_decimal_minutes(s: &str) -> Option<Position> {
    let (lat_dir, lat_digits, lon_dir, lon_digits) = split_coord(s)?;
    let lat_sign = dir_sign(lat_dir)?;
    let lon_sign = dir_sign(lon_dir)?;
    if !(lat_dir == b'N' || lat_dir == b'S') || !(lon_dir == b'W' || lon_dir == b'E') {
        return None;
    }
    let lat_raw: f64 = lat_digits.parse().ok()?;
    let lon_raw: f64 = lon_digits.parse().ok()?;
    let lat_deg = (lat_raw / 1000.0).trunc();
    let lat_min = (lat_raw % 1000.0) / 10.0;
    let lon_deg = (lon_raw / 1000.0).trunc();
    let lon_min = (lon_raw % 1000.0) / 10.0;
    Some(Position {
        latitude: (lat_deg + lat_min / 60.0) * lat_sign,
        longitude: (lon_deg + lon_min / 60.0) * lon_sign,
    })
}

/// Decode the older `4J` literal-decimal-point form: `N5043.5E01121.8`
/// (50° 43.5′, 11° 21.8′). The minutes carry an explicit decimal point.
fn decode_literal_dot(s: &str) -> Option<Position> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    let lat_sign = dir_sign(b[0])?;
    if b[0] != b'N' && b[0] != b'S' {
        return None;
    }
    // Latitude: 2-digit degrees + decimal-minutes up to the lon direction.
    let lon_pos = s[1..].find(['E', 'W'])? + 1;
    let lat_field = &s[1..lon_pos];
    let lon_sign = dir_sign(b[lon_pos])?;
    let lon_field = &s[lon_pos + 1..];

    let lat = parse_deg_min(lat_field, 2)?;
    let lon = parse_deg_min(lon_field, 3)?;
    Some(Position {
        latitude: lat * lat_sign,
        longitude: lon * lon_sign,
    })
}

/// Parse `DDMM.m` (degrees = first `deg_digits` chars, the rest are
/// decimal minutes).
fn parse_deg_min(field: &str, deg_digits: usize) -> Option<f64> {
    if field.len() <= deg_digits {
        return None;
    }
    let deg: f64 = field[..deg_digits].parse().ok()?;
    let min: f64 = field[deg_digits..].parse().ok()?;
    if min >= 60.0 {
        return None;
    }
    Some(deg + min / 60.0)
}

/// Extract a position from a free-text position report for `label`. Returns
/// `None` when the label is not a recognized free-text position report or no
/// coordinate is present.
pub fn decode(label: &str, text: &str) -> Option<Position> {
    match label {
        // Label 20/POS: "POS" preamble, comma-separated, field 0 is the
        // scaled-decimal coordinate.
        "20" => {
            let body = text.strip_prefix("POS")?;
            let first = body.split(',').next()?;
            decode_scaled(first)
        }
        // H1 POS: "POS" preamble, comma-separated, field 0 decimal-minutes.
        "H1" => {
            let body = text.strip_prefix("POS")?;
            let first = body.split(',').next()?;
            decode_decimal_minutes(first)
        }
        // 4J: either the slash-IEI form (".../PSN39277W077359,..." or
        // ".../POS N5043.5E01121.8") or the legacy free text.
        "4J" => decode_4j(text),
        _ => None,
    }
}

/// 4J carries the position in a `/PS` or `/POS ` IEI field.
fn decode_4j(text: &str) -> Option<Position> {
    for part in text.split('/') {
        if let Some(rest) = part.strip_prefix("PS") {
            // Newer packed decimal-minutes form: "PSN39277W077359,142800,..".
            let coord = rest.split(',').next()?.trim();
            if let Some(p) = decode_decimal_minutes(coord) {
                return Some(p);
            }
        }
        if let Some(rest) = part.strip_prefix("POS") {
            let coord = rest.split(',').next().unwrap_or(rest).trim();
            // Legacy literal-dot form first, then packed.
            if let Some(p) = decode_literal_dot(coord) {
                return Some(p);
            }
            if let Some(p) = decode_decimal_minutes(coord) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    // Reference strings + expected lat/lon are the real documented examples
    // from airframes' acars-decoder-typescript test suite and
    // acars-message-documentation (research/20/POS.md, H1/POS.md, 4J.md).

    #[test]
    fn label_20_pos_scaled() {
        // Label_20_POS.test.ts: 38.160 / -77.075.
        let p = decode("20", "POSN38160W077075,,211733,360,OTT,212041,,N42,19689,40,544")
            .unwrap();
        assert!(close(p.latitude, 38.160), "lat {}", p.latitude);
        assert!(close(p.longitude, -77.075), "lon {}", p.longitude);
    }

    #[test]
    fn label_20_pos_east_longitude() {
        // research/20/POS.md Example 2: N32249E045047 → 32.249 / 45.047.
        let p = decode("20", "POSN32249E045047,,082806,380,DEBNI").unwrap();
        assert!(close(p.latitude, 32.249));
        assert!(close(p.longitude, 45.047));
    }

    #[test]
    fn h1_pos_decimal_minutes() {
        // Label_H1_POS.test.ts variant 1: 43.52 / -123.29.
        let p = decode(
            "H1",
            "POSN43312W123174,EASON,215754,370,EBINY,220601,ELENN,M48,02216,185/TS215754,0921227A40",
        )
        .unwrap();
        assert!(close(p.latitude, 43.52), "lat {}", p.latitude);
        assert!(close(p.longitude, -123.29), "lon {}", p.longitude);
    }

    #[test]
    fn h1_pos_variant_2() {
        // Label_H1_POS.test.ts variant 2: 45.348 / -122.917.
        let p = decode(
            "H1",
            "POSN45209W122550,PEGTY,220309,134,MINNE,220424,HISKU,M6,060013,269,366,355K,292K,730A5B",
        )
        .unwrap();
        assert!(close(p.latitude, 45.348), "lat {}", p.latitude);
        assert!(close(p.longitude, -122.917), "lon {}", p.longitude);
    }

    #[test]
    fn label_4j_packed_ps() {
        // Label_4J_POS.test.ts: PSN39277W077359 → 39.462 / -77.598.
        let p = decode(
            "4J",
            "POS/ID91459S,BANKR31,/DC03032024,142813/MR64,0/ET31539/PSN39277W077359,142800,240,N39300W077110,031430,N38560W077150,M28,27619,MT370/CG311,160,350/FB732/VR329071",
        )
        .unwrap();
        assert!(close(p.latitude, 39.462), "lat {}", p.latitude);
        assert!(close(p.longitude, -77.598), "lon {}", p.longitude);
    }

    #[test]
    fn label_4j_legacy_literal_dot() {
        // research/4J.md: "/POS N5043.5E01121.8" → 50°43.5' / 11°21.8'.
        let p = decode(
            "4J",
            "4J01 POSWX 0318/20 ETAD/ETAD .00318S\n/POS N5043.5E01121.8/OVR 0817",
        )
        .unwrap();
        assert!(close(p.latitude, 50.0 + 43.5 / 60.0), "lat {}", p.latitude);
        assert!(close(p.longitude, 11.0 + 21.8 / 60.0), "lon {}", p.longitude);
    }

    #[test]
    fn rejects_non_position() {
        assert!(decode("20", "RST something").is_none());
        assert!(decode("H1", "#DFB engine data").is_none());
        assert!(decode("Q0", "").is_none());
        assert!(decode("4J", "no position here").is_none());
    }

    #[test]
    fn scaled_vs_decimal_minutes_differ() {
        // The same digits decode differently per convention; this guards
        // against using the wrong one.
        let scaled = decode_scaled("N43312W123174").unwrap();
        let dm = decode_decimal_minutes("N43312W123174").unwrap();
        assert!(close(scaled.latitude, 43.312));
        assert!(close(dm.latitude, 43.52));
    }
}
