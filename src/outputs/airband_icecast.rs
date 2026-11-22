//! Airband audio to an Icecast server (or anything that speaks the Icecast
//! source protocol) as a continuous Ogg/Opus stream.
//!
//! URL: `icecast://user:pass@host:port/mount[?stream=<freq|mix|scan>][&bitrate=N][&name=...]`.
//! Without `stream=` the runtime picks `scan` when the scanner is enabled,
//! else `mix`. Silence fills the gaps between transmissions so players see a
//! steady live stream.

use crate::airband::config::parse_stream;
use crate::airband::opus_enc::{OggOpus, FRAME};
use crate::airband::{AudioEvent, StreamId};
use base64::Engine as _;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use xng_types::Message;

/// Keep at most this much queued audio (s); older audio is dropped so the
/// stream stays near live.
const MAX_QUEUE_S: usize = 2;
/// Whole source handshake (connect, request, status line).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// One write to the server; a server that stops reading is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// A connection that lasted this long resets the retry delay.
const HEALTHY_AFTER: Duration = Duration::from_secs(30);
/// `name=` ends up in the Ogg comment header, which must fit in one page.
const MAX_NAME: usize = 1_024;

#[derive(Debug, Clone, PartialEq)]
pub struct IcecastTarget {
    pub host: String,
    pub port: u16,
    pub mount: String,
    pub user: String,
    pub password: String,
    pub stream: Option<StreamId>,
    pub bitrate: u32,
    pub name: Option<String>,
}

pub fn parse_url(s: &str) -> anyhow::Result<IcecastTarget> {
    let rest =
        s.strip_prefix("icecast://").ok_or_else(|| anyhow::anyhow!("icecast URL must start with icecast://"))?;
    let (auth, rest) = match rest.rsplit_once('@') {
        Some((a, r)) => (Some(a), r),
        None => (None, rest),
    };
    let (user, password) = match auth {
        Some(a) => match a.split_once(':') {
            Some((u, p)) => (u.to_string(), p.to_string()),
            None => ("source".to_string(), a.to_string()),
        },
        None => ("source".to_string(), String::new()),
    };
    let (hostport, path) = rest.split_once('/').ok_or_else(|| anyhow::anyhow!("icecast URL needs a /mount"))?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().map_err(|_| anyhow::anyhow!("bad icecast port {p:?}"))?),
        None => (hostport.to_string(), 8000),
    };
    let (mount, query) = match path.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (path, None),
    };
    anyhow::ensure!(!mount.is_empty(), "icecast URL needs a mount name");
    anyhow::ensure!(!host.is_empty(), "icecast URL needs a host");
    let mut t = IcecastTarget {
        host,
        port,
        mount: format!("/{mount}"),
        user,
        password,
        stream: None,
        bitrate: 32_000,
        name: None,
    };
    for kv in query.into_iter().flat_map(|q| q.split('&')).filter(|kv| !kv.is_empty()) {
        match kv.split_once('=') {
            Some(("stream", v)) => t.stream = Some(parse_stream(v)?),
            Some(("bitrate", v)) => t.bitrate = v.parse().map_err(|_| anyhow::anyhow!("bad bitrate {v}"))?,
            Some(("name", v)) => {
                anyhow::ensure!(v.len() <= MAX_NAME, "icecast name is longer than {MAX_NAME} bytes");
                t.name = Some(v.replace('+', " "))
            }
            _ => anyhow::bail!("unknown icecast option {kv:?}"),
        }
    }
    Ok(t)
}

async fn connect(t: &IcecastTarget, title: &str) -> anyhow::Result<TcpStream> {
    tokio::time::timeout(CONNECT_TIMEOUT, handshake(t, title))
        .await
        .map_err(|_| anyhow::anyhow!("no answer within {}s", CONNECT_TIMEOUT.as_secs()))?
}

async fn handshake(t: &IcecastTarget, title: &str) -> anyhow::Result<TcpStream> {
    let mut sock = TcpStream::connect((t.host.as_str(), t.port)).await?;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", t.user, t.password));
    let req = format!(
        "PUT {} HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Basic {auth}\r\nUser-Agent: xng/{}\r\n\
         Content-Type: application/ogg\r\nIce-Public: 0\r\nIce-Name: {}\r\nIce-Description: xng airband\r\n\
         Expect: 100-continue\r\n\r\n",
        t.mount,
        t.host,
        t.port,
        env!("CARGO_PKG_VERSION"),
        title.replace(['\r', '\n'], " "),
    );
    sock.write_all(req.as_bytes()).await?;
    // Read up to the end of the status line (bounded).
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    while !buf.windows(2).any(|w| w == b"\r\n") {
        anyhow::ensure!(buf.len() < 4_096, "icecast answer has no status line");
        let n = sock.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "icecast closed the connection");
        buf.extend_from_slice(&chunk[..n]);
    }
    let resp = String::from_utf8_lossy(&buf);
    let status = resp.split_whitespace().nth(1).unwrap_or("");
    anyhow::ensure!(
        status == "100" || status == "200",
        "icecast refused the source: {}",
        resp.lines().next().unwrap_or("")
    );
    Ok(sock)
}

