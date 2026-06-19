//! Aero Signal Units (ported from JAERO `aerol.cpp`): 12-byte SUs with
//! CRC-16/X-25 trailers, ISU (0x71) + SSU (0xC0|seq) reassembly into user
//! data, ACARS extraction, and multi-block defragmentation.

use serde::Serialize;
use xng_acars::block::AcarsBlock;
use xng_dsp::checksum::HDLC_FCS;

pub const SU_LEN: usize = 12;

/// Check a 12-byte SU: CRC-16/X-25 over the first 10 bytes, little-endian
/// trailer. The all-zero SU is accepted (JAERO rule).
pub fn su_crc_ok(su: &[u8]) -> bool {
    if su.len() != SU_LEN {
        return false;
    }
    if su.iter().all(|&b| b == 0) {
        return true;
    }
    HDLC_FCS.checksum(&su[..10]) == u16::from_le_bytes([su[10], su[11]])
}

/// Compute and append the SU CRC (testing/modulation).
pub fn su_with_crc(mut su10: Vec<u8>) -> Vec<u8> {
    debug_assert_eq!(su10.len(), 10);
    let crc = HDLC_FCS.checksum(&su10);
    su10.extend(crc.to_le_bytes());
    su10
}

/// Decoded content of one reassembled user-data unit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AeroUserData {
    pub aes_id: String,
    pub ges_id: u8,
    pub qno: u8,
    pub refno: u8,
    /// Raw reassembled user data (FF FF + ACARS block, or other).
    #[serde(skip_serializing)]
    pub data: Vec<u8>,
}

struct PendingIsu {
    aes_id: u32,
    ges_id: u8,
    qno: u8,
    refno: u8,
    seq_remaining: u8,
    last_ssu_octets: u8,
    data: Vec<u8>,
    age: u32,
}

/// ISU/SSU reassembler (keyed by AES/GES/QNO/REFNO, SSU SEQNO counts down).
pub struct Reassembler {
    pending: Vec<PendingIsu>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self { pending: Vec::new() }
    }

    /// Feed one CRC-valid SU; returns completed user data when a message
    /// finishes reassembly.
    pub fn push(&mut self, su: &[u8]) -> Option<AeroUserData> {
        debug_assert_eq!(su.len(), SU_LEN);
        let kind = su[0];

        // Age out stale partial messages.
        for p in &mut self.pending {
            p.age += 1;
        }
        self.pending.retain(|p| p.age < 10);

        if kind == 0x71 {
            let isu = PendingIsu {
                aes_id: u32::from_be_bytes([0, su[1], su[2], su[3]]),
                ges_id: su[4],
                qno: su[5] >> 4,
                refno: su[5] & 0x0F,
                seq_remaining: su[6] & 0x3F,
                last_ssu_octets: (su[7] >> 4) & 0x0F,
                data: su[8..10].to_vec(),
                age: 0,
            };
            if isu.seq_remaining == 0 {
                return Some(finish(isu));
            }
            self.pending.push(isu);
            return None;
        }
        if kind & 0xC0 == 0xC0 {
            let seq = su[0] & 0x3F;
            let qno = su[1] >> 4;
            let refno = su[1] & 0x0F;
            let idx = self
                .pending
                .iter()
                .position(|p| p.seq_remaining == seq + 1 && p.qno == qno && p.refno == refno)?;
            let p = &mut self.pending[idx];
            p.seq_remaining = seq;
            p.age = 0;
            if seq == 0 {
                // Final SSU: only the declared tail octets.
                let take = p.last_ssu_octets.min(8) as usize;
                p.data.extend_from_slice(&su[2..2 + take]);
                let done = self.pending.swap_remove(idx);
                return Some(finish(done));
            }
            p.data.extend_from_slice(&su[2..10]);
            return None;
        }
        None
    }
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

fn finish(p: PendingIsu) -> AeroUserData {
    AeroUserData {
        aes_id: format!("{:06X}", p.aes_id),
        ges_id: p.ges_id,
        qno: p.qno,
        refno: p.refno,
        data: p.data,
    }
}

/// Try to parse reassembled user data as an ACARS block (JAERO layout:
/// FF FF prefix then a standard SOH block).
pub fn parse_acars(data: &[u8]) -> Option<AcarsBlock> {
    let start = data.iter().position(|&b| b != 0xFF)?;
    xng_acars::block::parse(&data[start..])
}

/// Build the SU sequence for a user-data payload (testing/modulation):
/// one ISU + as many SSUs as needed.
pub fn build_isu_chain(aes_id: u32, ges_id: u8, qno: u8, refno: u8, data: &[u8]) -> Vec<Vec<u8>> {
    let rest = data.len().saturating_sub(2);
    let n_ssus = rest.div_ceil(8);
    let last_octets = if rest == 0 { 0 } else { rest - (n_ssus - 1) * 8 };
    let mut sus = Vec::with_capacity(1 + n_ssus);

    let aes = aes_id.to_be_bytes();
    let mut isu = vec![
        0x71,
        aes[1],
        aes[2],
        aes[3],
        ges_id,
        (qno << 4) | (refno & 0x0F),
        n_ssus as u8 & 0x3F,
        ((last_octets as u8) << 4) & 0xF0,
    ];
    isu.push(*data.first().unwrap_or(&0));
    isu.push(*data.get(1).unwrap_or(&0));
    sus.push(su_with_crc(isu));

    for k in 0..n_ssus {
        let seq = (n_ssus - 1 - k) as u8;
        let mut ssu = vec![0xC0 | seq, (qno << 4) | (refno & 0x0F)];
        let off = 2 + k * 8;
        for i in 0..8 {
            ssu.push(*data.get(off + i).unwrap_or(&0));
        }
        sus.push(su_with_crc(ssu));
    }
    sus
}

pub const R_SU_LEN: usize = 19;

/// Check a 19-byte R-channel SU (CRC-16/X-25 over the first 17 bytes).
pub fn r_su_crc_ok(su: &[u8]) -> bool {
    su.len() == R_SU_LEN
        && HDLC_FCS.checksum(&su[..17]) == u16::from_le_bytes([su[17], su[18]])
}

/// SEQINDICATOR nibble → (index k, total n), k and n 1-based. Verified
/// against JAERO `RISUData::update` / the R-channel switch in `aerol.cpp`
/// (1→(1,1), 2→(1,2), 3→(2,2), 4→(1,3), 5→(2,3), 6→(3,3); JAERO's SUindex
/// is 0-based so k = SUindex+1). See `seq_indicator_matches_jaero_switch`.
fn seq_indicator(v: u8) -> Option<(u8, u8)> {
    match v {
        1 => Some((1, 1)),
        2 => Some((1, 2)),
        3 => Some((2, 2)),
        4 => Some((1, 3)),
        5 => Some((2, 3)),
        6 => Some((3, 3)),
        _ => None,
    }
}

fn seq_indicator_for(k: u8, n: u8) -> u8 {
    match (k, n) {
        (1, 1) => 1,
        (1, 2) => 2,
        (2, 2) => 3,
        (1, 3) => 4,
        (2, 3) => 5,
        (3, 3) => 6,
        _ => 0,
    }
}

