//! Synthesize DF17 1090ES extended-squitter frames from a decoded aircraft
//! fix. This lets non-Mode-S position sources (UAT 978, HFDL) feed any raw-1090
//! / Beast consumer (tar1090, readsb, aggregators) that doesn't read SBS or
//! `aircraft.json` — the same trick `uat2esnt` uses for UAT (XM-2.2, Beast half).
//!
//! Every encoder is the inverse of a function in [`crate::decode`] and is
//! proved correct by ROUND-TRIPPING through this crate's own (benchmark-
//! validated) decoder in the tests — not by self-consistency alone. Altitude is
//! encoded by searching the real decoder for the field that reproduces it, so
//! there is no hand-rolled bit layout to get wrong.

use crate::decode::{altitude12, nl};
use xng_dsp::checksum::mode_s_crc;

const CPR_MAX: f64 = 131_072.0; // 2^17
const NZ: f64 = 15.0;

/// ADS-B message source — picks the downlink format + 3-bit control field of
/// the synthesized frame. Native ADS-B is DF17 (CA=5); rebroadcast traffic is
/// DF18 with the CF that [`crate::decode::df18_cf_class`] reads back as TIS-B
/// or ADS-R, so a UAT 978 target keeps its provenance when replotted on 1090.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EsSource {
    /// Native ADS-B — DF17, CA=5.
    Adsb,
    /// TIS-B (ground-station rebroadcast of secondary surveillance) — DF18 CF=2.
    TisB,
    /// ADS-R (rebroadcast from the other data link, e.g. UAT↔1090) — DF18 CF=6.
    AdsR,
}

impl EsSource {
    /// `(downlink format, 3-bit CA/CF)` for this source.
    fn df_cf(self) -> (u8, u8) {
        match self {
            EsSource::Adsb => (17, 5), // CA=5 level-2 transponder
            EsSource::TisB => (18, 2), // CF=2 fine TIS-B, ICAO address
            EsSource::AdsR => (18, 6), // CF=6 ADS-R rebroadcast
        }
    }

    /// Build a 14-byte extended squitter carrying `me` for this source.
    fn frame(self, icao: u32, me: u64) -> [u8; 14] {
        let (df, field) = self.df_cf();
        es_frame(df, field, icao, me)
    }
}

/// Pack a 56-bit ME field into a DF17/DF18 extended squitter for `icao`
/// (`field` = the 3-bit CA on DF17 / CF on DF18), append the clean 24-bit
/// Mode S parity, and return the 14 bytes.
fn es_frame(df: u8, field: u8, icao: u32, me: u64) -> [u8; 14] {
    let mut b = [0u8; 14];
    b[0] = (df << 3) | (field & 7);
    b[1] = (icao >> 16) as u8;
    b[2] = (icao >> 8) as u8;
    b[3] = icao as u8;
    for k in 0..7 {
        b[4 + k] = (me >> (8 * (6 - k))) as u8;
    }
    let crc = mode_s_crc(&b[..11]).to_be_bytes();
    b[11] = crc[1];
    b[12] = crc[2];
    b[13] = crc[3];
    b
}

/// CPR-encode `(lat, lon)` for the even (`odd=false`, i=0) or odd (i=1) airborne
/// frame → the two 17-bit fractions `(yz, xz)`. Standard CPR (the inverse of
/// [`crate::decode::cpr_global_airborne`]); `nl()` is shared with the decoder.
fn cpr_encode(lat: f64, lon: f64, odd: bool) -> (u32, u32) {
    let i = if odd { 1.0 } else { 0.0 };
    let dlat = 360.0 / (4.0 * NZ - i);
    let yz = (CPR_MAX * lat.rem_euclid(dlat) / dlat + 0.5).floor();
    let rlat = dlat * (yz / CPR_MAX + (lat / dlat).floor());
    let dlon = 360.0 / (nl(rlat) as f64 - i).max(1.0);
    let xz = (CPR_MAX * lon.rem_euclid(dlon) / dlon + 0.5).floor();
    (yz as u32 & 0x1_FFFF, xz as u32 & 0x1_FFFF)
}

