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
    /// Decoded element arguments in template order (when the element's
    /// argument structure is one we decode; see `decode`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
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

/// FANSAltitude CHOICE (3-bit index; widths/offsets and the value
/// semantics — QNH/QFE in tens of feet, flight level metric in tens of
/// meters — from libacars's generated constraints and text formatters).
fn read_altitude(b: &mut Bits) -> Option<String> {
    Some(match b.read(3)? {
        0 => format!("{} ft", b.read(12)? * 10),        // QNH (0..2500) x10
        1 => format!("{} m", b.read(14)?),              // QNH meters
        2 => format!("{} ft QFE", b.read(12)? * 10),    // QFE (0..2100) x10
        3 => format!("{} m QFE", b.read(13)?),          // QFE meters
        4 => format!("{} ft", b.read(18)?),             // GNSS feet
        5 => format!("{} m", b.read(16)?),              // GNSS meters
        6 => format!("FL{}", 30 + b.read(10)?),         // flight level (30..600)
        7 => format!("{} m", (100 + b.read(11)?) * 10), // metric FL (100..2000) x10
        _ => unreachable!(),
    })
}

/// FANSTime: hours (0..23, 5 bits) + minutes (0..59, 6 bits).
fn read_time(b: &mut Bits) -> Option<String> {
    let h = b.read(5)?;
    let m = b.read(6)?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}"))
}

/// Decode the element's arguments when its type (the tag's suffix after
/// `dMnn`/`uMnn`) is one of the simple shapes we handle. Returns None
/// for argument structures not decoded yet — the caller keeps the
/// bracketed template untouched.
fn read_args(tag: &str, b: &mut Bits) -> Option<Vec<String>> {
    let ty = tag.trim_start_matches(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == 'M');
    match ty {
        "NULL" => Some(Vec::new()),
        "Altitude" => Some(vec![read_altitude(b)?]),
        // SEQUENCE SIZE(2..2) OF Altitude: fixed size, no length bits.
        "AltitudeAltitude" => Some(vec![read_altitude(b)?, read_altitude(b)?]),
        "Time" => Some(vec![read_time(b)?]),
        _ => None,
    }
}

/// Substitute decoded arguments into the bracketed template slots.
fn render(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for a in args {
        let Some(start) = out.find('[') else { break };
        let Some(end) = out[start..].find(']') else { break };
        out.replace_range(start..start + end + 1, a);
    }
    out
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
    let args = read_args(tag, &mut b).unwrap_or_default();
    let text = if args.is_empty() { label.to_string() } else { render(label, &args) };
    Some(CpdlcMessage {
        msg_id,
        msg_ref,
        timestamp,
        element: tag.to_string(),
        text,
        args,
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
    fn altitude_arguments_render() {
        // dM9Altitude = "REQUEST CLIMB TO [altitude]"; arg = flight level
        // CHOICE (index 6) + FL360 (offset 330 from lower bound 30).
        let mut body = build(11, None, None, 9, false);
        // Append the altitude arg bits: 3-bit choice 6, 10-bit offset 330.
        let mut bits: Vec<u8> = Vec::new();
        for k in (0..3).rev() {
            bits.push(((6 >> k) & 1) as u8);
        }
        for k in (0..10).rev() {
            bits.push(((330u32 >> k) & 1) as u8);
        }
        // The header for this build is 3+6+8 = 17 bits; continue packing
        // from bit 17.
        let mut all = body.clone();
        all.resize(5, 0);
        for (i, &v) in bits.iter().enumerate() {
            let p = 17 + i;
            all[p / 8] |= v << (7 - p % 8);
        }
        body = all;
        let m = decode(&body, true).unwrap();
        assert_eq!(m.element, "dM9Altitude");
        assert_eq!(m.args, vec!["FL360"]);
        assert_eq!(m.text, "REQUEST CLIMB TO FL360");
    }

    #[test]
    fn undecoded_argument_keeps_template() {
        // dM22Position = "REQUEST DIRECT TO [position]" — position args
        // are not decoded; the bracketed template must survive.
        let m = decode(&build(2, None, None, 22, false), true).unwrap();
        assert_eq!(m.element, "dM22Position");
        assert!(m.args.is_empty());
        assert!(m.text.contains("[position]"), "{}", m.text);
    }

    #[test]
    fn rejects_out_of_range() {
        // Element index beyond the downlink table.
        assert!(decode(&build(1, None, None, 200, false), true).is_none());
        assert!(decode(&[], true).is_none());
    }
}
