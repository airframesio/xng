//! Beast binary output: the de facto Mode S feed format (dump1090
//! port-30005 style) — `0x1a` escape framing, type '2' (7-byte short)
//! or '3' (14-byte long) frames, 6-byte 12 MHz MLAT counter, one
//! signal byte, payload with 0x1a doubled.

use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody, Mode};

/// Render one message as a Beast frame; `None` for non-Mode S.
pub fn format_beast(msg: &Message) -> Option<Vec<u8>> {
    if msg.mode != Mode::Adsb {
        return None;
    }
    if !matches!(msg.body, MessageBody::ModeS { .. }) {
        return None;
    }
    let raw = msg.raw.as_ref()?;
    let kind = match raw.len() {
        7 => b'2',
        14 => b'3',
        _ => return None,
    };
    // 12 MHz MLAT counter from the message timestamp (wraps naturally
    // in 48 bits; receivers use it for relative timing only).
    let ticks = (msg.timestamp.timestamp_micros().max(0) as u64).wrapping_mul(12);
    let sig = msg
        .signal
        .rssi_db
        .map(|db| (((db + 50.0) / 50.0 * 255.0).clamp(0.0, 255.0)) as u8)
        .unwrap_or(0x80);

    let mut out = Vec::with_capacity(2 + 7 + 1 + raw.len() * 2);
    out.push(0x1a);
    out.push(kind);
    let push_esc = |b: u8, out: &mut Vec<u8>| {
        out.push(b);
        if b == 0x1a {
            out.push(0x1a);
        }
    };
    for k in (0..6).rev() {
        push_esc(((ticks >> (8 * k)) & 0xFF) as u8, &mut out);
    }
    push_esc(sig, &mut out);
    for &b in raw {
        push_esc(b, &mut out);
    }
    Some(out)
}

/// Serve Beast frames on `addr` (e.g. `0.0.0.0:30005`).
pub async fn run(rx: broadcast::Receiver<Arc<Message>>, addr: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Beast output on {addr}");
    loop {
        let (mut sock, peer) = listener.accept().await?;
        tracing::info!("Beast client connected: {peer}");
        let mut rx = rx.resubscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let Some(frame) = format_beast(&msg) {
                            if sock.write_all(&frame).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_types::{AppInfo, DecodeQuality, Provenance, SignalQuality, StationIdentity};

    #[test]
    fn long_frame_renders_with_escaping() {
        let mut raw = vec![0x8Du8; 14];
        raw[5] = 0x1a; // force one escape
        let msg = Message {
            mode: Mode::Adsb,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_090_000_000,
            signal: SignalQuality { rssi_db: Some(-20.0), ..Default::default() },
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::ModeS {
                df: 17,
                icao: Some("ABCDEF".into()),
                callsign: None,
                altitude_ft: None,
                squawk: None,
                lat: None,
                lon: None,
                speed_kt: None,
                speed_type: None,
                track_deg: None,
                vertical_rate_fpm: None,
                comm_b: None,
            },
            raw: Some(raw),
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let f = format_beast(&msg).unwrap();
        assert_eq!(f[0], 0x1a);
        assert_eq!(f[1], b'3');
        // 0x1a payload byte must be doubled.
        let esc_count = f[2..].windows(2).filter(|w| w == &[0x1a, 0x1a]).count();
        assert!(esc_count >= 1);
    }
}
