//! Mode S frame validation and basic field decoding.
//!
//! Parity rules (ICAO Annex 10 Vol IV): extended squitter (DF17/18)
//! transmits a clean PI field → CRC remainder 0. DF11 all-call overlays
//! only the interrogator code in the low 7 bits. Other formats (DF0/4/5,
//! DF16/20/21) overlay the aircraft address — the remainder *is* the ICAO,
//! verifiable only against addresses learned from squitters, kept in a
//! recent-ICAO cache.

use crate::decode::{self, Cpr, Velocity};
use std::collections::HashMap;
use std::sync::OnceLock;
use xng_dsp::checksum::mode_s_crc;

/// Syndrome → bit-position table for single-bit errors in 112-bit
/// frames (the Mode S CRC is linear; bit i alone yields syndrome
/// crc(e_i)). 56-bit frames reuse the table tail.
fn single_bit_fix(syndrome: u32, nbytes: usize) -> Option<usize> {
    static TABLE: OnceLock<std::collections::HashMap<u32, usize>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut map = std::collections::HashMap::new();
        for bit in 0..112usize {
            let mut msg = [0u8; 14];
            msg[bit / 8] = 0x80 >> (bit % 8);
            let syn = mode_s_crc(&msg[..11])
                ^ u32::from_be_bytes([0, msg[11], msg[12], msg[13]]);
            map.insert(syn, bit);
        }
        map
    });
    let bit = *table.get(&syndrome)?;
    // For 56-bit frames the error must land within the short frame.
    let total_bits = nbytes * 8;
    if total_bits == 112 {
        Some(bit)
    } else {
        // 56-bit short frame: its bit k corresponds to long-frame bit
        // k + 56 in CRC terms (the polynomial acts on the tail).
        bit.checked_sub(56).filter(|&b| b < total_bits)
    }
}

/// Identification charset (TC 1–4): index 1–26 = A–Z, 32 = space,
/// 48–57 = digits.
pub(crate) const IDENT_CHARSET: &[u8; 64] =
    b"#ABCDEFGHIJKLMNOPQRSTUVWXYZ##### ###############0123456789######";

const ICAO_CACHE_MAX: usize = 8192;

#[derive(Debug, Clone, PartialEq)]
pub struct AdsbFrame {
    pub df: u8,
    pub icao: u32,
    /// Raw frame bytes (7 or 14).
    pub bytes: Vec<u8>,
    /// Identification (TC 1–4), trailing spaces trimmed.
    pub callsign: Option<String>,
    /// Barometric altitude: TC 9–18 (Q-bit or Gillham) or the DF0/4/16/20
    /// AC field.
    pub altitude_ft: Option<i32>,
    /// Transponder code (DF5/21 identity field).
    pub squawk: Option<String>,
    /// CPR-encoded position awaiting global/local resolution (TC 5–8
    /// surface, 9–18 / 20–22 airborne).
    pub cpr: Option<Cpr>,
    /// TC 19 velocity.
    pub velocity: Option<Velocity>,
    /// Resolved position (filled by the per-aircraft tracker).
    pub position: Option<(f64, f64)>,
    /// Comm-B register content (DF20/21 MB field, BDS-inferred).
    pub comm_b: Option<serde_json::Value>,
    /// ADS-B operational status (TC31: version, NACp, SIL, NIC-supp, GVA) or
    /// aircraft/emergency status (TC28).
    pub adsb_status: Option<serde_json::Value>,
    /// Signal level at decode time.
    pub level_dbfs: f32,
}

/// Validates candidate frames and learns ICAO addresses from squitters.
pub struct FrameValidator {
    /// ICAO → frame counter at last sighting (confirmed aircraft only).
    icao_cache: HashMap<u32, u64>,
    counter: u64,
    /// First sighting of a not-yet-confirmed ICAO: the held frame and
    /// the sighting-counter at arrival. A second clean frame with the
    /// same address confirms (random CRC-passing noise never repeats
    /// an address); both frames are then released. Live captures
    /// measured ~30 phantom single-sighting DF17s/minute without this.
    pending: HashMap<u32, (Vec<u8>, f32, usize, u64)>,
    /// Frames released by a confirmation, drained by the caller.
    pub released: Vec<(usize, AdsbFrame)>,
}

