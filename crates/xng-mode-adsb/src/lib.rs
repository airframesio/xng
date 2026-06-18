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
pub mod mode_ac;
pub mod modulate;

use chrono::Utc;
use decode::{Cpr, PosTrust};
use num_complex::Complex;
use std::collections::HashMap;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// CPR pairing window (Annex 10: even/odd frames within 10 s).
const CPR_PAIR_SECS: f64 = 10.0;
/// Reference-position freshness for locally unambiguous decode.
const CPR_LOCAL_SECS: f64 = 180.0;
const TRACK_MAX: usize = 4096;
/// Plausibility gate: reject a fix implying faster motion than this
/// from the aircraft's last accepted fix (~1360 kt — comfortably above
/// anything subsonic, far below the ~111 km jumps a corrupted CPR
/// field produces). A corrupted-but-CRC-clean frame (e.g. an unlucky
/// single-bit "repair" of a two-bit error) otherwise lands anywhere in
/// the CPR zone and then drags the track as the new reference.
const MAX_SPEED_MPS: f64 = 700.0;
/// Slack for CPR quantization and same-chunk timestamps.
const SPEED_GATE_SLACK_M: f64 = 500.0;
/// Consecutive gate failures that mean the *reference* is wrong (a bad
/// fix got anchored): drop it and re-acquire from an even/odd pair.
const REJECTS_TO_REANCHOR: u8 = 3;

#[derive(Default, Clone)]
struct AcState {
    even: Option<(Cpr, f64)>,
    odd: Option<(Cpr, f64)>,
    last_pos: Option<(f64, f64, f64)>,
    rejects: u8,
}

