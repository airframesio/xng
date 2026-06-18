//! AX.25 v2.2 link-layer frame parsing (the framing layer).
//!
//! References (cited inline at the parsing code and in the tests):
//!
//! - **AX.25 Link Access Protocol for Amateur Packet Radio, Version 2.2**
//!   (TAPR / ARRL, July 1998). Relevant clauses:
//!   - §3.12 "The Address Field" — each callsign subfield is the 6 ASCII
//!     callsign characters shifted left one bit (`C << 1`), space-padded to
//!     6, followed by an SSID octet. The HDLC address extension bit (LSB) of
//!     every address octet is 0 **except the last octet of the whole address
//!     field, whose LSB is 1**.
//!   - §3.12.2 "SSID" — the SSID octet layout is
//!     `0 1 1 0 . SSID(4 bits) . extension-bit`: bit7 reserved, bits 6,5 are
//!     the C-bit / reserved bits, bits 4..1 carry the 0..15 SSID, bit0 is the
//!     HDLC extension bit. With the standard "11" reserved bits and C=0 the
//!     octet is `0x60 | (ssid << 1) | ext`.
//!   - §3.13 "The Control Field" — a UI (Unnumbered Information) frame uses
//!     control octet `0x03` (modulo-8).
//!   - §3.14 "The PID Field" — `0xF0` means "no layer 3 protocol" (used by
//!     APRS).
//!   - §3.9 "Frame Check Sequence" — the FCS is a 16-bit CRC computed per
//!     ISO 3309 / CCITT (the X.25 / HDLC FCS: poly 0x1021, reflected,
//!     init 0xFFFF, complemented), transmitted low-order byte first.
//!
//! The on-air HDLC framing (flags, bit-stuffing, NRZI) is handled in
//! [`crate::demod`]; this module operates on an already-deframed,
//! bit-unstuffed octet sequence (address … control PID info FCS).

use serde::Serialize;
use xng_dsp::checksum::{hdlc_fcs, hdlc_frame_ok};

/// One decoded AX.25 address subfield (callsign + SSID).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Address {
    /// Callsign, trailing spaces stripped (e.g. `"APRS"`, `"WIDE1"`).
    pub callsign: String,
    /// Secondary station identifier, 0..15.
    pub ssid: u8,
    /// The C-bit / "has-been-repeated" H-bit of the SSID octet (bit 7).
    /// For source/dest this is the command/response C-bit; for a digipeater
    /// it is the "has-been-repeated" flag.
    pub h_or_c: bool,
}

impl Address {
    /// Display form: `CALL` or `CALL-SSID` (SSID omitted when 0), with a
    /// trailing `*` when the H-bit (has-been-repeated) is set, matching the
    /// conventional APRS TNC-2 monitor text.
    pub fn display(&self) -> String {
        let base = if self.ssid == 0 {
            self.callsign.clone()
        } else {
            format!("{}-{}", self.callsign, self.ssid)
        };
        if self.h_or_c {
            format!("{base}*")
        } else {
            base
        }
    }
}

/// A decoded AX.25 UI frame as used by APRS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ax25Frame {
    /// Destination address (APRS uses this for the "tocall" / software id).
    pub dest: Address,
    /// Source address (the transmitting station).
    pub source: Address,
    /// Digipeater path (0..8 entries), in order.
    pub via: Vec<Address>,
    /// Control octet (0x03 for UI).
    pub control: u8,
    /// Protocol identifier (0xF0 = no layer 3 for APRS).
    pub pid: u8,
    /// The information field (APRS payload), raw bytes.
    pub info: Vec<u8>,
    /// True when the frame's transmitted FCS validated.
    pub fcs_ok: bool,
}

/// One callsign subfield occupies 7 octets: 6 shifted-ASCII chars + 1 SSID.
const ADDR_LEN: usize = 7;

/// Decode a single 7-octet address subfield (AX.25 v2.2 §3.12).
///
/// `last_in_field` is true when bit0 (HDLC extension) of the SSID octet is
/// set, i.e. this is the final address subfield of the frame.
fn parse_address(octets: &[u8]) -> Option<(Address, bool)> {
    if octets.len() < ADDR_LEN {
        return None;
    }
    let mut callsign = String::with_capacity(6);
    for &o in &octets[0..6] {
        // §3.12: callsign chars are ASCII shifted left one bit.
        let c = (o >> 1) & 0x7f;
        let ch = c as char;
        // Valid callsign chars are uppercase letters, digits, and space pad.
        if ch != ' ' {
            callsign.push(ch);
        }
    }
    let ssid_octet = octets[6];
    // §3.12.2: SSID is bits 4..1; bit0 is the HDLC extension bit; bit7 is the
    // C/H bit.
    let ssid = (ssid_octet >> 1) & 0x0f;
    let h_or_c = (ssid_octet & 0x80) != 0;
    let last = (ssid_octet & 0x01) != 0;
    Some((
        Address {
            callsign,
            ssid,
            h_or_c,
        },
        last,
    ))
}

