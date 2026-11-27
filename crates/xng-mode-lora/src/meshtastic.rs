//! Meshtastic over-the-air packets.
//!
//! A packet is a 16-byte clear header (to, from, id, flags, channel hash,
//! next hop, relay node) followed by an AES-CTR encrypted protobuf `Data`
//! message. Channels using the well-known default key (PSK `AQ==`) decrypt
//! out of the box; private channels decrypt when their key is configured.
//! Direct messages encrypted to a node's public key (PKI, channel hash 0)
//! cannot be read and are reported as encrypted.

use crate::meshtastic_tables::{hw_model_name, port_name, role_name};
use crate::pb::Msg;
use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use serde_json::{json, Map, Value};

pub const HEADER_LEN: usize = 16;
pub const BROADCAST: u32 = 0xFFFF_FFFF;
pub const SYNC_WORD: u8 = 0x2B;

/// The default channel key (PSK index 1, shown as `AQ==`).
pub const DEFAULT_KEY: [u8; 16] = [
    0xd4, 0xf1, 0xbb, 0x3a, 0x20, 0x29, 0x07, 0x59, 0xf0, 0xbc, 0xff, 0xab, 0xcf, 0x4e, 0x69, 0x01,
];

/// Modem presets: (name used for the default channel, SF, BW Hz, CR).
pub const PRESETS: &[(&str, u8, f64, u8)] = &[
    ("LongFast", 11, 250e3, 1),
    ("LongSlow", 12, 125e3, 4),
    ("LongMod", 11, 125e3, 4),
    ("LongTurbo", 11, 500e3, 4),
    ("MediumSlow", 10, 250e3, 1),
    ("MediumFast", 9, 250e3, 1),
    ("ShortSlow", 8, 250e3, 1),
    ("ShortFast", 7, 250e3, 1),
    ("ShortTurbo", 7, 500e3, 1),
];

/// A channel the decoder can decrypt.
#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub key: Vec<u8>,
    pub hash: u8,
}

impl Channel {
    /// `psk` as configured in Meshtastic: empty = no encryption, one byte =
    /// default key variant, 16 or 32 bytes = AES-128/256 key.
    pub fn new(name: &str, psk: &[u8]) -> Option<Self> {
        let key = match psk.len() {
            0 => Vec::new(),
            1 if psk[0] == 0 => Vec::new(),
            1 => {
                let mut k = DEFAULT_KEY.to_vec();
                k[15] = k[15].wrapping_add(psk[0] - 1);
                k
            }
            16 | 32 => psk.to_vec(),
            _ => return None,
        };
        let hash = name.bytes().fold(0u8, |a, b| a ^ b) ^ key.iter().fold(0u8, |a, b| a ^ b);
        Some(Self {
            name: name.to_string(),
            key,
            hash,
        })
    }
}

/// The channels to try: every preset's default channel plus `extra`.
pub fn default_channels() -> Vec<Channel> {
    PRESETS
        .iter()
        .filter_map(|(name, ..)| Channel::new(name, &[1]))
        .collect()
}

/// djb2, used by the firmware to pick the frequency slot from the channel name.
pub fn djb2(s: &str) -> u32 {
    s.bytes()
        .fold(5381u32, |h, c| h.wrapping_mul(33).wrapping_add(c as u32))
}

/// Default frequency for a preset in a region band (`start`/`end` MHz),
/// the way the firmware derives it from the channel name.
pub fn default_frequency_hz(preset: &str, bw_hz: f64, start_mhz: f64, end_mhz: f64) -> f64 {
    let bw_mhz = bw_hz / 1e6;
    let n = ((end_mhz - start_mhz) / bw_mhz).floor() as u32;
    let slot = djb2(preset) % n.max(1);
    (start_mhz + bw_mhz / 2.0 + slot as f64 * bw_mhz) * 1e6
}

