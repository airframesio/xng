//! Iridium IP-channel (LCW ft=1) frame parsing, following
//! iridium-toolkit's `IridiumIPMessage` ladder (oracle-validated):
//!
//! 1. CRC24 over the bit-reversed payload passes → **IIP**: a small
//!    ARQ frame (type, sequence, ack, header checksum, 32 data bytes)
//! 2. Otherwise the straight byte view is tried as the shortened
//!    GF(256) RS codeword (31 data + 8 checks + 8 erased):
//!    one's-complement sum of the message = 0 → **IIR**, else → **IIQ**
//!    (3 flag bits + 13-bit counter + 29 data bytes)
//! 3. Nothing fits → **IIU** (unknown).

use crate::voice::iip_crc24;
use serde_json::{json, Value};
use xng_dsp::rs::ReedSolomon;

/// IIP frame types observed by the toolkit (header byte).
fn iip_type_name(t: u8) -> &'static str {
    match t {
        0x01 => "ack-idle",
        0x04 => "data",
        _ => "unknown",
    }
}

/// The toolkit's 16-bit one's-complement-style checksum over the
/// 31-byte RS message: sum of 14 LE u16 + 1 byte + 1 LE u16, carry
/// folded once, complemented.
pub(crate) fn checksum_16(msg: &[u8; 31]) -> u16 {
    let mut sum: u32 = 0;
    for w in msg[..28].chunks_exact(2) {
        sum += u16::from_le_bytes([w[0], w[1]]) as u32;
    }
    sum += msg[28] as u32;
    sum += u16::from_le_bytes([msg[29], msg[30]]) as u32;
    let folded = (sum & 0xFFFF) + (sum >> 16);
    (folded as u16) ^ 0xFFFF
}

