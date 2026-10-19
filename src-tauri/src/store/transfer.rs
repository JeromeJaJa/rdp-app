//! Import / export of groups and connections.
//!
//! JSON format (`"format": "rdp-app/export"`, version 1) keeps everything:
//! groups, gateways, notes. Passwords are omitted, written in plain text, or
//! encrypted per field with AES-256-GCM under a key derived from a passphrase
//! (PBKDF2-HMAC-SHA256). The text format is bulk lines (`host:port:user:pass`)
//! with `# Group: <name>` headers.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

use super::{DEFAULT_GROUP, GatewayInput, NewConnection, StoredGateway, Store, now_ms};
use crate::{bulk, dpapi};

const FORMAT: &str = "rdp-app/export";
const KDF_ITERATIONS: u32 = 310_000;
const CHECK_PLAINTEXT: &str = "rdp-app passphrase check";

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Json,
    Text,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PasswordMode {
    #[default]
    None,
    Encrypted,
    Plain,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    #[serde(default)]
    pub group_ids: Option<Vec<String>>,
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub passwords: PasswordMode,
    #[serde(default)]
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSource {
    pub text: String,
    #[serde(default)]
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    #[default]
    Merge,
    Replace,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    #[serde(default)]
    pub mode: ImportMode,
    /// "byName" or a group id.
    #[serde(default = "by_name")]
    pub target: String,
}

fn by_name() -> String {
    "byName".into()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewGroup {
    pub name: String,
    pub connections: usize,
    pub existing_group_id: Option<String>,
    pub duplicates: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub format: &'static str,
    pub groups: Vec<PreviewGroup>,
    pub total: usize,
    pub needs_passphrase: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportResult {
    pub added: usize,
    pub updated: usize,
    /// Ids of connections removed by a replace import (sessions should be closed).
    pub removed: Vec<String>,
    pub groups_created: usize,
}

// ── Document ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Doc {
    format: String,
    version: u32,
    #[serde(default)]
    exported_at: String,
    #[serde(default = "default_password_mode")]
    passwords: PasswordMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encryption: Option<Encryption>,
    groups: Vec<DocGroup>,
}

fn default_password_mode() -> PasswordMode {
    PasswordMode::Plain
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Encryption {
    kdf: String,
    iterations: u32,
    salt: String,
    cipher: String,
    /// `CHECK_PLAINTEXT` sealed with the key, to tell a wrong passphrase apart.
    check: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocGroup {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway: Option<DocGateway>,
    #[serde(default)]
    connections: Vec<DocConnection>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocConnection {
    #[serde(default)]
    name: String,
    host: String,
    #[serde(default = "super::default_port")]
    port: u16,
    #[serde(default)]
    username: String,
    #[serde(default)]
    domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway: Option<DocGateway>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct DocGateway {
    enabled: bool,
    host: String,
    same_credentials: bool,
    username: String,
    domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

// ── Crypto ──────────────────────────────────────────────────────────────────

struct Key(Aes256Gcm);

impl Key {
    fn derive(passphrase: &str, salt: &[u8], iterations: u32) -> Self {
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
        Self(Aes256Gcm::new_from_slice(&key).expect("32-byte key"))
    }

    fn seal(&self, plaintext: &str) -> String {
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ct = self
            .0
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .expect("AES-GCM encryption cannot fail");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        B64.encode(out)
    }

    fn open(&self, sealed: &str) -> anyhow::Result<String> {
        let bytes = B64.decode(sealed).context("invalid encrypted value")?;
        anyhow::ensure!(bytes.len() > 12, "invalid encrypted value");
        let pt = self
            .0
            .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
            .map_err(|_| anyhow!("wrong passphrase"))?;
        String::from_utf8(pt).context("invalid encrypted value")
    }
}

// ── Export ──────────────────────────────────────────────────────────────────

impl Store {
    pub fn export(&self, options: &ExportOptions) -> anyhow::Result<String> {
        let groups: Vec<&super::StoredGroup> = match &options.group_ids {
            Some(ids) => self.data.groups.iter().filter(|g| ids.contains(&g.id)).collect(),
            None => self.data.groups.iter().collect(),
        };
        if groups.is_empty() {
            bail!("no matching groups to export");
        }
        let reveal = |sealed: &str| -> anyhow::Result<Option<String>> {
            if sealed.is_empty() || options.passwords == PasswordMode::None {
                Ok(None)
            } else {
                dpapi::unprotect(sealed).map(Some)
            }
        };

        match options.format {
            Format::Text => {
                if options.passwords == PasswordMode::Encrypted {
                    bail!("text exports cannot encrypt passwords; use JSON or omit them");
                }
                let mut out = String::new();
                for g in groups {
                    out.push_str(&format!("# Group: {}\n", g.name));
                    for c in self.data.connections.iter().filter(|c| c.group() == g.id) {
                        let pw = reveal(&c.password)?;
                        out.push_str(&bulk::format_line(&c.host, c.port, &c.username, &c.domain, pw.as_deref()));
                        out.push('\n');
                    }
                    out.push('\n');
                }
                Ok(out)
            }
            Format::Json => {
                let (key, encryption) = if options.passwords == PasswordMode::Encrypted {
                    let passphrase = options
                        .passphrase
                        .as_deref()
                        .filter(|p| !p.is_empty())
                        .context("a passphrase is required to encrypt passwords")?;
                    let mut salt = [0u8; 16];
                    rand::rngs::OsRng.fill_bytes(&mut salt);
                    let key = Key::derive(passphrase, &salt, KDF_ITERATIONS);
                    let check = key.seal(CHECK_PLAINTEXT);
                    (
                        Some(key),
                        Some(Encryption {
                            kdf: "pbkdf2-sha256".into(),
                            iterations: KDF_ITERATIONS,
                            salt: B64.encode(salt),
                            cipher: "aes-256-gcm".into(),
                            check,
                        }),
                    )
                } else {
                    (None, None)
                };
                let protect = |sealed: &str| -> anyhow::Result<Option<String>> {
                    Ok(reveal(sealed)?.map(|p| match &key {
                        Some(k) => k.seal(&p),
                        None => p,
                    }))
                };
                let gateway = |g: &StoredGateway| -> anyhow::Result<DocGateway> {
                    Ok(DocGateway {
                        enabled: g.enabled,
                        host: g.host.clone(),
                        same_credentials: g.same_credentials,
                        username: g.username.clone(),
                        domain: g.domain.clone(),
                        password: protect(&g.password)?,
                    })
                };
                let mut doc_groups = Vec::new();
                for g in groups {
                    let mut connections = Vec::new();
                    for c in self.data.connections.iter().filter(|c| c.group() == g.id) {
                        connections.push(DocConnection {
                            name: c.name.clone(),
                            host: c.host.clone(),
                            port: c.port,
                            username: c.username.clone(),
                            domain: c.domain.clone(),
                            password: protect(&c.password)?,
                            notes: c.notes.clone(),
                            gateway: c.gateway.as_ref().map(gateway).transpose()?,
                        });
                    }
                    doc_groups.push(DocGroup {
                        name: g.name.clone(),
                        gateway: g.gateway.as_ref().map(gateway).transpose()?,
                        connections,
                    });
                }
                let doc = Doc {
                    format: FORMAT.into(),
                    version: 1,
                    exported_at: rfc3339(now_ms()),
                    passwords: options.passwords,
                    encryption,
                    groups: doc_groups,
                };
                Ok(serde_json::to_string_pretty(&doc)?)
            }
        }
    }
}

// ── Import ──────────────────────────────────────────────────────────────────

/// A group of entries read from an import source.
struct ImportGroup {
    name: Option<String>,
    gateway: Option<GatewayInput>,
    entries: Vec<NewConnection>,
}

struct Parsed {
    format: &'static str,
    groups: Vec<ImportGroup>,
    needs_passphrase: bool,
    errors: Vec<String>,
}

fn parse_source(source: &ImportSource) -> anyhow::Result<Parsed> {
    let text = source.text.trim_start_matches('\u{feff}');
    if text.trim_start().starts_with('{') {
        let doc: Doc = serde_json::from_str(text).context("not a valid RDP App export")?;
        if doc.format != FORMAT {
            bail!("unsupported export format {:?}", doc.format);
        }
        if doc.version > 1 {
            bail!("export version {} is newer than this app supports", doc.version);
        }

        let key = match (&doc.encryption, doc.passwords) {
            (Some(enc), PasswordMode::Encrypted) => match source.passphrase.as_deref().filter(|p| !p.is_empty()) {
                None => None,
                Some(passphrase) => {
                    let salt = B64.decode(&enc.salt).context("invalid salt")?;
                    let key = Key::derive(passphrase, &salt, enc.iterations.max(1));
                    match key.open(&enc.check) {
                        Ok(check) if check == CHECK_PLAINTEXT => Some(key),
                        _ => None,
                    }
                }
            },
            _ => None,
        };
        let encrypted = doc.passwords == PasswordMode::Encrypted;
        let needs_passphrase = encrypted && key.is_none();
        let reveal = |p: Option<String>| -> Option<String> {
            let p = p.filter(|p| !p.is_empty())?;
            if encrypted { key.as_ref().and_then(|k| k.open(&p).ok()) } else { Some(p) }
        };
        let gateway = |g: DocGateway| GatewayInput {
            enabled: g.enabled,
            host: g.host,
            same_credentials: g.same_credentials,
            username: g.username,
            domain: g.domain,
            password: reveal(g.password),
        };

        let groups = doc
            .groups
            .into_iter()
            .map(|g| ImportGroup {
                name: Some(g.name),
                gateway: g.gateway.map(gateway),
                entries: g
                    .connections
                    .into_iter()
                    .map(|c| NewConnection {
                        name: Some(c.name).filter(|n| !n.is_empty()),
                        host: c.host,
                        port: Some(c.port),
                        username: c.username,
                        domain: Some(c.domain),
                        password: reveal(c.password),
                        notes: Some(c.notes).filter(|n| !n.is_empty()),
                        gateway: c.gateway.map(gateway),
                        ..Default::default()
                    })
                    .collect(),
            })
            .collect();
        return Ok(Parsed {
            format: "json",
            groups,
            needs_passphrase,
            errors: Vec::new(),
        });
    }

    let parsed = bulk::parse(text);
    let mut groups: Vec<ImportGroup> = Vec::new();
    for (name, entry) in parsed.entries {
        let item = NewConnection::from_entry(entry, None);
        match groups.last_mut() {
            Some(g) if g.name == name => g.entries.push(item),
            _ => groups.push(ImportGroup {
                name,
                gateway: None,
                entries: vec![item],
            }),
        }
    }
    Ok(Parsed {
        format: "text",
        groups,
        needs_passphrase: false,
        errors: parsed
            .errors
            .into_iter()
            .map(|e| format!("Line {}: {}", e.line, e.error))
            .collect(),
    })
}

/// Where an import group's entries go.
enum Target {
    Existing(String),
    New(String),
}

impl Store {
    fn resolve(&self, options: &ImportOptions, group: &ImportGroup) -> anyhow::Result<Target> {
        if options.target == "byName" {
            Ok(match &group.name {
                None => Target::Existing(DEFAULT_GROUP.into()),
                Some(name) => match self.group_by_name(name) {
                    Some(id) => Target::Existing(id),
                    None => Target::New(name.clone()),
                },
            })
        } else if self.data.groups.iter().any(|g| g.id == options.target) {
            Ok(Target::Existing(options.target.clone()))
        } else {
            bail!("target group not found")
        }
    }

    pub fn preview_import(&self, source: &ImportSource, options: &ImportOptions) -> anyhow::Result<ImportPreview> {
        let parsed = parse_source(source)?;
        let mut groups: Vec<PreviewGroup> = Vec::new();
        let mut total = 0;
        for g in &parsed.groups {
            let target = self.resolve(options, g)?;
            let existing_id = match &target {
                Target::Existing(id) => Some(id.clone()),
                Target::New(_) => None,
            };
            let display = match (&target, &g.name) {
                (Target::Existing(id), _) if options.target != "byName" => {
                    self.group(id).map(|x| x.name).unwrap_or_default()
                }
                (_, Some(name)) => name.clone(),
                (_, None) => "All".into(),
            };
            let existing_keys: Vec<String> = match &existing_id {
                Some(id) => self
                    .data
                    .connections
                    .iter()
                    .filter(|c| c.group() == id)
                    .map(|c| c.key())
                    .collect(),
                None => Vec::new(),
            };
            let duplicates = g
                .entries
                .iter()
                .filter(|e| existing_keys.contains(&entry_key(e)))
                .count();
            total += g.entries.len();
            // Several import groups may land in one target: fold them together.
            match groups
                .iter_mut()
                .find(|p| p.existing_group_id.is_some() && p.existing_group_id == existing_id)
            {
                Some(p) => {
                    p.connections += g.entries.len();
                    p.duplicates += duplicates;
                }
                None => groups.push(PreviewGroup {
                    name: display,
                    connections: g.entries.len(),
                    existing_group_id: existing_id,
                    duplicates,
                }),
            }
        }
        Ok(ImportPreview {
            format: parsed.format,
            groups,
            total,
            needs_passphrase: parsed.needs_passphrase,
            errors: parsed.errors,
        })
    }

    pub fn apply_import(&mut self, source: &ImportSource, options: &ImportOptions) -> anyhow::Result<ImportResult> {
        let parsed = parse_source(source)?;
        if parsed.needs_passphrase {
            bail!(if source.passphrase.as_deref().is_some_and(|p| !p.is_empty()) {
                "wrong passphrase"
            } else {
                "this export has encrypted passwords: a passphrase is required"
            });
        }

        // Group entries by target group id, creating groups as needed.
        let mut result = ImportResult::default();
        let mut buckets: Vec<(String, Vec<NewConnection>)> = Vec::new();
        for g in parsed.groups {
            let id = match self.resolve(options, &g)? {
                Target::Existing(id) => {
                    if let Some(gw) = g.gateway {
                        let stored = self.data.groups.iter_mut().find(|x| x.id == id);
                        if let Some(stored) = stored.filter(|s| s.gateway.is_none() && s.id != DEFAULT_GROUP) {
                            stored.gateway = Some(StoredGateway::from_input(None, gw)?);
                        }
                    }
                    id
                }
                Target::New(name) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    self.data.groups.push(super::StoredGroup {
                        id: id.clone(),
                        name,
                        collapsed: false,
                        gateway: g.gateway.map(|gw| StoredGateway::from_input(None, gw)).transpose()?,
                    });
                    result.groups_created += 1;
                    id
                }
            };
            match buckets.iter_mut().find(|(b, _)| *b == id) {
                Some((_, entries)) => entries.extend(g.entries),
                None => buckets.push((id, g.entries)),
            }
        }

        for (group_id, entries) in buckets {
            match options.mode {
                ImportMode::Replace => {
                    let outcome = self.replace_group_unsaved(&group_id, entries)?;
                    result.added += outcome.added;
                    result.updated += outcome.kept;
                    result.removed.extend(outcome.removed);
                }
                ImportMode::Merge => {
                    let now = now_ms();
                    for mut item in entries {
                        item.group_id = Some(group_id.clone());
                        let key = entry_key(&item);
                        let existing = self
                            .data
                            .connections
                            .iter_mut()
                            .find(|c| c.group() == group_id && c.key() == key);
                        match existing {
                            Some(c) => {
                                if let Some(name) = item.name.filter(|n| !n.trim().is_empty()) {
                                    c.name = name.trim().to_owned();
                                }
                                if let Some(domain) = item.domain {
                                    c.domain = domain.trim().to_owned();
                                }
                                if let Some(password) = item.password.filter(|p| !p.is_empty()) {
                                    c.password = dpapi::protect(&password)?;
                                }
                                if let Some(notes) = item.notes {
                                    c.notes = notes;
                                }
                                if let Some(gw) = item.gateway {
                                    c.gateway = Some(StoredGateway::from_input(c.gateway.as_ref(), gw)?);
                                }
                                result.updated += 1;
                            }
                            None => {
                                if let Some(c) = self.build(item, now)? {
                                    self.data.connections.push(c);
                                    result.added += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        self.save()?;
        Ok(result)
    }
}

fn entry_key(e: &NewConnection) -> String {
    bulk::key(e.host.trim(), e.port.filter(|p| *p != 0).unwrap_or(3389), e.username.trim())
}

/// `2026-10-19T07:00:00Z` from Unix milliseconds (UTC).
fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{nc, temp_store};
    use super::*;

    fn opts(format: Format, passwords: PasswordMode, passphrase: Option<&str>, groups: Option<Vec<String>>) -> ExportOptions {
        ExportOptions {
            group_ids: groups,
            format,
            passwords,
            passphrase: passphrase.map(Into::into),
        }
    }

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_760_857_200_000), "2025-10-19T07:00:00Z");
    }

    #[test]
    fn json_round_trip_with_encrypted_passwords() {
        let (mut src, d1) = temp_store();
        let g = src.create_group("Prod", None).unwrap();
        src.add(vec![nc("a", "admin", "p:1", Some(&g.id)), nc("b", "root", "p2", None)]).unwrap();
        let json = src
            .export(&opts(Format::Json, PasswordMode::Encrypted, Some("hunter2"), None))
            .unwrap();
        assert!(!json.contains("p:1") && !json.contains("p2"), "passwords must be encrypted");

        let (mut dst, d2) = temp_store();
        let by_name = ImportOptions {
            mode: ImportMode::Merge,
            target: "byName".into(),
        };
        let no_pass = ImportSource {
            text: json.clone(),
            passphrase: None,
        };
        let preview = dst.preview_import(&no_pass, &by_name).unwrap();
        assert!(preview.needs_passphrase);
        assert_eq!(preview.total, 2);
        assert!(dst.apply_import(&no_pass, &by_name).is_err());
        let wrong = ImportSource {
            text: json.clone(),
            passphrase: Some("nope".into()),
        };
        assert!(dst.preview_import(&wrong, &by_name).unwrap().needs_passphrase);

        let right = ImportSource {
            text: json,
            passphrase: Some("hunter2".into()),
        };
        let result = dst.apply_import(&right, &by_name).unwrap();
        assert_eq!((result.added, result.groups_created), (2, 1));
        let lib = dst.library();
        let prod = lib.groups.iter().find(|g| g.name == "Prod").unwrap();
        let a = lib.connections.iter().find(|c| c.host == "a").unwrap();
        assert_eq!(a.group_id, prod.id);
        assert_eq!(dst.credentials(&a.id).unwrap().password, "p:1");
        let b = lib.connections.iter().find(|c| c.host == "b").unwrap();
        assert_eq!(b.group_id, DEFAULT_GROUP, "the All group maps onto the All group");

        // Importing again merges instead of duplicating.
        let again = dst.apply_import(&right, &by_name).unwrap();
        assert_eq!((again.added, again.updated, again.groups_created), (0, 2, 0));
        let _ = std::fs::remove_dir_all(d1);
        let _ = std::fs::remove_dir_all(d2);
    }

    #[test]
    fn text_export_import_and_replace_into_group() {
        let (mut store, dir) = temp_store();
        let g = store.create_group("Lab", None).unwrap();
        store.add(vec![nc("x", "u", "old", Some(&g.id)), nc("y", "u", "p", Some(&g.id))]).unwrap();
        let text = store
            .export(&opts(Format::Text, PasswordMode::Plain, None, Some(vec![g.id.clone()])))
            .unwrap();
        assert_eq!(text, "# Group: Lab\nx:3389:u:old\ny:3389:u:p\n\n");
        assert!(store
            .export(&opts(Format::Text, PasswordMode::Encrypted, Some("x"), None))
            .is_err());

        let replace = ImportOptions {
            mode: ImportMode::Replace,
            target: g.id.clone(),
        };
        let source = ImportSource {
            text: "x:u:new\nz:u:p\nhost-without-user\n".into(),
            passphrase: None,
        };
        let preview = store.preview_import(&source, &replace).unwrap();
        assert_eq!((preview.total, preview.groups[0].duplicates, preview.errors.len()), (2, 1, 1));
        let result = store.apply_import(&source, &replace).unwrap();
        assert_eq!((result.added, result.updated, result.removed.len()), (1, 1, 1));
        let lib = store.library();
        let hosts: Vec<_> = lib.connections.iter().map(|c| c.host.as_str()).collect();
        assert_eq!(hosts, vec!["x", "z"]);
        let x = &lib.connections[0];
        assert_eq!(store.credentials(&x.id).unwrap().password, "new");
        let _ = std::fs::remove_dir_all(dir);
    }
}
