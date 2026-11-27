//! MeshCore over-the-air packets.
//!
//! Layout: header byte (route type, payload type, version), optional 4 bytes
//! of transport codes, path length byte (count + hash size), the path of
//! node hashes, then the payload. Adverts are signed (Ed25519) and carry the
//! node's name and position in clear. Group messages on the built-in
//! "Public" channel (or a configured channel key) decrypt (AES-128 ECB with
//! a truncated HMAC-SHA256); direct messages are encrypted to a key pair and
//! are reported with their source and destination hashes.

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::meshtastic::hex;

pub const SYNC_WORD: u8 = 0x12;
/// The pre-configured "Public" channel key.
pub const PUBLIC_PSK: &str = "izOH6cXN6mrJ5e26oRXNcg==";

const PAYLOAD_TYPES: [&str; 16] = [
    "req",
    "response",
    "txt_msg",
    "ack",
    "advert",
    "grp_txt",
    "grp_data",
    "anon_req",
    "path",
    "trace",
    "multipart",
    "control",
    "type_12",
    "type_13",
    "type_14",
    "raw_custom",
];
const ROUTE_TYPES: [&str; 4] = ["transport_flood", "flood", "direct", "transport_direct"];

/// A group channel the decoder can read.
#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub secret: Vec<u8>,
    pub hash: u8,
}

impl Channel {
    pub fn new(name: &str, secret: Vec<u8>) -> Option<Self> {
        if secret.len() != 16 && secret.len() != 32 {
            return None;
        }
        let hash = Sha256::digest(&secret)[0];
        Some(Self {
            name: name.to_string(),
            secret,
            hash,
        })
    }
    pub fn from_base64(name: &str, psk: &str) -> Option<Self> {
        let key = base64::engine::general_purpose::STANDARD
            .decode(psk.trim())
            .ok()?;
        Self::new(name, key)
    }
    /// A hashtag channel (`#name`): the key is the first 16 bytes of
    /// SHA-256 of the name.
    pub fn hashtag(name: &str) -> Self {
        let name = if name.starts_with('#') {
            name.to_string()
        } else {
            format!("#{name}")
        };
        let key = Sha256::digest(name.as_bytes())[..16].to_vec();
        Self::new(&name, key).unwrap()
    }
    pub fn public() -> Self {
        Self::from_base64("Public", PUBLIC_PSK).unwrap()
    }
}

fn hmac2(secret: &[u8], data: &[u8]) -> [u8; 2] {
    // The firmware keys the HMAC with the 32-byte secret buffer (a 16-byte
    // key is zero padded, which HMAC treats identically).
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret).unwrap();
    m.update(data);
    let r = m.finalize().into_bytes();
    [r[0], r[1]]
}

/// Verify the MAC and decrypt `mac || ciphertext`.
pub fn mac_then_decrypt(secret: &[u8], src: &[u8]) -> Option<Vec<u8>> {
    if src.len() <= 2 || !(src.len() - 2).is_multiple_of(16) {
        return None;
    }
    let (mac, ct) = src.split_at(2);
    if hmac2(secret, ct) != [mac[0], mac[1]] {
        return None;
    }
    let c = aes::Aes128::new(GenericArray::from_slice(&secret[..16]));
    let mut out = ct.to_vec();
    for b in out.chunks_mut(16) {
        c.decrypt_block(GenericArray::from_mut_slice(b));
    }
    Some(out)
}

pub fn encrypt_then_mac(secret: &[u8], plain: &[u8]) -> Vec<u8> {
    let c = aes::Aes128::new(GenericArray::from_slice(&secret[..16]));
    let mut ct = plain.to_vec();
    ct.resize(plain.len().div_ceil(16) * 16, 0);
    for b in ct.chunks_mut(16) {
        c.encrypt_block(GenericArray::from_mut_slice(b));
    }
    let mut out = hmac2(secret, &ct).to_vec();
    out.extend(ct);
    out
}

#[derive(Debug, Clone)]
pub struct Decoded {
    pub kind: String,
    pub details: Value,
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).to_string()
}