impl FrameValidator {
    pub fn new() -> Self {
        Self {
            icao_cache: HashMap::new(),
            counter: 0,
            pending: HashMap::new(),
            released: Vec::new(),
        }
    }

    /// Validate a demodulated candidate; returns a frame when parity
    /// checks out.
    pub fn validate(&mut self, bytes: &[u8], level_dbfs: f32, pos: usize) -> Option<AdsbFrame> {
        let df = bytes[0] >> 3;
        // Syndrome = expected parity over the data bits XOR the received
        // parity field: 0 for clean parity, the overlaid address otherwise.
        let n = bytes.len();
        let expected = mode_s_crc(&bytes[..n - 3]);
        let received = u32::from_be_bytes([0, bytes[n - 3], bytes[n - 2], bytes[n - 1]]);
        let syndrome = expected ^ received;
        let mut bytes = bytes.to_vec();
        let mut syndrome = syndrome;
        let icao = match df {
            // Extended squitter: clean parity; address in AA field.
            // The CRC is linear, so a single bit error has a unique,
            // precomputable syndrome — repair it (the parity then
            // re-verifies clean) instead of dropping the frame.
            17 | 18 => {
                if syndrome != 0 {
                    let Some(bit) = single_bit_fix(syndrome, bytes.len()) else {
                        return None;
                    };
                    bytes[bit / 8] ^= 0x80 >> (bit % 8);
                    let n = bytes.len();
                    let expected = mode_s_crc(&bytes[..n - 3]);
                    let received =
                        u32::from_be_bytes([0, bytes[n - 3], bytes[n - 2], bytes[n - 1]]);
                    syndrome = expected ^ received;
                    if syndrome != 0 || bytes[0] >> 3 != df {
                        return None;
                    }
                }
                let icao = u32::from_be_bytes([0, bytes[1], bytes[2], bytes[3]]);
                if !self.confirm(icao, &bytes, level_dbfs, pos) {
                    return None;
                }
                icao
            }
            // DF19 military extended squitter: clean PI parity with the
            // address in the AA field, exactly like DF17/18. Gate on the
            // same two-sighting confirmation. (Single-bit repair is left
            // to the ES path; military AF≠0 sub-formats are non-public.)
            19 => {
                if syndrome != 0 {
                    return None;
                }
                let icao = u32::from_be_bytes([0, bytes[1], bytes[2], bytes[3]]);
                if !self.confirm(icao, &bytes, level_dbfs, pos) {
                    return None;
                }
                icao
            }
            // All-call reply: PI overlaid with the interrogator code
            // only. DF11 carries no payload worth emitting on first
            // sight; it counts as a confirmation sighting.
            11 => {
                if syndrome & 0xFF_FF80 != 0 {
                    return None;
                }
                let icao = u32::from_be_bytes([0, bytes[1], bytes[2], bytes[3]]);
                if !self.confirm(icao, &bytes, level_dbfs, pos) {
                    return None;
                }
                icao
            }
            // Address-overlaid parity: accept only known aircraft. DF24–27
            // (Comm-D ELM) share the address-overlaid parity convention;
            // they are always 112-bit, so require the long frame.
            0 | 4 | 5 | 16 | 20 | 21 => {
                if !self.icao_cache.contains_key(&syndrome) {
                    return None;
                }
                self.learn(syndrome);
                syndrome
            }
            24..=27 => {
                if bytes.len() != 14 || !self.icao_cache.contains_key(&syndrome) {
                    return None;
                }
                self.learn(syndrome);
                syndrome
            }
            _ => return None,
        };

        let mut f = AdsbFrame {
            df,
            icao,
            bytes: bytes.clone(),
            callsign: None,
            altitude_ft: None,
            squawk: None,
            cpr: None,
            velocity: None,
            position: None,
            comm_b: None,
            adsb_status: None,
            level_dbfs,
        };
        match df {
            17 | 18 => {
                decode_extended_squitter(&bytes[4..11], &mut f);
                if df == 18 {
                    tag_df18_source(&mut f);
                }
            }
            // DF19 military extended squitter: tag the source and, for
            // AF=0, the embedded ME type code.
            19 => {
                f.adsb_status = Some(decode::military_es(&bytes));
            }
            // Surveillance altitude reply: 13-bit AC field (bits 20–32).
            // DF4/20 carry the FS/DR/UM surveillance header; DF0/16 do not.
            0 | 4 | 16 | 20 => {
                let ac = ((bytes[2] as u32 & 0x1F) << 8) | bytes[3] as u32;
                f.altitude_ft = decode::altitude13(ac);
                if df == 4 || df == 20 {
                    f.adsb_status = Some(decode::surveillance_status(&bytes));
                }
                if df == 20 && bytes.len() == 14 {
                    f.comm_b = decode::bds_infer(&bytes[4..11]);
                }
            }
            // Surveillance identity reply: 13-bit ID field → squawk, with
            // the FS/DR/UM surveillance header (DF5/21).
            5 | 21 => {
                let id = ((bytes[2] as u32 & 0x1F) << 8) | bytes[3] as u32;
                f.squawk = Some(decode::squawk13(id));
                f.adsb_status = Some(decode::surveillance_status(&bytes));
                if df == 21 && bytes.len() == 14 {
                    f.comm_b = decode::bds_infer(&bytes[4..11]);
                }
            }
            // Comm-D Extended Length Message (DF24–27): the ELM control
            // bit, D-segment number, and 80-bit message segment.
            24..=27 => {
                f.comm_b = decode::comm_d(&bytes);
            }
            _ => {}
        }
        Some(f)
    }

