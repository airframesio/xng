//! ATN Baseline 1 CPDLC (protected mode) and CM logon decoding.
//!
//! Message structures are from the ICAO Doc 9880/9705 ASN.1 modules
//! (vendored as spec text in docs/asn1/, obtained via Wireshark's
//! transcription of the ICAO standard — module text only; no Wireshark
//! code consulted). Encoding is unaligned PER (ITU-T X.691) as profiled
//! by the ATN upper layers.
//!
//! v1 scope: ProtectedAircraftPDUs / ProtectedGroundPDUs walk, the
//! ATCUplink/DownlinkMessage header (msg id/ref, date-time, logical
//! ack), and element identification — the full 238-uplink/114-downlink
//! element tables with the standard phraseology, generated from the
//! module. Elements with arguments report the argument type and the
//! phrase; argument value rendering follows (the FANS-1/A path).

use serde_json::{Value, json};

include!("atn_cpdlc_tables.rs");

/// Unaligned-PER bit reader.
struct Per<'a> {
    bits: &'a [u8],
    pos: usize,
}

impl<'a> Per<'a> {
    fn new(bytes: &'a [u8], store: &'a mut Vec<u8>) -> Per<'a> {
        store.clear();
        store.extend(
            bytes.iter().flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1)),
        );
        Per { bits: store, pos: 0 }
    }

    fn bit(&mut self) -> Option<u8> {
        let b = *self.bits.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn uint(&mut self, n: usize) -> Option<u64> {
        if self.pos + n > self.bits.len() {
            return None;
        }
        let v = self.bits[self.pos..self.pos + n]
            .iter()
            .fold(0u64, |v, &b| (v << 1) | b as u64);
        self.pos += n;
        Some(v)
    }

    /// Constrained whole number (X.691 §10.5): bit-field of the minimal
    /// width for the range.
    fn constrained(&mut self, lo: i64, hi: i64) -> Option<i64> {
        let range = (hi - lo + 1) as u64;
        if range == 1 {
            return Some(lo);
        }
        let bits = 64 - (range - 1).leading_zeros() as usize;
        Some(lo + self.uint(bits)? as i64)
    }

    /// General length determinant (X.691 §10.9, unaligned): 0xxxxxxx or
    /// 10xxxxxx xxxxxxxx (fragmentation unsupported — not seen in CPDLC).
    fn length(&mut self) -> Option<usize> {
        if self.bit()? == 0 {
            return Some(self.uint(7)? as usize);
        }
        if self.bit()? == 0 {
            return Some(self.uint(14)? as usize);
        }
        None
    }

    /// IA5String with a constrained SIZE: 7 bits per character (UPER).
    fn ia5(&mut self, min: i64, max: i64) -> Option<String> {
        let n = self.constrained(min, max)? as usize;
        let mut s = String::with_capacity(n);
        for _ in 0..n {
            s.push(self.uint(7)? as u8 as char);
        }
        Some(s)
    }

    fn remaining_bytes(&mut self, nbits: usize) -> Option<Vec<u8>> {
        if self.pos + nbits > self.bits.len() {
            return None;
        }
        let out = self.bits[self.pos..self.pos + nbits]
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |v, (i, &b)| v | (b << (7 - i))))
            .collect();
        self.pos += nbits;
        Some(out)
    }
}

/// Try to decode an ATN-B1 protected-mode CPDLC APDU (either direction).
pub fn parse_apdu(bytes: &[u8]) -> Option<Value> {
    parse_pdus(bytes, true).or_else(|| parse_pdus(bytes, false))
}

fn parse_pdus(bytes: &[u8], downlink: bool) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    // CHOICE with extension marker: 1 extension bit + root index.
    if p.bit()? != 0 {
        return None; // extension alternatives: not decoded
    }
    // ProtectedAircraftPDUs: 4 root alternatives (2 bits);
    // ProtectedGroundPDUs: 6 root alternatives (3 bits).
    let (alts, idx_bits) = if downlink { (4u64, 2) } else { (6u64, 3) };
    let idx = p.uint(idx_bits)?;
    if idx >= alts {
        return None;
    }
    let kind = match (downlink, idx) {
        (_, 0) => "abort-user",
        (_, 1) => "abort-provider",
        (true, 2) => "startdown",
        (true, 3) => "send",
        (false, 2) => "startup",
        (false, 3) => "send",
        (false, 4) => "forward",
        (false, 5) => "forward-response",
        _ => return None,
    };
    let mut out = json!({
        "application": "CPDLC",
        "version": "ATN-B1",
        "direction": if downlink { "downlink" } else { "uplink" },
        "pdu": kind,
    });
    match kind {
        "send" | "startup" => {
            out["message"] = protected_message(&mut p, downlink)?;
        }
        "startdown" => {
            // ProtectedStartDownMessage: mode DEFAULT (presence bit) then
            // the protected message.
            if p.bit()? == 1 {
                out["mode"] = json!(if p.bit()? == 1 { "dsc" } else { "cpdlc" });
            }
            out["message"] = protected_message(&mut p, downlink)?;
        }
        "abort-user" | "abort-provider" => {
            // Extensible ENUMERATED: ext bit + root index.
            if p.bit()? == 0 {
                out["reason"] = json!(p.uint(3)?);
            }
        }
        _ => {}
    }
    Some(out)
}