/// Returns whether the signature verified.
fn advert(p: &[u8], d: &mut Map<String, Value>) -> Option<bool> {
    // App data is optional: key, timestamp and signature are the minimum.
    if p.len() < 32 + 4 + 64 {
        return None;
    }
    let pubkey = &p[..32];
    let ts = u32le(&p[32..36]);
    let sig = &p[36..100];
    let app = &p[100..p.len().min(100 + 32)];
    d.insert("public_key".into(), json!(hex(pubkey)));
    d.insert("node_hash".into(), json!(format!("{:02x}", pubkey[0])));
    d.insert("advert_time".into(), json!(ts));
    let mut msg = pubkey.to_vec();
    msg.extend_from_slice(&p[32..36]);
    msg.extend_from_slice(app);
    let valid = ed25519_dalek::VerifyingKey::from_bytes(pubkey.try_into().ok()?)
        .ok()
        .and_then(|k| {
            ed25519_dalek::Signature::from_slice(sig)
                .ok()
                .map(|s| k.verify_strict(&msg, &s).is_ok())
        })
        .unwrap_or(false);
    d.insert("signature_valid".into(), json!(valid));
    let Some(&flags) = app.first() else {
        return Some(valid);
    };
    let node_type = match flags & 0x0F {
        1 => "chat",
        2 => "repeater",
        3 => "room_server",
        4 => "sensor",
        _ => "none",
    };
    d.insert("node_type".into(), json!(node_type));
    let mut i = 1;
    if flags & 0x10 != 0 && app.len() >= i + 8 {
        let lat = u32le(&app[i..]) as i32 as f64 / 1e6;
        let lon = u32le(&app[i + 4..]) as i32 as f64 / 1e6;
        i += 8;
        if lat != 0.0 || lon != 0.0 {
            d.insert("lat".into(), json!(lat));
            d.insert("lon".into(), json!(lon));
        }
    }
    if flags & 0x20 != 0 {
        i += 2;
    }
    if flags & 0x40 != 0 {
        i += 2;
    }
    if flags & 0x80 != 0 && app.len() > i {
        d.insert("name".into(), json!(cstr(&app[i..])));
    }
    Some(valid)
}