/// Classify a 19-byte R-channel SU into a named control event when it is
/// not user data. JAERO (`aerol.cpp`, R-channel branch) reads the message
/// type from the **third** byte (`infofield[2]` = su[2]); a set user-data
/// flag (`infofield[1] & 0x08`) overrides any type byte and routes the SU
/// to the ISU/SSU reassembler instead. The named control types are JAERO's
/// `AEROTypeR` enum (`aerol.h`):
/// 0x20 general access-request (telephone), 0x23 abbreviated access-request
/// (telephone), 0x22 access-request (data, R/T channel), 0x61 request-for-
/// acknowledgement, 0x62 acknowledgement, 0x12 log-on/log-off control,
/// 0x30 call-progress, 0x15 log-on/log-off acknowledgement, 0x17 log-control
/// ready-for-reassignment, 0x60 telephony-acknowledge.
///
/// Returns `None` for user-data SUs (handled by [`RIsuReassembler`]) and
/// for an unrecognized control byte. JAERO only *names* these control
/// types — for a control SU the type occupies the same byte (su[2]) the
/// user-data path uses for the AES high octet, so the AES/GES fields do
/// not apply; we surface just the named control event, matching JAERO.
pub fn parse_r_su(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < R_SU_LEN {
        return None;
    }
    // User-data flag (JAERO `infofield[1] & 0x08`) → not a control SU.
    if su[1] & 0x08 != 0 {
        return None;
    }
    let (su_type, kind) = match su[2] {
        0x20 => ("r-access-request", "general-telephone"),
        0x23 => ("r-access-request", "abbreviated-telephone"),
        0x22 => ("r-access-request", "data"),
        0x61 => ("r-request-for-acknowledgement", ""),
        0x62 => ("r-acknowledgement", ""),
        0x12 => ("r-log-on-off-control", ""),
        0x30 => ("r-call-progress", ""),
        0x15 => ("r-log-on-off-acknowledgement", ""),
        0x17 => ("r-log-control-ready-for-reassignment", ""),
        0x60 => ("r-telephony-acknowledge", ""),
        _ => return None,
    };
    let mut v = serde_json::json!({
        "su_type": su_type,
        "su_type_hex": format!("0x{:02X}", su[2]),
    });
    if !kind.is_empty() {
        v["request_kind"] = serde_json::json!(kind);
    }
    Some(v)
}

struct PendingRIsu {
    aes_id: u32,
    ges_id: u8,
    qno: u8,
    refno: u8,
    total: u8,
    parts: Vec<Option<Vec<u8>>>,
    age: u32,
}

/// R-channel SU reassembler: up to 3 SUs per message, 11 user bytes per
/// SU except the last (SUTYPE = user bytes in that SU).
pub struct RIsuReassembler {
    pending: Vec<PendingRIsu>,
}

impl RIsuReassembler {
    pub fn new() -> Self {
        Self { pending: Vec::new() }
    }

    /// Feed one CRC-valid 19-byte R SU.
    pub fn push(&mut self, su: &[u8]) -> Option<AeroUserData> {
        debug_assert_eq!(su.len(), R_SU_LEN);
        // JAERO routes an R SU to the ISU/SSU reassembler only when the
        // user-data flag (`infofield[1] & 0x08`) is set; otherwise the SU
        // is a control type (classified by [`parse_r_su`]).
        if su[1] & 0x08 == 0 {
            return None;
        }
        let (k, n) = seq_indicator(su[0] >> 4)?;
        let sutype = su[0] & 0x0F;
        if sutype == 15 || sutype == 0 {
            return None; // signalling, not user data
        }
        let qno = su[1] >> 4;
        let refno = su[1] & 0x07;
        let aes_id = u32::from_be_bytes([0, su[2], su[3], su[4]]);
        let ges_id = su[5];
        let take = if k == n { sutype.min(11) as usize } else { 11 };
        let part = su[6..6 + take].to_vec();

        for p in &mut self.pending {
            p.age += 1;
        }
        self.pending.retain(|p| p.age < 10);

        let idx = self
            .pending
            .iter()
            .position(|p| {
                p.aes_id == aes_id && p.ges_id == ges_id && p.qno == qno && p.refno == refno
            })
            .unwrap_or_else(|| {
                self.pending.push(PendingRIsu {
                    aes_id,
                    ges_id,
                    qno,
                    refno,
                    total: n,
                    parts: vec![None; n as usize],
                    age: 0,
                });
                self.pending.len() - 1
            });
        let p = &mut self.pending[idx];
        p.parts[(k - 1) as usize] = Some(part);
        p.age = 0;
        if p.parts.iter().all(Option::is_some) {
            let done = self.pending.swap_remove(idx);
            let mut data = Vec::new();
            for part in done.parts.into_iter().flatten() {
                data.extend(part);
            }
            return Some(AeroUserData {
                aes_id: format!("{:06X}", done.aes_id),
                ges_id: done.ges_id,
                qno: done.qno,
                refno: done.refno,
                data,
            });
        }
        let _ = p.total;
        None
    }
}

impl Default for RIsuReassembler {
    fn default() -> Self {
        Self::new()
    }
}

/// Build R SUs for a user payload (testing): up to 3 SUs, 11 bytes each.
pub fn build_r_sus(aes_id: u32, ges_id: u8, qno: u8, refno: u8, data: &[u8]) -> Vec<Vec<u8>> {
    let n = data.len().div_ceil(11).clamp(1, 3) as u8;
    let aes = aes_id.to_be_bytes();
    (1..=n)
        .map(|k| {
            let off = (k as usize - 1) * 11;
            let chunk = &data[off..data.len().min(off + 11)];
            let sutype = if k == n { chunk.len() as u8 } else { 11 };
            let mut su = vec![
                (seq_indicator_for(k, n) << 4) | (sutype & 0x0F),
                // byte1: QNO (high nibble), user-data flag (bit 3, JAERO
                // `infofield[1] & 0x08`), REFNO (low 3 bits).
                (qno << 4) | 0x08 | (refno & 0x07),
                aes[1],
                aes[2],
                aes[3],
                ges_id,
            ];
            su.extend_from_slice(chunk);
            su.resize(17, 0);
            let crc = HDLC_FCS.checksum(&su);
            su.extend(crc.to_le_bytes());
            su
        })
        .collect()
}

/// Fill-in SU (type 0x01) used to pad frames.
/// Parse a C-channel assignment SU (P-channel types 0x31–0x34): the
/// ground station tells an aircraft which voice-circuit frequency pair
/// to use. Channel numbers step 2.5 kHz from 1510.0 (receive) and
/// 1611.5 MHz (transmit); bit 7 of each high byte flags a spot beam.
/// (JAERO `CreateCAssignmentItem`.)
pub fn parse_c_assignment(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < 10 || !(0x31..=0x34).contains(&su[0]) {
        return None;
    }
    let service = match su[0] {
        0x31 => "distress",
        0x32 => "flight-safety",
        0x33 => "other-safety",
        _ => "non-safety",
    };
    let (aes_id, ges_id) = aes_ges(su);
    let (rx_mhz, rx_spot, tx_mhz, tx_spot) = assignment_freqs(su);
    Some(serde_json::json!({
        "su_type": "c-channel-assignment",
        "service": service,
        "aes_id": aes_id,
        "ges_id": ges_id,
        "receive_mhz": rx_mhz,
        "transmit_mhz": tx_mhz,
        "receive_spotbeam": rx_spot,
        "transmit_spotbeam": tx_spot,
    }))
}

/// AES id (24-bit Inmarsat terminal address) from SU bytes 2–4 and GES id
/// (octet 5), as JAERO reads them in `SendLogOnOff` / `SendCAssignment`
/// (`AESID = su[2]<<16 | su[3]<<8 | su[4]`, `GESID = su[5]`; 1-based JAERO
/// octets, i.e. our `su[1..=4]`).
fn aes_ges(su: &[u8]) -> (String, u8) {
    (
        format!("{:06X}", u32::from_be_bytes([0, su[1], su[2], su[3]])),
        su[4],
    )
}