/// Decodes Mode S from a capture centered on 1090 MHz.
pub struct AdsbDecoder {
    demod: demod::PpmDemod,
    input_rate: f64,
    samples_seen: u64,
    track: HashMap<u32, AcState>,
    /// Receiver location: reference for surface-position CPR (and a
    /// fallback reference for the first airborne fix of an aircraft).
    receiver: Option<(f64, f64)>,
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
            receiver: None,
        })
    }

    /// Live/embedded variant: a single half-sample extra grid instead
    /// of the full ⅛-sample set (~3× cheaper scan, small recall cost —
    /// see docs/notes/BENCHMARKS.md).
    pub fn new_live(input_rate: f64) -> Result<Self, String> {
        Ok(Self {
            demod: demod::PpmDemod::with_phases(input_rate, &[0.5])?,
            input_rate,
            samples_seen: 0,
            track: HashMap::new(),
            receiver: None,
        })
    }

    /// Set the receiver location (enables surface-position decode).
    pub fn set_receiver_position(&mut self, lat: f64, lon: f64) {
        self.receiver = Some((lat, lon));
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<frame::AdsbFrame> {
        self.samples_seen += input.len() as u64;
        let now = self.samples_seen as f64 / self.input_rate;
        let mut frames = self.demod.process(input);
        for f in &mut frames {
            if let Some(cpr) = f.cpr {
                // Per-fix integrity containment radius (metres): the
                // NUCp/NIC `nuc_p_radius_m` the position decoder folds into
                // adsb_status for this very frame. Used to bound how far a
                // locally-referenced decode may sit from the last good fix
                // before it is downgraded / rejected (ADSB-7).
                let rc_m = f
                    .adsb_status
                    .as_ref()
                    .and_then(|s| s.get("nuc_p_radius_m"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as u32);
                let resolved = self.resolve_position(f.icao, cpr, now, rc_m);
                f.position = resolved.map(|(lat, lon, _)| (lat, lon));
                f.trust = resolved.map(|(_, _, t)| t);
            }
        }
        frames
    }

    /// Resolve a CPR report into a *graded* position: locally against the
    /// aircraft's fresh last fix when available, else globally from a fresh
    /// even/odd pair (airborne only — surface global needs a receiver
    /// reference). Returns `(lat, lon, trust)`.
    ///
    /// Graded-trust layer (ADSB-7), built on the existing resolution +
    /// speed gate, mirroring the dump1090 / pyModeS CPR trust hierarchy:
    ///
    /// - A globally-unambiguous even/odd decode needs no prior reference and
    ///   is graded [`PosTrust::GlobalUnambiguous`].
    /// - A locally-referenced decode off the aircraft's last good fix is
    ///   trusted only when it lands inside the NIC/NUCp containment of that
    ///   fix ([`decode::within_local_containment`], radius `rc_m`) for the
    ///   elapsed time — graded [`PosTrust::LocalContained`]. Outside that
    ///   bound it is a zone-wrap or a corrupted field and is *rejected*
    ///   (not merely downgraded); repeated rejections re-anchor the track.
    /// - A decode anchored on the static receiver position is graded
    ///   [`PosTrust::LocalReceiver`] (weaker — the aircraft may be far from
    ///   the receiver) and still passes the receiver-relative containment
    ///   check.
    fn resolve_position(
        &mut self,
        icao: u32,
        cpr: Cpr,
        now: f64,
        rc_m: Option<u32>,
    ) -> Option<(f64, f64, PosTrust)> {
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

        // Candidate fix + provisional trust grade, tagged by resolution
        // path. `ref_age` carries the reference's age for the local
        // containment gate (`None` for the reference-free global path).
        let (cand, trust, ref_age): (Option<(f64, f64)>, PosTrust, Option<f64>) =
            match st.last_pos {
                Some((rlat, rlon, t)) if now - t < CPR_LOCAL_SECS => (
                    Some(decode::cpr_local(cpr, rlat, rlon)),
                    PosTrust::LocalContained,
                    Some(now - t),
                ),
                _ if !cpr.surface => match (st.even, st.odd) {
                    (Some((e, te)), Some((o, to))) if (te - to).abs() < CPR_PAIR_SECS => {
                        (decode::cpr_global_airborne(e, o, to >= te), PosTrust::GlobalUnambiguous, None)
                    }
                    _ => (None, PosTrust::GlobalUnambiguous, None),
                },
                // Surface (or airborne first fix) with no fresh aircraft
                // fix: the receiver location is the reference.
                _ => (
                    self.receiver.map(|(rlat, rlon)| decode::cpr_local(cpr, rlat, rlon)),
                    PosTrust::LocalReceiver,
                    None,
                ),
            };
        let cand = cand
            .filter(|(lat, lon)| (-90.0..=90.0).contains(lat) && (-180.0..=180.0).contains(lon));

        // Local-reference containment gate (ADSB-7b): a locally-referenced
        // fix must land within the integrity-derived containment of its
        // reference, else it wrapped a CPR zone — reject it. The global
        // (reference-free) path skips this gate; the speed gate below still
        // applies to it.
        let cand = match (cand, trust, ref_age, st.last_pos) {
            (Some((lat, lon)), PosTrust::LocalContained, Some(age), Some((rlat, rlon, _))) => {
                if decode::within_local_containment(
                    lat, lon, rlat, rlon, rc_m, age, MAX_SPEED_MPS, SPEED_GATE_SLACK_M,
                    flat_distance_m,
                ) {
                    Some((lat, lon))
                } else {
                    None
                }
            }
            (c, _, _, _) => c,
        };

        // Speed gate against the last accepted fix. Repeated failures
        // mean the anchor itself is the bad fix — drop it so the next
        // even/odd pair re-acquires globally.
        let pos = match (cand, st.last_pos) {
            (Some((lat, lon)), Some((plat, plon, pt))) => {
                let dt = (now - pt).max(0.1);
                let dist = flat_distance_m(lat, lon, plat, plon);
                if dist <= MAX_SPEED_MPS * dt + SPEED_GATE_SLACK_M {
                    st.rejects = 0;
                    Some((lat, lon))
                } else {
                    st.rejects += 1;
                    if st.rejects >= REJECTS_TO_REANCHOR {
                        st.last_pos = None;
                        st.rejects = 0;
                    }
                    None
                }
            }
            (p, _) => p,
        };
        // A local fix that failed the containment gate above also counts
        // toward re-anchoring (a persistently wrong anchor is bad).
        if cand.is_none() && trust == PosTrust::LocalContained && st.last_pos.is_some() {
            st.rejects += 1;
            if st.rejects >= REJECTS_TO_REANCHOR {
                st.last_pos = None;
                st.rejects = 0;
            }
        }
        if let Some((lat, lon)) = pos {
            st.last_pos = Some((lat, lon, now));
        }
        pos.map(|(lat, lon)| (lat, lon, trust))
    }

    /// Smoothed noise-floor estimate in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.noise_dbfs()
    }
}

