//! ZMQ output: publish each normalized message as a two-frame multipart
//! `[mode, json]` over a ZeroMQ `PUB` socket. The first frame is the mode
//! string (e.g. `vdl2`) so `SUB` consumers can filter broker-side
//! (`SUBSCRIBE vdl2`); the second frame is the same JSON `Message` the MQTT
//! and JSONL sinks emit. `SUBSCRIBE ""` receives everything.
//!
//! One PUB socket is shared for the whole process and attached to one or more
//! ZeroMQ endpoints; every published message fans out to all of them. Each
//! endpoint is a ZeroMQ endpoint string that by default *binds* (acting as a
//! PUB server consumers connect to); prefix an endpoint with `connect:` to
//! *connect* to a known XSUB/collector instead. Bind and connect endpoints can
//! be mixed freely:
//!
//! - `tcp://0.0.0.0:5555`          — bind (default)
//! - `ipc:///tmp/xng.sock`         — bind
//! - `connect:tcp://collector:5555` — connect

use std::sync::Arc;
use tokio::sync::broadcast;
use xng_types::Message;
use zeromq::{PubSocket, Socket, SocketSend, ZmqMessage};

/// How the PUB socket attaches to its endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attach {
    Bind,
    Connect,
}

/// Split an optional `connect:` prefix off the endpoint, returning the attach
/// mode and the bare ZeroMQ endpoint string. The scheme is validated to be one
/// the sink supports (`tcp://` or `ipc://`).
fn parse_endpoint(endpoint: &str) -> anyhow::Result<(Attach, &str)> {
    let (attach, addr) = match endpoint.strip_prefix("connect:") {
        Some(rest) => (Attach::Connect, rest),
        None => (Attach::Bind, endpoint),
    };
    anyhow::ensure!(
        addr.starts_with("tcp://") || addr.starts_with("ipc://"),
        "--zmq endpoint must be tcp://… or ipc://… (optionally prefixed with connect:)"
    );
    Ok((attach, addr))
}

/// Consume the bus until it closes, publishing each message as `[mode, json]`
/// to every configured endpoint. All endpoints share one `PUB` socket, so a
/// published message fans out to all of them.
pub async fn run(
    rx: broadcast::Receiver<Arc<Message>>,
    endpoints: Vec<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !endpoints.is_empty(),
        "zmq output requires at least one endpoint"
    );
    // Validate every endpoint up front so a typo fails fast rather than after
    // some endpoints are already attached.
    let parsed = endpoints
        .iter()
        .map(|e| parse_endpoint(e))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut socket = PubSocket::new();
    for (attach, addr) in parsed {
        match attach {
            Attach::Bind => {
                socket.bind(addr).await?;
                tracing::info!("zmq PUB bound on {addr}");
            }
            Attach::Connect => {
                socket.connect(addr).await?;
                tracing::info!("zmq PUB connected to {addr}");
            }
        }
    }
    publish_loop(rx, &mut socket).await;
    Ok(())
}