/// P-channel system log-on/log-off control SUs (0x10–0x17): the AES↔GES
/// session-management handshake. JAERO (`AEROTypeP`) names these eight
/// types; the AES id (octets 2–4) and GES id (octet 5) are the common
/// addressing fields (read exactly as `SendLogOnOff`). We surface a
/// structured session event keyed by the JAERO type name and the
/// originating direction.
///
/// Types (JAERO `aerol.h`): 0x10 log_on_request, 0x11 log_on_confirm,
/// 0x12 log_off_request, 0x13 log_on_reject, 0x14 log_on_interrogation,
/// 0x15 log_on/log_off_acknowledge, 0x16 log_on_prompt, 0x17
/// data_channel_reassignment.
pub fn parse_log_control(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || !(0x10..=0x17).contains(&su[0]) {
        return None;
    }
    let (event, direction) = match su[0] {
        // Aircraft (AES) initiates the log-on / log-off request.
        0x10 => ("log-on-request", "aes-to-ges"),
        0x12 => ("log-off-request", "aes-to-ges"),
        // Ground station (GES) responses / interrogations / prompts.
        0x11 => ("log-on-confirm", "ges-to-aes"),
        0x13 => ("log-on-reject", "ges-to-aes"),
        0x14 => ("log-on-interrogation", "ges-to-aes"),
        0x16 => ("log-on-prompt", "ges-to-aes"),
        0x17 => ("data-channel-reassignment", "ges-to-aes"),
        // Acknowledge can flow either way; leave direction unspecified.
        0x15 => ("log-on-log-off-acknowledge", "either"),
        _ => return None,
    };
    let (aes_id, ges_id) = aes_ges(su);
    Some(serde_json::json!({
        "su_type": "log-control",
        "su_type_hex": format!("0x{:02X}", su[0]),
        "event": event,
        "direction": direction,
        "aes_id": aes_id,
        "ges_id": ges_id,
    }))
}

/// Classify one CRC-valid 12-byte P-channel SU into a structured value,
/// when its type carries non-user-data control information we decode.
/// Returns `None` for user-data ISU/SSU (0x71/0xC0|seq) and fill (0x01),
/// which are handled by the reassembler, and for types we only frame but
/// don't yet interpret. The `kind` field in each result is the message
/// `MessageBody::Aero` kind tag. (Type table: JAERO `AEROTypeP`.)
pub fn parse_p_su(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN {
        return None;
    }
    match su[0] {
        0x31..=0x34 => parse_c_assignment(su),
        0x10..=0x17 => parse_log_control(su),
        0x21 => parse_call_announcement(su),
        0x51 => parse_t_channel_assignment(su),
        0x05 => parse_smc_channels(su),
        0x07 => parse_beam_support(su),
        0x0A => parse_broadcast_index(su),
        0x0C => parse_satellite_id(su),
        0x28 => parse_eirp_table(su),
        0x40 => parse_pr_control_isu(su),
        0x41 => parse_t_control_isu(su),
        0x61 => parse_rqa(su),
        0x62 => parse_rack_tack(su),
        0x74 | 0x76 => parse_lsdu(su),
        _ => None,
    }
}

/// JAERO's P/R-channel-control bit-rate code → bps map (`aerol.cpp`
/// `P_R_channel_control_ISU` handler, byte8 high nibble). Code 8 is
/// reserved (JAERO falls through to default −1).
fn control_isu_bitrate(code: u8) -> Option<u32> {
    match code {
        0 => Some(600),
        1 => Some(1200),
        2 => Some(2400),
        3 => Some(4800),
        4 => Some(6000),
        5 => Some(5250),
        6 => Some(10500),
        7 => Some(8400),
        9 => Some(21000),
        _ => None, // JAERO bitrate = -1
    }
}

/// Data EIRP-table broadcast, complete sequence (P-channel type 0x28).
/// JAERO (`AEROTypeP::Data_EIRP_table_broadcast_complete_sequence`) names
/// this type and decodes no further fields; we surface the named event.
pub fn parse_eirp_table(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x28 {
        return None;
    }
    Some(serde_json::json!({ "su_type": "eirp-table-broadcast" }))
}

/// P/R-channel control ISU (P-channel type 0x40): the GES advertises a
/// Pd (packet-data) carrier — its frequency, bit rate, and whether it is
/// a spot-beam carrier. JAERO (`aerol.cpp` `P_R_channel_control_ISU`):
/// - GES   = octet 5                                   [byte5 = su[4]]
/// - bitrate code = (byte8 >> 4) & 0x0F → bps table    [byte8 = su[7]]
/// - channel = ((byte9 & 0x7F) << 8) | byte10          [byte9/10 = su[8]/su[9]]
/// - freq = channel × 0.0025 + 1510.0 MHz; spot beam = byte9 bit 7.
/// (byteN = our su[N-1], JAERO's 1-based octet indexing.)
pub fn parse_pr_control_isu(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x40 {
        return None;
    }
    let ges_id = su[4];
    let bitrate_code = (su[7] >> 4) & 0x0F;
    let channel = (((su[8] & 0x7F) as u32) << 8) | su[9] as u32;
    let freq = channel as f64 * 0.0025 + 1510.0;
    let mut v = serde_json::json!({
        "su_type": "pr-channel-control-isu",
        "ges_id": ges_id,
        "pd_mhz": freq,
        "spotbeam": su[8] & 0x80 != 0,
    });
    // JAERO maps the bit-rate code through a table; reserved codes (8 and
    // ≥10) become −1 there — we omit the field rather than emit a bogus rate.
    if let Some(br) = control_isu_bitrate(bitrate_code) {
        v["bit_rate"] = serde_json::json!(br);
    }
    Some(v)
}

/// T-channel control ISU (P-channel type 0x41): JAERO names this type and
/// decodes no further fields; we surface the named event.
pub fn parse_t_control_isu(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x41 {
        return None;
    }
    Some(serde_json::json!({ "su_type": "t-channel-control-isu" }))
}

/// Request for acknowledgement, RQA (P-channel type 0x61): JAERO names
/// this type (`Request_for_acknowledgement_RQA_P_channel`) and decodes no
/// further fields; we surface the named event.
pub fn parse_rqa(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x61 {
        return None;
    }
    Some(serde_json::json!({ "su_type": "request-for-acknowledgement" }))
}

/// Acknowledge, RACK/TACK (P-channel type 0x62): JAERO names this type
/// (`Acknowledge_RACK_TACK_P_channel`) and decodes no further fields; we
/// surface the named event.
pub fn parse_rack_tack(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x62 {
        return None;
    }
    Some(serde_json::json!({ "su_type": "acknowledge" }))
}

/// Short LSDU user-data ISU (P-channel types 0x74/0x76): JAERO names the
/// 3-octet (0x74) and 4-octet (0x76) LSDU RLS P-channel user-data types
/// and decodes no further fields (they are not run through the ISU/SSU
/// reassembler), so we surface the named event with the LSDU length.
pub fn parse_lsdu(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN {
        return None;
    }
    let octets = match su[0] {
        0x74 => 3,
        0x76 => 4,
        _ => return None,
    };
    Some(serde_json::json!({ "su_type": "short-lsdu", "lsdu_octets": octets }))
}

/// `MessageBody::Aero` kind tag for a structured P-SU value.
pub fn p_su_kind(v: &serde_json::Value) -> String {
    v["su_type"].as_str().unwrap_or("aero-su").to_owned()
}