pub async fn run(
    mut rx: tokio::sync::broadcast::Receiver<crate::airband::hub::AudioEvent>,
    bus_rx: broadcast::Receiver<Arc<Message>>,
    t: IcecastTarget,
) -> std::io::Result<()> {
    let stream = t.stream.unwrap_or(StreamId::Mix);
    let title = t.name.clone().unwrap_or_else(|| format!("xng airband ({stream})"));
    let closed = crate::airband::bus_closed(bus_rx);
    tokio::pin!(closed);
    let mut backoff = Duration::from_secs(1);
    'outer: loop {
        let mut sock = tokio::select! {
            _ = &mut closed => break,
            r = connect(&t, &title) => match r {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("icecast {}:{}{}: {e}; retrying in {}s", t.host, t.port, t.mount, backoff.as_secs());
                    if wait(&mut closed, &mut backoff).await {
                        break;
                    }
                    continue;
                }
            },
        };
        tracing::info!("icecast streaming {stream} to {}:{}{}", t.host, t.port, t.mount);
        let connected = tokio::time::Instant::now();
        let (mut ogg, head) = match OggOpus::new(t.bitrate, rand_serial(), &title) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("icecast: opus encoder: {e}");
                return Ok(());
            }
        };
        let mut lost = !write(&mut sock, &head).await;
        let mut queue: VecDeque<f32> = VecDeque::new();
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !lost {
            tokio::select! {
                _ = &mut closed => {
                    let _ = write(&mut sock, &ogg.finish()).await;
                    break 'outer;
                }
                ev = rx.recv() => match ev {
                    Ok(AudioEvent::Audio(f)) if f.stream == stream => {
                        queue.extend(f.samples.iter());
                        let max = MAX_QUEUE_S * crate::airband::hub::AUDIO_RATE as usize;
                        while queue.len() > max {
                            queue.pop_front();
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => tracing::warn!("icecast lagged, dropped {n} events"),
                    Err(RecvError::Closed) => break 'outer,
                },
                _ = tick.tick() => {
                    let mut frame = [0f32; FRAME];
                    for (i, v) in queue.drain(..queue.len().min(FRAME)).enumerate() {
                        frame[i] = v;
                    }
                    let pages = ogg.push(&frame);
                    // A timed-out write may have sent part of a page, so the
                    // socket is dropped rather than reused.
                    lost = !pages.is_empty() && !write(&mut sock, &pages).await;
                }
            }
        }
        drop(sock);
        if connected.elapsed() >= HEALTHY_AFTER {
            backoff = Duration::from_secs(1);
        }
        tracing::warn!("icecast connection lost; reconnecting in {}s", backoff.as_secs());
        if wait(&mut closed, &mut backoff).await {
            break;
        }
    }
    Ok(())
}

/// One write with a deadline; false when the connection is unusable.
async fn write(sock: &mut TcpStream, bytes: &[u8]) -> bool {
    matches!(tokio::time::timeout(WRITE_TIMEOUT, sock.write_all(bytes)).await, Ok(Ok(())))
}

/// Sleep the retry delay (doubling it, up to a minute); true if the
/// session ended meanwhile.
async fn wait(closed: &mut std::pin::Pin<&mut impl std::future::Future<Output = ()>>, backoff: &mut Duration) -> bool {
    let d = *backoff;
    *backoff = (*backoff * 2).min(Duration::from_secs(60));
    tokio::select! {
        _ = closed.as_mut() => true,
        _ = tokio::time::sleep(d) => false,
    }
}

fn rand_serial() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let t = parse_url("icecast://source:hackme@radio.local:8000/airband?stream=scan&bitrate=24000&name=KSFO+Scan")
            .unwrap();
        assert_eq!(t.host, "radio.local");
        assert_eq!(t.port, 8000);
        assert_eq!(t.mount, "/airband");
        assert_eq!(t.user, "source");
        assert_eq!(t.password, "hackme");
        assert_eq!(t.stream, Some(StreamId::Scan));
        assert_eq!(t.bitrate, 24_000);
        assert_eq!(t.name.as_deref(), Some("KSFO Scan"));
        let t = parse_url("icecast://pw@host/m").unwrap();
        assert_eq!((t.user.as_str(), t.password.as_str(), t.port), ("source", "pw", 8000));
        assert!(parse_url("http://host/m").is_err());
        assert!(parse_url("icecast://host").is_err());
        // Errors never repeat the URL (it carries the password).
        let e = parse_url("icecast://u:secret@host:notaport/m").unwrap_err().to_string();
        assert!(!e.contains("secret"), "{e}");
        let long = format!("icecast://h/m?name={}", "x".repeat(MAX_NAME + 1));
        assert!(parse_url(&long).is_err());
    }
}