/// Parse a deframed AX.25 frame (the octets between two HDLC flags, with
/// bit-stuffing already removed and the trailing 2-octet FCS still present).
///
/// Returns `None` if the frame is too short, the address field is malformed,
/// or it is not a UI frame. The FCS is checked (`fcs_ok`) but a bad FCS does
/// not by itself reject the frame — the caller decides whether to keep
/// CRC-failed frames.
pub fn parse_frame(frame: &[u8]) -> Option<Ax25Frame> {
    // Minimum: dest(7) + source(7) + control(1) + pid(1) + fcs(2) = 18.
    if frame.len() < 18 {
        return None;
    }
    // FCS is the last two octets (transmitted low byte first); §3.9.
    let fcs_ok = hdlc_frame_ok(frame);
    let body = &frame[..frame.len() - 2];

    // Walk address subfields until the extension bit (LSB) is set. §3.12.
    let mut addrs: Vec<Address> = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos + ADDR_LEN > body.len() {
            return None;
        }
        let (addr, last) = parse_address(&body[pos..pos + ADDR_LEN])?;
        addrs.push(addr);
        pos += ADDR_LEN;
        if last {
            break;
        }
        // dest + source + up to 8 digipeaters = max 10 subfields.
        if addrs.len() > 10 {
            return None;
        }
    }
    if addrs.len() < 2 {
        return None;
    }

    // After the address field: control, PID, then info. §3.13 / §3.14.
    if pos + 2 > body.len() {
        return None;
    }
    let control = body[pos];
    // UI frame control = 0x03 (modulo-8). §3.13. (Accept the P/F bit set too:
    // 0x13.)
    if control & 0xef != 0x03 {
        return None;
    }
    let pid = body[pos + 1];
    let info = body[pos + 2..].to_vec();

    let mut it = addrs.into_iter();
    let dest = it.next()?;
    let source = it.next()?;
    let via: Vec<Address> = it.collect();

    Some(Ax25Frame {
        dest,
        source,
        via,
        control,
        pid,
        info,
        fcs_ok,
    })
}

/// True for the UI/PID combination APRS uses: control 0x03 (UI), PID 0xF0.
pub fn is_aprs_ui(frame: &Ax25Frame) -> bool {
    (frame.control & 0xef) == 0x03 && frame.pid == 0xf0
}

/// Encode a callsign + SSID into the 7-octet AX.25 address subfield.
///
/// Used by the modulator (tests only) and by the spec-vector test helpers.
/// `last` sets the HDLC extension bit (LSB of the SSID octet); `h_or_c` sets
/// the C/H bit (bit 7). The two reserved bits are set to 1 (the conventional
/// value), so the SSID octet is `0x60 | (ssid<<1) | ext | (h<<7)`. (AX.25 v2.2
/// §3.12 / §3.12.2.)
pub fn encode_address(callsign: &str, ssid: u8, last: bool, h_or_c: bool) -> [u8; 7] {
    let mut out = [b' ' << 1; 7];
    let bytes = callsign.as_bytes();
    for i in 0..6 {
        let c = if i < bytes.len() { bytes[i] } else { b' ' };
        out[i] = c << 1;
    }
    let mut ssid_octet = 0x60u8 | ((ssid & 0x0f) << 1);
    if last {
        ssid_octet |= 0x01;
    }
    if h_or_c {
        ssid_octet |= 0x80;
    }
    out[6] = ssid_octet;
    out
}

