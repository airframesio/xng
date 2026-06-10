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

/// SEQINDICATOR nibble → (index k, total n), k and n 1-based.
/// NOTE: mapping order is flagged in PROVENANCE.md for verification
/// against real captures.
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
                (qno << 4) | (refno & 0x07),
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
pub fn fill_su() -> Vec<u8> {
    su_with_crc(vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

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
