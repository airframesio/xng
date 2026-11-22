//! Airframes ATC ingest wire format (protocol v0.2, `airframes-atc/1`).
//!
//! Every frame on a byte stream is `type u8 | length u24 BE | payload`.
//! Control payloads are JSON objects; AUDIO is binary:
//!
//! ```text
//!  0  ch u16 | 2  seq u32 | 6  capture_us u64 | 14  flags u8 | 15  one Opus packet
//! ```
//!
//! On QUIC, AUDIO may go as a datagram instead: `0x10` followed by the AUDIO
//! payload, with no length field.

use tokio::io::{AsyncRead, AsyncReadExt};

pub const ALPN: &[u8] = b"airframes-atc/1";
pub const VERSION: u32 = 1;
pub const MAX_PAYLOAD: usize = 65_535;

pub const HELLO: u8 = 0x01;
pub const WELCOME: u8 = 0x02;
pub const OPEN: u8 = 0x03;
pub const OPENED: u8 = 0x04;
pub const CLOSE: u8 = 0x05;
pub const UPDATE: u8 = 0x06;
pub const SQUELCH: u8 = 0x07;
pub const TRANSCRIPT: u8 = 0x08;
pub const STATS: u8 = 0x09;
pub const REAUTH: u8 = 0x0A;
pub const NOTICE: u8 = 0x0B;
pub const PING: u8 = 0x0E;
pub const PONG: u8 = 0x0F;
pub const AUDIO: u8 = 0x10;
pub const ERROR: u8 = 0x7F;

pub const FLAG_TX_START: u8 = 1;
pub const FLAG_TX_END: u8 = 2;
pub const FLAG_DISCONTINUITY: u8 = 4;

/// One framed message.
pub fn frame(kind: u8, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(payload.len() <= MAX_PAYLOAD, "ATC frame of {} bytes is too large", payload.len());
    let n = payload.len() as u32;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.push(kind);
    out.extend_from_slice(&n.to_be_bytes()[1..]);
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn json_frame(kind: u8, v: &serde_json::Value) -> anyhow::Result<Vec<u8>> {
    frame(kind, &serde_json::to_vec(v)?)
}

/// AUDIO payload (without the frame header).
pub fn audio_payload(ch: u16, seq: u32, capture_us: u64, flags: u8, opus: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(15 + opus.len());
    p.extend_from_slice(&ch.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&capture_us.to_be_bytes());
    p.push(flags);
    p.extend_from_slice(opus);
    p
}

/// PONG payload: the peer's 8 bytes echoed, then our clock.
pub fn pong_payload(ping: &[u8], now_us: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(16);
    let mut echo = [0u8; 8];
    let n = ping.len().min(8);
    echo[..n].copy_from_slice(&ping[..n]);
    p.extend_from_slice(&echo);
    p.extend_from_slice(&now_us.to_be_bytes());
    p
}

/// Read one frame. `Ok(None)` at a clean end of stream.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Option<(u8, Vec<u8>)>> {
    // A clean end of stream is only possible before a frame starts; once its
    // first byte is in, a short read is a truncated frame.
    let mut head = [0u8; 4];
    match r.read_exact(&mut head[..1]).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    r.read_exact(&mut head[1..]).await.map_err(|e| anyhow::anyhow!("truncated ATC frame header: {e}"))?;
    let len = u32::from_be_bytes([0, head[1], head[2], head[3]]) as usize;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await?;
    Ok(Some((head[0], payload)))
}

pub fn type_name(t: u8) -> &'static str {
    match t {
        HELLO => "HELLO",
        WELCOME => "WELCOME",
        OPEN => "OPEN",
        OPENED => "OPENED",
        CLOSE => "CLOSE",
        UPDATE => "UPDATE",
        SQUELCH => "SQUELCH",
        TRANSCRIPT => "TRANSCRIPT",
        STATS => "STATS",
        REAUTH => "REAUTH",
        NOTICE => "NOTICE",
        PING => "PING",
        PONG => "PONG",
        AUDIO => "AUDIO",
        ERROR => "ERROR",
        _ => "?",
    }
}

/// Round a coordinate so it says no more than `precision_m` metres.
pub fn round_coord(v: f64, precision_m: u32) -> f64 {
    if precision_m <= 1 {
        return v;
    }
    // 1 degree of latitude is ~111 km; longitude is never coarser than that.
    let step = precision_m as f64 / 111_000.0;
    let r = (v / step).round() * step;
    (r * 1e6).round() / 1e6
}

/// Frequency keys the server accepts: 1-64 of `[A-Za-z0-9._:/@+-]`.
pub fn valid_key(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.chars().all(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout() {
        let f = frame(HELLO, b"{}").unwrap();
        assert_eq!(f, vec![0x01, 0, 0, 2, b'{', b'}']);
        assert!(frame(AUDIO, &vec![0u8; MAX_PAYLOAD + 1]).is_err());
    }

    #[test]
    fn audio_layout() {
        let p = audio_payload(3, 0x0102_0304, 1_700_000_000_000_000, FLAG_TX_START | FLAG_DISCONTINUITY, &[9, 9]);
        assert_eq!(&p[0..2], &[0, 3]);
        assert_eq!(&p[2..6], &[1, 2, 3, 4]);
        assert_eq!(u64::from_be_bytes(p[6..14].try_into().unwrap()), 1_700_000_000_000_000);
        assert_eq!(p[14], 5);
        assert_eq!(&p[15..], &[9, 9]);
    }

    #[test]
    fn pong_echoes() {
        let p = pong_payload(&7u64.to_be_bytes(), 42);
        assert_eq!(u64::from_be_bytes(p[0..8].try_into().unwrap()), 7);
        assert_eq!(u64::from_be_bytes(p[8..16].try_into().unwrap()), 42);
    }

    #[tokio::test]
    async fn reads_back() {
        let mut buf = frame(PING, &5u64.to_be_bytes()).unwrap();
        buf.extend(frame(NOTICE, b"{\"code\":\"x\"}").unwrap());
        let mut r = buf.as_slice();
        assert_eq!(read_frame(&mut r).await.unwrap().unwrap().0, PING);
        let (t, p) = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!((t, p.as_slice()), (NOTICE, b"{\"code\":\"x\"}".as_slice()));
        assert!(read_frame(&mut r).await.unwrap().is_none());
        let partial = [PING, 0u8];
        assert!(read_frame(&mut partial.as_slice()).await.is_err(), "a partial header is an error");
    }

    #[test]
    fn rounding_and_keys() {
        assert!((round_coord(37.61891, 1000) - 37.62).abs() < 0.005);
        assert_eq!(round_coord(37.618912, 0), 37.618912);
        assert!(valid_key("airport:KSFO:TWR:120500"));
        assert!(valid_key("rtl0:118300000"));
        assert!(!valid_key("has space"));
        assert!(!valid_key(""));
    }
}
