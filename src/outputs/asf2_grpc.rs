//! asf-2.0 gRPC output: bidirectional `AirframesFeed/Stream` to an ingest
//! (e.g. `xng ingest --grpc`). Reconnects with backoff; messages decoded
//! while disconnected are dropped (the bus does not back-pressure DSP).

use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use xng_proto::asf2::airframes_feed_client::AirframesFeedClient;
use xng_proto::asf2::{self, Envelope};
use xng_types::Message;

const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_BATCH: usize = 64;

/// Drop everything pending on the bus (the policy while disconnected);
/// returns true when the bus is closed and the output should exit.
/// Shared with the QUIC output.
pub(crate) fn drain_while_disconnected(rx: &mut broadcast::Receiver<Arc<Message>>) -> bool {
    loop {
        match rx.try_recv() {
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Empty) => return false,
            Err(broadcast::error::TryRecvError::Closed) => return true,
        }
    }
}

/// Collect one message (blocking) plus whatever else is immediately
/// available, as a single batch. Returns None when the bus closes.
/// Shared with the QUIC output.
pub(crate) async fn next_batch(
    rx: &mut broadcast::Receiver<Arc<Message>>,
    seq: u64,
) -> Option<Envelope> {
    let mut msgs: Vec<asf2::DecodedMessage> = Vec::new();
    loop {
        match rx.recv().await {
            Ok(m) => {
                msgs.push(asf2::DecodedMessage::from(&*m));
                break;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("asf2 output lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
    while msgs.len() < MAX_BATCH {
        match rx.try_recv() {
            Ok(m) => msgs.push(asf2::DecodedMessage::from(&*m)),
            Err(_) => break,
        }
    }
    Some(xng_proto::batch(seq, msgs))
}

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    url: String,
    station_id: String,
    station_ident: String,
) -> std::io::Result<()> {
    let mut seq: u64 = 0;
    let mut sent: u64 = 0;
    'reconnect: loop {
        let mut client = match AirframesFeedClient::connect(url.clone()).await {
            Ok(c) => c,
            Err(e) => {
                if drain_while_disconnected(&mut rx) {
                    tracing::info!("asf2 grpc output to {url}: session ended while disconnected");
                    return Ok(());
                }
                tracing::warn!("asf2 grpc connect to {url} failed: {e}; retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        let (tx, stream_rx) = tokio::sync::mpsc::channel::<Envelope>(64);
        let inbound = match client.stream(ReceiverStream::new(stream_rx)).await {
            Ok(r) => r.into_inner(),
            Err(e) => {
                if drain_while_disconnected(&mut rx) {
                    return Ok(());
                }
                tracing::warn!("asf2 grpc stream to {url} failed: {e}; retrying");
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        // Log server acks/hints in the background for this connection.
        let ack_task = tokio::spawn(async move {
            let mut inbound = inbound;
            while let Ok(Some(env)) = inbound.message().await {
                if let Some(asf2::envelope::Kind::Ack(a)) = env.kind {
                    tracing::debug!("asf2 grpc ack seq={}", a.seq);
                }
            }
        });

        if tx.send(xng_proto::hello(&station_id, &station_ident, "")).await.is_err() {
            tracing::warn!("asf2 grpc connection to {url} closed during hello");
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        }
        tracing::info!("asf2 grpc connected to {url}");

        loop {
            match next_batch(&mut rx, seq).await {
                Some(env) => {
                    let n = match &env.kind {
                        Some(asf2::envelope::Kind::Batch(b)) => b.messages.len() as u64,
                        _ => 0,
                    };
                    if tx.send(env).await.is_err() {
                        tracing::warn!("asf2 grpc connection to {url} lost; reconnecting");
                        ack_task.abort();
                        continue 'reconnect;
                    }
                    seq += 1;
                    sent += n;
                }
                None => {
                    // Bus closed: session over.
                    drop(tx);
                    let _ = ack_task.await;
                    tracing::info!("asf2 grpc output to {url}: {sent} messages in {seq} batches");
                    return Ok(());
                }
            }
        }
    }
}
