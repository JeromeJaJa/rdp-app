//! TLS for the direct link: a throwaway certificate per link, and peers pinned by the
//! fingerprint they sent over RDP (no certificate authorities involved).

use std::sync::Arc;

use anyhow::Context as _;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

/// A self-signed certificate and its key, made for one link.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Identity {
    pub fn generate() -> anyhow::Result<Self> {
        let made =
            rcgen::generate_simple_self_signed(vec![super::SERVER_NAME.to_owned()]).context("make a certificate")?;
        Ok(Self {
            cert: made.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(made.key_pair.serialize_der()),
        })
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        fingerprint(&self.cert)
    }

    fn chain(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        (vec![self.cert.clone()], PrivateKeyDer::Pkcs8(self.key.clone_key()))
    }
}

fn fingerprint(cert: &CertificateDer<'_>) -> [u8; 32] {
    Sha256::digest(cert.as_ref()).into()
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts exactly one certificate: the one whose fingerprint came over RDP.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl Pinned {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if fingerprint(cert) == self.fingerprint {
            Ok(())
        } else {
            Err(rustls::Error::General(
                "not the certificate the companion channel named".into(),
            ))
        }
    }
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity).map(|()| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for Pinned {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity).map(|()| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn pinned(fingerprint: [u8; 32], provider: &CryptoProvider) -> Arc<Pinned> {
    Arc::new(Pinned {
        fingerprint,
        algorithms: provider.signature_verification_algorithms,
    })
}

/// The client's QUIC TLS: presents `identity`, accepts only the server `peer` names.
pub(crate) fn client(identity: &Identity, peer: [u8; 32]) -> anyhow::Result<QuicClientConfig> {
    let provider = provider();
    let (chain, key) = identity.chain();
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(pinned(peer, &provider))
        .with_client_auth_cert(chain, key)?;
    config.alpn_protocols = vec![super::ALPN.to_vec()];
    Ok(QuicClientConfig::try_from(config)?)
}

/// The companion's QUIC TLS: presents `identity`, requires the client `peer` names.
pub(crate) fn server(identity: &Identity, peer: [u8; 32]) -> anyhow::Result<QuicServerConfig> {
    let provider = provider();
    let (chain, key) = identity.chain();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(pinned(peer, &provider))
        .with_single_cert(chain, key)?;
    config.alpn_protocols = vec![super::ALPN.to_vec()];
    Ok(QuicServerConfig::try_from(config)?)
}