/// ProtectedUplink/DownlinkMessage: extensible SEQUENCE with two
/// OPTIONAL components, the second being the PER-encoded
/// ATCUplink/DownlinkMessage in a BIT STRING.
fn protected_message(p: &mut Per, downlink: bool) -> Option<Value> {
    if p.bit()? != 0 {
        return None; // extension additions present: bail
    }
    let has_algo = p.bit()? == 1;
    let has_msg = p.bit()? == 1;
    if has_algo {
        // RELATIVE-OID: length determinant + octets (skipped).
        let n = p.length()?;
        p.remaining_bytes(n * 8)?;
    }
    if !has_msg {
        return Some(json!({ "empty": true }));
    }
    let nbits = p.length()?;
    let inner = p.remaining_bytes(nbits)?;
    // The BIT STRING length is in bits; the inner message is itself
    // PER, decoded from its own bit zero.
    atc_message(&inner, nbits, downlink)
}

/// ATCUplinkMessage / ATCDownlinkMessage.
fn atc_message(bytes: &[u8], nbits: usize, downlink: bool) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    p.bits = &p.bits[..nbits.min(p.bits.len())];

    // ATCMessageHeader: optional msgRef + defaulted logicalAck preamble.
    let has_ref = p.bit()? == 1;
    let has_ack = p.bit()? == 1;
    let msg_id = p.constrained(0, 63)?;
    let msg_ref = if has_ref { Some(p.constrained(0, 63)?) } else { None };
    // DateTimeGroup: Date{year 1996..2095, month 1..12, day 1..31} +
    // Timehhmmss{hours 0..23, minutes 0..59, seconds 0..59}.
    let (y, mo, d) = (
        p.constrained(1996, 2095)?,
        p.constrained(1, 12)?,
        p.constrained(1, 31)?,
    );
    let (h, mi, sec) = (
        p.constrained(0, 23)?,
        p.constrained(0, 59)?,
        p.constrained(0, 59)?,
    );
    let ack = if has_ack {
        if p.constrained(0, 1)? == 0 { "required" } else { "not-required" }
    } else {
        "not-required"
    };

    // MessageData: SEQUENCE {elementIds SIZE(1..5), constrainedData OPT}.
    let _has_constrained = p.bit()? == 1;
    let count = p.constrained(1, 5)? as usize;
    let table: &[(&str, &str, &str)] =
        if downlink { &DOWNLINK_ELEMENTS } else { &UPLINK_ELEMENTS };
    let idx_bits = 64 - (table.len() as u64 - 1).leading_zeros() as usize;

    let mut elements = Vec::new();
    for k in 0..count {
        // Element CHOICE (not extensible in the module).
        let idx = p.uint(idx_bits)? as usize;
        let (name, arg_ty, phrase) = table.get(idx).copied()?;
        let mut el = json!({ "element": name, "phrase": phrase });
        if arg_ty != "NULL" {
            el["argument_type"] = json!(arg_ty);
            // Argument value decoding lands next (FANS-1/A path);
            // without it, later elements cannot be located.
            if k + 1 < count {
                el["note"] = json!("remaining elements undecoded (argument sizes unknown)");
            }
            elements.push(el);
            break;
        }
        elements.push(el);
    }

    Some(json!({
        "msg_id": msg_id,
        "msg_ref": msg_ref,
        "timestamp": format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{sec:02}Z"),
        "logical_ack": ack,
        "elements": elements,
    }))
}

