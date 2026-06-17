//! ATCS Spec-200 Layer-3 packet header decode.
//!
//! Inside each HDLC frame (see [`crate::frame`]) is a Spec-200 packet. Its
//! header, per the AAR Standard Manual of ATCS, is:
//!
//! ```text
//! Octet 1  control:  Q D 1 0 P P P A   (MSB -> LSB)
//!     Q   service-signal indicator (0 on originate traffic)
//!     D   network-service-signal confirmation request
//!     10  fixed bits 5..4
//!     PPP 3-bit priority level
//!     A   ARQ-disable bit (1 disables automatic repeat request)
//! Octets 2..4  reserved, zero on origination
//! Octet 5  address length:
//!     upper nibble = source address length      (count of BCD digits)
//!     lower nibble = destination address length (count of BCD digits)
//! Octet 6.. source address then destination address, BCD-packed
//!           (two digits per octet, high nibble first), then user data.
//! ```
//!
//! This decoder extracts the control fields, both addresses (via
//! [`crate::address`]), and returns the remaining bytes as raw user data.
//! The vendor codeline payload inside the user data (e.g. Genisys / ARES)
//! is out of scope and is not interpreted.

use crate::address::AtcsAddress;
use serde::Serialize;

/// A decoded Spec-200 packet header plus the raw user-data payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Spec200Packet {
    /// Octet 1, verbatim.
    pub control: u8,
    /// `Q` — service-signal indicator (bit 7).
    pub service_signal: bool,
    /// `D` — network-service-signal confirmation requested (bit 6).
    pub confirm_request: bool,
    /// 3-bit priority level (bits 3..1).
    pub priority: u8,
    /// `A` — ARQ disabled (bit 0). When false, automatic repeat request is
    /// in effect.
    pub arq_disabled: bool,
    /// True iff bits 5..4 of the control octet are the expected `1 0`
    /// pattern (a sanity flag; a Spec-200 origination packet sets these).
    pub control_well_formed: bool,
    /// Source ATCS address (the sender).
    pub source: AtcsAddress,
    /// Destination ATCS address (the addressee).
    pub destination: AtcsAddress,
    /// Direction summary derived from source vs. destination types:
    /// "ground-to-field", "field-to-ground", or "other".
    pub direction: &'static str,
    /// Remaining user-data payload (vendor codeline protocol), undecoded.
    #[serde(with = "hex_bytes")]
    pub user_data: Vec<u8>,
}

/// Serialize/deserialize `Vec<u8>` as a lowercase hex string for JSON.
mod hex_bytes {
    use serde::Serializer;

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        s.serialize_str(&out)
    }
}

/// Pull `n` BCD digits (two per octet, high nibble first) starting at
/// `bytes[off]`, returning the digit string and the number of octets
/// consumed. An odd digit count consumes the final octet but takes only
/// its high nibble.
fn read_bcd(bytes: &[u8], off: usize, n: usize) -> Option<(String, usize)> {
    let octets = n.div_ceil(2);
    if off + octets > bytes.len() {
        return None;
    }
    let mut s = String::with_capacity(n);
    for i in 0..n {
        let octet = bytes[off + i / 2];
        let nib = if i % 2 == 0 { octet >> 4 } else { octet & 0x0F };
        if nib > 9 {
            return None; // not a valid BCD digit
        }
        s.push((b'0' + nib) as char);
    }
    Some((s, octets))
}