fn aes_ctr(key: &[u8], nonce: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let mut out = data.to_vec();
    let mut keystream = |enc: &dyn Fn(&mut [u8; 16])| {
        for (i, chunk) in out.chunks_mut(16).enumerate() {
            let mut block = *nonce;
            let ctr = u32::from_be_bytes(block[12..16].try_into().unwrap()).wrapping_add(i as u32);
            block[12..16].copy_from_slice(&ctr.to_be_bytes());
            enc(&mut block);
            for (b, k) in chunk.iter_mut().zip(block.iter()) {
                *b ^= k;
            }
        }
    };
    match key.len() {
        16 => {
            let c = aes::Aes128::new(GenericArray::from_slice(key));
            keystream(&|b| c.encrypt_block(GenericArray::from_mut_slice(b)));
        }
        32 => {
            let c = aes::Aes256::new(GenericArray::from_slice(key));
            keystream(&|b| c.encrypt_block(GenericArray::from_mut_slice(b)));
        }
        _ => return None,
    }
    Some(out)
}

/// Encrypt or decrypt a payload (CTR is symmetric).
pub fn crypt(key: &[u8], from: u32, id: u32, data: &[u8]) -> Option<Vec<u8>> {
    if key.is_empty() {
        return Some(data.to_vec());
    }
    let mut nonce = [0u8; 16];
    nonce[..8].copy_from_slice(&(id as u64).to_le_bytes());
    nonce[8..12].copy_from_slice(&from.to_le_bytes());
    aes_ctr(key, &nonce, data)
}

pub fn node_id(n: u32) -> String {
    format!("!{n:08x}")
}

/// The clear packet header.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub to: u32,
    pub from: u32,
    pub id: u32,
    pub flags: u8,
    pub channel: u8,
    pub next_hop: u8,
    pub relay_node: u8,
}

impl Header {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < HEADER_LEN {
            return None;
        }
        let u = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        Some(Self {
            to: u(0),
            from: u(4),
            id: u(8),
            flags: b[12],
            channel: b[13],
            next_hop: b[14],
            relay_node: b[15],
        })
    }
    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.to.to_le_bytes());
        b[4..8].copy_from_slice(&self.from.to_le_bytes());
        b[8..12].copy_from_slice(&self.id.to_le_bytes());
        b[12] = self.flags;
        b[13] = self.channel;
        b[14] = self.next_hop;
        b[15] = self.relay_node;
        b
    }
}

/// A decoded packet: a short kind and the JSON details.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub kind: String,
    pub details: Value,
}

/// Latitude/longitude from two 1e-7 degree fields. A field on the wire is
/// present (zero included); only a 0,0 pair, which firmware sends when it has
/// no fix, is dropped.
fn latlon(m: &Msg, lat_field: u32, lon_field: u32) -> Option<(f64, f64)> {
    let lat = m.get(lat_field).and_then(|v| v.as_i32());
    let lon = m.get(lon_field).and_then(|v| v.as_i32());
    if lat.is_none() && lon.is_none() || lat == Some(0) && lon == Some(0) {
        return None;
    }
    Some((lat.unwrap_or(0) as f64 * 1e-7, lon.unwrap_or(0) as f64 * 1e-7))
}

