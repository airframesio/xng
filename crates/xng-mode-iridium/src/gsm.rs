//! Iridium duplex GSM signalling decode over the reassembled IDA channel
//! (ported from iridium-toolkit `reassembler.py ReassembleIDAPP`). Iridium
//! tunnels a GSM-derived call-control / mobility-management / SMS protocol;
//! a reassembled IDA packet whose first byte is a CC/MM/SMS transaction
//! identifier carries one of these messages. SBD (0x76 / 0x0600) is handled
//! separately by the ACARS path.

use serde_json::{json, Value};

/// Major protocol label for a transaction-identifier high byte.
fn major(tmaj: u8) -> Option<&'static str> {
    Some(match tmaj {
        0x03 => "CC",
        0x83 => "CC(dest)",
        0x05 => "MM",
        0x06 => "06",
        0x08 => "08",
        0x09 => "SMS",
        0x89 => "SMS(dest)",
        _ => return None,
    })
}

/// Message label for a 16-bit transaction-identifier (high byte masked).
fn minor(tmin: u16) -> Option<&'static str> {
    Some(match tmin {
        0x0301 => "Alerting",
        0x0302 => "Call Proceeding",
        0x0303 => "Progress",
        0x0305 => "Setup",
        0x030f => "Connect Acknowledge",
        0x0325 => "Disconnect",
        0x032a => "Release Complete",
        0x032d => "Release",
        0x0502 => "Location Updating Accept",
        0x0504 => "Location Updating Reject",
        0x0508 => "Location Updating Request",
        0x0512 => "Authentication Request",
        0x0514 => "Authentication Response",
        0x0518 => "Identity request",
        0x0519 => "Identity response",
        0x051a => "TMSI Reallocation Command",
        0x0600 => "Register/SBD:uplink",
        0x0901 => "CP-DATA",
        0x0904 => "CP-ACK",
        0x0910 => "CP-ERROR",
        _ => return None,
    })
}

