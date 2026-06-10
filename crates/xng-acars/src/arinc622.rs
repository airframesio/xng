//! ARINC 622 ATS message envelope (ported from libacars `arinc.c`).
//!
//! Layout in ACARS text (leading `/` optional):
//! `<gs_addr>.<IMI><air_reg><hex payload>` where gs_addr is 7 or 4
//! uppercase/digit characters, IMI is one of AT1/CR1/CC1/DR1/ADS/DIS,
//! air_reg is 7 characters (including its leading separator, e.g.
//! `.VT-ANB`), and the payload is hex-encoded binary whose last two bytes
//! are a CRC-16 (poly 0x1021, MSB-first, init 0xFFFF) computed over the
//! IMI+air_reg ASCII followed by all payload bytes; appending the CRC
//! leaves residue 0x1D0F.

use crate::{adsc, AcarsApp};
use crc::{Crc, CRC_16_IBM_3740};
use serde::Serialize;

/// CRC-16/IBM-3740 = poly 0x1021, init 0xFFFF, unreflected — the ARINC
/// CRC; valid data+CRC leaves this residue.
const ARINC_CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);
const ARINC_CRC_GOOD: u16 = 0x1D0F;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Imi {
    /// FANS-1/A CPDLC message.
    At1,
    /// CPDLC connect request.
    Cr1,
    /// CPDLC connect confirm.
    Cc1,
    /// CPDLC disconnect request.
    Dr1,
    /// ADS-C message.
    Ads,
    /// ADS-C disconnect request.
    Dis,
}

impl Imi {
    pub fn as_str(&self) -> &'static str {
        match self {
            Imi::At1 => "AT1",
            Imi::Cr1 => "CR1",
            Imi::Cc1 => "CC1",
            Imi::Dr1 => "DR1",
            Imi::Ads => "ADS",
            Imi::Dis => "DIS",
        }
    }
}

/// Search order matches libacars' table order.
const IMI_TABLE: [(&str, Imi); 6] = [
    (".AT1", Imi::At1),
    (".CR1", Imi::Cr1),
    (".CC1", Imi::Cc1),
    (".DR1", Imi::Dr1),
    (".ADS", Imi::Ads),
    (".DIS", Imi::Dis),
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Envelope {
    /// Ground station address (7 chars downlink-style, 4 uplink-style).
    pub gs_addr: String,
    /// Aircraft registration field as transmitted (7 chars incl. leading
    /// separator, e.g. `.VT-ANB`).
    pub air_reg: String,
    pub imi: Imi,
    pub crc_ok: bool,
}

fn decode_hex(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut nibbles = Vec::with_capacity(s.len());
    for c in s.bytes() {
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'F' => 10 + c - b'A',
            b'a'..=b'f' => 10 + c - b'a',
            _ => break, // stop at first non-hex char (libacars behavior)
        };
        nibbles.push(v);
    }
    for pair in nibbles.chunks_exact(2) {
        out.push((pair[0] << 4) | pair[1]);
    }
    out
}

