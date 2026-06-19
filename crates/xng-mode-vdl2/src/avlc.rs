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
        let m = c & 0xEF; // mask the P/F bit (control bit 5)
        let kind = match m {
            0x03 => "UI",
            0x0F => "DM",
            0x43 => "DISC",
            0x63 => "UA",
            0x6F => "SABME",
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

/// Expanded FRMR (Frame Reject) information field — ISO/IEC 13239 §5.5.3.5,
/// basic (modulo-8) format: 3 octets carrying the rejected control field,
/// the receiver's V(S)/V(R) sequence state, and the W/X/Y/Z reject reason
/// flags.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrmrInfo {
    /// The control field of the frame that was rejected.
    pub rejected_control: u8,
    /// Decoded form of the rejected control field.
    pub rejected: Control,
    /// Send-state variable V(S) of the rejecting station.
    pub vs: u8,
    /// Receive-state variable V(R) of the rejecting station.
    pub vr: u8,
    /// C/R bit of the rejected frame (true = the rejected frame was a
    /// response).
    pub rejected_was_response: bool,
    /// W: control field undefined / not implemented.
    pub w_invalid_control: bool,
    /// X: info field present in a frame that may not carry one, or the
    /// rejected control field was invalid AND an info field was present.
    pub x_info_not_allowed: bool,
    /// Y: information field length exceeded the maximum (N1).
    pub y_info_too_long: bool,
    /// Z: N(R) sequence error (invalid receive count).
    pub z_invalid_nr: bool,
}

/// Decode a FRMR information field (basic, modulo-8: exactly 3 octets).
/// Returns None if the field is the wrong length.
pub fn parse_frmr(info: &[u8]) -> Option<FrmrInfo> {
    if info.len() != 3 {
        return None;
    }
    let rejected_control = info[0];
    // Octet 2 (transmitted LSB-first): bit1=0, bits2-4=V(S), bit5=C/R,
    // bits6-8=V(R). In a stored octet that is bit0=0, bits1-3=V(S),
    // bit4=C/R, bits5-7=V(R).
    let vs = (info[1] >> 1) & 0x07;
    let rejected_was_response = (info[1] >> 4) & 1 == 1;
    let vr = (info[1] >> 5) & 0x07;
    // Octet 3 (LSB-first): bit1=W, bit2=X, bit3=Y, bit4=Z. Stored octet
    // bit0=W, bit1=X, bit2=Y, bit3=Z.
    let w_invalid_control = info[2] & 1 == 1;
    let x_info_not_allowed = (info[2] >> 1) & 1 == 1;
    let y_info_too_long = (info[2] >> 2) & 1 == 1;
    let z_invalid_nr = (info[2] >> 3) & 1 == 1;
    Some(FrmrInfo {
        rejected_control,
        rejected: parse_control(rejected_control),
        vs,
        vr,
        rejected_was_response,
        w_invalid_control,
        x_info_not_allowed,
        y_info_too_long,
        z_invalid_nr,
    })
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
    /// Decoded scalar value for timer/counter parameters (big-endian int).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_int: Option<u32>,
    /// Decoded frequency in MHz (autotune-frequency parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freq_mhz: Option<f64>,
    /// Frequency-support-list entries: (ground station address, MHz).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub freq_support: Vec<FreqSupportEntry>,
}

/// One entry of the VDL2 frequency-support-list parameter (0xC0).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FreqSupportEntry {
    /// Ground station 24-bit address (hex).
    pub gs_addr: String,
    pub freq_mhz: f64,
}

/// XID group identifiers (ISO 8885): public (HDLC) and VDL-private.
pub const XID_GID_PUBLIC: u8 = 0x80;
pub const XID_GID_PRIVATE: u8 = 0xF0;

