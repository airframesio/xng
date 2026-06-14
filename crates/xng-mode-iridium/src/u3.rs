//! U3 (LCW frame type 3, "mission-control in-band signalling") inner
//! decode (iridium-toolkit `IridiumLCW3Message`). The 312-bit payload is
//! Reed-Solomon protected; try the GF(256) byte code first (→ I38, with a
//! 16-bit checksum and an odd byte), then the GF(64) 6-bit code (→ I36,
//! whose first symbol selects a numeric sub-format). Falls back to a raw
//! dump (IU3). The RS codes are the same ones the voice path uses.

use crate::iip::checksum_16;
use crate::voice::{rs6_correct, vod_correct, RS6_N};
use serde_json::{json, Value};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode the post-LCW payload bits of an ft==3 (U3) burst.
pub fn parse_u3(payload_bits: &[u8]) -> Value {
    if payload_bits.len() < 312 {
        return json!({ "u3_type": "IU3" });
    }
    let p = &payload_bits[..312];

    // I38: GF(256) RS over the 39-byte view (31 data + 8 checks).
    let mut bytes = [0u8; 39];
    for (i, c) in p.chunks(8).take(39).enumerate() {
        bytes[i] = c.iter().fold(0u8, |v, &b| (v << 1) | b);
    }
    if let Some(rs8m) = vod_correct(&bytes) {
        // rs8m: 31 bytes. csum over [0:28]+[29:31]; odd byte at [28].
        let cs_ok = checksum_16(&rs8m) == 0;
        let oddbyte = rs8m[28];
        // On a clean checksum the toolkit drops the trailing 3 bytes
        // (oddbyte + 2 csum) and the trailing zeros.
        let data: &[u8] = if cs_ok {
            let end = rs8m[..28].iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
            &rs8m[..end]
        } else {
            &rs8m[..]
        };
        return json!({
            "u3_type": "I38",
            "cs_ok": cs_ok,
            "odd_byte": oddbyte,
            "data_hex": hex(data),
        });
    }

    // I36: GF(64) RS(52,42) over the 6-bit view.
    let mut cw6 = [0u8; RS6_N];
    for (i, c) in p.chunks(6).take(RS6_N).enumerate() {
        cw6[i] = c.iter().fold(0u8, |v, &b| (v << 1) | b);
    }
    if let Ok(fixed) = rs6_correct(&mut cw6) {
        let rs6m = &cw6[..42];
        let subformat = rs6m[0];
        // Bit string of symbols 1..42 (each 6 bits, MSB-first).
        let v: Vec<u8> = rs6m[1..]
            .iter()
            .flat_map(|&s| (0..6).rev().map(move |k| (s >> k) & 1))
            .collect();
        let mut out = json!({
            "u3_type": "I36",
            "subformat": subformat,
            "rs_corrected": fixed,
        });
        // Numeric sub-formats carry 24-bit groups after a 2-bit lead.
        let group = |bits: &[u8], n: usize| -> Vec<u64> {
            bits.chunks(n)
                .filter(|c| c.len() == n)
                .map(|c| c.iter().fold(0u64, |a, &b| (a << 1) | b as u64))
                .collect()
        };
        match subformat {
            6 => {
                let mut nums = group(&v[2..], 24);
                while nums.last() == Some(&0) {
                    nums.pop();
                }
                out["numbers"] = json!(nums);
            }
            32 | 34 => {
                let body = &v[2..v.len().saturating_sub(4)];
                let mut nums = group(body, 24);
                let tail = group(&v[v.len().saturating_sub(4)..], 4);
                if let Some(&t) = tail.first() {
                    if t != 0 {
                        nums.push(t);
                    }
                }
                while nums.last() == Some(&0x7ffff) {
                    nums.pop();
                }
                out["numbers"] = json!(nums);
            }
            _ => {
                out["data_hex"] = json!(hex(rs6m));
            }
        }
        return out;
    }

    json!({ "u3_type": "IU3" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_payload_is_iu3() {
        assert_eq!(parse_u3(&[0u8; 100])["u3_type"], "IU3");
    }
}
