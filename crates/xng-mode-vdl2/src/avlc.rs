//! AVLC link layer (ETSI EN 301 841-2 §5.2 / ISO-IEC 13239): flag-delimited
//! bit-stuffed frames in the descrambled, RS-corrected bit stream.
//! `FLAG | dst(4) | src(4) | control(1) | info | FCS(2) | FLAG`.

use serde::Serialize;
use xng_dsp::checksum::HDLC_FCS;

const FLAG: u8 = 0x7E;
const MIN_FRAME_OCTETS: usize = 4 + 4 + 1 + 2;
const MAX_FRAME_OCTETS: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressType {
    Aircraft,
    GroundIcao,
    GroundDelegated,
    AllStations,
    Reserved,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AvlcAddress {
    pub kind: AddressType,
    /// 24-bit specific address, hex (ICAO address for aircraft).
    pub addr: String,
    /// Status bit: dest octet 1 carries A/G (transmitter on ground),
    /// src octet 5 carries C/R (response).
    pub status_bit: bool,
}

fn parse_address(octets: &[u8]) -> AvlcAddress {
    // Spec bit k (1-based, first transmitted) = our bit k-1.
    // Octet 1: bits 8..3 = da22..da27, bit 2 = status (A/G or C/R),
    // bit 1 = ext. Octets 2-4: bits 8..2 = da15..da21 / da8..da14 /
    // da1..da7, bit 1 = ext.
    let mut da = [0u8; 28]; // 1-based da1..da27
    let groups = [(0usize, 22u32, 6u32), (1, 15, 7), (2, 8, 7), (3, 1, 7)];
    for &(oct, base, count) in &groups {
        let b = octets[oct];
        for k in 0..count {
            // bit (8-k) of the octet = da(base+k)
            da[(base + k) as usize] = (b >> (7 - k)) & 1;
        }
    }
    let status_bit = (octets[0] >> 1) & 1 == 1;
    let type_bits = (da[27] << 2) | (da[26] << 1) | da[25];
    let kind = match type_bits {
        0b001 => AddressType::Aircraft,
        0b100 => AddressType::GroundIcao,
        0b101 => AddressType::GroundDelegated,
        0b111 => AddressType::AllStations,
        _ => AddressType::Reserved,
    };
    let mut addr: u32 = 0;
    for k in (1..=24).rev() {
        addr = (addr << 1) | da[k] as u32;
    }
    AvlcAddress { kind, addr: format!("{addr:06X}"), status_bit }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Info { ns: u8, nr: u8, poll: bool },
    Supervisory { kind: &'static str, nr: u8, poll: bool },
    Unnumbered { kind: &'static str, poll: bool },
}

fn parse_control(c: u8) -> Control {
    if c & 1 == 0 {
        Control::Info { ns: (c >> 1) & 7, poll: (c >> 4) & 1 == 1, nr: (c >> 5) & 7 }
    } else if c & 3 == 1 {
        let kind = match (c >> 2) & 3 {
            0 => "RR",
            1 => "RNR",
            2 => "REJ",
            _ => "SREJ",
        };
        Control::Supervisory { kind, poll: (c >> 4) & 1 == 1, nr: (c >> 5) & 7 }
    } else {
        let m = (c & 0xEF) | 0; // mask P/F (bit 5)
        let kind = match m {
            0x03 => "UI",
            0x0F => "DM",
            0x43 => "DISC",
            0x63 => "UA",
            0x87 => "FRMR",
            0xAF => "XID",
            0xE3 => "TEST",
            _ => "U?",
        };
        Control::Unnumbered { kind, poll: (c >> 4) & 1 == 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "payload", rename_all = "snake_case")]
pub enum Payload {
    /// ACARS over AVLC (information field led by 0xFF).
    Acars,
    /// ATN: CLNP (0x81), ES-IS (0x82), IDRP (0x83).
    Atn { ipi: u8 },
    Xid,
    Empty,
    Other { first: u8 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AvlcFrame {
    pub dst: AvlcAddress,
    pub src: AvlcAddress,
    pub control: Control,
    pub payload: Payload,
    /// Information field (after the control octet, FCS stripped).
    pub info: Vec<u8>,
    /// Whole frame octets (addresses..FCS) for raw preservation.
    pub raw: Vec<u8>,
}

/// One XID parameter (ISO 8885 group structure; VDL-specific parameter
/// names per ICAO Doc 9776).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XidParam {
    /// Group identifier (0x80 = ISO 8885 general, 0xF0 = VDL private).
    pub group: u8,
    pub id: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'static str>,
    pub value_hex: String,
    /// Printable interpretation where the value is plain text (e.g. the
    /// destination airport parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// VDL private parameter set names (ICAO Doc 9776 Table 5-3 area).
fn vdl_param_name(id: u8) -> Option<&'static str> {
    Some(match id {
        0x00 => "parameter-set-id",
        0x01 => "connection-management",
        0x02 => "signal-quality",
        0x03 => "xid-sequencing",
        0x04 => "avlc-options",
        0x05 => "expedited-sn-connection",
        0x06 => "lcr-cause",
        0x40 => "modulation-support",
        0x41 => "acceptable-alternate-ground-stations",
        0x42 => "destination-airport",
        0x43 => "aircraft-position",
        0x44 => "autotune-frequency",
        0x45 => "replacement-ground-stations",
        0x46 => "timer-t4",
        0x47 => "mac-persistence",
        0x48 => "counter-m1",
        0x49 => "timer-tm2",
        _ => return None,
    })
}

/// Parse an XID information field: FI octet, then groups of
/// `GI | GL(2, big endian) | params{PI, PL, PV}` (ISO 8885).
pub fn parse_xid(info: &[u8]) -> Option<Vec<XidParam>> {
    if info.is_empty() {
        return None;
    }
    let mut params = Vec::new();
    let mut pos = 1; // skip the format identifier octet
    while pos + 3 <= info.len() {
        let group = info[pos];
        let glen = u16::from_be_bytes([info[pos + 1], info[pos + 2]]) as usize;
        pos += 3;
        let end = (pos + glen).min(info.len());
        while pos + 2 <= end {
            let id = info[pos];
            let plen = info[pos + 1] as usize;
            pos += 2;
            if pos + plen > end {
                return None; // malformed; don't emit half-parsed garbage
            }
            let value = &info[pos..pos + plen];
            pos += plen;
            let printable = value.len() >= 2
                && value.iter().all(|&b| (0x20..0x7F).contains(&b));
            params.push(XidParam {
                group,
                id,
                name: if group == 0xF0 { vdl_param_name(id) } else { None },
                value_hex: value.iter().map(|b| format!("{b:02x}")).collect(),
                text: printable.then(|| String::from_utf8_lossy(value).into_owned()),
            });
        }
        pos = end;
    }
    if params.is_empty() { None } else { Some(params) }
}

/// Scan a descrambled, RS-corrected bit stream for AVLC frames.
pub fn scan(bits: &[u8]) -> Vec<AvlcFrame> {
    let mut frames = Vec::new();
    let mut shift: u8 = 0;
    let mut collecting = false;
    let mut ones = 0u32;
    let mut buf: Vec<u8> = Vec::new();

    let close = |buf: &[u8], frames: &mut Vec<AvlcFrame>| {
        if buf.len() < MIN_FRAME_OCTETS * 8 || buf.len() % 8 != 0 {
            return;
        }
        let octets: Vec<u8> = buf
            .chunks_exact(8)
            .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << i)))
            .collect();
        if octets.len() > MAX_FRAME_OCTETS {
            return;
        }
        // FCS: accept either trailing octet order (free-spec ambiguity;
        // see PROVENANCE.md).
        let n = octets.len();
        let fcs = HDLC_FCS.checksum(&octets[..n - 2]);
        let le = u16::from_le_bytes([octets[n - 2], octets[n - 1]]);
        let be = u16::from_be_bytes([octets[n - 2], octets[n - 1]]);
        if fcs != le && fcs != be {
            return;
        }
        let dst = parse_address(&octets[0..4]);
        let src = parse_address(&octets[4..8]);
        let control = parse_control(octets[8]);
        let info = octets[9..n - 2].to_vec();
        let payload = match (&control, info.first()) {
            (Control::Unnumbered { kind: "XID", .. }, _) => Payload::Xid,
            (_, Some(0xFF)) => Payload::Acars,
            (_, Some(&ipi @ (0x81 | 0x82 | 0x83))) => Payload::Atn { ipi },
            (_, Some(&first)) => Payload::Other { first },
            (_, None) => Payload::Empty,
        };
        frames.push(AvlcFrame { dst, src, control, payload, info, raw: octets });
    };

    for &bit in bits {
        shift = (shift >> 1) | (bit << 7);
        if !collecting {
            if shift == FLAG {
                collecting = true;
                buf.clear();
                ones = 0;
            }
            continue;
        }
        if bit == 1 {
            ones += 1;
            if ones > 6 {
                collecting = false;
                continue;
            }
            buf.push(1);
        } else if ones == 5 {
            ones = 0; // stuffed zero
        } else if ones == 6 {
            // closing flag; buf ends with its leading 0111111
            let len = buf.len().saturating_sub(7);
            close(&buf[..len], &mut frames);
            buf.clear();
            ones = 0;
        } else {
            buf.push(0);
            ones = 0;
            if buf.len() > MAX_FRAME_OCTETS * 8 {
                collecting = false;
            }
        }
    }
    frames
}

/// Build the bit stream for one or more frames (testing/modulation):
/// flags, stuffing, FCS (little-endian octet order).
pub fn build(frames: &[Vec<u8>]) -> Vec<u8> {
    let flag = [0u8, 1, 1, 1, 1, 1, 1, 0];
    let mut bits: Vec<u8> = Vec::new();
    bits.extend(flag);
    for frame in frames {
        let mut octets = frame.clone();
        let fcs = HDLC_FCS.checksum(&octets);
        octets.extend(fcs.to_le_bytes());
        let mut ones = 0;
        for &o in &octets {
            for i in 0..8 {
                let b = (o >> i) & 1;
                bits.push(b);
                if b == 1 {
                    ones += 1;
                    if ones == 5 {
                        bits.push(0);
                        ones = 0;
                    }
                } else {
                    ones = 0;
                }
            }
        }
        bits.extend(flag);
    }
    bits
}

/// Encode an address field (testing): `specific` 24-bit, with type bits.
pub fn encode_address(kind: AddressType, specific: u32, status_bit: bool, last: bool) -> [u8; 4] {
    let type_bits: u8 = match kind {
        AddressType::Aircraft => 0b001,
        AddressType::GroundIcao => 0b100,
        AddressType::GroundDelegated => 0b101,
        AddressType::AllStations => 0b111,
        AddressType::Reserved => 0b000,
    };
    let mut da = [0u8; 28];
    for k in 1..=24 {
        da[k] = ((specific >> (k - 1)) & 1) as u8;
    }
    da[25] = type_bits & 1;
    da[26] = (type_bits >> 1) & 1;
    da[27] = (type_bits >> 2) & 1;
    let mut out = [0u8; 4];
    let groups = [(0usize, 22u32, 6u32), (1, 15, 7), (2, 8, 7), (3, 1, 7)];
    for &(oct, base, count) in &groups {
        let mut b = 0u8;
        for k in 0..count {
            b |= da[(base + k) as usize] << (7 - k);
        }
        out[oct] = b;
    }
    if status_bit {
        out[0] |= 0b10;
    }
    if last {
        out[3] |= 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_frame() -> Vec<u8> {
        // dst: aircraft A6F123 (A/G=0), src: ground 2C0A55 (C/R=1),
        // control UI (0x03), info = 0xFF + payload.
        let mut f = Vec::new();
        f.extend(encode_address(AddressType::Aircraft, 0xA6F123, false, false));
        f.extend(encode_address(AddressType::GroundIcao, 0x2C0A55, true, true));
        f.push(0x03);
        f.push(0xFF);
        f.extend(b"PAYLOAD");
        f
    }

    #[test]
    fn roundtrip_frame() {
        let bits = build(&[test_frame()]);
        let frames = scan(&bits);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert_eq!(f.dst.kind, AddressType::Aircraft);
        assert_eq!(f.dst.addr, "A6F123");
        assert!(!f.dst.status_bit);
        assert_eq!(f.src.kind, AddressType::GroundIcao);
        assert_eq!(f.src.addr, "2C0A55");
        assert!(f.src.status_bit);
        assert_eq!(f.control, Control::Unnumbered { kind: "UI", poll: false });
        assert_eq!(f.payload, Payload::Acars);
        assert_eq!(&f.info[1..], b"PAYLOAD");
    }

    #[test]
    fn two_frames_one_stream() {
        let mut f2 = test_frame();
        f2[8] = 0x01; // RR
        let bits = build(&[test_frame(), f2]);
        let frames = scan(&bits);
        assert_eq!(frames.len(), 2);
        assert!(matches!(frames[1].control, Control::Supervisory { kind: "RR", .. }));
    }

    #[test]
    fn bad_fcs_rejected() {
        let mut bits = build(&[test_frame()]);
        // Flip a payload bit mid-frame (avoid creating a flag/abort).
        bits[8 * 12 + 2] ^= 1;
        assert!(scan(&bits).is_empty());
    }
}

#[cfg(test)]
mod body_tests {
    use super::*;

    #[test]
    fn off_air_s_frame_parses_as_rr() {
        // The exact 11-byte frame a live Airspy session surfaced as
        // "undecoded": dst/src addresses + control 0xA1 + FCS.
        let octets = [0x14u8, 0x22, 0xcc, 0x54, 0xb2, 0x0c, 0x42, 0xb5, 0xa1];
        let bits = build(&[octets.to_vec()]);
        let frames = scan(&bits);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert_eq!(f.control, Control::Supervisory { kind: "RR", nr: 5, poll: false });
        assert_eq!(f.payload, Payload::Empty);
        assert!(f.info.is_empty());
    }

    #[test]
    fn xid_parameters_decode_with_names_and_text() {
        // FI 0x82, VDL private group 0xF0, two params:
        // parameter-set-id = "V", destination-airport = "KSMF".
        let info = [
            0x82, 0xF0, 0x00, 0x09, 0x00, 0x01, b'V', 0x42, 0x04, b'K', b'S', b'M', b'F',
        ];
        let params = parse_xid(&info).expect("params");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, Some("parameter-set-id"));
        assert_eq!(params[1].name, Some("destination-airport"));
        assert_eq!(params[1].text.as_deref(), Some("KSMF"));
        assert_eq!(params[1].value_hex, "4b534d46");
    }

    #[test]
    fn malformed_xid_returns_none() {
        // Param claims more bytes than the group holds.
        let info = [0x82, 0xF0, 0x00, 0x04, 0x42, 0x40];
        assert!(parse_xid(&info).is_none());
    }
}