/// GSM Mobile Identity IE → (identity object, bytes consumed). Returns
/// None (PARSE_FAIL) on a malformed IE.
fn p_mi_iei(d: &[u8]) -> Option<(Value, usize)> {
    if d.len() < 2 {
        return None;
    }
    let iei_len = d[0] as usize;
    let iei_dig = d[1] >> 4;
    let iei_odd = (d[1] >> 3) & 1;
    let iei_typ = d[1] & 7;
    match iei_typ {
        1 | 2 => {
            // IMSI / IMEI: 15 digits, odd indicator set, length 8.
            if iei_odd == 1 && iei_len == 8 && d.len() >= 9 {
                let mut s = format!("{iei_dig:x}");
                for &b in &d[2..9] {
                    s.push_str(&format!("{:x}{:x}", b & 0xf, b >> 4));
                }
                let label = if iei_typ == 1 { "imsi" } else { "imei" };
                Some((json!({ "type": label, "value": s }), 9))
            } else {
                None
            }
        }
        4 => {
            // TMSI: 4 raw bytes.
            if iei_odd == 0 && iei_len == 5 && iei_dig == 0xf && d.len() >= 6 {
                Some((
                    json!({ "type": "tmsi", "value": format!("{:02x}{:02x}{:02x}{:02x}", d[2], d[3], d[4], d[5]) }),
                    6,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Location Area Identification IE → ({mcc,mnc,lac}, consumed).
fn p_lai(d: &[u8]) -> Option<(Value, usize)> {
    if d.len() < 5 || d[1] >> 4 != 0xf {
        return None;
    }
    let mcc = format!("{}{}{}", d[0] & 0xf, d[0] >> 4, d[1] & 0xf);
    let mnc = format!("{}{}", d[2] >> 4, d[2] & 0xf);
    let lac = format!("{:02x}{:02x}", d[3], d[4]);
    Some((json!({ "mcc": mcc, "mnc": mnc, "lac": lac }), 5))
}

/// Disconnect-cause IE → ({location,cause,cause_text}, consumed).
fn p_disc(d: &[u8]) -> Option<(Value, usize)> {
    if d.len() < 3 || d[0] < 2 || d[1] >> 4 != 0xe {
        return None;
    }
    let net = d[1] & 0xf;
    let cause = d[2] & 0x7f;
    let location = match net {
        0 => "user".to_string(),
        2 => "local".to_string(),
        3 => "transit".to_string(),
        4 => "remote".to_string(),
        n => format!("net:{n}"),
    };
    let cause_text = match cause {
        1 => "Unassigned number",
        16 => "Normal call clearing",
        17 => "User busy",
        31 => "Normal, unspecified",
        34 => "No channel available",
        41 => "Temporary failure",
        57 => "Bearer cap. not authorized",
        127 => "Interworking, unspecified",
        _ => "",
    };
    let consumed = if (d[2] >> 7) == 1 && d[0] == 3 && d.len() >= 4 && d[3] == 0x88 {
        4 // CCBS not possible
    } else {
        3
    };
    Some((
        json!({ "location": location, "cause": cause, "cause_text": cause_text }),
        consumed,
    ))
}

/// Decode a reassembled IDA packet as a GSM CC/MM/SMS message. Returns
/// None if the first byte is not a recognised CC/MM/SMS transaction major.
pub fn decode(data: &[u8]) -> Option<Value> {
    if data.len() <= 2 {
        return None;
    }
    let tmaj = data[0];
    let maj = major(tmaj)?;
    // SBD majors are handled by the ACARS path, not here.
    if tmaj == 0x76 || (tmaj == 0x06 && data[1] == 0x00) {
        return None;
    }
    let b0 = if tmaj == 0x83 || tmaj == 0x89 { tmaj & 0x7f } else { tmaj };
    let tmin = ((b0 as u16) << 8) | data[1] as u16;
    let body = &data[2..];

    let mut out = json!({
        "type": "gsm",
        "protocol": maj,
        "message": minor(tmin).unwrap_or("?"),
        "tmin": format!("{tmin:04x}"),
    });
    let obj = out.as_object_mut().unwrap();

    // Per-message body decode (the subset iridium-toolkit field-parses).
    match tmin {
        0x032d | 0x032a => {
            // Release / Release Complete: optional disconnect cause.
            if body.len() == 4 && body[0] == 8 {
                if let Some((v, _)) = p_disc(&body[1..]) {
                    obj.insert("disconnect".into(), v);
                }
            }
        }
        0x0325 => {
            if let Some((v, _)) = p_disc(body) {
                obj.insert("disconnect".into(), v);
            }
        }
        0x0502 => {
            // Location Updating Accept: LAI [+ mobile identity] [+ follow-on].
            if let Some((lai, n)) = p_lai(body) {
                obj.insert("lai".into(), lai);
                let mut rest = &body[n..];
                if rest.first() == Some(&0x17) {
                    if let Some((mi, _)) = p_mi_iei(&rest[1..]) {
                        obj.insert("mobile_id".into(), mi);
                    }
                    rest = &rest[1..];
                }
                if rest.first() == Some(&0xa1) {
                    obj.insert("follow_on".into(), json!(true));
                }
            }
        }
        0x0508 => {
            // Location Updating Request.
            if body.len() >= 7 && body[0] & 0xf == 0 && body[6] == 0x28 {
                let key = body[0] >> 4;
                obj.insert("key_seq".into(), if key == 7 { json!("none") } else { json!(key) });
                if let Some((lai, _)) = p_lai(&body[1..]) {
                    obj.insert("lai".into(), lai);
                }
                // byte after LAI (5) + classmark (1) = offset 7.
                if body.len() > 7 {
                    if let Some((mi, _)) = p_mi_iei(&body[7..]) {
                        obj.insert("mobile_id".into(), mi);
                    }
                }
            }
        }
        0x051a => {
            if let Some((lai, n)) = p_lai(body) {
                obj.insert("lai".into(), lai);
                if let Some((mi, _)) = p_mi_iei(&body[n..]) {
                    obj.insert("mobile_id".into(), mi);
                }
            }
        }
        0x0504 => {
            if body.first() == Some(&2) {
                obj.insert("reject".into(), json!("IMSI unknown in HLR"));
            }
        }
        0x0518 => match body.first() {
            Some(2) => {
                obj.insert("requested".into(), json!("IMEI"));
            }
            Some(1) => {
                obj.insert("requested".into(), json!("IMSI"));
            }
            _ => {}
        },
        0x0519 => {
            if let Some((mi, _)) = p_mi_iei(body) {
                obj.insert("mobile_id".into(), mi);
            }
        }
        _ => {}
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_gsm_major_is_none() {
        assert!(decode(&[0x76, 0x08, 0, 0, 0]).is_none());
        assert!(decode(&[0x06, 0x00, 0, 0, 0]).is_none());
    }

    #[test]
    fn identity_request_imei() {
        // CC/MM major 0x05, Identity request 0x0518, body 0x02 = IMEI.
        let v = decode(&[0x05, 0x18, 0x02]).expect("decodes");
        assert_eq!(v["protocol"], "MM");
        assert_eq!(v["message"], "Identity request");
        assert_eq!(v["requested"], "IMEI");
    }
}