    /// Two-sighting confirmation for addresses asserted by a clean
    /// CRC: known ICAOs pass straight through; an unknown ICAO's first
    /// frame is held; its second sighting confirms, releases the held
    /// frame via `released`, and admits the address to the cache.
    fn confirm(&mut self, icao: u32, bytes: &[u8], level_dbfs: f32, pos: usize) -> bool {
        if self.icao_cache.contains_key(&icao) {
            self.learn(icao);
            return true;
        }
        if let Some((held, held_level, held_pos, _)) = self.pending.remove(&icao) {
            self.learn(icao);
            // Decode and release the held first frame.
            if let Some(f) = self.decode_known(&held, held_level) {
                self.released.push((held_pos, f));
            }
            return true;
        }
        // First sighting: hold. Cap the pending table (phantom
        // addresses are random and never repeat; FIFO-ish eviction by
        // age keeps it small).
        if self.pending.len() >= 64 {
            let oldest = self
                .pending
                .iter()
                .min_by_key(|(_, (_, _, _, at))| *at)
                .map(|(k, _)| *k);
            if let Some(k) = oldest {
                self.pending.remove(&k);
            }
        }
        self.pending.insert(icao, (bytes.to_vec(), level_dbfs, pos, self.counter));
        false
    }

    /// Decode a frame whose address is already trusted (used when a
    /// held first frame is released by its confirmation).
    fn decode_known(&mut self, bytes: &[u8], level_dbfs: f32) -> Option<AdsbFrame> {
        let df = bytes[0] >> 3;
        let icao = u32::from_be_bytes([0, bytes[1], bytes[2], bytes[3]]);
        let mut f = AdsbFrame {
            df,
            icao,
            bytes: bytes.to_vec(),
            callsign: None,
            altitude_ft: None,
            squawk: None,
            cpr: None,
            velocity: None,
            position: None,
            comm_b: None,
            adsb_status: None,
            level_dbfs,
        };
        if df == 17 || df == 18 {
            decode_extended_squitter(&bytes[4..11], &mut f);
            if df == 18 {
                tag_df18_source(&mut f);
            }
        } else if df == 19 {
            f.adsb_status = Some(decode::military_es(bytes));
        }
        Some(f)
    }

    fn learn(&mut self, icao: u32) {
        // The staleness clock ticks on sightings, not candidate
        // attempts: near-floor gates plus in-frame collision scanning
        // make attempt counts explode, and an attempt-based clock
        // thrashes the cache (measured: −7 unique frames).
        self.counter += 1;
        if self.icao_cache.len() >= ICAO_CACHE_MAX {
            // Drop the stalest half (rare; cheap enough).
            let cutoff = self.counter.saturating_sub((ICAO_CACHE_MAX / 2) as u64);
            self.icao_cache.retain(|_, last| *last >= cutoff);
        }
        self.icao_cache.insert(icao, self.counter);
    }
}