/// Encode a barometric altitude (ft) into the 12-bit AC12 field (Q=1, 25 ft) by
/// finding the field the real decoder maps back to it — correct by construction.
/// 0 (= "no altitude") when out of the encodable range.
fn encode_alt12(alt_ft: i32) -> u32 {
    let target = (alt_ft as f64 / 25.0).round() as i32 * 25;
    (1u32..4096)
        .find(|&ac| ac & 0x10 != 0 && altitude12(ac) == Some(target))
        .unwrap_or(0)
}

/// The 6-bit ADS-B callsign charset index for an ASCII byte (A–Z, 0–9, space);
/// anything else maps to space. Inverse of [`crate::frame::IDENT_CHARSET`].
fn ident_code(c: u8) -> u64 {
    match c {
        b'A'..=b'Z' => (c - b'A' + 1) as u64,
        b'0'..=b'9' => (c - b'0' + 48) as u64,
        b'a'..=b'z' => (c - b'a' + 1) as u64, // fold lowercase → uppercase
        _ => 32,                              // space
    }
}

/// The even+odd airborne-position pair (DF17 TC11). Both are needed for a
/// receiver to globally decode the position. `alt_ft` of `None` emits a
/// zero (not-available) altitude.
pub fn airborne_position(
    src: EsSource,
    icao: u32,
    lat: f64,
    lon: f64,
    alt_ft: Option<i32>,
) -> [[u8; 14]; 2] {
    let ac = alt_ft.map(encode_alt12).unwrap_or(0) as u64;
    let frame = |odd: bool| {
        let (yz, xz) = cpr_encode(lat, lon, odd);
        // ME (56b): TC(5)=11 · SS(2)=0 · NICsupp(1)=0 · ALT(12) · T(1)=0 ·
        //           F(1)=odd · LAT-CPR(17) · LON-CPR(17)
        let mut me: u64 = 11; // TC
        me = (me << 2) | 0; // SS
        me = (me << 1) | 0; // NIC supplement
        me = (me << 12) | ac; // altitude
        me = (me << 1) | 0; // T (UTC sync)
        me = (me << 1) | odd as u64; // CPR odd/even
        me = (me << 17) | yz as u64; // LAT-CPR
        me = (me << 17) | xz as u64; // LON-CPR
        src.frame(icao, me)
    };
    [frame(false), frame(true)]
}

/// The identification/callsign frame (DF17 TC4). `callsign` is upper-cased,
/// space-padded, and truncated to 8 chars.
pub fn identification(src: EsSource, icao: u32, callsign: &str) -> [u8; 14] {
    // ME (56b): TC(5)=4 · EC(3)=0 · 8 × CHAR(6)
    let mut me: u64 = 4;
    me = (me << 3) | 0; // emitter category
    let cs = callsign.as_bytes();
    for k in 0..8 {
        let c = cs.get(k).copied().unwrap_or(b' ');
        me = (me << 6) | ident_code(c);
    }
    src.frame(icao, me)
}

/// The airborne-velocity frame (DF17 TC19 subtype 1, ground speed) from a
/// ground speed (kt) + true track (deg) and optional vertical rate (fpm).
/// Inverse of [`crate::decode::velocity`]'s subtype-1 path.
pub fn velocity_frame(
    src: EsSource,
    icao: u32,
    gs_kt: f64,
    track_deg: f64,
    vrate_fpm: Option<i32>,
) -> [u8; 14] {
    let tr = track_deg.to_radians();
    let vx = gs_kt * tr.sin(); // east
    let vy = gs_kt * tr.cos(); // north
    let (s_ew, v_ew) = ((vx < 0.0) as u64, (vx.abs().round() as u64 + 1).min(1023));
    let (s_ns, v_ns) = ((vy < 0.0) as u64, (vy.abs().round() as u64 + 1).min(1023));
    let (svr, vr) = match vrate_fpm {
        Some(r) => ((r < 0) as u64, ((r.abs() as f64 / 64.0).round() as u64 + 1).min(511)),
        None => (0, 0),
    };
    // ME (56b): TC(5)=19 · ST(3)=1 · IC(1) · RESV(1) · NACv(3) · Dew(1) ·
    //   Vew(10) · Dns(1) · Vns(10) · VrSrc(1)=GNSS · Svr(1) · VR(9) · RESV(2) ·
    //   Sdif(1) · Ddif(7)
    let mut me: u64 = 19;
    me = (me << 3) | 1; // subtype 1 (subsonic ground speed)
    me = (me << 1) | 0; // intent change
    me = (me << 1) | 0; // reserved (IFR)
    me = (me << 3) | 0; // NACv
    me = (me << 1) | s_ew;
    me = (me << 10) | v_ew;
    me = (me << 1) | s_ns;
    me = (me << 10) | v_ns;
    me = (me << 1) | 0; // vertical-rate source = GNSS
    me = (me << 1) | svr;
    me = (me << 9) | vr;
    me = (me << 2) | 0; // reserved
    me = (me << 1) | 0; // GNSS-baro sign
    me = (me << 7) | 0; // GNSS-baro difference
    src.frame(icao, me)
}

