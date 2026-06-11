//! NMEA-over-TCP server: AIVDM sentences to every connected client —
//! the pull-style transport AIS tools (OpenCPN, aggregator pollers)
//! expect alongside the UDP push.

use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

pub async fn run(rx: broadcast::Receiver<Arc<Message>>, addr: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("NMEA TCP output on {addr}");
    loop {
        let (mut sock, peer) = listener.accept().await?;
        tracing::info!("NMEA client connected: {peer}");
        let mut rx = rx.resubscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let MessageBody::Ais { nmea, .. } = &msg.body else { continue };
                        if !msg.decode.crc_ok {
                            continue;
                        }
                        for sentence in nmea {
                            if sock
                                .write_all(format!("{sentence}\r\n").as_bytes())
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
        });
    }
}
