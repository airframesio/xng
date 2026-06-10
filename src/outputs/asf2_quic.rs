//! asf-2.0 QUIC output: length-prefixed Envelopes over one bidirectional
//! quinn stream (ALPN `asf2`). Certificate verification is skipped (dev
//! mode) — production trust configuration arrives with deployment.

use std::sync::Arc;
use tokio::sync::broadcast;
use xng_types::Message;

const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// rustls verifier that accepts any server certificate (dev/self-signed).
#[derive(Debug)]
struct AcceptAnyCert(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn client_endpoint() -> anyhow::Result<quinn::Endpoint> {
    let provider = rustls::crypto::ring::default_provider();
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(provider.clone()))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![xng_proto::ALPN.to_vec()];
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    )));
    Ok(endpoint)
}

async fn connect(endpoint: &quinn::Endpoint, target: &str) -> anyhow::Result<quinn::SendStream> {
    let addr = tokio::net::lookup_host(target)
        .await?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {target}"))?;
    let conn = endpoint.connect(addr, "asf2-ingest")?.await?;
    let (send, _recv) = conn.open_bi().await?;
    Ok(send)
}

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    target: String,
    station_id: String,
    station_ident: String,
) -> std::io::Result<()> {
    let endpoint = match client_endpoint() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("asf2 quic endpoint setup failed: {e}");
            return Ok(());
        }
    };
    let mut seq: u64 = 0;
    let mut sent: u64 = 0;
    'reconnect: loop {
        let mut stream = match connect(&endpoint, &target).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("asf2 quic connect to {target} failed: {e}; retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        let hello = xng_proto::frame_envelope(&xng_proto::hello(&station_id, &station_ident, ""));
        if stream.write_all(&hello).await.is_err() {
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        }
        tracing::info!("asf2 quic connected to {target}");

        loop {
            match super::asf2_grpc::next_batch(&mut rx, seq).await {
                Some(env) => {
                    let n = match &env.kind {
                        Some(xng_proto::asf2::envelope::Kind::Batch(b)) => b.messages.len() as u64,
                        _ => 0,
                    };
                    if stream.write_all(&xng_proto::frame_envelope(&env)).await.is_err() {
                        tracing::warn!("asf2 quic connection to {target} lost; reconnecting");
                        continue 'reconnect;
                    }
                    seq += 1;
                    sent += n;
                }
                None => {
                    let _ = stream.finish();
                    // Give the peer a moment to read the tail.
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    tracing::info!("asf2 quic output to {target}: {sent} messages in {seq} batches");
                    return Ok(());
                }
            }
        }
    }
}