/// The send loop, split out so it can be exercised against any `SocketSend`.
async fn publish_loop<S: SocketSend>(mut rx: broadcast::Receiver<Arc<Message>>, socket: &mut S) {
    loop {
        match rx.recv().await {
            Ok(msg) => match serde_json::to_vec(&*msg) {
                Ok(payload) => {
                    let mut frame = ZmqMessage::from(msg.mode.as_str().to_owned());
                    frame.push_back(payload.into());
                    if let Err(e) = socket.send(frame).await {
                        tracing::warn!("zmq publish: {e}");
                    }
                }
                Err(e) => tracing::warn!("zmq encode: {e}"),
            },
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("zmq output lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use xng_types::{
        AppInfo, DecodeQuality, MessageBody, Mode, Provenance, SignalQuality, StationIdentity,
    };
    use zeromq::{SocketRecv, SubSocket};

    #[test]
    fn endpoint_forms() {
        assert_eq!(
            parse_endpoint("tcp://0.0.0.0:5555").unwrap(),
            (Attach::Bind, "tcp://0.0.0.0:5555")
        );
        assert_eq!(
            parse_endpoint("connect:tcp://host:5555").unwrap(),
            (Attach::Connect, "tcp://host:5555")
        );
        assert_eq!(
            parse_endpoint("ipc:///tmp/xng.sock").unwrap(),
            (Attach::Bind, "ipc:///tmp/xng.sock")
        );
        assert!(parse_endpoint("udp://x").is_err());
        assert!(parse_endpoint("connect:udp://x").is_err());
    }

    fn sample_message() -> Message {
        Message {
            mode: Mode::Vdl2,
            timestamp: chrono::Utc::now(),
            frequency_hz: 136_975_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality::default(),
            body: MessageBody::Vdl2 {
                kind: "xid".to_owned(),
                details: serde_json::json!({ "test": true }),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("XNG-TEST"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[tokio::test]
    async fn pub_sub_roundtrip() {
        // Bind a PUB sink on an ephemeral port, connect a SUB, and assert the
        // [mode, json] frames arrive. The contract is that frame 1 is exactly
        // the shared `serde_json::to_vec(&Message)` bytes the other sinks emit,
        // so assert byte-equality (not merely that it deserializes).
        let mut pubs = PubSocket::new();
        let endpoint = pubs.bind("tcp://127.0.0.1:0").await.unwrap();
        let addr = endpoint.to_string();

        let mut sub = SubSocket::new();
        sub.connect(&addr).await.unwrap();
        sub.subscribe("").await.unwrap();
        // Give the SUB connection a moment to establish before publishing
        // (PUB drops messages with no connected subscribers).
        tokio::time::sleep(Duration::from_millis(200)).await;

        let msg = Arc::new(sample_message());
        let expected_json = serde_json::to_vec(&*msg).unwrap();
        let (tx, rx) = broadcast::channel(8);
        tx.send(msg).unwrap();
        drop(tx); // close the bus so publish_loop returns after draining

        publish_loop(rx, &mut pubs).await;

        let frame = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .expect("recv timed out")
            .expect("recv failed");
        assert_eq!(frame.len(), 2, "expected [mode, json] multipart");
        assert_eq!(&frame.get(0).unwrap()[..], b"vdl2");
        assert_eq!(&frame.get(1).unwrap()[..], &expected_json[..]);
    }

    #[tokio::test]
    async fn run_fans_out_to_multiple_endpoints() {
        // `run` with a two-endpoint list binds one PUB socket to both; a SUB
        // connected to each must receive the same message (the fan-out).
        // Use unique IPC endpoints rather than ephemeral TCP ports: allocating
        // a port via a throwaway TcpListener and rebinding it later is racy
        // (the port is free between drop and rebind). A per-test-run temp path
        // has no such window.
        let dir = std::env::temp_dir().join(format!(
            "xng-zmq-fanout-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |name: &str| format!("ipc://{}", dir.join(name).display());
        let (a1, a2) = (path("a"), path("b"));

        let (tx, rx) = broadcast::channel(8);
        let endpoints = vec![a1.clone(), a2.clone()];
        let handle = tokio::spawn(run(rx, endpoints));
        // Let `run` bind both endpoints before connecting subscribers.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut sub1 = SubSocket::new();
        sub1.connect(&a1).await.unwrap();
        sub1.subscribe("").await.unwrap();
        let mut sub2 = SubSocket::new();
        sub2.connect(&a2).await.unwrap();
        sub2.subscribe("").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        tx.send(Arc::new(sample_message())).unwrap();

        for sub in [&mut sub1, &mut sub2] {
            let frame = tokio::time::timeout(Duration::from_secs(2), sub.recv())
                .await
                .expect("recv timed out")
                .expect("recv failed");
            assert_eq!(frame.len(), 2);
            assert_eq!(&frame.get(0).unwrap()[..], b"vdl2");
        }

        drop(tx); // close the bus so `run` returns
        handle.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_rejects_empty_endpoint_list() {
        let (_tx, rx) = broadcast::channel::<Arc<Message>>(1);
        assert!(run(rx, vec![]).await.is_err());
    }
}