/// CM (context management) logon request — the dialogue that precedes
/// CPDLC; identifies the flight.
pub fn parse_cm_logon(bytes: &[u8]) -> Option<Value> {
    let mut store = Vec::new();
    let mut p = Per::new(bytes, &mut store);
    // CMAircraftMessage CHOICE (extensible, 3 root): ext bit + 2 bits.
    if p.bit()? != 0 {
        return None;
    }
    if p.uint(2)? != 0 {
        return None; // only cmLogonRequest decoded for now
    }
    // CMLogonRequest: 6 OPTIONAL components → presence bitmap.
    let present: Vec<bool> = (0..6).map(|_| p.bit() == Some(1)).collect();
    let flight_id = p.ia5(2, 8)?;
    Some(json!({
        "application": "CM",
        "pdu": "logon-request",
        "flight_id": flight_id,
        "optional_fields_present": present.iter().filter(|&&b| b).count(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-builder for synthetic UPER vectors.
    struct Bits(Vec<u8>);
    impl Bits {
        fn new() -> Self {
            Bits(Vec::new())
        }
        fn push(&mut self, v: u64, n: usize) {
            for k in (0..n).rev() {
                self.0.push(((v >> k) & 1) as u8);
            }
        }
        fn bytes(&self) -> Vec<u8> {
            self.0
                .chunks(8)
                .map(|c| {
                    c.iter().enumerate().fold(0u8, |v, (i, &b)| v | (b << (7 - i)))
                })
                .collect()
        }
    }

    fn build_downlink_wilco() -> Vec<u8> {
        // Inner ATCDownlinkMessage: header + one dM0NULL (WILCO).
        let mut m = Bits::new();
        m.push(0, 1); // msgRef absent
        m.push(0, 1); // logicalAck default
        m.push(12, 6); // msg id 12
        m.push((2026 - 1996) as u64, 7); // year
        m.push(6 - 1, 4); // month (1..12)
        m.push(11 - 1, 5); // day (1..31)
        m.push(1, 5); // hours
        m.push(22, 6); // minutes
        m.push(33, 6); // seconds
        m.push(0, 1); // no constrainedData
        m.push(0, 3); // element count 1 (range 1..5 → 3 bits, offset 0)
        m.push(0, 7); // CHOICE index 0 = dM0NULL (114 → 7 bits)
        let inner_bits = m.0.len();

        // Outer ProtectedAircraftPDUs: send → ProtectedDownlinkMessage.
        let mut o = Bits::new();
        o.push(0, 1); // choice not extended
        o.push(3, 2); // send
        o.push(0, 1); // sequence not extended
        o.push(0, 1); // no algorithmIdentifier
        o.push(1, 1); // protectedMessage present
        // BIT STRING length determinant (short form).
        o.push(0, 1);
        o.push(inner_bits as u64, 7);
        o.0.extend(&m.0);
        // integrityCheck BIT STRING: zero-length is fine for the test.
        o.push(0, 1);
        o.push(0, 7);
        o.bytes()
    }

    #[test]
    fn downlink_wilco_decodes() {
        let v = parse_apdu(&build_downlink_wilco()).expect("apdu");
        assert_eq!(v["application"], "CPDLC");
        assert_eq!(v["direction"], "downlink");
        assert_eq!(v["pdu"], "send");
        let msg = &v["message"];
        assert_eq!(msg["msg_id"], 12);
        assert_eq!(msg["timestamp"], "2026-06-11T01:22:33Z");
        assert_eq!(msg["elements"][0]["element"], "dM0NULL");
        assert_eq!(msg["elements"][0]["phrase"], "WILCO");
    }

    #[test]
    fn uplink_element_with_argument_reports_type() {
        // Uplink: uM20Level "CLIMB TO [level]".
        let mut m = Bits::new();
        m.push(0, 1);
        m.push(0, 1);
        m.push(5, 6);
        m.push(30, 7);
        m.push(0, 4);
        m.push(0, 5);
        m.push(10, 5);
        m.push(0, 6);
        m.push(0, 6);
        m.push(0, 1);
        m.push(0, 3); // 1 element
        m.push(20, 8); // uM20 (238 → 8 bits)
        let inner_bits = m.0.len();
        let mut o = Bits::new();
        o.push(0, 1);
        o.push(3, 3); // ground PDUs: 3 bits, send
        o.push(0, 1);
        o.push(0, 1);
        o.push(1, 1);
        o.push(0, 1);
        o.push(inner_bits as u64, 7);
        o.0.extend(&m.0);
        o.push(0, 1);
        o.push(0, 7);
        let v = parse_pdus(&o.bytes(), false).expect("apdu");
        let el = &v["message"]["elements"][0];
        assert_eq!(el["element"], "uM20Level");
        assert_eq!(el["phrase"], "CLIMB TO [level]");
        assert_eq!(el["argument_type"], "Level");
    }

    #[test]
    fn cm_logon_request_flight_id_decodes() {
        let mut b = Bits::new();
        b.push(0, 1); // not extended
        b.push(0, 2); // cmLogonRequest
        b.push(0, 6); // six absent optionals
        // AircraftFlightIdentification IA5 SIZE(2..8): "UAL123" len 6.
        b.push(4, 3); // 6 - 2
        for c in b"UAL123" {
            b.push(*c as u64, 7);
        }
        let v = parse_cm_logon(&b.bytes()).expect("cm");
        assert_eq!(v["pdu"], "logon-request");
        assert_eq!(v["flight_id"], "UAL123");
    }
}