/// Receive/transmit channel frequencies (MHz) for an assignment-style SU,
/// from octets 7/8 (rx) and 9/10 (tx) — our `su[6..=9]`. The high bit of
/// each high octet flags a spot beam; the low 15 bits index 2.5 kHz steps
/// from 1510.0 MHz (receive, AES→GES Pd/voice) and 1611.5 MHz (transmit,
/// GES→AES). (JAERO `SendCAssignment` / `CreateCAssignmentItem`.)
fn assignment_freqs(su: &[u8]) -> (f64, bool, f64, bool) {
    let rx_chan = (((su[6] & 0x7F) as u32) << 8) | su[7] as u32;
    let tx_chan = (((su[8] & 0x7F) as u32) << 8) | su[9] as u32;
    (
        rx_chan as f64 * 0.0025 + 1510.0,
        su[6] & 0x80 != 0,
        tx_chan as f64 * 0.0025 + 1611.5,
        su[8] & 0x80 != 0,
    )
}

/// Call_announcement SU (P-channel type 0x21): the GES announces an
/// incoming call to an aircraft, naming the receive/transmit channel pair
/// it should use. JAERO routes this through `SendCAssignment`, i.e. it
/// reuses the C-channel-assignment octet layout (AES octets 2–4, GES
/// octet 5, rx octets 7/8, tx octets 9/10, spot-beam flags in the high
/// octets).
pub fn parse_call_announcement(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x21 {
        return None;
    }
    let (aes_id, ges_id) = aes_ges(su);
    let (rx_mhz, rx_spot, tx_mhz, tx_spot) = assignment_freqs(su);
    Some(serde_json::json!({
        "su_type": "call-announcement",
        "aes_id": aes_id,
        "ges_id": ges_id,
        "receive_mhz": rx_mhz,
        "transmit_mhz": tx_mhz,
        "receive_spotbeam": rx_spot,
        "transmit_spotbeam": tx_spot,
    }))
}

/// T_channel_assignment SU (P-channel type 0x51): the GES assigns a
/// reservation (TDMA) T channel to an aircraft for burst data return.
/// JAERO names this type but decodes no further fields beyond the common
/// AES/GES addressing, so we surface just the named assignment event with
/// AES (octets 2–4) and GES (octet 5).
pub fn parse_t_channel_assignment(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x51 {
        return None;
    }
    let (aes_id, ges_id) = aes_ges(su);
    Some(serde_json::json!({
        "su_type": "t-channel-assignment",
        "aes_id": aes_id,
        "ges_id": ges_id,
    }))
}

/// AES system-table broadcast: satellite_identification (P-channel type
/// 0x0C). The GES broadcasts which satellite serves this beam, its orbital
/// longitude, and the Psmc (P-channel) carrier frequencies. JAERO
/// (`aerol.cpp`):
/// - seqno  = (byte3 >> 2) & 0x3F                  [byte3 = su[2]]
/// - satid  = ((byte3 << 4) & 0x30) | ((byte4 >> 4) & 0x0F)   [byte4 = su[3]]
/// - longitude = byte6 × 1.5 degrees (>180 ⇒ west)  [byte6 = su[5]]
/// - Psmc1 = (((byte7&0x7F)<<8 | byte8) × 0.0025) + 1510.0 MHz, spot-beam
///   in byte7 bit 7                                 [byte7/8 = su[6]/su[7]]
/// - Psmc2 likewise from byte9/byte10 (omitted when its channel is 0).
pub fn parse_satellite_id(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x0C {
        return None;
    }
    let byte3 = su[2] as u16;
    let byte4 = su[3] as u16;
    let seqno = (byte3 >> 2) & 0x3F;
    let satid = ((byte3 << 4) & 0x30) | ((byte4 >> 4) & 0x0F);
    let longitude = su[5] as f64 * 1.5;
    let (lon_value, lon_dir) = if longitude > 180.0 {
        (360.0 - longitude, "W")
    } else {
        (longitude, "E")
    };
    let channel1 = (((su[6] & 0x7F) as u32) << 8) | su[7] as u32;
    let channel2 = (((su[8] & 0x7F) as u32) << 8) | su[9] as u32;
    let psmc1 = channel1 as f64 * 0.0025 + 1510.0;
    let mut v = serde_json::json!({
        "su_type": "satellite-id",
        "seq": seqno,
        "satellite_id": satid,
        "longitude_deg": lon_value,
        "longitude_dir": lon_dir,
        "psmc1_mhz": psmc1,
        "psmc1_spotbeam": su[6] & 0x80 != 0,
    });
    // JAERO only reports Psmc2 when its channel is non-zero.
    if channel2 != 0 {
        let psmc2 = channel2 as f64 * 0.0025 + 1510.0;
        v["psmc2_mhz"] = serde_json::json!(psmc2);
        v["psmc2_spotbeam"] = serde_json::json!(su[8] & 0x80 != 0);
    }
    Some(v)
}

/// AES system-table broadcast: GES Psmc and Rsmc channels (P-channel type
/// 0x05). A GES broadcasts its P-channel (Psmc, AES receive) and R-channel
/// (Rsmc, AES transmit) carrier frequencies across a sequence of SUs
/// indexed by `lsu`. JAERO (`aerol.cpp`):
/// - seqno = (byte3 >> 2) & 0x3F, lsu = byte3 & 0x03   [byte3 = su[2]]
/// - ges   = byte4                                      [byte4 = su[3]]
/// - three 16-bit channels at byte5/6, byte7/8, byte9/10 → ×0.0025 +1510.0
/// - the Rsmc (transmit) carriers sit +101.5 MHz from the channel base:
///   for lsu ≤ 1 the first carrier is the Psmc (RX) and carriers 2,3 are
///   Rsmc0,Rsmc1 (TX, +101.5); for lsu ≥ 2 all three are Rsmc carriers
///   (TX, +101.5) — Rsmc2..4 (lsu 2) / Rsmc5..7 (lsu 3).
pub fn parse_smc_channels(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x05 {
        return None;
    }
    let byte3 = su[2];
    let seqno = (byte3 >> 2) & 0x3F;
    let lsu = byte3 & 0x03;
    let ges_id = su[3];
    let ch = |hi: u8, lo: u8| -> u32 { ((hi as u32) << 8) | lo as u32 };
    let mut f1 = ch(su[4], su[5]) as f64 * 0.0025 + 1510.0;
    let mut f2 = ch(su[6], su[7]) as f64 * 0.0025 + 1510.0;
    let mut f3 = ch(su[8], su[9]) as f64 * 0.0025 + 1510.0;
    let names: [&str; 3] = if lsu <= 1 {
        // Psmc (RX) + two Rsmc (TX, +101.5).
        f2 += 101.5;
        f3 += 101.5;
        ["psmc_rx", "rsmc0_tx", "rsmc1_tx"]
    } else {
        // All three are Rsmc (TX, +101.5).
        f1 += 101.5;
        f2 += 101.5;
        f3 += 101.5;
        if lsu == 2 {
            ["rsmc2_tx", "rsmc3_tx", "rsmc4_tx"]
        } else {
            ["rsmc5_tx", "rsmc6_tx", "rsmc7_tx"]
        }
    };
    Some(serde_json::json!({
        "su_type": "smc-channels",
        "seq": seqno,
        "lsu": lsu,
        "ges_id": ges_id,
        "channels": [
            { "name": names[0], "mhz": f1 },
            { "name": names[1], "mhz": f2 },
            { "name": names[2], "mhz": f3 },
        ],
    }))
}

/// AES system-table broadcast: GES beam support (P-channel type 0x07).
/// JAERO names this type (`AEROTypeP`) but decodes no further fields; we
/// surface the named broadcast (the raw SU bytes carry the beam list).
pub fn parse_beam_support(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x07 {
        return None;
    }
    Some(serde_json::json!({ "su_type": "ges-beam-support" }))
}