/// Parse an ARINC 622 message from (sublabel-stripped) ACARS text.
pub fn parse(text: &str, downlink: bool) -> Option<AcarsApp> {
    let txt = text.strip_prefix('/').unwrap_or(text);

    // First IMI from the table found anywhere in the text wins, but the
    // ground address before it must span exactly the start of the text.
    let (pos, imi) = IMI_TABLE
        .iter()
        .find_map(|(pat, imi)| txt.find(pat).map(|p| (p, *imi)))?;
    if pos != 7 && pos != 4 {
        return None;
    }
    let gs_addr = &txt[..pos];
    if !gs_addr.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return None;
    }

    // Payload: IMI(3) + air_reg(7) + hex (>= 2 CRC bytes).
    let payload = &txt[pos + 1..];
    if payload.len() < 3 + 7 + 4 {
        return None;
    }
    let imi_str = &payload[..3];
    let air_reg = &payload[3..10];
    let bytes = decode_hex(&payload[10..]);
    if bytes.len() < 2 {
        return None;
    }

    let mut digest = ARINC_CRC.digest();
    digest.update(imi_str.as_bytes());
    digest.update(air_reg.as_bytes());
    digest.update(&bytes);
    let crc_ok = digest.finalize() == ARINC_CRC_GOOD;

    let envelope = Envelope {
        gs_addr: gs_addr.to_owned(),
        air_reg: air_reg.to_owned(),
        imi,
        crc_ok,
    };
    // CRC failure is recorded but does not abort decoding (libacars
    // behavior).
    let body = &bytes[..bytes.len() - 2];

    Some(match imi {
        Imi::Ads | Imi::Dis => AcarsApp::Adsc {
            message: adsc::parse(body, downlink, imi == Imi::Dis),
            envelope,
        },
        Imi::At1 | Imi::Cr1 | Imi::Cc1 | Imi::Dr1 => AcarsApp::Cpdlc {
            // Only AT1 carries ATCDownlink/UplinkMessage; CR1/CC1/DR1 are
            // context-management bodies (different ASN.1, not decoded yet).
            message: if imi == Imi::At1 {
                crate::cpdlc::decode(body, downlink)
            } else {
                None
            },
            envelope,
            payload_hex: body.iter().map(|b| format!("{b:02x}")).collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_and_crc_on_real_message() {
        let app = parse(
            "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
            true,
        )
        .expect("should parse");
        let AcarsApp::Adsc { envelope, .. } = &app else {
            panic!("expected ADS-C");
        };
        assert_eq!(envelope.gs_addr, "BOMASAI");
        assert_eq!(envelope.air_reg, ".VT-ANB");
        assert_eq!(envelope.imi, Imi::Ads);
        assert!(envelope.crc_ok, "real off-air message must pass CRC");
    }

    #[test]
    fn cpdlc_wilco_end_to_end() {
        // UPER ATCDownlinkMessage: no extra elements, no msgRef, no
        // timestamp, msgId = 5, element 0 (dM0NULL = WILCO):
        // bits 0,0,0 | 000101 | 00000000 → 0x02 0x80 0x00.
        let body = [0x02u8, 0x80, 0x00];
        // Find the CRC trailer that satisfies the ARINC residue.
        let crc_bytes = (0..=u16::MAX)
            .map(u16::to_be_bytes)
            .find(|x| {
                let mut d = ARINC_CRC.digest();
                d.update(b"AT1");
                d.update(b".N123AB");
                d.update(&body);
                d.update(x);
                d.finalize() == ARINC_CRC_GOOD
            })
            .expect("a valid CRC trailer exists");
        let hex: String = body
            .iter()
            .chain(&crc_bytes)
            .map(|b| format!("{b:02X}"))
            .collect();
        let app = parse(&format!("/MSTEC7X.AT1.N123AB{hex}"), true).expect("parses");
        let AcarsApp::Cpdlc { envelope, message, .. } = &app else {
            panic!("expected CPDLC");
        };
        assert!(envelope.crc_ok);
        assert_eq!(envelope.imi, Imi::At1);
        let m = message.as_ref().expect("CPDLC body decodes");
        assert_eq!(m.msg_id, 5);
        assert_eq!(m.element, "dM0NULL");
        assert_eq!(m.text, "WILCO");
        assert!(!m.more_elements);
    }

    #[test]
    fn corrupted_payload_fails_crc_but_parses() {
        let app = parse(
            "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F4",
            true,
        )
        .expect("should still parse");
        let AcarsApp::Adsc { envelope, .. } = &app else {
            panic!("expected ADS-C");
        };
        assert!(!envelope.crc_ok);
    }

    #[test]
    fn rejects_non_arinc_text() {
        assert!(parse("POSN 4737.2N 12218.1W", true).is_none());
        assert!(parse("/SHORTX.ADS.A", true).is_none());
    }
}
