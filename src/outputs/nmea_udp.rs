//! NMEA-over-UDP output: AIVDM sentences pushed as datagrams to a single
//! target — the fire-and-forget transport AIS aggregators/plotters expect
//! alongside the pull-style TCP server. Optionally prefixes each sentence
//! with an NMEA tag-block (`\s:<station>,c:<unix_ts>*HH\`).

use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    target: String,
    tag_blocks: bool,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    tracing::info!("NMEA UDP output to {target}");
    let mut sent: u64 = 0;
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let MessageBody::Ais { nmea, .. } = &msg.body else { continue };
                if !msg.decode.crc_ok {
                    continue;
                }
                let prefix = tag_blocks
                    .then(|| {
                        xng_mode_ais::nmea::tag_block(
                            msg.source.station.ident.as_str(),
                            msg.timestamp.timestamp(),
                        )
                    })
                    .unwrap_or_default();
                for sentence in nmea {
                    let line = format!("{prefix}{sentence}\r\n");
                    if let Err(e) = socket.send_to(line.as_bytes(), &target).await {
                        tracing::warn!("nmea udp send to {target} failed: {e}");
                    } else {
                        sent += 1;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    tracing::info!("nmea udp output to {target}: {sent} sentences sent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_types::{
        AppInfo, DecodeQuality, Mode, Provenance, SignalQuality, StationIdentity,
    };

    fn ais_msg(sentence: &str) -> Message {
        Message {
            mode: Mode::Ais,
            timestamp: chrono::DateTime::from_timestamp(1_577_836_800, 0).unwrap(),
            frequency_hz: 161_975_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::Ais {
                nmea: vec![sentence.to_string()],
                msg_type: Some(1),
                mmsi: Some(123_456_789),
                details: None,
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("XX-TEST"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    // The sink pushes each AIVDM sentence as a datagram; with tag_blocks on,
    // the published station id + timestamp prefix it. Round-trips over a real
    // loopback socket so the transport + prefixing are both exercised.
    #[tokio::test]
    async fn pushes_sentence_with_tag_block() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = listener.local_addr().unwrap().to_string();
        let (tx, rx) = broadcast::channel(8);
        tokio::spawn(super::run(rx, target, true));
        // Give the sink a moment to subscribe before publishing.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tx.send(Arc::new(ais_msg("!AIVDM,1,1,,A,15M,0*7B"))).unwrap();

        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv(&mut buf))
            .await
            .expect("datagram within 2s")
            .unwrap();
        let got = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(got.starts_with("\\s:XX-TEST,c:1577836800*"), "{got:?}");
        assert!(got.contains("!AIVDM,1,1,,A,15M,0*7B"), "{got:?}");
        assert!(got.ends_with("\r\n"), "{got:?}");
    }
}
