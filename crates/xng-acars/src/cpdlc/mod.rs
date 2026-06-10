//! CPDLC (FANS-1/A) message identification, ported from MIT-licensed
//! libacars (asn1c-generated FANSAC* tables + asn1-format-cpdlc-text.c
//! labels; see ../PROVENANCE.md).
//!
//! Scope (v1): unaligned-PER decode of the ATC message header (message
//! id, optional reference number, optional timestamp) and the FIRST
//! message element's CHOICE tag, mapped to its human-readable template.
//! Element arguments and additional elements (rare; their presence is
//! reported) are left as remaining raw bits — full argument decoding is
//! a planned follow-up.
//!
//! UPER layout (from the generated constraint tables):
//! - ATCDownlinkMessage / ATCUplinkMessage = SEQUENCE { header,
//!   first-element, additional-elements OPTIONAL } → 1 presence bit
//! - header = SEQUENCE { msgId (0..63, 6 bits), msgRef OPTIONAL
//!   (0..63, 6 bits), timestamp OPTIONAL } → 2 presence bits
//! - timestamp = hours (0..23, 5 bits), minutes (0..59, 6 bits),
//!   seconds (0..59, 6 bits)
//! - element = CHOICE: downlink (0..128, 8 bits), uplink (0..182,
//!   8 bits), non-extensible

use serde::Serialize;

mod tables;
use tables::{DOWNLINK_ELEMENTS, UPLINK_ELEMENTS};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CpdlcMessage {
    pub msg_id: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_ref: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// First message element's ASN.1 tag (e.g. "dM0NULL").
    pub element: String,
    /// Human-readable template for the element ("WILCO",
    /// "REQUEST [altitude]", ...). Bracketed arguments are not decoded
    /// in v1.
    pub text: String,
    /// The message carries additional elements beyond the first.
    pub more_elements: bool,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

/// Decode a FANS-1/A ATC message body (the octets after the ARINC 622
/// IMI + aircraft registration, before the CRC).
pub fn decode(body: &[u8], downlink: bool) -> Option<CpdlcMessage> {
    let mut b = Bits { data: body, pos: 0 };
    let has_more = b.read(1)? == 1;
    let has_ref = b.read(1)? == 1;
    let has_ts = b.read(1)? == 1;
    let msg_id = b.read(6)? as u8;
    let msg_ref = if has_ref { Some(b.read(6)? as u8) } else { None };
    let timestamp = if has_ts {
        let h = b.read(5)?;
        let m = b.read(6)?;
        let s = b.read(6)?;
        if h > 23 || m > 59 || s > 59 {
            return None;
        }
        Some(format!("{h:02}:{m:02}:{s:02}"))
    } else {
        None
    };
    let table: &[(&str, &str)] =
        if downlink { &DOWNLINK_ELEMENTS } else { &UPLINK_ELEMENTS };
    let idx = b.read(8)? as usize;
    let (tag, label) = table.get(idx).copied()?;
    Some(CpdlcMessage {
        msg_id,
        msg_ref,
        timestamp,
        element: tag.to_string(),
        text: label.to_string(),
        more_elements: has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a UPER body for testing (mirrors the decoder's layout).
    fn build(
        msg_id: u8,
        msg_ref: Option<u8>,
        ts: Option<(u32, u32, u32)>,
        elem: u32,
        more: bool,
    ) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        let mut push = |v: u32, n: usize| {
            for k in (0..n).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        };
        push(more as u32, 1);
        push(msg_ref.is_some() as u32, 1);
        push(ts.is_some() as u32, 1);
        push(msg_id as u32, 6);
        if let Some(r) = msg_ref {
            push(r as u32, 6);
        }
        if let Some((h, m, s)) = ts {
            push(h, 5);
            push(m, 6);
            push(s, 6);
        }
        push(elem, 8);
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, &v) in bits.iter().enumerate() {
            out[i / 8] |= v << (7 - i % 8);
        }
        out
    }

    #[test]
    fn wilco_downlink() {
        let body = build(12, Some(5), Some((14, 32, 7)), 0, false);
        let m = decode(&body, true).unwrap();
        assert_eq!(m.msg_id, 12);
        assert_eq!(m.msg_ref, Some(5));
        assert_eq!(m.timestamp.as_deref(), Some("14:32:07"));
        assert_eq!(m.element, "dM0NULL");
        assert_eq!(m.text, "WILCO");
        assert!(!m.more_elements);
    }

    #[test]
    fn uplink_unable_and_altitude_request() {
        let m = decode(&build(3, None, None, 0, false), false).unwrap();
        assert_eq!(m.element, "uM0NULL");
        assert_eq!(m.text, "UNABLE");
        let m = decode(&build(7, None, None, 6, true), true).unwrap();
        assert_eq!(m.element, "dM6Altitude");
        assert_eq!(m.text, "REQUEST [altitude]");
        assert!(m.more_elements);
    }

    #[test]
    fn rejects_out_of_range() {
        // Element index beyond the downlink table.
        assert!(decode(&build(1, None, None, 200, false), true).is_none());
        assert!(decode(&[], true).is_none());
    }
}