/// All synthesizable frames for an aircraft fix: the position pair, a callsign
/// frame when present, and a velocity frame when a ground speed + track are
/// present. Returns empty when there is no position to convey (a synthesized ES
/// with no payload is pointless).
pub fn synth_frames(
    src: EsSource,
    icao: u32,
    lat: Option<f64>,
    lon: Option<f64>,
    alt_ft: Option<i32>,
    callsign: Option<&str>,
    gs_kt: Option<f64>,
    track_deg: Option<f64>,
    vrate_fpm: Option<i32>,
) -> Vec<[u8; 14]> {
    let mut out = Vec::new();
    if let (Some(la), Some(lo)) = (lat, lon) {
        out.extend(airborne_position(src, icao, la, lo, alt_ft));
    }
    if let Some(cs) = callsign {
        let cs = cs.trim();
        if !cs.is_empty() {
            out.push(identification(src, icao, cs));
        }
    }
    if let (Some(gs), Some(tr)) = (gs_kt, track_deg) {
        out.push(velocity_frame(src, icao, gs, tr, vrate_fpm));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{cpr_global_airborne, Cpr};
    use crate::frame::IDENT_CHARSET;

    // Extract a Cpr from a synthesized frame the same way the decoder does.
    fn cpr_of(f: &[u8; 14]) -> Cpr {
        let me = &f[4..11];
        let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
        let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
        Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false }
    }

    fn crc_clean(f: &[u8; 14]) -> bool {
        let recv = u32::from_be_bytes([0, f[11], f[12], f[13]]);
        mode_s_crc(&f[..11]) == recv
    }

    #[test]
    fn position_round_trips_through_decoder() {
        // The 1090 Megahertz Riddle worked example.
        let (icao, lat, lon) = (0x40621D, 52.25720, 3.91937);
        let [even, odd] = airborne_position(EsSource::Adsb, icao, lat, lon, Some(38000));
        assert!(crc_clean(&even) && crc_clean(&odd), "parity clean");
        assert_eq!(even[0] >> 3, 17, "DF17");
        assert_eq!(u32::from_be_bytes([0, even[1], even[2], even[3]]), icao);
        let (dlat, dlon) = cpr_global_airborne(cpr_of(&even), cpr_of(&odd), false).unwrap();
        assert!((dlat - lat).abs() < 1e-4, "lat {dlat} vs {lat}");
        assert!((dlon - lon).abs() < 1e-4, "lon {dlon} vs {lon}");
    }

    #[test]
    fn position_round_trips_southern_western_hemisphere() {
        let (icao, lat, lon) = (0xABCDEF, -33.8688, 151.2093); // Sydney
        let [even, odd] = airborne_position(EsSource::Adsb, icao, lat, lon, None);
        let (dlat, dlon) = cpr_global_airborne(cpr_of(&even), cpr_of(&odd), false).unwrap();
        assert!((dlat - lat).abs() < 1e-4, "lat {dlat} vs {lat}");
        assert!((dlon - lon).abs() < 1e-4, "lon {dlon} vs {lon}");
    }

    // NEW-P0-1.3: a rebroadcast source synthesizes a DF18 frame whose CF the
    // decoder reads back (df18_cf_class) as the right provenance, while the
    // position still decodes — so UAT TIS-B/ADS-R keep their class on 1090.
    #[test]
    fn rebroadcast_synthesizes_df18_with_correct_cf() {
        use crate::decode::df18_cf_class;
        let (icao, lat, lon) = (0xA12345, 37.6189, -122.3750);
        for (src, want_class, want_key) in [
            (EsSource::TisB, "TIS-B", "tisb_icao"),
            (EsSource::AdsR, "ADS-R", "adsr_icao"),
        ] {
            let [even, odd] = airborne_position(src, icao, lat, lon, Some(9500));
            assert!(crc_clean(&even) && crc_clean(&odd), "parity clean");
            assert_eq!(even[0] >> 3, 18, "DF18 for {src:?}");
            let cf = even[0] & 7;
            let (class, key, _) = df18_cf_class(cf);
            assert_eq!(class, want_class, "{src:?} cf={cf}");
            assert_eq!(key, want_key, "{src:?} cf={cf}");
            // Position still decodes through the same CPR path.
            let (dlat, dlon) = cpr_global_airborne(cpr_of(&even), cpr_of(&odd), false).unwrap();
            assert!((dlat - lat).abs() < 1e-4 && (dlon - lon).abs() < 1e-4, "{src:?}");
        }
    }

    #[test]
    fn altitude_encodes_to_nearest_25ft() {
        // Decoder reproduces the encoded altitude (rounded to 25 ft).
        for alt in [0, 1000, 38000, 9525, 45000] {
            let ac = encode_alt12(alt);
            assert_eq!(altitude12(ac), Some((alt as f64 / 25.0).round() as i32 * 25), "alt {alt}");
        }
    }

    #[test]
    fn identification_round_trips_callsign() {
        let f = identification(EsSource::Adsb, 0x484149, "KLM1023 ");
        assert!(crc_clean(&f));
        assert_eq!(f[4] >> 3, 4, "TC4");
        let me = &f[4..11];
        let bit = |i: usize| (me[i / 8] >> (7 - i % 8)) & 1;
        let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i) as u32);
        let cs: String = (0..8).map(|k| IDENT_CHARSET[field(8 + 6 * k, 6) as usize] as char).collect();
        assert_eq!(cs.trim_end(), "KLM1023");
    }

    // Read a 56-bit ME field's `velocity` back through the real decoder.
    fn velocity_of(f: &[u8; 14]) -> crate::decode::Velocity {
        crate::decode::velocity(&f[4..11]).expect("subtype-1 velocity")
    }

    #[test]
    fn velocity_round_trips_through_decoder() {
        // Due west at 420 kt, descending 1024 fpm.
        let f = velocity_frame(EsSource::Adsb, 0x40621D, 420.0, 270.0, Some(-1024));
        assert!(crc_clean(&f), "parity clean");
        assert_eq!(f[4] >> 3, 19, "TC19");
        let v = velocity_of(&f);
        assert!(!v.airspeed, "ground speed, not airspeed");
        assert!((v.speed_kt - 420.0).abs() < 1.0, "speed {}", v.speed_kt);
        assert!((v.track_deg - 270.0).abs() < 0.5, "track {}", v.track_deg);
        assert_eq!(v.vertical_rate_fpm, Some(-1024), "vrate");

        // A north-east climb exercises the other quadrant + positive vrate.
        let v2 = velocity_of(&velocity_frame(EsSource::Adsb, 0x40621D, 300.0, 45.0, Some(1472)));
        assert!((v2.speed_kt - 300.0).abs() < 1.0, "speed {}", v2.speed_kt);
        assert!((v2.track_deg - 45.0).abs() < 0.5, "track {}", v2.track_deg);
        assert_eq!(v2.vertical_rate_fpm, Some(1472), "vrate");
    }

    #[test]
    fn synth_frames_emits_pair_plus_ident_plus_velocity() {
        // Position + callsign + ground speed/track → even, odd, ident, velocity.
        let v = synth_frames(
            EsSource::Adsb,
            0x40621D,
            Some(52.2),
            Some(3.9),
            Some(35000),
            Some("TEST123"),
            Some(450.0),
            Some(90.0),
            Some(0),
        );
        assert_eq!(v.len(), 4, "even + odd + ident + velocity");
        // Position only (no callsign, no velocity) → just the even/odd pair.
        let p =
            synth_frames(EsSource::Adsb, 0x40621D, Some(52.2), Some(3.9), None, None, None, None, None);
        assert_eq!(p.len(), 2, "even + odd");
        // No position → nothing (a payload-less ES is pointless).
        assert!(
            synth_frames(EsSource::Adsb, 0x40621D, None, None, Some(35000), None, None, None, None)
                .is_empty()
        );
    }
}
