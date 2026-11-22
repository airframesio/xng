//! Airband audio as UDP datagrams of 16 kHz mono PCM (s16le).
//!
//! Target syntax: `host:port[,stream=<freq|mix|scan>][,raw]`. Without
//! `stream=` every channel is sent and each datagram carries a 24-byte
//! header saying which channel it is:
//!
//! ```text
//!  0  "XNGA"
//!  4  u8   version (1)
//!  5  u8   flags: bit0 transmission start, bit1 transmission end,
//!          bit2 discontinuity (samples were dropped before this one)
//!  6  u16  sample rate / 100 (BE)
//!  8  u64  channel frequency in Hz (BE; 0 for the mix stream)
//! 16  u64  capture time of the first sample, µs since the Unix epoch (BE)
//! 24  PCM  s16le samples (up to 688 per datagram)
//! ```
//!
//! `raw` drops the header and sends bare PCM, which plays directly with
//! e.g. `nc -lu 7355 | aplay -r 16000 -f S16_LE -c 1` (pick one stream).

use crate::airband::config::{parse_stream, selected};
use crate::airband::{hub, AudioEvent, StreamId};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use xng_mode_airband::codec::to_i16;
use xng_types::Message;

const MAX_SAMPLES: usize = 688;
pub const HEADER_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq)]
pub struct UdpTarget {
    pub addr: String,
    pub streams: Vec<StreamId>,
    pub raw: bool,
}

pub fn parse_target(s: &str) -> anyhow::Result<UdpTarget> {
    let mut parts = s.split(',');
    let addr = parts.next().unwrap_or_default().trim().to_string();
    anyhow::ensure!(addr.contains(':'), "airband UDP target {s:?} needs host:port");
    let mut t = UdpTarget { addr, streams: Vec::new(), raw: false };
    for p in parts {
        match p.trim().split_once('=') {
            Some(("stream", v)) => t.streams.push(parse_stream(v)?),
            None if p.trim() == "raw" => t.raw = true,
            _ => anyhow::bail!("unknown airband UDP option {p:?} in {s:?}"),
        }
    }
    anyhow::ensure!(
        !t.raw || t.streams.len() == 1,
        "airband UDP target {s:?}: raw PCM needs exactly one stream= (it has no header to tell channels apart)"
    );
    Ok(t)
}

pub fn header(flags: u8, freq_hz: u64, at_us: u64) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(b"XNGA");
    h[4] = 1;
    h[5] = flags;
    h[6..8].copy_from_slice(&((crate::airband::hub::AUDIO_RATE / 100) as u16).to_be_bytes());
    h[8..16].copy_from_slice(&freq_hz.to_be_bytes());
    h[16..24].copy_from_slice(&at_us.to_be_bytes());
    h
}

pub async fn run(bus_rx: broadcast::Receiver<Arc<Message>>, target: UdpTarget) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(&target.addr).await?;
    tracing::info!("airband UDP audio to {}", target.addr);
    let mut rx = hub().subscribe();
    let closed = crate::airband::bus_closed(bus_rx);
    tokio::pin!(closed);
    let mut starting: std::collections::HashSet<StreamId> = Default::default();
    loop {
        let ev = tokio::select! {
            _ = &mut closed => break,
            ev = rx.recv() => match ev {
                Ok(ev) => ev,
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("airband UDP lagged, dropped {n} events");
                    continue;
                }
                Err(RecvError::Closed) => break,
            },
        };
        if !selected(&target.streams, ev.stream()) {
            continue;
        }
        match ev {
            AudioEvent::Open { stream, .. } => {
                starting.insert(stream);
            }
            AudioEvent::Audio(f) => {
                let pcm = to_i16(&f.samples);
                let mut at_us = f.at.timestamp_micros().max(0) as u64;
                let mut first = true;
                for chunk in pcm.chunks(MAX_SAMPLES) {
                    let mut pkt = Vec::with_capacity(HEADER_LEN + chunk.len() * 2);
                    if !target.raw {
                        let mut flags = 0u8;
                        if first && starting.remove(&f.stream) {
                            flags |= 1;
                        }
                        if first && f.discontinuity {
                            flags |= 4;
                        }
                        pkt.extend_from_slice(&header(flags, f.chan.freq_hz, at_us));
                    }
                    for s in chunk {
                        pkt.extend_from_slice(&s.to_le_bytes());
                    }
                    // A full socket buffer or no listener is not an error.
                    let _ = sock.send(&pkt).await;
                    at_us += chunk.len() as u64 * 1_000_000 / crate::airband::hub::AUDIO_RATE as u64;
                    first = false;
                }
            }
            AudioEvent::Close { chan, at, .. } if !target.raw => {
                let _ = sock.send(&header(2, chan.freq_hz, at.timestamp_micros().max(0) as u64)).await;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        let t = parse_target("127.0.0.1:7355").unwrap();
        assert!(t.streams.is_empty() && !t.raw);
        let t = parse_target("127.0.0.1:7355,stream=118.3,raw").unwrap();
        assert_eq!(t.streams, vec![StreamId::Channel(118_300_000)]);
        assert!(t.raw);
        assert!(parse_target("127.0.0.1:7355,raw").is_err());
        assert!(parse_target("localhost").is_err());
        assert!(parse_target("h:1,bogus=1").is_err());
    }

    #[test]
    fn header_layout() {
        let h = header(5, 118_300_000, 1_700_000_000_000_000);
        assert_eq!(&h[0..4], b"XNGA");
        assert_eq!(h[5], 5);
        assert_eq!(u16::from_be_bytes([h[6], h[7]]), 160);
        assert_eq!(u64::from_be_bytes(h[8..16].try_into().unwrap()), 118_300_000);
    }
}
