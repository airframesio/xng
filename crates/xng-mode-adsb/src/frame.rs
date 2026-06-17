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
            // Address-overlaid parity: accept only known aircraft.
            0 | 4 | 5 | 16 | 20 | 21 => {
                if !self.icao_cache.contains_key(&syndrome) {
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
            17 | 18 => decode_extended_squitter(&bytes[4..11], &mut f),
            // Surveillance altitude reply: 13-bit AC field (bits 20–32).
            0 | 4 | 16 | 20 => {
                let ac = ((bytes[2] as u32 & 0x1F) << 8) | bytes[3] as u32;
                f.altitude_ft = decode::altitude13(ac);
                if df == 20 && bytes.len() == 14 {
                    f.comm_b = decode::bds_infer(&bytes[4..11]);
                }
            }
            // Surveillance identity reply: 13-bit ID field → squawk.
            5 | 21 => {
                let id = ((bytes[2] as u32 & 0x1F) << 8) | bytes[3] as u32;
                f.squawk = Some(decode::squawk13(id));
                if df == 21 && bytes.len() == 14 {
                    f.comm_b = decode::bds_infer(&bytes[4..11]);
                }
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
        // Airborne position with barometric altitude.
        9..=18 => {
            let alt12 = field(8, 12);
            let q = (alt12 >> 4) & 1;
            if q == 1 {
                let n = ((alt12 & 0xFE0) >> 1) | (alt12 & 0x00F);
                f.altitude_ft = Some((n as i32) * 25 - 1000);
            }
            f.cpr = Some(Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false });
        }
        19 => f.velocity = decode::velocity(me),
        // Airborne position with GNSS height: take the position; the
        // altitude encoding differs (HAE) and is left undecoded.
        20..=22 => {
            f.cpr = Some(Cpr { odd: bit(21) == 1, lat: field(22, 17), lon: field(39, 17), surface: false });
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