/// VDL private parameter-set names (ICAO Doc 9776 Table 5-3 / VDL2 SARPs),
/// cross-checked against dumpvdl2 `xid_vdl_params` (xid.c). The
/// previous table mis-numbered 0x40–0x49 entirely (e.g. 0x42 is Timer T4,
/// not Destination airport — the airport parameter is 0x83 in this group).
fn vdl_param_name(id: u8) -> Option<&'static str> {
    Some(match id {
        0x00 => "parameter-set-id",
        0x01 => "connection-management",
        0x02 => "signal-quality", // SQP
        0x03 => "xid-sequencing",
        0x04 => "avlc-specific-options",
        0x05 => "expedited-sn-connection",
        0x06 => "lcr-cause",
        0x40 => "autotune-frequency",
        0x41 => "replacement-ground-stations",
        0x42 => "timer-t4",
        0x43 => "mac-persistence",
        0x44 => "counter-m1",
        0x45 => "timer-tm2",
        0x46 => "timer-tg5",
        0x47 => "timer-t3min",
        0x48 => "ground-station-address-filter",
        0x49 => "broadcast-connection",
        0x81 => "modulation-support",
        0x82 => "alternate-ground-stations",
        0x83 => "destination-airport",
        0x84 => "aircraft-location",
        0xC0 => "frequency-support-list",
        0xC1 => "airport-coverage",
        0xC3 => "nearest-airport-id",
        0xC4 => "atn-router-nets",
        0xC5 => "system-mask",
        0xC6 => "timer-tg3",
        0xC7 => "timer-tg4",
        0xC8 => "ground-station-location",
        _ => return None,
    })
}

/// Public (ISO 8885 HDLC) parameter-set names, group 0x80; cross-checked
/// against dumpvdl2 `xid_pub_params` (xid.c).
fn pub_param_name(id: u8) -> Option<&'static str> {
    Some(match id {
        0x01 => "parameter-set-id",
        0x02 => "procedure-classes",
        0x03 => "hdlc-options",
        0x05 => "n1-downlink",
        0x06 => "n1-uplink",
        0x07 => "k-downlink",
        0x08 => "k-uplink",
        0x09 => "timer-t1-downlink",
        0x0A => "counter-n2",
        0x0B => "timer-t2",
        _ => return None,
    })
}

