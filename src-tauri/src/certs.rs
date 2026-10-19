//! Pinned server certificates (`certs.json` in the app data directory).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rdp_engine::{CertDecision, CertificateStore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedCertificate {
    pub endpoint: String,
    pub fingerprint: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub first_seen: u64,
}

pub struct CertStore {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, TrustedCertificate>>,
}

fn normalize(endpoint: &str) -> String {
    endpoint.trim().to_lowercase()
}

impl CertStore {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("certs.json");
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<TrustedCertificate>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| (normalize(&c.endpoint), c))
            .collect();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    fn save(&self, entries: &BTreeMap<String, TrustedCertificate>) {
        let list: Vec<&TrustedCertificate> = entries.values().collect();
        if let Ok(json) = serde_json::to_vec_pretty(&list) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn list(&self) -> Vec<TrustedCertificate> {
        self.entries.lock().values().cloned().collect()
    }

    pub fn set(&self, endpoint: &str, fingerprint: &str, subject: &str) {
        let mut entries = self.entries.lock();
        entries.insert(
            normalize(endpoint),
            TrustedCertificate {
                endpoint: endpoint.trim().to_owned(),
                fingerprint: fingerprint.trim().to_uppercase(),
                subject: subject.to_owned(),
                first_seen: crate::store::now_ms(),
            },
        );
        self.save(&entries);
    }

    pub fn forget(&self, endpoint: &str) -> bool {
        let mut entries = self.entries.lock();
        let removed = entries.remove(&normalize(endpoint)).is_some();
        if removed {
            self.save(&entries);
        }
        removed
    }
}

impl CertificateStore for CertStore {
    fn check(&self, endpoint: &str, fingerprint: &str) -> CertDecision {
        match self.entries.lock().get(&normalize(endpoint)) {
            None => CertDecision::Unknown,
            Some(c) if c.fingerprint.eq_ignore_ascii_case(fingerprint) => CertDecision::Trusted,
            Some(c) => CertDecision::Changed {
                previous: c.fingerprint.clone(),
            },
        }
    }

    fn trust(&self, endpoint: &str, fingerprint: &str, subject: &str) {
        self.set(endpoint, fingerprint, subject);
    }
}