/// Decode a Spec-200 packet from raw HDLC frame bytes (FCS already
/// stripped). Returns `None` if the bytes are too short or the addresses
/// are not valid BCD.
pub fn decode_packet(bytes: &[u8]) -> Option<Spec200Packet> {
    // Need at least: control(1) + reserved(3) + addr-len(1) = 5 octets.
    if bytes.len() < 5 {
        return None;
    }
    let control = bytes[0];
    let service_signal = control & 0x80 != 0;
    let confirm_request = control & 0x40 != 0;
    let control_well_formed = (control & 0x30) == 0x20; // bits 5..4 == 10
    let priority = (control >> 1) & 0x07;
    let arq_disabled = control & 0x01 != 0;

    // Octets 2..4 reserved; octet 5 is address length.
    let addr_len = bytes[4];
    let src_len = (addr_len >> 4) as usize;
    let dst_len = (addr_len & 0x0F) as usize;
    if src_len == 0 || dst_len == 0 {
        return None;
    }

    let mut off = 5;
    let (src_digits, used) = read_bcd(bytes, off, src_len)?;
    off += used;
    let (dst_digits, used) = read_bcd(bytes, off, dst_len)?;
    off += used;

    let source = AtcsAddress::parse(&src_digits)?;
    let destination = AtcsAddress::parse(&dst_digits)?;

    let direction = match (
        source.addr_type.is_ground(),
        destination.addr_type.is_field(),
    ) {
        (true, true) => "ground-to-field",
        _ => {
            if source.addr_type.is_field() && destination.addr_type.is_ground() {
                "field-to-ground"
            } else {
                "other"
            }
        }
    };

    let user_data = bytes[off..].to_vec();

    Some(Spec200Packet {
        control,
        service_signal,
        confirm_request,
        priority,
        arq_disabled,
        control_well_formed,
        source,
        destination,
        direction,
        user_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AddressType;

    /// Build a spec-derived Spec-200 packet byte string per the AAR header
    /// layout, then decode it. This is documented in PROVENANCE.md as
    /// SPEC-DERIVED (no public raw capture exists): the bytes are laid out
    /// by hand from the standard and the decode is checked against the
    /// standard's field definitions and the sigidwiki worked example — not
    /// against an encoder in this crate.
    ///
    /// Header byte layout:
    ///   00: control = 0x24 -> Q=0 D=0 bits54=10 PPP=010(priority 2) A=0
    ///   01..03: reserved = 00 00 00
    ///   04: addr-len = 0xAA -> source 10 digits, dest 10 digits
    ///   05..09: source BCD "5125013826" (field MCP, from sigidwiki sample)
    ///   0A..0E: dest   BCD "2125385538" (host/dispatch, from sigidwiki)
    ///   0F..  : user data
    #[test]
    fn decodes_spec_derived_packet() {
        let bytes: &[u8] = &[
            0x24, // control
            0x00, 0x00, 0x00, // reserved
            0xAA, // addr lengths: src=10, dst=10
            0x51, 0x25, 0x01, 0x38, 0x26, // source "5125013826"
            0x21, 0x25, 0x38, 0x55, 0x38, // dest   "2125385538"
            0x02, 0x04, 0x05, // user data (opaque codeline payload)
        ];
        let p = decode_packet(bytes).unwrap();

        // Control field decode.
        assert!(!p.service_signal);
        assert!(!p.confirm_request);
        assert!(p.control_well_formed);
        assert_eq!(p.priority, 2);
        assert!(!p.arq_disabled);

        // Addresses (cross-checked against the address-decode oracle).
        assert_eq!(p.source.digits, "5125013826");
        assert_eq!(p.source.addr_type, AddressType::WaysideRf);
        assert_eq!(p.source.railroad, 125);
        assert_eq!(p.destination.digits, "2125385538");
        assert_eq!(p.destination.addr_type, AddressType::Host);
        assert_eq!(p.destination.railroad, 125);

        // Direction: field MCP -> dispatch office.
        assert_eq!(p.direction, "field-to-ground");

        // Raw payload preserved, not decoded.
        assert_eq!(p.user_data, vec![0x02, 0x04, 0x05]);
    }

    /// Ground-to-field origination with ARQ disabled and priority 7.
    #[test]
    fn decodes_ground_to_field_control_bits() {
        let bytes: &[u8] = &[
            0x2F, // Q=0 D=0 10 PPP=111(7) A=1
            0x00, 0x00, 0x00, 0xAA, //
            0x21, 0x25, 0x38, 0x55, 0x38, // source host "2125385538"
            0x51, 0x25, 0x01, 0x38, 0x26, // dest MCP "5125013826"
            0x00,
        ];
        let p = decode_packet(bytes).unwrap();
        assert_eq!(p.priority, 7);
        assert!(p.arq_disabled);
        assert!(p.control_well_formed);
        assert_eq!(p.source.addr_type, AddressType::Host);
        assert_eq!(p.destination.addr_type, AddressType::WaysideRf);
        assert_eq!(p.direction, "ground-to-field");
    }

    /// Service-signal + confirmation-request control bits, and odd-length
    /// BCD address handling (5-digit short address takes the high nibble of
    /// its final octet only).
    #[test]
    fn service_signal_bits_and_odd_bcd() {
        let bytes: &[u8] = &[
            0xE0, // Q=1 D=1 10 PPP=000 A=0
            0x00, 0x00, 0x00, 0x55, // src=5 digits, dst=5 digits
            0x21, 0x23, 0x40, // source "21234" (high nibble of 0x40 = 4)
            0x59, 0x99, 0x90, // dest   "59999" (high nibble of 0x90 = 9)
            0xAB,
        ];
        let p = decode_packet(bytes).unwrap();
        assert!(p.service_signal);
        assert!(p.confirm_request);
        assert_eq!(p.source.digits, "21234");
        assert_eq!(p.destination.digits, "59999");
        assert_eq!(p.user_data, vec![0xAB]);
    }

    #[test]
    fn rejects_short_and_bad_bcd() {
        assert!(decode_packet(&[0x24, 0, 0]).is_none()); // too short
                                                         // addr-len claims 10+10 digits but only 1 address octet present.
        assert!(decode_packet(&[0x24, 0, 0, 0, 0xAA, 0x51]).is_none());
        // Non-BCD nibble (0xF) in the source address.
        assert!(decode_packet(&[0x24, 0, 0, 0, 0x22, 0x2F, 0x21]).is_none());
    }

    /// The whole struct must round-trip through serde_json (it is a
    /// `Serialize` decode result, used by downstream output layers).
    #[test]
    fn serializes_to_json() {
        let bytes: &[u8] = &[
            0x24, 0x00, 0x00, 0x00, 0xAA, 0x51, 0x25, 0x01, 0x38, 0x26, 0x21, 0x25, 0x38, 0x55,
            0x38, 0x02,
        ];
        let p = decode_packet(bytes).unwrap();
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["priority"], 2);
        assert_eq!(j["source"]["addr_type"], "wayside_rf");
        assert_eq!(j["destination"]["addr_type"], "host");
        assert_eq!(j["user_data"], "02");
        assert_eq!(j["direction"], "field-to-ground");
    }
}