/// Decode the 2-octet VDL2 frequency field (autotune / freq-support-list
/// entries): top nibble = modulation-support bitfield, low 12 bits encode
/// the channel as `freq_khz = (raw + 10000) * 10`, rounded up to the next
/// 25 kHz step. Returns (MHz, modulation_bits). Matches dumpvdl2 parse_freq.
fn decode_vdl2_freq(buf: &[u8]) -> Option<(f64, u8)> {
    if buf.len() < 2 {
        return None;
    }
    let modulations = buf[0] >> 4;
    let raw = (u16::from_be_bytes([buf[0], buf[1]]) & 0x0FFF) as u32;
    let mut freq_khz = (raw + 10_000) * 10;
    if freq_khz % 25 != 0 {
        freq_khz += 25 - freq_khz % 25;
    }
    Some((freq_khz as f64 / 1000.0, modulations))
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
            let name = match group {
                XID_GID_PRIVATE => vdl_param_name(id),
                XID_GID_PUBLIC => pub_param_name(id),
                _ => None,
            };
            let printable = value.len() >= 2
                && value.iter().all(|&b| (0x20..0x7F).contains(&b));
            // Ground-station-list parameters carry 4-octet AVLC addresses:
            // replacement (0x41), GS-address-filter (0x48),
            // alternate (0x82), and system-mask (0xC5).
            let is_addr_list = group == XID_GID_PRIVATE
                && matches!(id, 0x41 | 0x48 | 0x82 | 0xC5)
                && !value.is_empty()
                && value.len() % 4 == 0;
            let text = if is_addr_list {
                Some(
                    value
                        .chunks_exact(4)
                        .map(|c| parse_address(c).addr)
                        .collect::<Vec<_>>()
                        .join(","),
                )
            } else {
                printable.then(|| String::from_utf8_lossy(value).into_owned())
            };
            // Autotune frequency (0x40 in the VDL group) → MHz.
            let freq_mhz = if group == XID_GID_PRIVATE && id == 0x40 {
                decode_vdl2_freq(value).map(|(mhz, _)| mhz)
            } else {
                None
            };
            // Frequency-support-list (0xC0): 6-octet entries, freq(2)+gs(4).
            let freq_support = if group == XID_GID_PRIVATE
                && id == 0xC0
                && !value.is_empty()
                && value.len() % 6 == 0
            {
                value
                    .chunks_exact(6)
                    .filter_map(|c| {
                        let (mhz, _) = decode_vdl2_freq(&c[0..2])?;
                        Some(FreqSupportEntry {
                            gs_addr: parse_address(&c[2..6]).addr,
                            freq_mhz: mhz,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            // Timer / counter parameters carry a big-endian integer
            // (1–4 octets); decode the scalar in addition to the raw hex.
            let value_int = match name {
                Some(n)
                    if (n.starts_with("timer-") || n.starts_with("counter-"))
                        && (1..=4).contains(&value.len()) =>
                {
                    let mut v: u32 = 0;
                    for &b in value {
                        v = (v << 8) | b as u32;
                    }
                    Some(v)
                }
                _ => None,
            };
            params.push(XidParam {
                group,
                id,
                name,
                value_hex: value.iter().map(|b| format!("{b:02x}")).collect(),
                text,
                value_int,
                freq_mhz,
                freq_support,
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
        // FCS: HDLC/X.25 transmits the 16-bit FCS low octet first
        // (little-endian on the wire) — ISO/IEC 13239 §4.4, the same
        // order dumpvdl2's GOOD_FCS residue check implies and the order
        // build() emits. Pinning this single order drops a false-accept
        // path (the byte-swapped variant accepted ~1 in 65536 bad frames).
        let n = octets.len();
        let fcs = HDLC_FCS.checksum(&octets[..n - 2]);
        let le = u16::from_le_bytes([octets[n - 2], octets[n - 1]]);
        if fcs != le {
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

    #[test]
    fn sabme_u_command_recognized() {
        // SABME control octet 0x6F (ISO/IEC 13239 §5.5.3.3); 0x7F with
        // the poll bit set must decode identically with poll=true.
        assert_eq!(
            parse_control(0x6F),
            Control::Unnumbered { kind: "SABME", poll: false }
        );
        assert_eq!(
            parse_control(0x7F),
            Control::Unnumbered { kind: "SABME", poll: true }
        );
    }

    #[test]
    fn byte_swapped_fcs_now_rejected() {
        // With the FCS octet order pinned to little-endian, a frame whose
        // two FCS octets are swapped (the old big-endian accept path) must
        // be rejected — unless the FCS happens to be a palindrome.
        let mut octets = test_frame();
        let fcs = HDLC_FCS.checksum(&octets);
        let [lo, hi] = fcs.to_le_bytes();
        if lo == hi {
            return; // palindromic FCS — swap is a no-op; nothing to test
        }
        // Append the FCS in the wrong (big-endian) order, then bit-stuff
        // and frame it the same way build() does, bypassing build()'s own
        // (correct) FCS append.
        octets.push(hi);
        octets.push(lo);
        let flag = [0u8, 1, 1, 1, 1, 1, 1, 0];
        let mut bits: Vec<u8> = Vec::new();
        bits.extend(flag);
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
        assert!(scan(&bits).is_empty(), "byte-swapped FCS must not be accepted");
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
        // parameter-set-id (0x00) = "V", destination-airport (0x83) = "KSMF".
        // (Destination-airport is parameter 0x83 in the VDL group, per
        // dumpvdl2 xid_vdl_params — NOT 0x42, which is Timer T4.)
        let info = [
            0x82, 0xF0, 0x00, 0x09, 0x00, 0x01, b'V', 0x83, 0x04, b'K', b'S', b'M', b'F',
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

#[cfg(test)]
mod frmr_tests {
    use super::*;

    #[test]
    fn frmr_info_field_expands() {
        // ISO/IEC 13239 §5.5.3.5 basic format, 3 octets:
        //   octet 1: rejected control field = 0x64 (I-frame N(S)=2 N(R)=3)
        //   octet 2: V(S)=4, C/R=1 (response), V(R)=5
        //            = (4<<1) | (1<<4) | (5<<5) = 0xB8
        //   octet 3: Z flag set (N(R) sequence error) = bit3 = 0x08
        let info = [0x64, 0xB8, 0x08];
        let frmr = parse_frmr(&info).expect("frmr decodes");
        assert_eq!(frmr.rejected_control, 0x64);
        assert_eq!(frmr.rejected, Control::Info { ns: 2, nr: 3, poll: false });
        assert_eq!(frmr.vs, 4);
        assert_eq!(frmr.vr, 5);
        assert!(frmr.rejected_was_response);
        assert!(!frmr.w_invalid_control);
        assert!(!frmr.x_info_not_allowed);
        assert!(!frmr.y_info_too_long);
        assert!(frmr.z_invalid_nr);
    }

    #[test]
    fn frmr_w_and_y_flags() {
        // octet 3 = W (bit1) | Y (bit3) = 0x01 | 0x04 = 0x05.
        let frmr = parse_frmr(&[0x6F, 0x00, 0x05]).expect("frmr decodes");
        // 0x6F = a rejected SABME U-command.
        assert_eq!(frmr.rejected, Control::Unnumbered { kind: "SABME", poll: false });
        assert!(frmr.w_invalid_control);
        assert!(!frmr.x_info_not_allowed);
        assert!(frmr.y_info_too_long);
        assert!(!frmr.z_invalid_nr);
        assert_eq!(frmr.vs, 0);
        assert_eq!(frmr.vr, 0);
    }

    #[test]
    fn frmr_wrong_length_rejected() {
        assert!(parse_frmr(&[0x64, 0xB8]).is_none());
        assert!(parse_frmr(&[0x64, 0xB8, 0x08, 0x00]).is_none());
    }
}

#[cfg(test)]
mod xid_gs_tests {
    use super::*;

    #[test]
    fn ground_station_list_param_decodes_addresses() {
        let gs1 = encode_address(AddressType::GroundIcao, 0x2C0A55, false, false);
        let gs2 = encode_address(AddressType::GroundIcao, 0x2D4917, false, true);
        let mut info = vec![0x82, 0xF0, 0x00, (2 + 8) as u8, 0x41, 8];
        info.extend_from_slice(&gs1);
        info.extend_from_slice(&gs2);
        let params = parse_xid(&info).unwrap();
        // 0x41 in the VDL group is replacement-ground-stations.
        assert_eq!(params[0].name, Some("replacement-ground-stations"));
        assert_eq!(params[0].text.as_deref(), Some("2C0A55,2D4917"));
    }

    #[test]
    fn gs_address_filter_and_system_mask_decode_addresses() {
        let gs = encode_address(AddressType::GroundIcao, 0x2C0A55, false, true);
        // GS-address-filter (0x48) and system-mask (0xC5) are address lists.
        for id in [0x48u8, 0xC5] {
            let mut info = vec![0x82, 0xF0, 0x00, 6, id, 4];
            info.extend_from_slice(&gs);
            let params = parse_xid(&info).unwrap();
            assert_eq!(params[0].id, id);
            assert_eq!(params[0].text.as_deref(), Some("2C0A55"));
        }
    }
}

#[cfg(test)]
mod xid_vdl2_3_tests {
    use super::*;

    /// Build an XID info field with a single VDL-private parameter.
    fn vdl_one(id: u8, value: &[u8]) -> Vec<u8> {
        let glen = 2 + value.len();
        let mut info = vec![0x82, 0xF0, (glen >> 8) as u8, glen as u8, id, value.len() as u8];
        info.extend_from_slice(value);
        info
    }

    #[test]
    fn new_vdl_param_names() {
        // The parameter IDs added in VDL2-3, verified against dumpvdl2
        // xid_vdl_params.
        let cases: &[(u8, &str)] = &[
            (0x46, "timer-tg5"),
            (0x47, "timer-t3min"),
            (0x48, "ground-station-address-filter"),
            (0x49, "broadcast-connection"),
            (0xC0, "frequency-support-list"),
            (0xC1, "airport-coverage"),
            (0xC3, "nearest-airport-id"),
            (0xC4, "atn-router-nets"),
            (0xC5, "system-mask"),
            (0xC6, "timer-tg3"),
            (0xC7, "timer-tg4"),
        ];
        for &(id, name) in cases {
            assert_eq!(vdl_param_name(id), Some(name), "id {id:#04x}");
        }
    }

    #[test]
    fn public_group_params_named() {
        // ISO 8885 HDLC parameter set, group 0x80.
        let info = [
            0x82, 0x80, 0x00, 0x06, 0x09, 0x02, 0x00, 0x64, 0x0A, 0x00,
        ];
        // 0x09 = timer-t1-downlink (len 2, value 0x0064 = 100),
        // 0x0A = counter-n2 (len 0).
        let params = parse_xid(&info).unwrap();
        assert_eq!(params[0].group, XID_GID_PUBLIC);
        assert_eq!(params[0].name, Some("timer-t1-downlink"));
        assert_eq!(params[0].value_int, Some(100));
        assert_eq!(params[1].name, Some("counter-n2"));
    }

    #[test]
    fn autotune_frequency_decodes_to_mhz() {
        // raw=3697 (low 12 bits), top nibble (modulation) = 0 →
        // freq_khz=(3697+10000)*10=136970, rounded up to 136975 → 136.975.
        let params = parse_xid(&vdl_one(0x40, &[0x0E, 0x71])).unwrap();
        assert_eq!(params[0].name, Some("autotune-frequency"));
        assert_eq!(params[0].freq_mhz, Some(136.975));
    }

    #[test]
    fn frequency_support_list_decodes_entries() {
        // One entry: freq(2)=0x0E71 (136.975) + gs addr.
        let gs = encode_address(AddressType::GroundIcao, 0x2C0A55, false, true);
        let mut value = vec![0x0E, 0x71];
        value.extend_from_slice(&gs);
        let params = parse_xid(&vdl_one(0xC0, &value)).unwrap();
        assert_eq!(params[0].name, Some("frequency-support-list"));
        assert_eq!(params[0].freq_support.len(), 1);
        assert_eq!(params[0].freq_support[0].gs_addr, "2C0A55");
        assert_eq!(params[0].freq_support[0].freq_mhz, 136.975);
    }

    #[test]
    fn timers_decode_to_int() {
        // Timer TG5 (0x46), big-endian 2-octet value 0x012C = 300.
        let params = parse_xid(&vdl_one(0x46, &[0x01, 0x2C])).unwrap();
        assert_eq!(params[0].name, Some("timer-tg5"));
        assert_eq!(params[0].value_int, Some(300));
        // Counter M1 (0x44), single octet 0x05.
        let params = parse_xid(&vdl_one(0x44, &[0x05])).unwrap();
        assert_eq!(params[0].name, Some("counter-m1"));
        assert_eq!(params[0].value_int, Some(5));
    }

    #[test]
    fn freq_decode_known_channels() {
        // Common VDL2 channels; raw = (MHz*1000/10 - 10000), low 12 bits.
        assert_eq!(decode_vdl2_freq(&[0x0E, 0x70]).unwrap().0, 136.975);
        assert_eq!(decode_vdl2_freq(&[0x0E, 0x66]).unwrap().0, 136.875);
        assert_eq!(decode_vdl2_freq(&[0x0E, 0x57]).unwrap().0, 136.725);
        assert_eq!(decode_vdl2_freq(&[0x0E, 0x4F]).unwrap().0, 136.650);
        // The modulation bits in the top nibble must not affect the freq.
        assert_eq!(decode_vdl2_freq(&[0xCE, 0x70]).unwrap(), (136.975, 0xC));
    }
}