/// Build a complete AX.25 UI frame (address… control PID info FCS), with the
/// FCS appended low byte first. Tests / modulator only. `via` is a list of
/// `(callsign, ssid)` digipeaters.
pub fn build_ui_frame(
    dest: (&str, u8),
    source: (&str, u8),
    via: &[(&str, u8)],
    info: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    let last_idx = 1 + via.len(); // index of the final address subfield
    let mut idx = 0usize;
    let push_addr = |call: &str, ssid: u8, body: &mut Vec<u8>, idx: &mut usize| {
        let last = *idx == last_idx;
        body.extend_from_slice(&encode_address(call, ssid, last, false));
        *idx += 1;
    };
    push_addr(dest.0, dest.1, &mut body, &mut idx);
    push_addr(source.0, source.1, &mut body, &mut idx);
    for &(call, ssid) in via {
        push_addr(call, ssid, &mut body, &mut idx);
    }
    body.push(0x03); // UI control
    body.push(0xf0); // PID = no layer 3
    body.extend_from_slice(info);
    let fcs = hdlc_fcs(&body);
    body.extend_from_slice(&fcs.to_le_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPEC GROUND TRUTH — AX.25 v2.2 §3.12 address-octet construction.
    ///
    /// Hand-encode the callsign "APRS" per the spec's stated rule (ASCII
    /// shifted left one bit) and assert the exact octets, independent of this
    /// crate's parser. 'A'=0x41<<1=0x82, 'P'=0x50<<1=0xA0, 'R'=0x52<<1=0xA4,
    /// 'S'=0x53<<1=0xA6, space=0x20<<1=0x40. SSID octet for SSID 0, not last,
    /// reserved "11", C=0: 0x60.
    #[test]
    fn address_octets_match_spec_shift_rule() {
        let enc = encode_address("APRS", 0, false, false);
        assert_eq!(
            enc,
            [0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0x60],
            "AX.25 §3.12: callsign chars are ASCII<<1, space-padded; SSID octet 0x60"
        );
        // Final-octet extension bit: SSID 0, last subfield -> 0x61.
        let last = encode_address("APRS", 0, true, false);
        assert_eq!(last[6], 0x61, "AX.25 §3.12 HDLC extension LSB set on last octet");
        // SSID 11 in bits 4..1: 0x60 | (11<<1) = 0x60|0x16 = 0x76.
        let s11 = encode_address("WIDE1", 11, false, false);
        assert_eq!(s11[6], 0x76, "AX.25 §3.12.2 SSID 11 -> octet 0x76");
    }

    /// SPEC GROUND TRUTH — round of the address rule under the parser.
    ///
    /// Hand-build the address octets per §3.12 and assert the parser recovers
    /// callsign + SSID + the final-octet extension flag. The octets are
    /// constructed by the spec rule above (NOT by calling our encoder), so
    /// this is a spec-anchored decode, not an encode/decode loopback.
    #[test]
    fn parse_address_from_handbuilt_spec_octets() {
        // "N0CALL" SSID 5, last subfield. Per §3.12: chars <<1.
        // N=0x4E<<1=0x9C 0=0x30<<1=0x60 C=0x43<<1=0x86 A=0x41<<1=0x82
        // L=0x4C<<1=0x98 L=0x98. SSID octet: 0x60|(5<<1)|1 = 0x60|0x0A|1 = 0x6B.
        let octets = [0x9C, 0x60, 0x86, 0x82, 0x98, 0x98, 0x6B];
        let (addr, last) = parse_address(&octets).expect("parse");
        assert_eq!(addr.callsign, "N0CALL");
        assert_eq!(addr.ssid, 5);
        assert!(last, "extension bit set => final address subfield");
        assert!(!addr.h_or_c);
    }

    /// SPEC GROUND TRUTH — a full AX.25 UI frame hand-built from §3.12–3.14
    /// octets, then parsed.
    ///
    /// The frame octets are constructed directly from the spec rules (address
    /// = dest+source+digi, each callsign ASCII<<1 + SSID octet with the last
    /// LSB=1; control 0x03; PID 0xF0; then info; then the X.25 FCS low byte
    /// first). The parser must recover dest/source/digipeater callsigns +
    /// SSIDs, the UI control, the PID, the info field, and validate the FCS.
    #[test]
    fn parse_full_ui_frame_from_spec_octets() {
        // dest APRS-0 (not last), source N0CALL-5 (not last),
        // digi WIDE1-1 (last). control 0x03, pid 0xF0, info "!hi".
        let mut frame = Vec::new();
        // dest APRS, ssid 0, not last
        frame.extend_from_slice(&[0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0x60]);
        // source N0CALL, ssid 5, not last: SSID octet 0x60|(5<<1)=0x6A
        frame.extend_from_slice(&[0x9C, 0x60, 0x86, 0x82, 0x98, 0x98, 0x6A]);
        // digi WIDE1, ssid 1, LAST: W=0x57<<1=0xAE I=0x49<<1=0x92 D=0x44<<1=0x88
        // E=0x45<<1=0x8A 1=0x31<<1=0x62 space=0x40. SSID octet 0x60|(1<<1)|1=0x63
        frame.extend_from_slice(&[0xAE, 0x92, 0x88, 0x8A, 0x62, 0x40, 0x63]);
        frame.push(0x03); // UI
        frame.push(0xF0); // PID no-L3
        frame.extend_from_slice(b"!hi");
        // Append a correct FCS so fcs_ok is exercised against the X.25 CRC.
        let fcs = hdlc_fcs(&frame);
        frame.extend_from_slice(&fcs.to_le_bytes());

        let f = parse_frame(&frame).expect("parse UI frame");
        assert_eq!(f.dest.callsign, "APRS");
        assert_eq!(f.dest.ssid, 0);
        assert_eq!(f.source.callsign, "N0CALL");
        assert_eq!(f.source.ssid, 5);
        assert_eq!(f.via.len(), 1);
        assert_eq!(f.via[0].callsign, "WIDE1");
        assert_eq!(f.via[0].ssid, 1);
        assert_eq!(f.control, 0x03);
        assert_eq!(f.pid, 0xF0);
        assert_eq!(f.info, b"!hi");
        assert!(f.fcs_ok, "X.25 FCS must validate");
        assert!(is_aprs_ui(&f));
    }

    /// A corrupted info byte must make the FCS fail (proves the FCS check is
    /// real, AX.25 v2.2 §3.9).
    #[test]
    fn corrupt_info_breaks_fcs() {
        let mut frame = build_ui_frame(("APRS", 0), ("N0CALL", 5), &[("WIDE1", 1)], b"!hi");
        let ok = parse_frame(&frame).unwrap();
        assert!(ok.fcs_ok);
        let n = frame.len();
        frame[n - 3] ^= 0x01; // flip a bit in the info field
        let bad = parse_frame(&frame).unwrap();
        assert!(!bad.fcs_ok, "FCS must catch the corruption");
    }
}
