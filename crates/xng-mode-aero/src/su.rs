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