/// Decode one LoRa payload as a MeshCore packet.
pub fn decode(frame: &[u8], channels: &[Channel]) -> Option<Decoded> {
    let mut i = 0;
    let header = *frame.first()?;
    i += 1;
    let route = header & 0x03;
    let ptype = (header >> 2) & 0x0F;
    let ver = header >> 6;
    let mut d = Map::new();
    d.insert("protocol".into(), json!("meshcore"));
    d.insert("route".into(), json!(ROUTE_TYPES[route as usize]));
    d.insert("payload_type".into(), json!(PAYLOAD_TYPES[ptype as usize]));
    d.insert("payload_version".into(), json!(ver + 1));
    if route == 0 || route == 3 {
        let tc = frame.get(i..i + 4)?;
        d.insert(
            "transport_codes".into(),
            json!([
                u16::from_le_bytes([tc[0], tc[1]]),
                u16::from_le_bytes([tc[2], tc[3]])
            ]),
        );
        i += 4;
    }
    let path_len = *frame.get(i)?;
    i += 1;
    let hsize = ((path_len >> 6) + 1) as usize;
    let count = (path_len & 63) as usize;
    // Firmware caps the path at 64 bytes (MAX_PATH_SIZE).
    if hsize > 3 || hsize * count > 64 {
        return None;
    }
    let path = frame.get(i..i + hsize * count)?;
    i += hsize * count;
    let payload = frame.get(i..)?;
    if payload.is_empty() {
        return None;
    }
    if ptype == 9 {
        // Trace: the path holds the SNR (quarter dB) each hop measured.
        d.insert(
            "path_snr".into(),
            json!(path
                .iter()
                .map(|&s| s as i8 as f64 / 4.0)
                .collect::<Vec<_>>()),
        );
    } else {
        d.insert(
            "path".into(),
            json!(path.chunks(hsize).map(hex).collect::<Vec<_>>()),
        );
        d.insert("hops".into(), json!(count));
    }
    let kind = match ptype {
        0 | 1 | 2 | 8 if payload.len() >= 4 => {
            d.insert("dest_hash".into(), json!(format!("{:02x}", payload[0])));
            d.insert("src_hash".into(), json!(format!("{:02x}", payload[1])));
            d.insert("encrypted".into(), json!(true));
            PAYLOAD_TYPES[ptype as usize].to_string()
        }
        3 if payload.len() >= 4 => {
            d.insert("ack_crc".into(), json!(format!("{:08x}", u32le(payload))));
            "ack".into()
        }
        // A forged or corrupt advert must not rename or move a node.
        4 => match advert(payload, &mut d)? {
            true => "advert".into(),
            false => "advert_bad_signature".into(),
        },
        5 | 6 if payload.len() > 3 => {
            let ch_hash = payload[0];
            d.insert("channel_hash".into(), json!(format!("{ch_hash:02x}")));
            let mut kind = if ptype == 5 { "grp_txt" } else { "grp_data" }.to_string();
            let mut done = false;
            for ch in channels.iter().filter(|c| c.hash == ch_hash) {
                let Some(plain) = mac_then_decrypt(&ch.secret, &payload[1..]) else {
                    continue;
                };
                if ptype == 6 {
                    // Datagram: data type (2), data length (1), data; the
                    // rest is AES padding.
                    let Some(n) = plain.get(2).map(|&n| n as usize) else {
                        continue;
                    };
                    let Some(data) = plain.get(3..3 + n) else {
                        continue;
                    };
                    d.insert("channel".into(), json!(ch.name));
                    d.insert("data_type".into(), json!(u16::from_le_bytes([plain[0], plain[1]])));
                    d.insert("data_hex".into(), json!(hex(data)));
                    kind = "channel_data".into();
                    done = true;
                    break;
                }
                if plain.len() < 5 {
                    continue;
                }
                d.insert("channel".into(), json!(ch.name));
                d.insert("sent_time".into(), json!(u32le(&plain)));
                {
                    let txt_type = plain[4] >> 2;
                    d.insert("txt_type".into(), json!(txt_type));
                    let text = cstr(&plain[5..]);
                    match text.split_once(": ") {
                        Some((who, msg)) => {
                            d.insert("sender".into(), json!(who));
                            d.insert("text".into(), json!(msg));
                        }
                        None => {
                            d.insert("text".into(), json!(text));
                        }
                    }
                    kind = "channel_text".into();
                }
                done = true;
                break;
            }
            if !done {
                d.insert("encrypted".into(), json!(true));
            }
            kind
        }
        7 if payload.len() >= 1 + 32 + 2 => {
            d.insert("dest_hash".into(), json!(format!("{:02x}", payload[0])));
            d.insert("sender_public_key".into(), json!(hex(&payload[1..33])));
            d.insert("encrypted".into(), json!(true));
            "anon_req".into()
        }
        9 if payload.len() >= 9 => {
            d.insert("trace_tag".into(), json!(format!("{:08x}", u32le(payload))));
            let flags = payload[8];
            let sz = 1usize << (flags & 3);
            d.insert(
                "trace_path".into(),
                json!(payload[9..].chunks(sz).map(hex).collect::<Vec<_>>()),
            );
            "trace".into()
        }
        _ => {
            d.insert("payload_hex".into(), json!(hex(payload)));
            PAYLOAD_TYPES[ptype as usize].to_string()
        }
    };
    Some(Decoded {
        kind,
        details: Value::Object(d),
    })
}

/// Build a flood group text packet (tests and the signal generator).
pub fn encode_group_text(ch: &Channel, ts: u32, sender: &str, text: &str, path: &[u8]) -> Vec<u8> {
    let mut plain = ts.to_le_bytes().to_vec();
    plain.push(0);
    plain.extend_from_slice(format!("{sender}: {text}").as_bytes());
    let mut out = vec![(5 << 2) | 1, path.len() as u8];
    out.extend_from_slice(path);
    out.push(ch.hash);
    out.extend(encrypt_then_mac(&ch.secret, &plain));
    out
}

