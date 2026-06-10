//! asf-2.0 QUIC output: length-prefixed Envelopes over one bidirectional
//! quinn stream (ALPN `asf2`).
//!
//! Certificate verification is ON by default (system roots). For
//! self-hosted ingests with self-signed certificates, pin the server
//! certificate with `--asf2-quic-ca` (see `xng ingest --quic-cert-out`).
//! `--asf2-quic-insecure` disables verification entirely and exists for
//! throwaway lab setups only.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use xng_types::Message;

const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// How the client validates the ingest's certificate.
#[derive(Clone)]
pub enum TrustMode {
    /// System/webpki roots (public ingests, e.g. feed.airframes.io).
    SystemRoots,
    /// Trust exactly the certificate(s) in this PEM file (self-signed
    /// ingest pinning).
    CaFile(PathBuf),
    /// No verification. Lab use only.
    Insecure,
}

/// rustls verifier that accepts any server certificate. Only reachable
/// via the explicit `--asf2-quic-insecure` flag.
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

fn client_endpoint(trust: &TrustMode) -> anyhow::Result<quinn::Endpoint> {
    let provider = rustls::crypto::ring::default_provider();
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(provider.clone()))
        .with_safe_default_protocol_versions()?;

    let mut crypto = match trust {
        TrustMode::SystemRoots => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        TrustMode::CaFile(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| anyhow::anyhow!("cannot read --asf2-quic-ca {}: {e}", path.display()))?;
            let mut roots = rustls::RootCertStore::empty();
            let mut added = 0;
            for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
                roots.add(cert?)?;
                added += 1;
            }
            anyhow::ensure!(added > 0, "no certificates found in {}", path.display());
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        TrustMode::Insecure => {
            tracing::warn!(
                "asf2 quic: TLS certificate verification is DISABLED \
                 (--asf2-quic-insecure). Feeds can be intercepted or spoofed; \
                 use --asf2-quic-ca with the ingest's certificate instead."
            );
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
                .with_no_client_auth()
        }
    };
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
    // SNI / certificate name = the host part of the target.
    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
    let conn = endpoint.connect(addr, host)?.await?;
    let (send, _recv) = conn.open_bi().await?;
    Ok(send)
}

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    target: String,
    trust: TrustMode,
    station_id: String,
    station_ident: String,
) -> std::io::Result<()> {
    if matches!(trust, TrustMode::Insecure) {
        let loopback = target.starts_with("127.") || target.starts_with("localhost:") || target.starts_with("[::1]");
        if !loopback {
            tracing::warn!(
                "asf2 quic: --asf2-quic-insecure with a non-loopback target ({target}) — \
                 anyone on the path can read or forge this feed"
            );
        }
    }
    let endpoint = match client_endpoint(&trust) {
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
                if super::asf2_grpc::drain_while_disconnected(&mut rx) {
                    tracing::info!("asf2 quic output to {target}: session ended while disconnected");
                    return Ok(());
                }
                tracing::warn!("asf2 quic connect to {target} failed: {e}; retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        let hello = xng_proto::frame_envelope(&xng_proto::hello(&station_id, &station_ident, ""));
        if stream.write_all(&hello).await.is_err() {
            if super::asf2_grpc::drain_while_disconnected(&mut rx) {
                return Ok(());
            }
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
