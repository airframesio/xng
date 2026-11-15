//! `xng ingest` — reference asf-2.0 ingest server (both transports).
//!
//! Accepts gRPC (tonic) and QUIC (quinn, self-signed dev certificate)
//! feeds, prints received messages, and acks batches. This is the
//! template for the Airframes Go/NATS-stack ingest: replace the printer
//! with a NATS publisher (subject per mode, e.g. `asf2.msg.acars`).

use super::{say, Reporter};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// What an ingest run received, reported when it stops.
#[derive(Clone, Debug, Default, Serialize)]
pub struct IngestReport {
    /// Feeders that introduced themselves (one hello per connection).
    pub feeders: u64,
    /// Messages received across every feeder.
    pub messages: u64,
}

/// Where received traffic is reported and counted.
#[derive(Clone)]
struct Sink {
    rep: Reporter,
    feeders: Arc<AtomicU64>,
    messages: Arc<AtomicU64>,
}

/// Handle one envelope; returns an ack for batches.
fn handle(env: Envelope, station: &mut String, transport: &str, sink: &Sink) -> Option<Envelope> {
    let rep = &sink.rep;
    match env.kind? {
        asf2::envelope::Kind::Hello(h) => {
            *station = h.station_ident.clone();
            sink.feeders.fetch_add(1, Ordering::Relaxed);
            say!(
                rep,
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
            sink.messages.fetch_add(b.messages.len() as u64, Ordering::Relaxed);
            for m in &b.messages {
                say!(rep, "{}", describe(m, station, transport));
            }
            Some(Envelope {
                kind: Some(asf2::envelope::Kind::Ack(asf2::Ack { seq: b.seq })),
            })
        }
        asf2::envelope::Kind::Stats(s) => {
            say!(
                rep,
                "[{transport}] {station} stats: {} msgs, {} channel(s)",
                s.messages_total,
                s.channels.len()
            );
            None
        }
        _ => None,
    }
}

struct FeedService {
    sink: Sink,
}

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
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let mut station = String::from("?");
            while let Some(env) = inbound.next().await {
                match env {
                    Ok(env) => {
                        if let Some(reply) = handle(env, &mut station, "grpc", &sink) {
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
            say!(sink.rep, "[grpc] {station} disconnected");
        });
        Ok(tonic::Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}

async fn quic_server(
    listen: std::net::SocketAddr,
    cert_out: Option<std::path::PathBuf>,
    sink: Sink,
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
        let sink = sink.clone();
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
                        handle(env, &mut station, "quic", &sink);
                    }
                    Err(e) => {
                        tracing::warn!("quic decode error from {peer}: {e}");
                        break;
                    }
                }
            }
            say!(sink.rep, "[quic] {station} disconnected");
        });
    }
    Ok(())
}

/// `xng ingest`: serve until Ctrl-C.
pub fn run(
    grpc: Option<String>,
    quic: Option<String>,
    quic_cert_out: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let rep = Reporter::stdout();
    let cancel = rep.cancel_flag().clone();
    ctrlc_flag(cancel);
    run_with(&rep, grpc, quic, quic_cert_out).map(|_| ())
}

/// Set `flag` on Ctrl-C, from a thread of its own (the server's runtime is
/// not up yet when the CLI asks for this).
fn ctrlc_flag(flag: Arc<std::sync::atomic::AtomicBool>) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        rt.block_on(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                flag.store(true, Ordering::Relaxed);
            }
        });
    });
}

/// Serve the ingest endpoints, reporting received traffic through `rep`,
/// until `rep` is cancelled. Fails at once if an address cannot be parsed,
/// and as soon as a listener stops (for example, its port is taken).
pub fn run_with(
    rep: &Reporter,
    grpc: Option<String>,
    quic: Option<String>,
    quic_cert_out: Option<&std::path::Path>,
) -> anyhow::Result<IngestReport> {
    anyhow::ensure!(grpc.is_some() || quic.is_some(), "enable at least one of --grpc / --quic");
    let sink = Sink { rep: rep.clone(), feeders: Default::default(), messages: Default::default() };
    let rt = tokio::runtime::Runtime::new()?;
    let result: anyhow::Result<()> = rt.block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        if let Some(addr) = grpc {
            let addr: std::net::SocketAddr =
                addr.parse().map_err(|e| anyhow::anyhow!("gRPC listen address {addr:?}: {e}"))?;
            tracing::info!("asf2 grpc ingest listening on {addr}");
            say!(rep, "Listening for gRPC feeds on {addr}");
            let service = FeedService { sink: sink.clone() };
            tasks.spawn(async move {
                tonic::transport::Server::builder()
                    .add_service(AirframesFeedServer::new(service))
                    .serve(addr)
                    .await
                    .map_err(|e| anyhow::anyhow!("gRPC listener on {addr}: {e:?}"))
            });
        }
        if let Some(addr) = quic {
            let addr: std::net::SocketAddr =
                addr.parse().map_err(|e| anyhow::anyhow!("QUIC listen address {addr:?}: {e}"))?;
            say!(rep, "Listening for QUIC feeds on {addr} (self-signed certificate)");
            let (cert_out, sink) = (quic_cert_out.map(|p| p.to_owned()), sink.clone());
            tasks.spawn(async move {
                quic_server(addr, cert_out, sink)
                    .await
                    .map_err(|e| anyhow::anyhow!("QUIC listener on {addr}: {e:#}"))
            });
        }
        loop {
            tokio::select! {
                // A listener only returns when it has failed or closed.
                Some(ended) = tasks.join_next() => {
                    ended.map_err(|e| anyhow::anyhow!("ingest task failed: {e}"))??;
                    anyhow::bail!("an ingest listener stopped unexpectedly");
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if rep.cancelled() {
                        tracing::info!("ingest shutting down");
                        tasks.abort_all();
                        return Ok(());
                    }
                }
            }
        }
    });
    rt.shutdown_background();
    result?;
    Ok(IngestReport {
        feeders: sink.feeders.load(Ordering::Relaxed),
        messages: sink.messages.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn stops_when_cancelled_and_reports() {
        let cancel = Arc::new(AtomicBool::new(false));
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rep = Reporter::new(
            {
                let lines = lines.clone();
                move |l| lines.lock().unwrap().push(l.to_owned())
            },
            cancel.clone(),
        );
        let t = std::thread::spawn(move || run_with(&rep, Some("127.0.0.1:0".into()), None, None));
        std::thread::sleep(std::time::Duration::from_millis(300));
        cancel.store(true, Ordering::Relaxed);
        let report = t.join().unwrap().unwrap();
        assert_eq!(report.messages, 0);
        assert!(lines.lock().unwrap().iter().any(|l| l.contains("gRPC")));
    }

    #[test]
    fn a_bad_address_is_refused() {
        let err = run_with(&Reporter::stdout(), Some("not-an-address".into()), None, None).unwrap_err();
        assert!(err.to_string().contains("gRPC listen address"), "{err}");
    }

    #[test]
    fn a_taken_port_fails_instead_of_waiting_forever() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        let err = run_with(&Reporter::stdout(), Some(addr), None, None).unwrap_err();
        assert!(err.to_string().contains("gRPC listener"), "{err}");
    }
}