/// Build a signed advert (tests and the signal generator).
pub fn encode_advert(
    seed: &[u8; 32],
    ts: u32,
    node_type: u8,
    name: &str,
    latlon: Option<(f64, f64)>,
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let sk = ed25519_dalek::SigningKey::from_bytes(seed);
    let pk = sk.verifying_key().to_bytes();
    let mut app = vec![node_type | 0x80 | if latlon.is_some() { 0x10 } else { 0 }];
    if let Some((la, lo)) = latlon {
        app.extend(((la * 1e6) as i32).to_le_bytes());
        app.extend(((lo * 1e6) as i32).to_le_bytes());
    }
    app.extend_from_slice(name.as_bytes());
    let mut msg = pk.to_vec();
    msg.extend(ts.to_le_bytes());
    msg.extend(&app);
    let sig = sk.sign(&msg).to_bytes();
    let mut out = vec![(4 << 2) | 1, 0];
    out.extend(pk);
    out.extend(ts.to_le_bytes());
    out.extend(sig);
    out.extend(app);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_channel_hash() {
        let p = Channel::public();
        assert_eq!(p.secret.len(), 16);
        assert_eq!(p.hash, 0x11);
    }

    #[test]
    fn group_text_roundtrip() {
        let ch = Channel::public();
        let pkt = encode_group_text(&ch, 1_764_000_000, "KD6XYZ", "hello public", &[0xa1, 0x07]);
        let d = decode(&pkt, &[ch]).unwrap();
        assert_eq!(d.kind, "channel_text");
        assert_eq!(d.details["sender"], "KD6XYZ");
        assert_eq!(d.details["text"], "hello public");
        assert_eq!(d.details["path"], json!(["a1", "07"]));
        // Unknown channel stays encrypted.
        let other = Channel::hashtag("#test");
        let pkt = encode_group_text(&other, 1, "a", "b", &[]);
        let d = decode(&pkt, &[Channel::public()]).unwrap();
        assert_eq!(d.details["encrypted"], true);
        let d = decode(&pkt, &[other]).unwrap();
        assert_eq!(d.details["text"], "b");
    }

    #[test]
    fn advert_signature() {
        let pkt = encode_advert(
            &[9u8; 32],
            1_764_000_000,
            2,
            "Ridge Repeater",
            Some((38.68, -120.99)),
        );
        let d = decode(&pkt, &[]).unwrap();
        assert_eq!(d.kind, "advert");
        assert_eq!(d.details["signature_valid"], true);
        assert_eq!(d.details["name"], "Ridge Repeater");
        assert_eq!(d.details["node_type"], "repeater");
        assert!((d.details["lat"].as_f64().unwrap() - 38.68).abs() < 1e-5);
        let mut bad = pkt.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        let bad = decode(&bad, &[]).unwrap();
        assert_eq!(bad.details["signature_valid"], false);
        assert_eq!(bad.kind, "advert_bad_signature");
    }

    #[test]
    fn advert_without_app_data() {
        use ed25519_dalek::Signer;
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let mut msg = pk.to_vec();
        msg.extend(1_764_000_000u32.to_le_bytes());
        let sig = sk.sign(&msg).to_bytes();
        let mut pkt = vec![(4 << 2) | 1, 0];
        pkt.extend(&msg);
        pkt.extend(sig);
        let d = decode(&pkt, &[]).unwrap();
        assert_eq!(d.kind, "advert");
        assert!(d.details.get("name").is_none());
    }

    #[test]
    fn path_over_64_bytes_is_rejected() {
        // Two-byte hashes, 33 hops: 66 bytes of path.
        let mut pkt = vec![(3 << 2) | 1, (1 << 6) | 33];
        pkt.extend([0u8; 66]);
        pkt.extend([1, 2, 3, 4]);
        assert!(decode(&pkt, &[]).is_none());
        let mut ok = vec![(3 << 2) | 1, (1 << 6) | 32];
        ok.extend([0u8; 64]);
        ok.extend([1, 2, 3, 4]);
        assert_eq!(decode(&ok, &[]).unwrap().kind, "ack");
    }
}