fn round(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

fn f32v(v: f32) -> Value {
    json!(round(v as f64, 3))
}

/// Decode the fields of a message given a table of (field, name, type)
/// where type is 'u' varint, 'i' int32, 's' sint32, 'f' float, 'x' fixed32,
/// 't' string, 'b' bool.
fn fields(m: &Msg, table: &[(u32, &str, char)]) -> Map<String, Value> {
    let mut o = Map::new();
    for &(f, name, ty) in table {
        let Some(v) = m.get(f) else { continue };
        let val = match ty {
            'u' | 'x' => v.as_u64().map(|x| json!(x)),
            'i' => v.as_i32().map(|x| json!(x)),
            's' => v.as_sint32().map(|x| json!(x)),
            'f' => v.as_f32().filter(|x| x.is_finite()).map(f32v),
            't' => v.as_str().map(|s| json!(s)),
            'b' => v.as_u64().map(|x| json!(x != 0)),
            _ => None,
        };
        if let Some(val) = val {
            o.insert(name.to_string(), val);
        }
    }
    o
}

fn position(m: &Msg) -> Map<String, Value> {
    let mut o = fields(
        m,
        &[
            (3, "altitude", 'i'),
            (4, "time", 'x'),
            (9, "altitude_hae", 's'),
            (12, "hdop", 'u'),
            (15, "ground_speed", 'u'),
            (16, "ground_track", 'u'),
            (19, "sats_in_view", 'u'),
            (23, "precision_bits", 'u'),
        ],
    );
    if let Some((lat, lon)) = latlon(m, 1, 2) {
        o.insert("lat".into(), json!(round(lat, 7)));
        o.insert("lon".into(), json!(round(lon, 7)));
    }
    o
}

fn user(m: &Msg) -> Map<String, Value> {
    let mut o = fields(
        m,
        &[
            (1, "id", 't'),
            (2, "long_name", 't'),
            (3, "short_name", 't'),
            (6, "is_licensed", 'b'),
        ],
    );
    if let Some(hw) = m.get(5).and_then(|v| v.as_u64()) {
        o.insert(
            "hw_model".into(),
            json!(hw_model_name(hw)
                .map(str::to_string)
                .unwrap_or_else(|| hw.to_string())),
        );
    }
    if let Some(r) = m.get(7).and_then(|v| v.as_u64()) {
        o.insert(
            "role".into(),
            json!(role_name(r)
                .map(str::to_string)
                .unwrap_or_else(|| r.to_string())),
        );
    }
    if let Some(pk) = m
        .get(8)
        .and_then(|v| v.as_bytes())
        .filter(|b| !b.is_empty())
    {
        o.insert("public_key".into(), json!(hex(pk)));
    }
    o
}

fn telemetry(m: &Msg) -> (String, Map<String, Value>) {
    let mut o = fields(m, &[(1, "time", 'x')]);
    let sub = |f: u32| m.get(f).and_then(|v| v.as_bytes()).and_then(Msg::parse);
    let mut kind = "telemetry".to_string();
    if let Some(d) = sub(2) {
        kind = "device_metrics".into();
        o.insert(
            "device_metrics".into(),
            Value::Object(fields(
                &d,
                &[
                    (1, "battery_level", 'u'),
                    (2, "voltage", 'f'),
                    (3, "channel_utilization", 'f'),
                    (4, "air_util_tx", 'f'),
                    (5, "uptime_seconds", 'u'),
                ],
            )),
        );
    }
    if let Some(e) = sub(3) {
        kind = "environment_metrics".into();
        o.insert(
            "environment_metrics".into(),
            Value::Object(fields(
                &e,
                &[
                    (1, "temperature", 'f'),
                    (2, "relative_humidity", 'f'),
                    (3, "barometric_pressure", 'f'),
                    (4, "gas_resistance", 'f'),
                    (5, "voltage", 'f'),
                    (6, "current", 'f'),
                    (7, "iaq", 'u'),
                    (9, "lux", 'f'),
                    (13, "wind_direction", 'u'),
                    (14, "wind_speed", 'f'),
                    (16, "wind_gust", 'f'),
                    (19, "rainfall_1h", 'f'),
                    (20, "rainfall_24h", 'f'),
                ],
            )),
        );
    }
    if let Some(a) = sub(4) {
        kind = "air_quality_metrics".into();
        o.insert(
            "air_quality_metrics".into(),
            Value::Object(fields(
                &a,
                &[
                    (1, "pm10_standard", 'u'),
                    (2, "pm25_standard", 'u'),
                    (3, "pm100_standard", 'u'),
                    (13, "co2", 'u'),
                ],
            )),
        );
    }
    if let Some(p) = sub(5) {
        kind = "power_metrics".into();
        o.insert(
            "power_metrics".into(),
            Value::Object(fields(
                &p,
                &[
                    (1, "ch1_voltage", 'f'),
                    (2, "ch1_current", 'f'),
                    (3, "ch2_voltage", 'f'),
                    (4, "ch2_current", 'f'),
                ],
            )),
        );
    }
    if let Some(l) = sub(6) {
        kind = "local_stats".into();
        o.insert(
            "local_stats".into(),
            Value::Object(fields(
                &l,
                &[
                    (1, "uptime_seconds", 'u'),
                    (2, "channel_utilization", 'f'),
                    (3, "air_util_tx", 'f'),
                    (4, "num_packets_tx", 'u'),
                    (5, "num_packets_rx", 'u'),
                    (6, "num_packets_rx_bad", 'u'),
                    (7, "num_online_nodes", 'u'),
                    (8, "num_total_nodes", 'u'),
                    (15, "noise_floor", 'i'),
                ],
            )),
        );
    }
    (kind, o)
}

fn route(m: &Msg) -> Map<String, Value> {
    let ids = |f: u32| -> Value {
        json!(m
            .repeated(f, 4)
            .iter()
            .map(|&n| node_id(n as u32))
            .collect::<Vec<_>>())
    };
    // SNR values are int32 in quarter dB.
    let snrs = |f: u32| -> Value {
        json!(m
            .repeated(f, 0)
            .iter()
            .map(|&v| (v as i64 as i32) as f64 / 4.0)
            .collect::<Vec<_>>())
    };
    let mut o = Map::new();
    o.insert("route".into(), ids(1));
    o.insert("snr_towards".into(), snrs(2));
    o.insert("route_back".into(), ids(3));
    o.insert("snr_back".into(), snrs(4));
    o
}

const ROUTING_ERRORS: &[(u64, &str)] = &[
    (0, "NONE"),
    (1, "NO_ROUTE"),
    (2, "GOT_NAK"),
    (3, "TIMEOUT"),
    (4, "NO_INTERFACE"),
    (5, "MAX_RETRANSMIT"),
    (6, "NO_CHANNEL"),
    (7, "TOO_LARGE"),
    (8, "NO_RESPONSE"),
    (9, "DUTY_CYCLE_LIMIT"),
    (32, "BAD_REQUEST"),
    (33, "NOT_AUTHORIZED"),
    (34, "PKI_FAILED"),
    (35, "PKI_UNKNOWN_PUBKEY"),
    (36, "ADMIN_BAD_SESSION_KEY"),
    (37, "ADMIN_PUBLIC_KEY_UNAUTHORIZED"),
    (38, "RATE_LIMIT_EXCEEDED"),
    (39, "PKI_SEND_FAIL_PUBLIC_KEY"),
];

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Decode the inner `Data` payload by port. Returns (kind, fields).
fn app(port: u64, payload: &[u8]) -> (String, Map<String, Value>) {
    let mut o = Map::new();
    let sub = Msg::parse(payload);
    let kind = match (port, &sub) {
        (1, _) => {
            o.insert("text".into(), json!(String::from_utf8_lossy(payload)));
            "text".to_string()
        }
        (3, Some(m)) => {
            let p = position(m);
            if let (Some(la), Some(lo)) = (p.get("lat").cloned(), p.get("lon").cloned()) {
                o.insert("lat".into(), la);
                o.insert("lon".into(), lo);
            }
            if let Some(a) = p.get("altitude").cloned() {
                o.insert("altitude".into(), a);
            }
            o.insert("position".into(), Value::Object(p));
            "position".into()
        }
        (4, Some(m)) => {
            let u = user(m);
            for k in ["long_name", "short_name", "hw_model"] {
                if let Some(v) = u.get(k) {
                    o.insert(k.into(), v.clone());
                }
            }
            o.insert("user".into(), Value::Object(u));
            "nodeinfo".into()
        }
        (5, Some(m)) => {
            if let Some(e) = m.get(3).and_then(|v| v.as_u64()) {
                let name = ROUTING_ERRORS
                    .iter()
                    .find(|(c, _)| *c == e)
                    .map(|(_, n)| *n)
                    .unwrap_or("UNKNOWN");
                o.insert("error_reason".into(), json!(name));
                if e == 0 {
                    "ack".into()
                } else {
                    "nak".into()
                }
            } else if let Some(r) = m
                .get(1)
                .or_else(|| m.get(2))
                .and_then(|v| v.as_bytes())
                .and_then(Msg::parse)
            {
                o.insert("route".into(), Value::Object(route(&r)));
                "routing".into()
            } else {
                "routing".into()
            }
        }
        (8, Some(m)) => {
            let mut w = fields(
                m,
                &[
                    (1, "id", 'u'),
                    (4, "expire", 'u'),
                    (6, "name", 't'),
                    (7, "description", 't'),
                ],
            );
            if let Some((la, lo)) = latlon(m, 2, 3) {
                w.insert("lat".into(), json!(round(la, 7)));
                w.insert("lon".into(), json!(round(lo, 7)));
            }
            o.insert("waypoint".into(), Value::Object(w));
            "waypoint".into()
        }
        (66, _) => {
            o.insert("text".into(), json!(String::from_utf8_lossy(payload)));
            "range_test".into()
        }
        (67, Some(m)) => {
            let (k, t) = telemetry(m);
            o.insert("telemetry".into(), Value::Object(t));
            k
        }
        (70, Some(m)) => {
            o.insert("traceroute".into(), Value::Object(route(m)));
            "traceroute".into()
        }
        (71, Some(m)) => {
            let mut n = Map::new();
            if let Some(id) = m.get(1).and_then(|v| v.as_u32()) {
                n.insert("node_id".into(), json!(node_id(id)));
            }
            if let Some(iv) = m.get(3).and_then(|v| v.as_u64()) {
                n.insert("broadcast_interval_secs".into(), json!(iv));
            }
            let list: Vec<Value> = m
                .all(4)
                .filter_map(|v| v.as_bytes())
                .filter_map(Msg::parse)
                .map(|nb| {
                    json!({
                        "node_id": nb.get(1).and_then(|v| v.as_u32()).map(node_id),
                        "snr": nb.get(2).and_then(|v| v.as_f32()).map(|s| round(s as f64, 2)),
                    })
                })
                .collect();
            n.insert("neighbors".into(), json!(list));
            o.insert("neighbor_info".into(), Value::Object(n));
            "neighborinfo".into()
        }
        (73, Some(m)) => {
            let mut r = fields(
                m,
                &[
                    (1, "long_name", 't'),
                    (2, "short_name", 't'),
                    (5, "firmware_version", 't'),
                    (13, "num_online_local_nodes", 'u'),
                ],
            );
            if let Some((la, lo)) = latlon(m, 9, 10) {
                for t in [&mut r, &mut o] {
                    t.insert("lat".into(), json!(round(la, 7)));
                    t.insert("lon".into(), json!(round(lo, 7)));
                }
            }
            o.insert("map_report".into(), Value::Object(r));
            "map_report".into()
        }
        _ => {
            o.insert("payload_hex".into(), json!(hex(payload)));
            port_name(port)
                .map(|n| n.trim_end_matches("_APP").to_ascii_lowercase())
                .unwrap_or_else(|| "data".into())
        }
    };
    (kind, o)
}

/// Try to read decrypted bytes as a `Data` message.
fn parse_data(plain: &[u8]) -> Option<(u64, Msg<'_>)> {
    let m = Msg::parse(plain)?;
    // portnum is an enum: only a varint is well formed.
    let port = match m.get(1)? {
        crate::pb::Value::Varint(v) => *v,
        _ => return None,
    };
    if port == 0 || port > 511 {
        return None;
    }
    // Payload must be bytes when present.
    if let Some(p) = m.get(2) {
        p.as_bytes()?;
    }
    Some((port, m))
}

/// Decode one LoRa payload as a Meshtastic packet. `None` if too short.
pub fn decode(frame: &[u8], channels: &[Channel]) -> Option<Decoded> {
    let h = Header::parse(frame)?;
    let body = &frame[HEADER_LEN..];
    let hop_limit = h.flags & 0x07;
    let hop_start = h.flags >> 5;
    let mut d = Map::new();
    d.insert("protocol".into(), json!("meshtastic"));
    d.insert("from".into(), json!(node_id(h.from)));
    d.insert(
        "to".into(),
        json!(if h.to == BROADCAST {
            "broadcast".to_string()
        } else {
            node_id(h.to)
        }),
    );
    d.insert("packet_id".into(), json!(h.id));
    d.insert("hop_limit".into(), json!(hop_limit));
    if hop_start > 0 {
        d.insert("hop_start".into(), json!(hop_start));
        d.insert(
            "hops_away".into(),
            json!(hop_start.saturating_sub(hop_limit)),
        );
    }
    d.insert("want_ack".into(), json!(h.flags & 0x08 != 0));
    if h.flags & 0x10 != 0 {
        d.insert("via_mqtt".into(), json!(true));
    }
    d.insert("channel_hash".into(), json!(h.channel));
    if h.next_hop != 0 {
        d.insert("next_hop".into(), json!(format!("{:02x}", h.next_hop)));
    }
    if h.relay_node != 0 {
        d.insert("relay_node".into(), json!(format!("{:02x}", h.relay_node)));
    }

    for ch in channels.iter().filter(|c| c.hash == h.channel) {
        let Some(plain) = crypt(&ch.key, h.from, h.id, body) else {
            continue;
        };
        let Some((port, m)) = parse_data(&plain) else {
            continue;
        };
        d.insert("channel".into(), json!(ch.name));
        d.insert("portnum".into(), json!(port));
        if let Some(n) = port_name(port) {
            d.insert("port".into(), json!(n));
        }
        for (f, k) in [(6u32, "request_id"), (7, "reply_id"), (8, "emoji")] {
            if let Some(v) = m.get(f).and_then(|v| v.as_u64()).filter(|&v| v != 0) {
                d.insert(k.into(), json!(v));
            }
        }
        if m.get(3).and_then(|v| v.as_u64()) == Some(1) {
            d.insert("want_response".into(), json!(true));
        }
        let payload = m.get(2).and_then(|v| v.as_bytes()).unwrap_or(&[]);
        let (kind, fields) = app(port, payload);
        d.extend(fields);
        return Some(Decoded {
            kind,
            details: Value::Object(d),
        });
    }
    d.insert("encrypted".into(), json!(true));
    d.insert("payload_hex".into(), json!(hex(body)));
    let kind = if h.channel == 0 && h.to != BROADCAST {
        "direct_encrypted"
    } else {
        "encrypted"
    };
    Some(Decoded {
        kind: kind.into(),
        details: Value::Object(d),
    })
}

/// Build an encrypted packet (for tests and the signal generator).
pub fn encode(h: &Header, port: u64, payload: &[u8], key: &[u8]) -> Vec<u8> {
    let mut data = vec![0x08];
    push_varint(&mut data, port);
    data.push(0x12);
    push_varint(&mut data, payload.len() as u64);
    data.extend_from_slice(payload);
    let mut out = h.to_bytes().to_vec();
    out.extend(crypt(key, h.from, h.id, &data).unwrap());
    out
}

pub fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longfast_default_hash_is_8() {
        assert_eq!(Channel::new("LongFast", &[1]).unwrap().hash, 8);
    }

    #[test]
    fn us_longfast_frequency() {
        let f = default_frequency_hz("LongFast", 250e3, 902.0, 928.0);
        assert!((f - 906.875e6).abs() < 1.0, "{f}");
        let f = default_frequency_hz("LongFast", 250e3, 869.4, 869.65);
        assert!((f - 869.525e6).abs() < 1.0, "{f}");
    }

    #[test]
    fn text_roundtrip_default_key() {
        let h = Header {
            to: BROADCAST,
            from: 0xa1b2c3d4,
            id: 0x1234_5678,
            flags: 0x63,
            channel: 8,
            next_hop: 0,
            relay_node: 0xd4,
        };
        let pkt = encode(&h, 1, b"Hello from xng", &DEFAULT_KEY);
        let d = decode(&pkt, &default_channels()).unwrap();
        assert_eq!(d.kind, "text");
        assert_eq!(d.details["text"], "Hello from xng");
        assert_eq!(d.details["from"], "!a1b2c3d4");
        assert_eq!(d.details["channel"], "LongFast");
        assert_eq!(d.details["hop_start"], 3);
        assert_eq!(d.details["hops_away"], 0);
    }

    #[test]
    fn position_and_unknown_key() {
        // Position: lat 38.5 (sfixed32 field 1), lon -121.25, alt 52.
        let mut pos = vec![0x0d];
        pos.extend(385_000_000i32.to_le_bytes());
        pos.push(0x15);
        pos.extend((-1_212_500_000i32).to_le_bytes());
        pos.extend([0x18, 52]);
        let h = Header {
            to: BROADCAST,
            from: 7,
            id: 99,
            flags: 3,
            channel: 8,
            next_hop: 0,
            relay_node: 0,
        };
        let pkt = encode(&h, 3, &pos, &DEFAULT_KEY);
        let d = decode(&pkt, &default_channels()).unwrap();
        assert_eq!(d.kind, "position");
        assert_eq!(d.details["lat"], 38.5);
        assert_eq!(d.details["lon"], -121.25);
        let private = encode(&h, 3, &pos, &[7u8; 16]);
        let d = decode(&private, &default_channels()).unwrap();
        assert_eq!(d.kind, "encrypted");
    }
}
