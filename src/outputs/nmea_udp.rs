//! Raw NMEA-over-UDP output: AIVDM sentences as datagrams, the format
//! marine aggregators (MarineTraffic, AISHub, VesselFinder) ingest.

use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    target: String,
) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(&target).await?;
    tracing::info!("NMEA UDP output to {target}");
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let MessageBody::Ais { nmea, .. } = &msg.body else { continue };
                if !msg.decode.crc_ok {
                    continue;
                }
                for sentence in nmea {
                    let _ = sock.send(format!("{sentence}\r\n").as_bytes()).await;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return Ok(()),
        }
    }
}
