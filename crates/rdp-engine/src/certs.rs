//! Server certificate trust.
//!
//! RDP servers almost always present self-signed certificates, so chain
//! validation alone is useless. Certificates that pass platform validation are
//! accepted as-is; others are pinned per endpoint (`host:port`) by SHA-256
//! fingerprint, like SSH known_hosts.

use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CertPolicy {
    /// Pin the first certificate seen; refuse a different one later.
    #[default]
    TrustOnFirstUse,
    /// Ask before trusting a certificate never seen before.
    Prompt,
    /// Accept any certificate (no protection against interception).
    Ignore,
}

pub enum CertDecision {
    Trusted,
    Unknown,
    Changed { previous: String },
}

/// Remembered certificates, provided by the host.
pub trait CertificateStore: Send + Sync {
    fn check(&self, endpoint: &str, fingerprint: &str) -> CertDecision;
    fn trust(&self, endpoint: &str, fingerprint: &str, subject: &str);
}

/// Why a certificate was refused.
#[derive(Debug, Clone)]
pub struct CertIssue {
    pub endpoint: String,
    pub fingerprint: String,
    pub subject: String,
    pub changed: bool,
    pub previous: Option<String>,
}

/// SHA-256 fingerprint as `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn subject(der: &[u8]) -> String {
    use x509_cert::der::Decode as _;
    x509_cert::Certificate::from_der(der)
        .map(|c| c.tbs_certificate().subject().to_string())
        .unwrap_or_default()
}

/// Builds the TLS validation callback for one connection attempt. A refused
/// certificate is recorded in `issue` so the session can report it.
pub(crate) fn callback(
    store: Arc<dyn CertificateStore>,
    policy: CertPolicy,
    issue: Arc<Mutex<Option<CertIssue>>>,
) -> ironrdp_tls::CertificateValidationCallback {
    Arc::new(move |der: &[u8], endpoint: &str, _error: &str| {
        let fp = fingerprint(der);
        match store.check(endpoint, &fp) {
            CertDecision::Trusted => true,
            CertDecision::Unknown if policy == CertPolicy::TrustOnFirstUse => {
                store.trust(endpoint, &fp, &subject(der));
                true
            }
            CertDecision::Unknown => {
                *issue.lock() = Some(CertIssue {
                    endpoint: endpoint.to_owned(),
                    fingerprint: fp,
                    subject: subject(der),
                    changed: false,
                    previous: None,
                });
                false
            }
            CertDecision::Changed { previous } => {
                *issue.lock() = Some(CertIssue {
                    endpoint: endpoint.to_owned(),
                    fingerprint: fp,
                    subject: subject(der),
                    changed: true,
                    previous: Some(previous),
                });
                false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"abc");
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert!(fp.starts_with("BA:78:16:BF"));
    }
}
