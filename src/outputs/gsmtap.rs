//! GSMTAP output: send Iridium duplex GSM (CC/MM/SMS) frames to Wireshark
//! via UDP (the osmocom de-facto standard, default port 4729). Wireshark's
//! GSMTAP dissector then unpacks the LAPDm/GSM layers. The 16-byte header
//! mirrors iridium-sniffer `gsmtap.c`: type ABIS, ARFCN = the Iridium
//! channel index (uplink flagged 0x4000), the absolute frequency carried
//! in frame_number.

use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

const GSMTAP_VERSION: u8 = 2;
const GSMTAP_HDR_LEN_WORDS: u8 = 4; // 4×32-bit = 16 bytes
const GSMTAP_TYPE_ABIS: u8 = 2;
const GSMTAP_SUB_BCCH: u8 = 1;
const ARFCN_F_UPLINK: u16 = 0x4000;
const IR_BASE_FREQ: f64 = 1_616_000_000.0;
const IR_CHANNEL_WIDTH: f64 = 41_666.667;

/// Build a GSMTAP packet (16-byte header + L2 payload) for one Iridium
/// GSM frame.
fn packet(l2: &[u8], freq_hz: f64, ul: bool, signal_dbm: i8) -> Vec<u8> {
    let fchan = ((freq_hz - IR_BASE_FREQ) / IR_CHANNEL_WIDTH) as i64 as u16;
    let arfcn = if ul { fchan | ARFCN_F_UPLINK } else { fchan };
    let l2 = &l2[..l2.len().min(240)];
    let mut p = Vec::with_capacity(16 + l2.len());
    p.push(GSMTAP_VERSION);
    p.push(GSMTAP_HDR_LEN_WORDS);
    p.push(GSMTAP_TYPE_ABIS);
    p.push(0); // timeslot
    p.extend_from_slice(&arfcn.to_be_bytes());
    p.push(signal_dbm as u8);
    p.push(0); // snr_db
    p.extend_from_slice(&(freq_hz as u32).to_be_bytes()); // frame_number
    p.push(GSMTAP_SUB_BCCH);
    p.push(0); // antenna_nr
    p.push(0); // sub_slot
    p.push(0); // res
    p.extend_from_slice(l2);
    p
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Build the GSMTAP packet for a message, or None if it is not an
/// Iridium GSM frame carrying raw L2 bytes.
fn message_packet(
    kind: &str,
    details: &serde_json::Value,
    freq_hz: u64,
    rssi_db: Option<f32>,
) -> Option<Vec<u8>> {
    if kind != "gsm" {
        return None;
    }
    let l2 = hex_to_bytes(details.get("raw_l2_hex")?.as_str()?);
    if l2.is_empty() {
        return None;
    }
    let ul = details.get("ul").and_then(|v| v.as_bool()).unwrap_or(false);
    let signal = rssi_db.unwrap_or(0.0).clamp(-127.0, 0.0) as i8;
    Some(packet(&l2, freq_hz as f64, ul, signal))
}

pub async fn run(rx: broadcast::Receiver<Arc<Message>>, addr: String) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(&addr).await?;
    tracing::info!("GSMTAP output → {addr}");
    let mut rx = rx;
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let MessageBody::Iridium { kind, details } = &msg.body else { continue };
                if let Some(pkt) = message_packet(kind, details, msg.frequency_hz, msg.signal.rssi_db)
                {
                    let _ = sock.send(&pkt).await;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout() {
        // freq 1624.000 MHz → channel (1624e6-1616e6)/41666.667 = 191.99…,
        // truncated to 191 (matching the toolkit/sniffer int() convention).
        let p = packet(&[0xaa, 0xbb], 1_624_000_000.0, false, -20);
        assert_eq!(p.len(), 18);
        assert_eq!(p[0], 2); // version
        assert_eq!(p[2], 2); // type ABIS
        let arfcn = u16::from_be_bytes([p[4], p[5]]);
        assert_eq!(arfcn, 191);
        // uplink sets the flag.
        let pu = packet(&[0xaa], 1_624_000_000.0, true, -20);
        assert_eq!(u16::from_be_bytes([pu[4], pu[5]]), 191 | 0x4000);
        // frame_number carries the frequency.
        assert_eq!(u32::from_be_bytes([p[8], p[9], p[10], p[11]]), 1_624_000_000);
    }

    #[test]
    fn message_glue() {
        // A gsm frame with raw L2 → a packet; everything else → None.
        let d = serde_json::json!({ "raw_l2_hex": "0518aabb", "ul": true });
        let p = message_packet("gsm", &d, 1_624_000_000, Some(-30.0)).expect("packet");
        assert_eq!(&p[16..], &[0x05, 0x18, 0xaa, 0xbb]); // L2 after 16-byte hdr
        assert_eq!(u16::from_be_bytes([p[4], p[5]]) & 0x4000, 0x4000); // uplink flag
        assert!(message_packet("ida", &d, 1_624_000_000, None).is_none());
        assert!(message_packet("gsm", &serde_json::json!({}), 0, None).is_none());
    }
}