/// AES system-table broadcast: index (P-channel type 0x0A). JAERO names
/// this type but decodes no further fields; we surface the named broadcast.
pub fn parse_broadcast_index(su: &[u8]) -> Option<serde_json::Value> {
    if su.len() < SU_LEN || su[0] != 0x0A {
        return None;
    }
    Some(serde_json::json!({ "su_type": "broadcast-index" }))
}

pub fn fill_su() -> Vec<u8> {
    su_with_crc(vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VERIFY-8: the AEROTypeP / AEROTypeR / AEROTypeC enumerator hex
    /// values this crate dispatches on are pinned against the JAERO source
    /// (`JAERO/aerol.h`, namespaces `AEROTypeP` / `AEROTypeR` / `AEROTypeC`,
    /// fetched from github.com/jontio/JAERO master). Every value below is
    /// transcribed verbatim from that header; this test fails if a handler's
    /// type byte ever drifts from the JAERO enumerator it claims to decode.
    ///
    /// No mismatches were found during VERIFY-8 — every type byte already
    /// matched JAERO; this test locks that in.
    #[test]
    fn aero_type_enumerators_match_jaero_aerol_h() {
        // ---- AEROTypeP (aerol.h `namespace AEROTypeP`) ----------------
        // Build a CRC-valid 12-byte SU with the given type byte.
        let p = |su0: u8| -> serde_json::Value {
            let mut s = vec![0u8; 10];
            s[0] = su0;
            parse_p_su(&su_with_crc(s)).unwrap_or(serde_json::Value::Null)
        };
        // Fill (0x01) and the reserved types (0x00/0x18/0x19/0x26) are
        // named-but-not-classified in JAERO; parse_p_su returns None → Null.
        for reserved in [0x00u8, 0x01, 0x18, 0x19, 0x26] {
            assert!(p(reserved).is_null(), "P 0x{reserved:02X} not classified");
        }
        // AES system-table broadcasts.
        assert_eq!(p(0x05)["su_type"], "smc-channels");
        assert_eq!(p(0x07)["su_type"], "ges-beam-support");
        assert_eq!(p(0x0A)["su_type"], "broadcast-index");
        assert_eq!(p(0x0C)["su_type"], "satellite-id");
        // System log-on/log-off (0x10..=0x17).
        let log_events = [
            (0x10u8, "log-on-request"),
            (0x11, "log-on-confirm"),
            (0x12, "log-off-request"),
            (0x13, "log-on-reject"),
            (0x14, "log-on-interrogation"),
            (0x15, "log-on-log-off-acknowledge"),
            (0x16, "log-on-prompt"),
            (0x17, "data-channel-reassignment"),
        ];
        for (ty, event) in log_events {
            assert_eq!(p(ty)["su_type"], "log-control", "P 0x{ty:02X}");
            assert_eq!(p(ty)["event"], event, "P 0x{ty:02X}");
        }
        // Call initiation, EIRP, C-channel related, channel info, ack, LSDU.
        assert_eq!(p(0x21)["su_type"], "call-announcement");
        assert_eq!(p(0x28)["su_type"], "eirp-table-broadcast");
        // 0x30 Call_progress is named on the P channel but JAERO decodes no
        // P-channel fields for it (it is a C/R-channel event) — not in our
        // P dispatch, so None.
        assert!(p(0x30).is_null(), "P 0x30 Call_progress not classified");
        let c_assign = [
            (0x31u8, "distress"),
            (0x32, "flight-safety"),
            (0x33, "other-safety"),
            (0x34, "non-safety"),
        ];
        for (ty, service) in c_assign {
            assert_eq!(p(ty)["su_type"], "c-channel-assignment", "P 0x{ty:02X}");
            assert_eq!(p(ty)["service"], service, "P 0x{ty:02X}");
        }
        assert_eq!(p(0x40)["su_type"], "pr-channel-control-isu");
        assert_eq!(p(0x41)["su_type"], "t-channel-control-isu");
        assert_eq!(p(0x51)["su_type"], "t-channel-assignment");
        assert_eq!(p(0x61)["su_type"], "request-for-acknowledgement");
        assert_eq!(p(0x62)["su_type"], "acknowledge");
        // User data: ISU 0x71 routes to the reassembler (None from classifier);
        // the 3-/4-octet LSDU types are named events.
        assert!(p(0x71).is_null(), "P 0x71 ISU → reassembler");
        assert_eq!(p(0x74)["lsdu_octets"], 3);
        assert_eq!(p(0x76)["lsdu_octets"], 4);

        // ---- AEROTypeR (aerol.h `namespace AEROTypeR`) ----------------
        // (enumerator, hex value, our su_type, request_kind) verbatim.
        let r = |su2: u8| -> serde_json::Value {
            let mut s = vec![0u8; R_SU_LEN];
            s[1] = 0x00; // user-data flag clear → control SU
            s[2] = su2; // JAERO infofield[2] holds the R control type
            let crc = HDLC_FCS.checksum(&s[..17]);
            s[17] = (crc & 0xFF) as u8;
            s[18] = (crc >> 8) as u8;
            parse_r_su(&s).unwrap_or(serde_json::Value::Null)
        };
        let r_types = [
            (0x20u8, "r-access-request", Some("general-telephone")),
            (0x23, "r-access-request", Some("abbreviated-telephone")),
            (0x22, "r-access-request", Some("data")),
            (0x61, "r-request-for-acknowledgement", None),
            (0x62, "r-acknowledgement", None),
            (0x12, "r-log-on-off-control", None),
            (0x30, "r-call-progress", None),
            (0x15, "r-log-on-off-acknowledgement", None),
            (0x17, "r-log-control-ready-for-reassignment", None),
            (0x60, "r-telephony-acknowledge", None),
        ];
        for (ty, su_type, kind) in r_types {
            let v = r(ty);
            assert_eq!(v["su_type"], su_type, "R 0x{ty:02X}");
            assert_eq!(v["su_type_hex"], format!("0x{ty:02X}"), "R 0x{ty:02X}");
            match kind {
                Some(k) => assert_eq!(v["request_kind"], k, "R 0x{ty:02X}"),
                None => assert!(v.get("request_kind").is_none(), "R 0x{ty:02X}"),
            }
        }

        // ---- AEROTypeC (aerol.h `namespace AEROTypeC`) ----------------
        // Fill 0x01, Call_progress 0x30, Telephony_acknowledge 0x60.
        assert_eq!(crate::cchannel::su_type_name(0x01), "fill");
        assert_eq!(crate::cchannel::su_type_name(0x30), "call-progress");
        assert_eq!(crate::cchannel::su_type_name(0x60), "telephony-acknowledge");
        // Any type not in AEROTypeC names as "other".
        assert_eq!(crate::cchannel::su_type_name(0x71), "other");
    }

    #[test]
    fn c_assignment_parses_frequencies() {
        // type 0x32 flight-safety, AES ABCDEF, GES 0x44,
        // rx channel 4000 (spot beam), tx channel 2000.
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x32;
        su10[1..4].copy_from_slice(&[0xAB, 0xCD, 0xEF]);
        su10[4] = 0x44;
        su10[6] = 0x80 | ((4000u16 >> 8) as u8);
        su10[7] = (4000u16 & 0xFF) as u8;
        su10[8] = (2000u16 >> 8) as u8;
        su10[9] = (2000u16 & 0xFF) as u8;
        let su = su_with_crc(su10);
        let a = parse_c_assignment(&su).unwrap();
        assert_eq!(a["service"], "flight-safety");
        assert_eq!(a["aes_id"], "ABCDEF");
        assert_eq!(a["ges_id"], 0x44);
        assert_eq!(a["receive_mhz"], 1520.0);
        assert_eq!(a["transmit_mhz"], 1616.5);
        assert_eq!(a["receive_spotbeam"], true);
        assert_eq!(a["transmit_spotbeam"], false);
        // non-assignment types pass through
        assert!(parse_c_assignment(&[0x71; 12]).is_none());
    }

    /// AERO-1.1: log-on/log-off control SUs (0x10–0x17).
    /// Oracle = JAERO `aerol.h` AEROTypeP names + `aerol.cpp` SendLogOnOff
    /// field layout (AESID = octets 2–4 → our su[1..4]; GESID = octet 5 →
    /// su[4]).
    #[test]
    fn log_control_classifies_all_eight_types() {
        // JAERO AEROTypeP enumerators for the log-on/log-off block.
        let expect = [
            (0x10u8, "log-on-request", "aes-to-ges"),
            (0x11, "log-on-confirm", "ges-to-aes"),
            (0x12, "log-off-request", "aes-to-ges"),
            (0x13, "log-on-reject", "ges-to-aes"),
            (0x14, "log-on-interrogation", "ges-to-aes"),
            (0x15, "log-on-log-off-acknowledge", "either"),
            (0x16, "log-on-prompt", "ges-to-aes"),
            (0x17, "data-channel-reassignment", "ges-to-aes"),
        ];
        for (ty, event, direction) in expect {
            // AES 0xABCDEF, GES 0x2A in JAERO's SendLogOnOff octet layout.
            let mut su10 = vec![0u8; 10];
            su10[0] = ty;
            su10[1..4].copy_from_slice(&[0xAB, 0xCD, 0xEF]);
            su10[4] = 0x2A;
            let su = su_with_crc(su10);
            let v = parse_log_control(&su).expect("log-control SU parses");
            assert_eq!(v["su_type"], "log-control");
            assert_eq!(v["su_type_hex"], format!("0x{ty:02X}"));
            assert_eq!(v["event"], event, "type 0x{ty:02X}");
            assert_eq!(v["direction"], direction, "type 0x{ty:02X}");
            assert_eq!(v["aes_id"], "ABCDEF");
            assert_eq!(v["ges_id"], 0x2A);
            // parse_p_su dispatches to the same handler.
            assert_eq!(parse_p_su(&su).unwrap()["event"], event);
        }
        // Out-of-range / non-log types are not classified as log-control.
        assert!(parse_log_control(&[0x01; 12]).is_none()); // fill
        assert!(parse_log_control(&[0x18; 12]).is_none()); // reserved_18
        assert!(parse_log_control(&[0x0F; 12]).is_none()); // below range
        assert!(parse_log_control(&[0x71; 12]).is_none()); // user-data ISU
    }

    /// AERO-1.2: Call_announcement 0x21 reuses JAERO's SendCAssignment
    /// octet layout — rx from octets 7/8 (+1510.0), tx from 9/10
    /// (+1611.5), spot-beam in the high octets. Same arithmetic as the
    /// verified C-assignment path.
    #[test]
    fn call_announcement_parses_channel_pair() {
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x21;
        su10[1..4].copy_from_slice(&[0xAB, 0xCD, 0xEF]);
        su10[4] = 0x44;
        // rx channel 4000 (spot beam), tx channel 2000 (global).
        su10[6] = 0x80 | ((4000u16 >> 8) as u8);
        su10[7] = (4000u16 & 0xFF) as u8;
        su10[8] = (2000u16 >> 8) as u8;
        su10[9] = (2000u16 & 0xFF) as u8;
        let su = su_with_crc(su10);
        let v = parse_call_announcement(&su).unwrap();
        assert_eq!(v["su_type"], "call-announcement");
        assert_eq!(v["aes_id"], "ABCDEF");
        assert_eq!(v["ges_id"], 0x44);
        assert_eq!(v["receive_mhz"], 4000.0 * 0.0025 + 1510.0); // 1520.0
        assert_eq!(v["transmit_mhz"], 2000.0 * 0.0025 + 1611.5); // 1616.5
        assert_eq!(v["receive_spotbeam"], true);
        assert_eq!(v["transmit_spotbeam"], false);
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "call-announcement");
        // Other types are not call announcements.
        assert!(parse_call_announcement(&[0x32; 12]).is_none());
    }

    /// AERO-1.2: T_channel_assignment 0x51. JAERO names this type and
    /// decodes no further fields than the common AES/GES addressing, so we
    /// surface exactly that.
    #[test]
    fn t_channel_assignment_named_with_addressing() {
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x51;
        su10[1..4].copy_from_slice(&[0x12, 0x34, 0x56]);
        su10[4] = 0x07;
        let su = su_with_crc(su10);
        let v = parse_t_channel_assignment(&su).unwrap();
        assert_eq!(v["su_type"], "t-channel-assignment");
        assert_eq!(v["aes_id"], "123456");
        assert_eq!(v["ges_id"], 0x07);
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "t-channel-assignment");
        assert!(parse_t_channel_assignment(&[0x21; 12]).is_none());
    }

    /// AERO-1.3: satellite_identification 0x0C. Field layout from JAERO
    /// `aerol.cpp` (seqno, satid split across byte3/byte4, longitude =
    /// byte6×1.5°, Psmc1/2 from byte7/8 and byte9/10). Two cases pin the
    /// satid bit-split and the east/west longitude rule.
    #[test]
    fn satellite_id_decodes_jaero_layout() {
        // satid = 20 (needs the high 2 bits), seqno = 10, longitude byte
        // 200 → 300.0° → 60.0°W, Psmc1 channel 0x0123 (global),
        // Psmc2 channel 0x0456 (spot beam).
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x0C;
        // byte3 (su[2]): seqno<<2 | satid_hi(=(20>>4)&3=1) = 40|1 = 0x29.
        su10[2] = 0x29;
        // byte4 (su[3]): satid_lo(=20&0xF=4) << 4 = 0x40.
        su10[3] = 0x40;
        // byte6 (su[5]): longitude index 200 → 300.0° → 60.0°W.
        su10[5] = 200;
        // byte7/8 (su[6]/su[7]): Psmc1 channel 0x0123 (no spot beam).
        su10[6] = 0x01;
        su10[7] = 0x23;
        // byte9/10 (su[8]/su[9]): Psmc2 channel 0x0456 + spot beam bit.
        su10[8] = 0x80 | 0x04;
        su10[9] = 0x56;
        let su = su_with_crc(su10);
        let v = parse_satellite_id(&su).unwrap();
        assert_eq!(v["su_type"], "satellite-id");
        assert_eq!(v["seq"], 10);
        assert_eq!(v["satellite_id"], 20);
        assert_eq!(v["longitude_deg"], 60.0); // 360 - 300
        assert_eq!(v["longitude_dir"], "W");
        assert_eq!(v["psmc1_mhz"], 0x0123 as f64 * 0.0025 + 1510.0);
        assert_eq!(v["psmc1_spotbeam"], false);
        assert_eq!(v["psmc2_mhz"], 0x0456 as f64 * 0.0025 + 1510.0);
        assert_eq!(v["psmc2_spotbeam"], true);
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "satellite-id");

        // East longitude, satid in low nibble only, no Psmc2 (channel 0).
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x0C;
        su10[2] = 40; // seqno 10, satid_hi 0
        su10[3] = 0x50; // satid_lo 5
        su10[5] = 100; // 150.0°E
        su10[6] = 0x02;
        su10[7] = 0x00; // Psmc1 channel 0x0200
        // su[8]/su[9] left 0 → no Psmc2 reported.
        let su = su_with_crc(su10);
        let v = parse_satellite_id(&su).unwrap();
        assert_eq!(v["satellite_id"], 5);
        assert_eq!(v["longitude_deg"], 150.0);
        assert_eq!(v["longitude_dir"], "E");
        assert!(v.get("psmc2_mhz").is_none());
        assert!(parse_satellite_id(&[0x05; 12]).is_none());
    }

    /// AERO-1.3: GES Psmc/Rsmc channels 0x05. Field layout from JAERO
    /// `aerol.cpp`: seqno/lsu from byte3, GES from byte4, three channels,
    /// the Rsmc (TX) carriers offset +101.5 MHz. Covers lsu≤1 (Psmc+Rsmc)
    /// and lsu≥2 (all-Rsmc) cases.
    #[test]
    fn smc_channels_decodes_jaero_layout() {
        let base = |ch: u32| ch as f64 * 0.0025 + 1510.0;
        // lsu = 0: first carrier is Psmc (RX), next two Rsmc (TX, +101.5).
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x05;
        su10[2] = (7 << 2) | 0; // seqno 7, lsu 0
        su10[3] = 0x2A; // GES 0x2A
        su10[4] = 0x01;
        su10[5] = 0x00; // ch1 0x0100
        su10[6] = 0x02;
        su10[7] = 0x00; // ch2 0x0200
        su10[8] = 0x03;
        su10[9] = 0x00; // ch3 0x0300
        let su = su_with_crc(su10);
        let v = parse_smc_channels(&su).unwrap();
        assert_eq!(v["su_type"], "smc-channels");
        assert_eq!(v["seq"], 7);
        assert_eq!(v["lsu"], 0);
        assert_eq!(v["ges_id"], 0x2A);
        let ch = v["channels"].as_array().unwrap();
        assert_eq!(ch[0]["name"], "psmc_rx");
        assert_eq!(ch[0]["mhz"], base(0x0100));
        assert_eq!(ch[1]["name"], "rsmc0_tx");
        assert_eq!(ch[1]["mhz"], base(0x0200) + 101.5);
        assert_eq!(ch[2]["name"], "rsmc1_tx");
        assert_eq!(ch[2]["mhz"], base(0x0300) + 101.5);
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "smc-channels");

        // lsu = 2: all three are Rsmc (TX, +101.5), named Rsmc2..4.
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x05;
        su10[2] = (3 << 2) | 2; // seqno 3, lsu 2
        su10[3] = 0x11;
        su10[4] = 0x04;
        su10[5] = 0x00; // ch1 0x0400
        let su = su_with_crc(su10);
        let v = parse_smc_channels(&su).unwrap();
        assert_eq!(v["lsu"], 2);
        let ch = v["channels"].as_array().unwrap();
        assert_eq!(ch[0]["name"], "rsmc2_tx");
        assert_eq!(ch[0]["mhz"], base(0x0400) + 101.5);
        assert_eq!(ch[1]["name"], "rsmc3_tx");
        assert_eq!(ch[2]["name"], "rsmc4_tx");
        assert!(parse_smc_channels(&[0x0C; 12]).is_none());
    }

    /// AERO-1.3: 0x07 GES beam support and 0x0A broadcast index are named
    /// by JAERO without further field decode; we surface the named events.
    #[test]
    fn beam_support_and_broadcast_index_named() {
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x07;
        let su = su_with_crc(su10);
        assert_eq!(parse_beam_support(&su).unwrap()["su_type"], "ges-beam-support");
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "ges-beam-support");

        let mut su10 = vec![0u8; 10];
        su10[0] = 0x0A;
        let su = su_with_crc(su10);
        assert_eq!(parse_broadcast_index(&su).unwrap()["su_type"], "broadcast-index");
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "broadcast-index");

        assert!(parse_beam_support(&[0x0A; 12]).is_none());
        assert!(parse_broadcast_index(&[0x07; 12]).is_none());
    }

    /// AERO-1.4: P/R-channel control ISU 0x40. Field layout from JAERO
    /// `aerol.cpp` `P_R_channel_control_ISU`: GES = octet 5 (su[4]),
    /// bit-rate code = (byte8>>4)&0x0F mapped through JAERO's table,
    /// channel = ((byte9&0x7F)<<8)|byte10 → ×0.0025+1510.0 MHz, spot-beam =
    /// byte9 bit 7. byteN = su[N-1].
    #[test]
    fn pr_control_isu_decodes_jaero_layout() {
        // GES 0x2A, bit-rate code 1 → 1200 bps, channel 0x0123 (no spot
        // beam). byte8 = su[7] high nibble = code; byte9/10 = su[8]/su[9].
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x40;
        su10[4] = 0x2A; // GES (octet 5)
        su10[7] = 0x10; // byte8: bit-rate code 1 in high nibble
        su10[8] = 0x01; // byte9 high (no spot-beam bit)
        su10[9] = 0x23; // byte10
        let su = su_with_crc(su10);
        let v = parse_pr_control_isu(&su).unwrap();
        assert_eq!(v["su_type"], "pr-channel-control-isu");
        assert_eq!(v["ges_id"], 0x2A);
        assert_eq!(v["bit_rate"], 1200);
        assert_eq!(v["pd_mhz"], 0x0123 as f64 * 0.0025 + 1510.0);
        assert_eq!(v["spotbeam"], false);
        assert_eq!(parse_p_su(&su).unwrap()["su_type"], "pr-channel-control-isu");

        // Spot-beam carrier, bit-rate code 6 → 10500 bps.
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x40;
        su10[4] = 0x11;
        su10[7] = 0x60; // bit-rate code 6
        su10[8] = 0x80 | 0x02; // spot beam + channel high
        su10[9] = 0x00; // channel 0x0200
        let su = su_with_crc(su10);
        let v = parse_pr_control_isu(&su).unwrap();
        assert_eq!(v["bit_rate"], 10500);
        assert_eq!(v["spotbeam"], true);
        assert_eq!(v["pd_mhz"], 0x0200 as f64 * 0.0025 + 1510.0);

        // Reserved bit-rate code 8 → JAERO −1; we omit the field.
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x40;
        su10[7] = 0x80; // code 8 (reserved)
        let su = su_with_crc(su10);
        let v = parse_pr_control_isu(&su).unwrap();
        assert!(v.get("bit_rate").is_none());

        // Whole JAERO bit-rate code table.
        assert_eq!(control_isu_bitrate(0), Some(600));
        assert_eq!(control_isu_bitrate(1), Some(1200));
        assert_eq!(control_isu_bitrate(2), Some(2400));
        assert_eq!(control_isu_bitrate(3), Some(4800));
        assert_eq!(control_isu_bitrate(4), Some(6000));
        assert_eq!(control_isu_bitrate(5), Some(5250));
        assert_eq!(control_isu_bitrate(6), Some(10500));
        assert_eq!(control_isu_bitrate(7), Some(8400));
        assert_eq!(control_isu_bitrate(8), None);
        assert_eq!(control_isu_bitrate(9), Some(21000));
        assert_eq!(control_isu_bitrate(10), None);

        assert!(parse_pr_control_isu(&[0x41; 12]).is_none());
    }

    /// AERO-1.4: the named-only P-channel control/user-data types JAERO
    /// enumerates in `AEROTypeP` but decodes no further fields — EIRP-table
    /// 0x28, T-channel-control-ISU 0x41, RQA 0x61, RACK/TACK 0x62, and the
    /// short 3-/4-octet LSDU user-data types 0x74/0x76. We surface each as
    /// its named event (LSDU also carries its octet length).
    #[test]
    fn named_only_control_types() {
        let named = [
            (0x28u8, "eirp-table-broadcast"),
            (0x41, "t-channel-control-isu"),
            (0x61, "request-for-acknowledgement"),
            (0x62, "acknowledge"),
        ];
        for (ty, name) in named {
            let mut su10 = vec![0u8; 10];
            su10[0] = ty;
            let su = su_with_crc(su10);
            assert_eq!(parse_p_su(&su).unwrap()["su_type"], name, "type 0x{ty:02X}");
        }
        // Each handler rejects other types.
        assert!(parse_eirp_table(&[0x41; 12]).is_none());
        assert!(parse_t_control_isu(&[0x28; 12]).is_none());
        assert!(parse_rqa(&[0x62; 12]).is_none());
        assert!(parse_rack_tack(&[0x61; 12]).is_none());

        // Short LSDU 0x74 (3 octets) / 0x76 (4 octets).
        let mut su10 = vec![0u8; 10];
        su10[0] = 0x74;
        let su = su_with_crc(su10);
        let v = parse_p_su(&su).unwrap();
        assert_eq!(v["su_type"], "short-lsdu");
        assert_eq!(v["lsdu_octets"], 3);

        let mut su10 = vec![0u8; 10];
        su10[0] = 0x76;
        let su = su_with_crc(su10);
        let v = parse_p_su(&su).unwrap();
        assert_eq!(v["su_type"], "short-lsdu");
        assert_eq!(v["lsdu_octets"], 4);

        assert!(parse_lsdu(&[0x71; 12]).is_none());
    }

    /// AERO-3: R-channel named control set. JAERO (`aerol.cpp` R-channel
    /// branch) reads the message type from the third byte (`infofield[2]` =
    /// su[2]) and routes to user-data only when `infofield[1] & 0x08` is
    /// set. The named types are JAERO's `AEROTypeR` enum (`aerol.h`).
    #[test]
    fn r_control_set_classifies_aerotype_r() {
        let expect = [
            (0x20u8, "r-access-request", Some("general-telephone")),
            (0x23, "r-access-request", Some("abbreviated-telephone")),
            (0x22, "r-access-request", Some("data")),
            (0x61, "r-request-for-acknowledgement", None),
            (0x62, "r-acknowledgement", None),
            (0x12, "r-log-on-off-control", None),
            (0x30, "r-call-progress", None),
            (0x15, "r-log-on-off-acknowledgement", None),
            (0x17, "r-log-control-ready-for-reassignment", None),
            (0x60, "r-telephony-acknowledge", None),
        ];
        for (ty, su_type, kind) in expect {
            // Control SU: user-data flag (su[1] bit 3) clear, type at su[2].
            let mut su = vec![0u8; R_SU_LEN];
            su[1] = 0x00; // user-data flag clear
            su[2] = ty;
            let crc = HDLC_FCS.checksum(&su[..17]);
            su[17] = (crc & 0xFF) as u8;
            su[18] = (crc >> 8) as u8;
            assert!(r_su_crc_ok(&su));
            let v = parse_r_su(&su).unwrap_or_else(|| panic!("R type 0x{ty:02X} classifies"));
            assert_eq!(v["su_type"], su_type, "type 0x{ty:02X}");
            assert_eq!(v["su_type_hex"], format!("0x{ty:02X}"));
            match kind {
                Some(k) => assert_eq!(v["request_kind"], k, "type 0x{ty:02X}"),
                None => assert!(v.get("request_kind").is_none(), "type 0x{ty:02X}"),
            }
        }

        // User-data flag set → not a control SU (handled by reassembler).
        let mut su = vec![0u8; R_SU_LEN];
        su[1] = 0x08; // user-data flag
        su[2] = 0x20; // would otherwise be access-request
        let crc = HDLC_FCS.checksum(&su[..17]);
        su[17] = (crc & 0xFF) as u8;
        su[18] = (crc >> 8) as u8;
        assert!(parse_r_su(&su).is_none());

        // Unrecognized control byte → None.
        let mut su = vec![0u8; R_SU_LEN];
        su[2] = 0xAA;
        let crc = HDLC_FCS.checksum(&su[..17]);
        su[17] = (crc & 0xFF) as u8;
        su[18] = (crc >> 8) as u8;
        assert!(parse_r_su(&su).is_none());

        // Wrong length → None.
        assert!(parse_r_su(&[0u8; 12]).is_none());
    }

    /// AERO-3 verify: SEQINDICATOR nibble → (SUindex, SUtotal) must match
    /// JAERO's `RISUData::update` switch exactly (`aerol.cpp` lines 59-87:
    /// 1→(1,1), 2→(1,2), 3→(2,2), 4→(1,3), 5→(2,3), 6→(3,3); k,n 1-based).
    #[test]
    fn seq_indicator_matches_jaero_switch() {
        // (SEQINDICATOR, SUindex 0-based in JAERO, SUtotal in JAERO).
        let jaero = [(1u8, 0u8, 1u8), (2, 0, 2), (3, 1, 2), (4, 0, 3), (5, 1, 3), (6, 2, 3)];
        for (ind, su_index, su_total) in jaero {
            let (k, n) = seq_indicator(ind).unwrap_or_else(|| panic!("SEQINDICATOR {ind}"));
            // Our k is 1-based; JAERO's SUindex is 0-based → k = SUindex+1.
            assert_eq!(k, su_index + 1, "SEQINDICATOR {ind} index");
            assert_eq!(n, su_total, "SEQINDICATOR {ind} total");
            // Round-trip the encoder against the same table.
            assert_eq!(seq_indicator_for(k, n), ind, "SEQINDICATOR {ind} encode");
        }
        // 0 and 7..=15 are not valid SEQINDICATOR values in JAERO's switch.
        assert!(seq_indicator(0).is_none());
        for v in 7..=15 {
            assert!(seq_indicator(v).is_none(), "SEQINDICATOR {v}");
        }
    }

    #[test]
    fn isu_chain_reassembles() {
        let payload: Vec<u8> = (0..27).map(|i| i as u8 + 0x40).collect();
        let sus = build_isu_chain(0xA1B2C3, 0x44, 2, 5, &payload);
        // 27 bytes: 2 in ISU + 25 rest → 4 SSUs (8+8+8+1).
        assert_eq!(sus.len(), 1 + 4);
        let mut r = Reassembler::new();
        let mut out = None;
        for su in &sus {
            assert!(su_crc_ok(su));
            out = r.push(su);
        }
        let u = out.expect("reassembly completes");
        assert_eq!(u.aes_id, "A1B2C3");
        assert_eq!(u.ges_id, 0x44);
        assert_eq!(u.data, payload);
    }

    #[test]
    fn acars_user_data_parses() {
        let mut data = vec![0xFF, 0xFF];
        data.extend(xng_acars::block::build(
            '2', "VT-ANB", None, "B6", '4', Some("M11A"), Some("AI0142"),
            "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
            false,
        ));
        let b = parse_acars(&data).expect("ACARS parses");
        assert!(b.crc_ok);
        assert_eq!(b.core.label, "B6");
        assert_eq!(b.core.app.as_ref().unwrap()["app"], "adsc");
    }

    #[test]
    fn bad_crc_su_rejected() {
        let mut su = fill_su();
        su[3] ^= 1;
        assert!(!su_crc_ok(&su));
        assert!(su_crc_ok(&fill_su()));
        assert!(su_crc_ok(&[0u8; 12])); // all-zero rule
    }
}