/// Flat-earth distance in metres — fine at speed-gate scales.
fn flat_distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = (lat1 - lat2) * 111_320.0;
    let dlon = (lon1 - lon2) * 111_320.0 * lat1.to_radians().cos();
    (dlat * dlat + dlon * dlon).sqrt()
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &frame::AdsbFrame,
    frequency_hz: u64,
    source: Provenance,
) -> Message {
    // Fold the position trust grade into the adsb_status JSON channel the
    // crate already serializes (xng-types `MessageBody::ModeS` has no typed
    // position-trust field — see shared_needs). Only emitted alongside a
    // resolved position.
    let adsb_status = match (&f.trust, f.position.is_some()) {
        (Some(trust), true) => {
            let mut o = match f.adsb_status.clone() {
                Some(serde_json::Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            o.insert("position_trust".into(), serde_json::json!(trust.as_str()));
            Some(serde_json::Value::Object(o))
        }
        _ => f.adsb_status.clone(),
    };
    Message {
        mode: Mode::Adsb,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(f.level_dbfs),
            rx_ticks_12mhz: Some(f.rx_ticks_12mhz),
            ..Default::default()
        },
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
            comm_b: f.comm_b.clone(),
            adsb_status,
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
        assert_eq!(dec.resolve_position(0x40621D, EVEN, 0.0, None), None);
        // Odd within the pairing window: global decode. The newest
        // (odd) frame's own fix is reported — the aircraft moved a
        // little between the pair, so it sits ~0.01° from the even fix
        // the decode.rs vector test pins exactly.
        let (lat, lon, trust) = dec.resolve_position(0x40621D, ODD, 1.0, None).unwrap();
        assert!((lat - 52.266).abs() < 0.01, "{lat}");
        assert!((lon - 3.939).abs() < 0.01, "{lon}");
        assert_eq!(trust, PosTrust::GlobalUnambiguous);
        // A later lone frame resolves locally off the cached fix.
        let (lat2, lon2, trust2) = dec.resolve_position(0x40621D, EVEN, 30.0, None).unwrap();
        assert!((lat2 - 52.2572).abs() < 1e-3, "{lat2}");
        assert!((lon2 - 3.91937).abs() < 1e-3, "{lon2}");
        assert_eq!(trust2, PosTrust::LocalContained);
    }

    #[test]
    fn stale_pairs_do_not_resolve() {
        let mut dec = AdsbDecoder::new(2_000_000.0).unwrap();
        assert_eq!(dec.resolve_position(0x40621D, EVEN, 0.0, None), None);
        // 60 s later: outside the 10 s pairing window, no reference.
        assert_eq!(dec.resolve_position(0x40621D, ODD, 60.0, None), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpr_of(frame_hex: &str) -> Cpr {
        let bytes: Vec<u8> = (0..frame_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&frame_hex[i..i + 2], 16).unwrap())
            .collect();
        let me = &bytes[4..11];
        let bit = |i: usize| ((me[i / 8] >> (7 - i % 8)) & 1) as u32;
        let field = |s: usize, l: usize| (s..s + l).fold(0u32, |v, i| (v << 1) | bit(i));
        Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false }
    }

    // Worked examples from "The 1090 Megahertz Riddle".
    const EVEN: &str = "8D40621D58C382D690C8AC2863A7";
    const ODD: &str = "8D40621D58C386435CC412692AD6";

    #[test]
    fn trust_grades_global_then_local_contained() {
        // Oracle: synthetic CPR round-trip. A fresh even/odd pair yields a
        // globally-unambiguous fix; a subsequent lone frame decoded off
        // that fix is local and lands inside the NUCp-7 containment of the
        // anchor → graded LocalContained.
        let mut d = AdsbDecoder::new(2_000_000.0).unwrap();
        let icao = 0x40621D;
        let (even, odd) = (cpr_of(EVEN), cpr_of(ODD));
        assert!(d.resolve_position(icao, even, 0.0, Some(93)).is_none());
        let (_, _, t0) = d.resolve_position(icao, odd, 1.0, Some(93)).expect("global");
        assert_eq!(t0, PosTrust::GlobalUnambiguous);
        // The book even/odd fixes are ~960 m apart (real aircraft motion at
        // ~82 m/s, i.e. ~12 s of flight); a local EVEN decode 30 s after the
        // ODD anchor is well inside the motion + NUCp-7 containment budget.
        let (lat, lon, t1) = d.resolve_position(icao, even, 30.0, Some(93)).expect("local");
        assert_eq!(t1, PosTrust::LocalContained);
        assert!((lat - 52.2572).abs() < 1e-3 && (lon - 3.91937).abs() < 1e-3);
    }

    #[test]
    fn local_containment_gate_rejects_zone_wrap() {
        // A corrupted local CPR (high longitude bit flipped) decodes a full
        // CPR zone away — far outside the NUCp containment of the fresh
        // anchor for the tiny elapsed time. The containment gate (ADSB-7b)
        // rejects it even though a single bad fix is below the coarse
        // speed-gate threshold for a few seconds of flight.
        let mut d = AdsbDecoder::new(2_000_000.0).unwrap();
        let icao = 0x40621D;
        let (even, odd) = (cpr_of(EVEN), cpr_of(ODD));
        assert!(d.resolve_position(icao, even, 0.0, Some(93)).is_none());
        assert!(d.resolve_position(icao, odd, 1.0, Some(93)).is_some());
        // Flip a high longitude bit → the local decode lands a CPR
        // sub-zone away (tens of km), far outside the NUCp-7 containment of
        // the fresh anchor for the small elapsed time. A single rejection
        // does not drop the anchor (needs 3 in a row).
        let wrapped = Cpr { lon: even.lon ^ 0x8000, ..even };
        assert!(
            d.resolve_position(icao, wrapped, 1.5, Some(93)).is_none(),
            "zone-wrapped local fix rejected by containment gate"
        );
        // The good anchor survives: a clean local frame 30 s later (enough
        // motion budget for the genuine ~960 m even/odd offset) resolves.
        let good = d.resolve_position(icao, even, 31.0, Some(93)).expect("good local");
        assert_eq!(good.2, PosTrust::LocalContained);
    }

    #[test]
    fn containment_cap_is_additive_over_speed_gate() {
        // The strictly-additive case the half-zone cap buys over the
        // existing (unbounded) speed gate: a ~339 km jump (a corrupted CPR
        // sub-zone of the book even frame) sits ABOVE the capped containment
        // bound but BELOW the raw speed budget at 600 s elapsed — so the
        // containment gate rejects it while a pure speed gate would admit
        // it. (Oracle: dump1090 local-decode ±½-zone range limit; the
        // 339 km figure is the flat-earth distance of `even.lon ^ 0x10000`
        // decoded locally against the book anchor.)
        let bound =
            decode::local_containment_radius_m(Some(93), 600.0, MAX_SPEED_MPS, SPEED_GATE_SLACK_M);
        let speed_bound = MAX_SPEED_MPS * 600.0 + SPEED_GATE_SLACK_M;
        let jump_m = flat_distance_m(52.2572, 8.91937, 52.2572, 3.91937);
        assert!((jump_m - 339_369.0).abs() < 2000.0, "jump {jump_m}");
        assert!(jump_m > bound, "containment rejects: jump {jump_m} > bound {bound}");
        assert!(jump_m < speed_bound, "speed gate would admit: {jump_m} < {speed_bound}");
    }

    #[test]
    fn to_message_surfaces_position_trust() {
        // The trust grade reaches the serialized adsb_status JSON channel.
        let f = frame::AdsbFrame {
            df: 17,
            icao: 0x40621D,
            bytes: vec![0u8; 14],
            callsign: None,
            altitude_ft: None,
            squawk: None,
            cpr: None,
            velocity: None,
            position: Some((52.2572, 3.91937)),
            trust: Some(PosTrust::GlobalUnambiguous),
            comm_b: None,
            adsb_status: Some(serde_json::json!({ "nuc_p": 7 })),
            level_dbfs: -20.0,
            rx_ticks_12mhz: 0,
        };
        let source = Provenance {
            station: xng_types::StationIdentity::new("XX-TEST-ADSB"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let msg = to_message(&f, 1_090_000_000, source);
        if let MessageBody::ModeS { adsb_status: Some(st), lat, .. } = msg.body {
            assert_eq!(st["position_trust"], "global");
            assert_eq!(st["nuc_p"], 7); // existing quality preserved
            assert_eq!(lat, Some(52.2572));
        } else {
            panic!("expected ModeS body with adsb_status");
        }
    }

    #[test]
    fn speed_gate_rejects_corrupted_fix_and_reanchors() {
        let mut d = AdsbDecoder::new(2_000_000.0).unwrap();
        let icao = 0x40621D;
        let (even, odd) = (cpr_of(EVEN), cpr_of(ODD));

        // Establish a track from a fresh even/odd pair.
        assert!(d.resolve_position(icao, even, 0.0, None).is_none());
        let fix = d.resolve_position(icao, odd, 1.0, None).expect("global fix");
        assert!((fix.0 - 52.26578).abs() < 0.01, "lat {}", fix.0);

        // A corrupted-but-CRC-clean report: high CPR lat bit flipped.
        // Local decode would land tens of km away — the gate drops it
        // and the last good fix stays the reference.
        let bad = Cpr { lat: even.lat ^ 0x10000, ..even };
        assert!(d.resolve_position(icao, bad, 2.0, None).is_none());
        let good = d.resolve_position(icao, even, 3.0, None).expect("good fix still accepted");
        assert!((good.0 - 52.2572).abs() < 0.01, "lat {}", good.0);

        // Three consecutive implausible fixes mean the *anchor* is bad:
        // the track drops it and re-acquires from a fresh pair.
        for t in 0..3 {
            assert!(d.resolve_position(icao, bad, 4.0 + t as f64, None).is_none());
        }
        assert!(d.track[&icao].last_pos.is_none(), "anchor dropped");
        assert!(d.resolve_position(icao, even, 20.0, None).is_none());
        assert!(d.resolve_position(icao, odd, 20.5, None).is_some(), "re-anchored");
    }
}
