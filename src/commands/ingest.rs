//! `xng ingest` — reference asf-2.0 ingest server (both transports).
//!
//! Accepts gRPC (tonic) and QUIC (quinn, self-signed dev certificate)
//! feeds, prints received messages, and acks batches. This is the
//! template for the Airframes Go/NATS-stack ingest: replace the printer
//! with a NATS publisher (subject per mode, e.g. `asf2.msg.acars`).

use std::sync::Arc;
use tokio_stream::StreamExt;
use xng_proto::asf2::airframes_feed_server::{AirframesFeed, AirframesFeedServer};
use xng_proto::asf2::{self, Envelope};

fn describe(m: &asf2::DecodedMessage, station: &str, transport: &str) -> String {
    let freq = m.frequency_hz as f64 / 1e6;
    let body = match &m.body {
        Some(asf2::decoded_message::Body::Acars(a)) => format!(
            "ACARS {} {} lbl={}{}",
            a.tail.as_deref().unwrap_or("-"),
            a.flight.as_deref().unwrap_or("-"),
            a.label,
            if a.text.is_empty() { String::new() } else { format!(" | {}", a.text.replace('\n', "·")) }
        ),
        Some(asf2::decoded_message::Body::Ais(a)) => format!(
            "AIS type={} mmsi={}",
            a.msg_type.unwrap_or(0),
            a.mmsi.unwrap_or(0)
        ),
        Some(asf2::decoded_message::Body::ModeS(s)) => format!(
            "MODE-S df={} icao={}{}{}",
            s.df,
            s.icao.as_deref().unwrap_or("-"),
            s.callsign.as_deref().map(|c| format!(" ident={c}")).unwrap_or_default(),
            s.altitude_ft.map(|a| format!(" alt={a}ft")).unwrap_or_default(),
        ),
        _ => format!("FRAME ({} raw bytes)", m.raw.len()),
    };
    format!("[{transport}] {station} {} {:.3} MHz {}", m.mode, freq, body)
}

/// Handle one envelope; returns an ack for batches.
fn handle(env: Envelope, station: &mut String, transport: &str) -> Option<Envelope> {
    match env.kind? {
        asf2::envelope::Kind::Hello(h) => {
            *station = h.station_ident.clone();
            println!(
                "[{transport}] hello from {} ({}) — {} v{} (proto v{})",
                h.station_ident,
                h.station_id,
                h.app.as_ref().map(|a| a.name.as_str()).unwrap_or("?"),
                h.app.as_ref().map(|a| a.version.as_str()).unwrap_or("?"),
                h.protocol_version
            );
            None
        }
        asf2::envelope::Kind::Batch(b) => {
            for m in &b.messages {
                println!("{}", describe(m, station, transport));
            }
            Some(Envelope {
                kind: Some(asf2::envelope::Kind::Ack(asf2::Ack { seq: b.seq })),
            })
        }
        asf2::envelope::Kind::Stats(s) => {
            println!(
                "[{transport}] {station} stats: {} msgs, {} channel(s)",
                s.messages_total,
                s.channels.len()
            );
            None
        }
        _ => None,
    }
}

struct FeedService;

#[tonic::async_trait]
impl AirframesFeed for FeedService {
    type StreamStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<Envelope, tonic::Status>> + Send>>;

    async fn stream(
        &self,
        request: tonic::Request<tonic::Streaming<Envelope>>,
    ) -> Result<tonic::Response<Self::StreamStream>, tonic::Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let mut station = String::from("?");
            while let Some(env) = inbound.next().await {
                match env {
                    Ok(env) => {
                        if let Some(reply) = handle(env, &mut station, "grpc") {
                            if tx.send(Ok(reply)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("grpc stream ended: {e}");
                        break;
                    }
                }
            }
            println!("[grpc] {station} disconnected");
        });
        Ok(tonic::Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}

async fn quic_server(
    listen: std::net::SocketAddr,
    cert_out: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let cert = rcgen::generate_simple_self_signed(vec![
        "asf2-ingest".into(),
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ])?;
    if let Some(path) = &cert_out {
        std::fs::write(path, cert.cert.pem())?;
        tracing::info!(
            "quic certificate written to {} — feeders pin it with --asf2-quic-ca",
            path.display()
        );
    }
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|e| anyhow::anyhow!("key encoding: {e}"))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    crypto.alpn_protocols = vec![xng_proto::ALPN.to_vec()];

    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(crypto)?,
    ));
    let endpoint = quinn::Endpoint::server(server_config, listen)?;
    tracing::info!("asf2 quic ingest listening on {listen} (self-signed dev certificate)");

    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            let peer = conn.remote_address();
            let Ok((_send, mut recv)) = conn.accept_bi().await else { return };
            let mut station = String::from("?");
            loop {
                let mut len_buf = [0u8; 4];
                if recv.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > 16 * 1024 * 1024 {
                    tracing::warn!("quic frame too large from {peer}: {len}");
                    break;
                }
                let mut body = vec![0u8; len];
                if recv.read_exact(&mut body).await.is_err() {
                    break;
                }
                match <Envelope as prost::Message>::decode(&body[..]) {
                    Ok(env) => {
                        handle(env, &mut station, "quic");
                    }
                    Err(e) => {
                        tracing::warn!("quic decode error from {peer}: {e}");
                        break;
                    }
                }
            }
            println!("[quic] {station} disconnected");
        });
    }
    Ok(())
}

pub fn run(
    grpc: Option<String>,
    quic: Option<String>,
    quic_cert_out: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    anyhow::ensure!(grpc.is_some() || quic.is_some(), "enable at least one of --grpc / --quic");
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut tasks = Vec::new();
        if let Some(addr) = grpc {
            let addr: std::net::SocketAddr = addr.parse()?;
            tracing::info!("asf2 grpc ingest listening on {addr}");
            tasks.push(tokio::spawn(async move {
                tonic::transport::Server::builder()
                    .add_service(AirframesFeedServer::new(FeedService))
                    .serve(addr)
                    .await
                    .map_err(anyhow::Error::from)
            }));
        }
        if let Some(addr) = quic {
            let addr: std::net::SocketAddr = addr.parse()?;
            tasks.push(tokio::spawn(quic_server(addr, quic_cert_out.map(|p| p.to_owned()))));
        }
        tokio::signal::ctrl_c().await?;
        tracing::info!("ingest shutting down");
        for t in tasks {
            t.abort();
        }
        Ok(())
    })
}