impl Default for FrameValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Tag a DF18 frame with its CF-derived ADS-B source classification
/// (non-transponder ADS-B / TIS-B / ADS-R). The CF field is frame bits
/// 5–7 (the low 3 bits of the first byte). The source/cf tag is folded
/// into `adsb_status` — merged into an existing TC28/29/31 status object
/// when one is present, otherwise emitted as a standalone object — so the
/// TIS-B/ADS-R provenance reaches the JSON/asf-2.0 output the crate
/// already serializes from `adsb_status`.
fn tag_df18_source(f: &mut AdsbFrame) {
    let cf = f.bytes[0] & 0x07;
    let (source, addr_type, detail) = decode::df18_cf_class(cf);
    let obj = match f.adsb_status.take() {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let mut obj = obj;
    obj.insert("cf".into(), serde_json::json!(cf));
    obj.insert("source".into(), serde_json::json!(source));
    obj.insert("source_addr_type".into(), serde_json::json!(addr_type));
    obj.insert("source_detail".into(), serde_json::json!(detail));
    f.adsb_status = Some(serde_json::Value::Object(obj));
}

/// Decode the 7-byte ME field of an extended squitter into the frame.
fn decode_extended_squitter(me: &[u8], f: &mut AdsbFrame) {
    let bit = |i: usize| (me[i / 8] >> (7 - i % 8)) & 1;
    let field = |start: usize, len: usize| -> u32 {
        (start..start + len).fold(0u32, |v, i| (v << 1) | bit(i) as u32)
    };
    let tc = field(0, 5) as u8;
    match tc {
        1..=4 => {
            let s: String = (0..8)
                .map(|i| IDENT_CHARSET[field(8 + 6 * i, 6) as usize] as char)
                .collect();
            let s = s.trim_end().to_owned();
            if !s.is_empty() && !s.contains('#') {
                f.callsign = Some(s);
            }
        }
        // Surface position: CPR over a quarter-globe span, plus the
        // Movement (ground speed) and Ground-Track fields.
        5..=8 => {
            f.cpr = Some(Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: true });
            f.velocity = decode::surface_velocity(me);
        }
        // Airborne position with barometric altitude (Q=1 25-ft linear or
        // Q=0 100-ft Gillham — both decoded via the dump1090/pyModeS path).
        9..=18 => {
            f.altitude_ft = decode::altitude12(field(8, 12));
            f.cpr = Some(Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false });
            // Per-fix position quality: version-0 NUCp from the TC plus the
            // in-message NICb supplement bit (ME bit 7). Version-aware NIC
            // needs the aircraft's TC31 supplement, applied downstream.
            f.adsb_status = decode::position_quality(tc, bit(7), None, 0, 0);
        }
        19 => {
            f.velocity = decode::velocity(me);
            // Fold the velocity-quality fields (NACv + figure of merit,
            // vertical-rate source, GNSS-minus-baro altitude difference)
            // into adsb_status — the JSON channel the crate serializes.
            if let Some(v) = f.velocity {
                let mut o = serde_json::Map::new();
                o.insert("nac_v".into(), serde_json::json!(v.nac_v));
                if let Some(hfom) = decode::nac_v_hfom_mps(v.nac_v) {
                    o.insert("nac_v_hfom_mps".into(), serde_json::json!(hfom));
                }
                o.insert(
                    "vertical_rate_source".into(),
                    serde_json::json!(if v.vr_baro_source { "baro" } else { "gnss" }),
                );
                if let Some(d) = v.geo_minus_baro_ft {
                    o.insert("geo_minus_baro_ft".into(), serde_json::json!(d));
                }
                f.adsb_status = Some(serde_json::Value::Object(o));
            }
        }
        // Airborne position with GNSS (geometric) height: the 12-bit
        // altitude is HAE metres, not barometric — surfaced under
        // adsb_status.geometric_altitude_ft rather than altitude_ft.
        20..=22 => {
            f.cpr = Some(Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false });
            let mut q = decode::position_quality(tc, bit(7), None, 0, 0)
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            if let Some(geo) = decode::gnss_height_ft(field(8, 12)) {
                q.insert("geometric_altitude_ft".into(), serde_json::json!(geo));
            }
            f.adsb_status = Some(serde_json::Value::Object(q));
        }
        // Aircraft status (emergency/priority + ACAS RA broadcast).
        28 => f.adsb_status = decode::aircraft_status(me),
        // Target state and status (MCP/FCU selected alt/heading, QNH,
        // autopilot/VNAV/approach/LNAV flags).
        29 => f.adsb_status = decode::target_state(me),
        // Operational status: ADS-B version + NACp/SIL/NIC-supp/GVA.
        31 => f.adsb_status = decode::operational_status(me),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_FRAME: [u8; 14] = [
        0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98,
    ];
    const POS_FRAME: [u8; 14] = [
        0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC, 0x28, 0x63, 0xA7,
    ];

    /// Confirm an ICAO so subsequent single frames validate directly.
    fn confirmed(v: &mut FrameValidator, frame: &[u8]) -> AdsbFrame {
        assert!(v.validate(frame, -20.0, 0).is_none(), "first sighting held");
        v.validate(frame, -20.0, 1000).expect("second sighting confirms")
    }

    #[test]
    fn decodes_published_ident_frame() {
        let mut v = FrameValidator::new();
        let f = confirmed(&mut v, &ID_FRAME);
        assert_eq!(f.df, 17);
        assert_eq!(f.icao, 0x4840D6);
        assert_eq!(f.callsign.as_deref(), Some("KLM1023"));
        assert_eq!(f.altitude_ft, None);
        // The held first frame was released alongside, fully decoded.
        assert_eq!(v.released.len(), 1);
        assert_eq!(v.released[0].0, 0);
        assert_eq!(v.released[0].1.callsign.as_deref(), Some("KLM1023"));
    }

    #[test]
    fn decodes_published_altitude_frame() {
        let mut v = FrameValidator::new();
        let f = confirmed(&mut v, &POS_FRAME);
        assert_eq!(f.icao, 0x40621D);
        assert_eq!(f.altitude_ft, Some(38_000));
        assert_eq!(v.released[0].1.altitude_ft, Some(38_000));
        // The 38000 ft frame is a TC11 airborne position → NUCp 7 in the
        // per-fix position-quality object (ADSB-1.5/2 wiring).
        let st = f.adsb_status.expect("position quality present");
        assert_eq!(st["nuc_p"], 7);
        assert_eq!(st["nuc_p_radius_m"], 93);
    }

    /// Hex → 14-byte frame.
    fn frame_bytes(hex: &str) -> [u8; 14] {
        let mut b = [0u8; 14];
        for i in 0..14 {
            b[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        b
    }

    #[test]
    fn decodes_q0_gillham_airborne_altitude() {
        // CRC-valid DF17 TC11 frame whose AC12 is a Q=0 Gillham code for
        // 5000 ft (pyModeS decode() → altitude 5000). Confirms the
        // airborne-position path now decodes the legacy 100-ft encoding.
        let mut v = FrameValidator::new();
        let frame = frame_bytes("8D40621D582482B504C5C9D9B414");
        let f = confirmed(&mut v, &frame);
        assert_eq!(f.icao, 0x40621D);
        assert_eq!(f.altitude_ft, Some(5000));
    }

    #[test]
    fn decodes_geometric_altitude_into_status() {
        // CRC-valid DF17 TC20 frame with a GNSS height of 3000 m
        // (pyModeS decode() → altitude 9842 ft). The geometric altitude
        // lands in adsb_status, not the barometric altitude_ft field.
        let mut v = FrameValidator::new();
        let frame = frame_bytes("8D40621DA0BB82B504C5C90C5BBF");
        let f = confirmed(&mut v, &frame);
        assert_eq!(f.altitude_ft, None, "geometric is not barometric alt");
        let st = f.adsb_status.expect("status present");
        assert_eq!(st["geometric_altitude_ft"], 9842);
        assert_eq!(st["nuc_p"], 9); // TC20 → NUCp 9
    }

    #[test]
    fn velocity_quality_folds_into_status() {
        // CRC-valid DF17 TC19 velocity frame (the published ground-speed
        // example): NACv/VR-source/geo-minus-baro fold into adsb_status.
        let mut v = FrameValidator::new();
        let frame = frame_bytes("8D485020994409940838175B284F");
        let f = confirmed(&mut v, &frame);
        let st = f.adsb_status.expect("velocity quality present");
        assert_eq!(st["nac_v"], 0);
        assert_eq!(st["vertical_rate_source"], "gnss");
        assert_eq!(st["geo_minus_baro_ft"], 550);
    }

    /// Hex → variable-length frame bytes.
    fn hex_frame(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn df4_surveillance_status_emitted_for_known_icao() {
        // CRC-valid DF4 reply address-overlaid with 40621D (pyModeS
        // decode() → FS 2 / DR 3 / UM 5). Accepted once that ICAO is
        // confirmed from squitters; FS/DR/UM land in adsb_status.
        let mut v = FrameValidator::new();
        // POS_FRAME confirms ICAO 40621D.
        confirmed(&mut v, &POS_FRAME);
        let f = v.validate(&hex_frame("2218A190EAA749"), -20.0, 0).expect("DF4 accepted");
        assert_eq!(f.df, 4);
        assert_eq!(f.icao, 0x40621D);
        let st = f.adsb_status.expect("surveillance status present");
        assert_eq!(st["flight_status"], 2);
        assert_eq!(st["alert"], true);
        assert_eq!(st["downlink_request"], 3);
        assert_eq!(st["utility_message"], 5);
    }

    #[test]
    fn df19_military_es_decodes_with_confirmation() {
        // CRC-clean DF19 (clean PI parity, AA = ABCDEF), AF=0 with a TC=4
        // ME. Two-sighting confirmation, then the military source tag and
        // embedded ME type code are surfaced.
        let mut v = FrameValidator::new();
        let frame = hex_frame("98ABCDEF202CC371C32CE0FC7172");
        assert!(v.validate(&frame, -20.0, 0).is_none(), "first held");
        let f = v.validate(&frame, -20.0, 1).expect("confirmed");
        assert_eq!(f.df, 19);
        assert_eq!(f.icao, 0xABCDEF);
        let st = f.adsb_status.expect("military tag present");
        assert_eq!(st["source"], "military");
        assert_eq!(st["af"], 0);
        assert_eq!(st["me_type_code"], 4);
        // The held first frame was released with the same decode.
        assert_eq!(v.released.len(), 1);
        assert_eq!(v.released[0].1.adsb_status.as_ref().unwrap()["source"], "military");
    }

    #[test]
    fn df24_comm_d_elm_decoded_for_known_icao() {
        // CRC-valid DF24 Comm-D ELM address-overlaid with 40621D (KE=0,
        // ND=5, MD 11..AA). Accepted once 40621D is confirmed.
        let mut v = FrameValidator::new();
        confirmed(&mut v, &POS_FRAME);
        let f = v
            .validate(&hex_frame("C5112233445566778899AA622DA2"), -20.0, 0)
            .expect("DF24 accepted");
        assert_eq!(f.df, 24);
        assert_eq!(f.icao, 0x40621D);
        let cd = f.comm_b.expect("comm-d present");
        assert_eq!(cd["ke"], 0);
        assert_eq!(cd["segment_number"], 5);
        assert_eq!(cd["comm_d_segment"], "112233445566778899aa");
    }

    #[test]
    fn df24_rejected_for_unknown_icao() {
        // Without a confirmed ICAO the Comm-D ELM has no verifiable
        // address and must be dropped (address-overlaid parity).
        let mut v = FrameValidator::new();
        assert!(v.validate(&hex_frame("C5112233445566778899AA622DA2"), -20.0, 0).is_none());
    }

    #[test]
    fn repairs_single_bit_rejects_double() {
        // One flipped bit: the syndrome identifies it and the frame
        // repairs to the original.
        let mut v = FrameValidator::new();
        confirmed(&mut v, &ID_FRAME);
        let mut one = ID_FRAME;
        one[6] ^= 0x01;
        let f = v.validate(&one, -20.0, 0).expect("repaired");
        assert_eq!(f.bytes, &ID_FRAME);
        // Two flipped bits: not repairable, rejected.
        let mut two = ID_FRAME;
        two[6] ^= 0x01;
        two[9] ^= 0x10;
        assert!(v.validate(&two, -20.0, 0).is_none());
    }

    #[test]
    fn unconfirmed_singletons_never_emit() {
        // A CRC-clean DF17 whose address is never seen again — the
        // phantom signature on quiet live captures — stays held.
        let mut v = FrameValidator::new();
        assert!(v.validate(&ID_FRAME, -20.0, 0).is_none());
        assert!(v.released.is_empty());
        // A different aircraft doesn't confirm it.
        assert!(v.validate(&POS_FRAME, -20.0, 500).is_none());
        assert!(v.released.is_empty());
    }

    /// Build a CRC-clean DF18 frame from a chosen CF, ICAO, and 7-byte ME
    /// (DF18 uses the same clean-PI parity as DF17 with II=0).
    fn df18_frame(cf: u8, icao: u32, me: [u8; 7]) -> [u8; 14] {
        let mut b = [0u8; 14];
        b[0] = (18 << 3) | (cf & 0x07);
        b[1] = (icao >> 16) as u8;
        b[2] = (icao >> 8) as u8;
        b[3] = icao as u8;
        b[4..11].copy_from_slice(&me);
        let crc = mode_s_crc(&b[..11]).to_be_bytes();
        b[11] = crc[1];
        b[12] = crc[2];
        b[13] = crc[3];
        b
    }

    #[test]
    fn df18_cf_tags_tisb_and_adsr_source() {
        let mut v = FrameValidator::new();
        // CF=6 (ADS-R) carrying a TC19 velocity ME (so adsb_status is not
        // otherwise populated). Two-sighting confirmation as usual.
        let me = [0x99, 0x09, 0x94, 0x09, 0x94, 0x08, 0x38];
        let frame = df18_frame(6, 0xABCDEF, me);
        assert!(v.validate(&frame, -20.0, 0).is_none(), "first held");
        let f = v.validate(&frame, -20.0, 1).expect("confirmed");
        assert_eq!(f.df, 18);
        let st = f.adsb_status.expect("source tag present");
        assert_eq!(st["cf"], 6);
        assert_eq!(st["source"], "ADS-R");
        assert_eq!(st["source_addr_type"], "adsr_icao");

        // CF=2 (fine TIS-B) on a different aircraft.
        let frame2 = df18_frame(2, 0x112233, me);
        assert!(v.validate(&frame2, -20.0, 2).is_none());
        let f2 = v.validate(&frame2, -20.0, 3).expect("confirmed");
        let st2 = f2.adsb_status.expect("source tag present");
        assert_eq!(st2["cf"], 2);
        assert_eq!(st2["source"], "TIS-B");
    }

    #[test]
    fn df18_cf_source_merges_with_tc_status() {
        // A DF18 carrying a TC28 emergency status (subtype 1) must keep
        // both the emergency fields and the CF source tag in adsb_status.
        let mut v = FrameValidator::new();
        // TC28 (= 28 << 3 = 0xE0) subtype 1, emergency state 5 at ME
        // bits 8-10 (the top 3 bits of ME byte 1 → 0xA0).
        let me = [0xE0 | 1, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x00];
        let frame = df18_frame(0, 0x445566, me);
        assert!(v.validate(&frame, -20.0, 0).is_none());
        let f = v.validate(&frame, -20.0, 1).expect("confirmed");
        let st = f.adsb_status.expect("status present");
        // CF=0 source tag.
        assert_eq!(st["source"], "ADS-B");
        assert_eq!(st["cf"], 0);
        // TC28 emergency fields preserved alongside.
        assert_eq!(st["emergency_state"], 5);
        assert_eq!(st["emergency"], "unlawful interference");
    }

    #[test]
    fn address_overlaid_frames_require_known_icao() {
        let mut v = FrameValidator::new();
        // A DF4 whose parity is overlaid with address 0x4840D6.
        let df4 = [0x20u8, 0x00, 0x05, 0x30];
        let crc = mode_s_crc(&df4).to_be_bytes();
        let addr = 0x4840D6u32.to_be_bytes();
        let mut frame = df4.to_vec();
        frame.extend([crc[1] ^ addr[1], crc[2] ^ addr[2], crc[3] ^ addr[3]]);

        // Unknown aircraft → rejected.
        assert!(v.validate(&frame, -20.0, 0).is_none());
        // Confirm the ICAO from squitters, then the DF4 is accepted.
        confirmed(&mut v, &ID_FRAME);
        let f = v.validate(&frame, -20.0, 0).unwrap();
        assert_eq!(f.df, 4);
        assert_eq!(f.icao, 0x4840D6);
    }
}