fn rs8_fix(payload_f: &[u8; 39]) -> Option<[u8; 31]> {
    let rs = ReedSolomon::new(0x11d, 16, 0);
    let mut cw = [0u8; 255];
    cw[208..247].copy_from_slice(payload_f);
    let erasures: Vec<usize> = (247..255).collect();
    rs.correct(&mut cw, &erasures).ok()?;
    let mut out = [0u8; 31];
    out.copy_from_slice(&cw[208..239]);
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a CRC-valid IIP frame from the bit-reversed payload bytes
/// (also the body of a VDA frame on the voice channel).
pub fn parse_iip_frame(payload_r: &[u8]) -> Value {
    let hdr = payload_r[0];
    let seq = payload_r[1];
    let ack = payload_r[2];
    let cs = payload_r[3];
    // Header checksum: the four bytes sum to 0 mod 255 (the toolkit's
    // subtract-to-255 loop).
    let sum = hdr as u32 + seq as u32 + ack as u32 + cs as u32;
    let cs_ok = sum > 0 && sum % 255 == 0;
    let mut out = json!({
        "ip_type": iip_type_name(hdr),
        "ip_type_code": hdr,
        "seq": seq,
        "ack": ack,
        "header_cs_ok": cs_ok,
    });
    let data = &payload_r[4..payload_r.len().saturating_sub(3).max(4)];
    match hdr {
        0x04 if !data.is_empty() => {
            // First byte is the payload length; the rest should be
            // zero-fill.
            let len = (data[0] as usize).min(data.len() - 1);
            out["data_hex"] = json!(hex(&data[1..1 + len]));
            let text: String = data[1..1 + len]
                .iter()
                .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                .collect();
            out["data_ascii"] = json!(text);
        }
        0x01 => {
            // ACK/IDLE: trailing zeros stripped.
            let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
            if end > 0 {
                out["data_hex"] = json!(hex(&data[..end]));
            }
        }
        _ => {
            out["data_hex"] = json!(hex(data));
        }
    }
    out
}

/// Classify an ft=1 (IP channel) payload: needs at least 312 bits.
pub fn parse_ip_payload(payload_bits: &[u8]) -> Option<Value> {
    if payload_bits.len() < 312 {
        return None;
    }
    let nbytes = payload_bits.len() / 8;
    let mut payload_f = Vec::with_capacity(nbytes);
    let mut payload_r = Vec::with_capacity(nbytes);
    for c in payload_bits.chunks(8).take(nbytes) {
        let f = c.iter().fold(0u8, |v, &b| (v << 1) | b);
        payload_f.push(f);
        payload_r.push(f.reverse_bits());
    }

    if iip_crc24(&payload_r) == 0 {
        let mut v = parse_iip_frame(&payload_r);
        v["ip_frame"] = json!("IIP");
        return Some(v);
    }

    if payload_f.len() >= 39 {
        let mut pf = [0u8; 39];
        pf.copy_from_slice(&payload_f[..39]);
        if let Some(msg) = rs8_fix(&pf) {
            if checksum_16(&msg) == 0 {
                return Some(json!({
                    "ip_frame": "IIR",
                    "data_hex": hex(&msg[..29]),
                }));
            }
            let val = u16::from_le_bytes([msg[0], msg[1]]);
            return Some(json!({
                "ip_frame": "IIQ",
                "flags": val & 7,
                "counter": val >> 3,
                "data_hex": hex(&msg[2..]),
            }));
        }
    }

    Some(json!({ "ip_frame": "IIU" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_of_hex(h: &str) -> Vec<u8> {
        h.as_bytes()
            .chunks(2)
            .flat_map(|c| {
                let b = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
                (0..8).rev().map(move |i| (b >> i) & 1)
            })
            .collect()
    }

    // Vectors generated with iridium-toolkit's own crcmod/rs code and
    // asserted against its classification ladder (see PR notes).
    const IIP_ACK: &str =
        "805488c300000000000000000000000000000000000000000000000000000000000000004fd4e0";
    const IIP_DATA: &str =
        "20e0c689a012a23232f200000000000000000000000000000000000000000000000000002d6aa5";
    const IIQ: &str =
        "1d09e7eee7615ef35f30e49b482e15cae75007201e12617b0feda7e1647796da2d5bcc9793d179";
    const IIR: &str =
        "ff022bea8ed02a82a175930f2337cd3794c52208006d6b1af0c0cbd625f2de9780ccb7e209fece";
    const IIU: &str =
        "658aac2c9faa07d13c447e33051eeef95a60e56143d6c43bcad76c008a9b0a6b5fc933154a6de2";

    #[test]
    fn iip_ack_frame() {
        let v = parse_ip_payload(&bits_of_hex(IIP_ACK)).unwrap();
        assert_eq!(v["ip_frame"], "IIP");
        assert_eq!(v["ip_type"], "ack-idle");
        assert_eq!(v["seq"], 42);
        assert_eq!(v["ack"], 17);
        assert_eq!(v["header_cs_ok"], true);
    }

    #[test]
    fn iip_data_frame_extracts_payload() {
        let v = parse_ip_payload(&bits_of_hex(IIP_DATA)).unwrap();
        assert_eq!(v["ip_frame"], "IIP");
        assert_eq!(v["ip_type"], "data");
        assert_eq!(v["seq"], 7);
        assert_eq!(v["ack"], 99);
        assert_eq!(v["header_cs_ok"], true);
        assert_eq!(v["data_ascii"], "HELLO");
    }

    #[test]
    fn iiq_frame() {
        let v = parse_ip_payload(&bits_of_hex(IIQ)).unwrap();
        assert_eq!(v["ip_frame"], "IIQ");
        assert_eq!(v["flags"], 5);
        assert_eq!(v["counter"], 0x123);
        assert_eq!(
            v["data_hex"],
            "e7eee7615ef35f30e49b482e15cae75007201e12617b0feda7e1647796"
        );
    }

    #[test]
    fn iir_frame() {
        let v = parse_ip_payload(&bits_of_hex(IIR)).unwrap();
        assert_eq!(v["ip_frame"], "IIR");
        assert_eq!(
            v["data_hex"],
            "ff022bea8ed02a82a175930f2337cd3794c52208006d6b1af0c0cbd625"
        );
    }

    #[test]
    fn iiu_fallback() {
        let v = parse_ip_payload(&bits_of_hex(IIU)).unwrap();
        assert_eq!(v["ip_frame"], "IIU");
    }
}
