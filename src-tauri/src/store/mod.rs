//! Connection library persistence.
//!
//! Everything lives in one JSON file in the app data directory, written
//! atomically (temp file + rename). Passwords (connection and gateway) are
//! stored DPAPI-encrypted and never sent to the UI. Settings are kept as opaque
//! JSON so the UI can add fields without a backend change.
//!
//! Invariants: the built-in group [`DEFAULT_GROUP`] ("All") always exists, and
//! every connection belongs to an existing group.

mod transfer;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bulk;
use crate::dpapi;

pub use transfer::{ExportOptions, ImportOptions, ImportPreview, ImportSource};

pub const DEFAULT_GROUP: &str = "all";
const DEFAULT_GROUP_NAME: &str = "All";

// ── Gateways ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct StoredGateway {
    enabled: bool,
    host: String,
    same_credentials: bool,
    username: String,
    domain: String,
    /// DPAPI-sealed, base64.
    password: String,
}

/// Gateway as the UI sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gateway {
    pub enabled: bool,
    pub host: String,
    pub same_credentials: bool,
    pub username: String,
    pub domain: String,
    pub has_password: bool,
}

/// Gateway as written by the UI / API (password write-only).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct GatewayInput {
    pub enabled: bool,
    pub host: String,
    pub same_credentials: bool,
    pub username: String,
    pub domain: String,
    /// None = unchanged, "" = clear, otherwise replace.
    pub password: Option<String>,
}

impl StoredGateway {
    fn view(&self) -> Gateway {
        Gateway {
            enabled: self.enabled,
            host: self.host.clone(),
            same_credentials: self.same_credentials,
            username: self.username.clone(),
            domain: self.domain.clone(),
            has_password: !self.password.is_empty(),
        }
    }

    fn from_input(previous: Option<&StoredGateway>, input: GatewayInput) -> anyhow::Result<Self> {
        let password = match input.password {
            None => previous.map(|p| p.password.clone()).unwrap_or_default(),
            Some(p) => dpapi::protect(&p)?,
        };
        Ok(Self {
            enabled: input.enabled,
            host: input.host.trim().to_owned(),
            same_credentials: input.same_credentials,
            username: input.username.trim().to_owned(),
            domain: input.domain.trim().to_owned(),
            password,
        })
    }
}

/// Deserializes `field: null` as `Some(None)` and a missing field as `None`.
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

// ── Groups ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredGroup {
    id: String,
    name: String,
    #[serde(default)]
    collapsed: bool,
    #[serde(default)]
    gateway: Option<StoredGateway>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
    pub builtin: bool,
    pub gateway: Option<Gateway>,
}

impl From<&StoredGroup> for Group {
    fn from(g: &StoredGroup) -> Self {
        Self {
            id: g.id.clone(),
            name: g.name.clone(),
            collapsed: g.collapsed,
            builtin: g.id == DEFAULT_GROUP,
            gateway: g.gateway.as_ref().map(StoredGateway::view),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInput {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
    /// Absent = keep, `null` = remove.
    #[serde(default, deserialize_with = "double_option")]
    pub gateway: Option<Option<GatewayInput>>,
}

// ── Connections ─────────────────────────────────────────────────────────────

/// A connection as stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConnection {
    id: String,
    #[serde(default)]
    name: String,
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    username: String,
    #[serde(default)]
    domain: String,
    /// DPAPI-sealed, base64. Empty = no password.
    #[serde(default)]
    password: String,
    /// Always set after load (older files may have `null`).
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    gateway: Option<StoredGateway>,
    /// Sound for this connection ("off" | "active" | "all"); absent = the app setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audio: Option<String>,
}

fn default_port() -> u16 {
    3389
}

impl StoredConnection {
    fn group(&self) -> &str {
        self.group_id.as_deref().unwrap_or(DEFAULT_GROUP)
    }

    fn key(&self) -> String {
        bulk::key(&self.host, self.port, &self.username)
    }
}

/// A connection as the UI sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub has_password: bool,
    pub group_id: String,
    pub notes: String,
    pub created_at: u64,
    pub gateway: Option<Gateway>,
    /// `null` = follow the app's audio setting.
    pub audio: Option<String>,
}

impl From<&StoredConnection> for Connection {
    fn from(c: &StoredConnection) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            host: c.host.clone(),
            port: c.port,
            username: c.username.clone(),
            domain: c.domain.clone(),
            has_password: !c.password.is_empty(),
            group_id: c.group().to_owned(),
            notes: c.notes.clone(),
            created_at: c.created_at,
            gateway: c.gateway.as_ref().map(StoredGateway::view),
            audio: c.audio.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewConnection {
    #[serde(default)]
    pub name: Option<String>,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Duplicate: reuse another connection's stored password (the UI never sees passwords).
    #[serde(default)]
    pub copy_password_from: Option<String>,
    #[serde(default)]
    pub gateway: Option<GatewayInput>,
    /// "off" | "active" | "all"; absent = follow the app setting.
    #[serde(default)]
    pub audio: Option<String>,
}

impl NewConnection {
    pub fn from_entry(e: bulk::Entry, group_id: Option<String>) -> Self {
        Self {
            host: e.host,
            port: Some(e.port),
            username: e.username,
            domain: Some(e.domain).filter(|d| !d.is_empty()),
            password: Some(e.password).filter(|p| !p.is_empty()),
            group_id,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPatch {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub domain: Option<String>,
    /// None = unchanged, "" = clear, otherwise replace.
    pub password: Option<String>,
    pub group_id: Option<String>,
    pub notes: Option<String>,
    /// Absent = unchanged; `null` = inherit the group's gateway.
    #[serde(default, deserialize_with = "double_option")]
    pub gateway: Option<Option<GatewayInput>>,
    /// Absent = unchanged; `null` = follow the app setting; otherwise "off" | "active" | "all".
    #[serde(default, deserialize_with = "double_option")]
    pub audio: Option<Option<String>>,
}

/// Valid per-connection audio values.
pub const AUDIO_MODES: [&str; 3] = ["off", "active", "all"];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderEntry {
    pub id: String,
    #[serde(default)]
    pub group_id: Option<String>,
}

// ── Library ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LibraryFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    groups: Vec<StoredGroup>,
    #[serde(default)]
    connections: Vec<StoredConnection>,
    #[serde(default)]
    settings: Map<String, Value>,
    /// DPAPI-sealed REST API token.
    #[serde(default)]
    api_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub groups: Vec<Group>,
    pub connections: Vec<Connection>,
    /// `null` until the UI saves settings for the first time (UI applies defaults).
    pub settings: Value,
}

/// Outcome of [`Store::replace_group`].
#[derive(Debug, Clone, Default)]
pub struct ReplaceOutcome {
    pub kept: usize,
    pub added: usize,
    /// Ids of connections that were removed (their sessions should be closed).
    pub removed: Vec<String>,
}

/// Decrypted gateway credentials for a connection.
#[derive(Clone)]
pub struct GatewayCredentials {
    pub host: String,
    pub username: String,
    pub domain: String,
    pub password: String,
}

/// Decrypted connection details for starting a session.
pub struct Credentials {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub password: String,
    pub gateway: Option<GatewayCredentials>,
    /// Per-connection audio override.
    pub audio: Option<String>,
}

pub struct Store {
    path: PathBuf,
    data: LibraryFile,
}

impl Store {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join("library.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LibraryFile>(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    // Never lose a user's library to a parse error: keep the bad file aside.
                    let backup = dir.join(format!("library.corrupt-{}.json", now_ms()));
                    let _ = std::fs::rename(&path, &backup);
                    tracing::error!(error = %e, backup = %backup.display(), "library unreadable; starting fresh");
                    LibraryFile::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LibraryFile::default(),
            Err(e) => return Err(e).context("read library"),
        };
        let mut store = Self { path, data };
        if store.normalize() {
            store.save()?;
        }
        Ok(store)
    }

    /// Restores the invariants (built-in group first, every connection in an existing group).
    /// Returns whether anything changed.
    fn normalize(&mut self) -> bool {
        let mut changed = false;
        match self.data.groups.iter().position(|g| g.id == DEFAULT_GROUP) {
            Some(0) => {}
            Some(pos) => {
                let g = self.data.groups.remove(pos);
                self.data.groups.insert(0, g);
                changed = true;
            }
            None => {
                self.data.groups.insert(
                    0,
                    StoredGroup {
                        id: DEFAULT_GROUP.into(),
                        name: DEFAULT_GROUP_NAME.into(),
                        collapsed: false,
                        gateway: None,
                    },
                );
                changed = true;
            }
        }
        if self.data.groups[0].name != DEFAULT_GROUP_NAME {
            self.data.groups[0].name = DEFAULT_GROUP_NAME.into();
            changed = true;
        }
        let ids: Vec<String> = self.data.groups.iter().map(|g| g.id.clone()).collect();
        for c in &mut self.data.connections {
            let valid = c.group_id.as_ref().is_some_and(|g| ids.contains(g));
            if !valid {
                c.group_id = Some(DEFAULT_GROUP.into());
                changed = true;
            }
        }
        if self.data.api_token.is_empty()
            && let Ok(sealed) = dpapi::protect(&random_token())
        {
            self.data.api_token = sealed;
            changed = true;
        }
        changed
    }

    fn save(&mut self) -> anyhow::Result<()> {
        self.data.version = 2;
        let json = serde_json::to_vec_pretty(&self.data)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).context("write library")?;
        std::fs::rename(&tmp, &self.path).context("replace library")?;
        Ok(())
    }

    pub fn library(&self) -> Library {
        Library {
            groups: self.data.groups.iter().map(Group::from).collect(),
            connections: self.data.connections.iter().map(Connection::from).collect(),
            settings: if self.data.settings.is_empty() {
                Value::Null
            } else {
                Value::Object(self.data.settings.clone())
            },
        }
    }

    pub fn settings(&self) -> &Map<String, Value> {
        &self.data.settings
    }

    pub fn connection(&self, id: &str) -> Option<Connection> {
        self.data.connections.iter().find(|c| c.id == id).map(Connection::from)
    }

    pub fn group(&self, id: &str) -> Option<Group> {
        self.data.groups.iter().find(|g| g.id == id).map(Group::from)
    }

    pub fn group_ids(&self) -> Vec<String> {
        self.data.groups.iter().map(|g| g.id.clone()).collect()
    }

    pub fn connection_ids_in(&self, group_id: &str) -> Vec<String> {
        self.data
            .connections
            .iter()
            .filter(|c| c.group() == group_id)
            .map(|c| c.id.clone())
            .collect()
    }

    pub fn all_connection_ids(&self) -> Vec<String> {
        self.data.connections.iter().map(|c| c.id.clone()).collect()
    }

    fn valid_group(&self, id: Option<&str>) -> String {
        match id {
            Some(g) if self.data.groups.iter().any(|x| x.id == g) => g.to_owned(),
            _ => DEFAULT_GROUP.to_owned(),
        }
    }

    fn build(&self, item: NewConnection, now: u64) -> anyhow::Result<Option<StoredConnection>> {
        let host = item.host.trim().to_owned();
        if host.is_empty() {
            return Ok(None);
        }
        let password = match (item.password.as_deref(), item.copy_password_from.as_deref()) {
            (Some(p), _) if !p.is_empty() => dpapi::protect(p)?,
            (_, Some(src)) => self
                .data
                .connections
                .iter()
                .find(|c| c.id == src)
                .map(|c| c.password.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        let gateway = item.gateway.map(|g| StoredGateway::from_input(None, g)).transpose()?;
        if let Some(mode) = item.audio.as_deref().filter(|m| !AUDIO_MODES.contains(m)) {
            bail!("audio must be off, active or all (got {mode:?})");
        }
        Ok(Some(StoredConnection {
            id: uuid::Uuid::new_v4().to_string(),
            name: item.name.unwrap_or_default().trim().to_owned(),
            host,
            port: item.port.filter(|p| *p != 0).unwrap_or(3389),
            username: item.username.trim().to_owned(),
            domain: item.domain.unwrap_or_default().trim().to_owned(),
            password,
            group_id: Some(self.valid_group(item.group_id.as_deref())),
            notes: item.notes.unwrap_or_default(),
            created_at: now,
            gateway,
            audio: item.audio,
        }))
    }

    pub fn add(&mut self, items: Vec<NewConnection>) -> anyhow::Result<Vec<Connection>> {
        let now = now_ms();
        let mut added = Vec::with_capacity(items.len());
        for item in items {
            if let Some(stored) = self.build(item, now)? {
                added.push(Connection::from(&stored));
                self.data.connections.push(stored);
            }
        }
        self.save()?;
        Ok(added)
    }

    pub fn update(&mut self, id: &str, patch: ConnectionPatch) -> anyhow::Result<Connection> {
        let group = patch.group_id.as_deref().map(|g| self.valid_group(Some(g)));
        let c = self
            .data
            .connections
            .iter_mut()
            .find(|c| c.id == id)
            .context("connection not found")?;
        if let Some(v) = patch.name {
            c.name = v.trim().to_owned();
        }
        if let Some(v) = patch.host {
            let v = v.trim();
            if v.is_empty() {
                bail!("host cannot be empty");
            }
            c.host = v.to_owned();
        }
        if let Some(v) = patch.port {
            c.port = if v == 0 { 3389 } else { v };
        }
        if let Some(v) = patch.username {
            c.username = v.trim().to_owned();
        }
        if let Some(v) = patch.domain {
            c.domain = v.trim().to_owned();
        }
        if let Some(v) = patch.password {
            c.password = dpapi::protect(&v)?;
        }
        if let Some(g) = group {
            c.group_id = Some(g);
        }
        if let Some(v) = patch.notes {
            c.notes = v;
        }
        if let Some(gateway) = patch.gateway {
            c.gateway = match gateway {
                None => None,
                Some(input) => Some(StoredGateway::from_input(c.gateway.as_ref(), input)?),
            };
        }
        if let Some(audio) = patch.audio {
            if let Some(mode) = audio.as_deref().filter(|m| !AUDIO_MODES.contains(m)) {
                bail!("audio must be off, active, all or null (got {mode:?})");
            }
            c.audio = audio;
        }
        let out = Connection::from(&*c);
        self.save()?;
        Ok(out)
    }

    pub fn delete(&mut self, ids: &[String]) -> anyhow::Result<()> {
        self.data.connections.retain(|c| !ids.contains(&c.id));
        self.save()
    }

    pub fn reorder(&mut self, order: Vec<OrderEntry>) -> anyhow::Result<()> {
        let groups = self.group_ids();
        let mut rest = std::mem::take(&mut self.data.connections);
        let mut sorted = Vec::with_capacity(rest.len());
        for entry in order {
            if let Some(pos) = rest.iter().position(|c| c.id == entry.id) {
                let mut c = rest.swap_remove(pos);
                if let Some(g) = entry.group_id {
                    c.group_id = Some(if groups.contains(&g) { g } else { DEFAULT_GROUP.into() });
                }
                sorted.push(c);
            }
        }
        // Anything not mentioned keeps its relative order at the end.
        rest.sort_by_key(|c| c.created_at);
        sorted.extend(rest);
        self.data.connections = sorted;
        self.save()
    }

    /// Replaces the group list. The built-in group is kept (first); connections of
    /// removed groups move to it.
    pub fn save_groups(&mut self, inputs: Vec<GroupInput>) -> anyhow::Result<()> {
        let mut next: Vec<StoredGroup> = Vec::with_capacity(inputs.len() + 1);
        for input in inputs {
            let id = input.id.trim().to_owned();
            if id.is_empty() || next.iter().any(|g| g.id == id) {
                continue;
            }
            let previous = self.data.groups.iter().find(|g| g.id == id);
            let gateway = match input.gateway {
                None => previous.and_then(|g| g.gateway.clone()),
                Some(None) => None,
                Some(Some(gw)) => Some(StoredGateway::from_input(
                    previous.and_then(|g| g.gateway.as_ref()),
                    gw,
                )?),
            };
            let name = if id == DEFAULT_GROUP {
                DEFAULT_GROUP_NAME.to_owned()
            } else {
                Some(input.name.trim().to_owned())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "Untitled".into())
            };
            next.push(StoredGroup {
                id,
                name,
                collapsed: input.collapsed,
                gateway,
            });
        }
        if !next.iter().any(|g| g.id == DEFAULT_GROUP) {
            let builtin = self
                .data
                .groups
                .iter()
                .find(|g| g.id == DEFAULT_GROUP)
                .cloned()
                .expect("built-in group always exists");
            next.insert(0, builtin);
        }
        self.data.groups = next;
        self.normalize();
        self.save()
    }

    /// Creates a group and returns its id.
    pub fn create_group(&mut self, name: &str, gateway: Option<GatewayInput>) -> anyhow::Result<Group> {
        let name = name.trim();
        if name.is_empty() {
            bail!("group name cannot be empty");
        }
        let group = StoredGroup {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
            collapsed: false,
            gateway: gateway.map(|g| StoredGateway::from_input(None, g)).transpose()?,
        };
        let view = Group::from(&group);
        self.data.groups.push(group);
        self.save()?;
        Ok(view)
    }

    /// Finds a group by name (case-insensitive).
    pub fn group_by_name(&self, name: &str) -> Option<String> {
        self.data
            .groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case(name.trim()))
            .map(|g| g.id.clone())
    }

    /// Makes `group_id` contain exactly `items` (in order). Matching connections
    /// (same host, port, username) keep their identity.
    pub fn replace_group(&mut self, group_id: &str, items: Vec<NewConnection>) -> anyhow::Result<ReplaceOutcome> {
        let outcome = self.replace_group_unsaved(group_id, items)?;
        self.save()?;
        Ok(outcome)
    }

    fn replace_group_unsaved(&mut self, group_id: &str, items: Vec<NewConnection>) -> anyhow::Result<ReplaceOutcome> {
        if !self.data.groups.iter().any(|g| g.id == group_id) {
            bail!("group not found");
        }
        let now = now_ms();
        let first_index = self.data.connections.iter().position(|c| c.group() == group_id);
        let (mut existing, others): (Vec<StoredConnection>, Vec<StoredConnection>) =
            std::mem::take(&mut self.data.connections)
                .into_iter()
                .partition(|c| c.group() == group_id);
        self.data.connections = others;

        let mut outcome = ReplaceOutcome::default();
        let mut next = Vec::with_capacity(items.len());
        for mut item in items {
            item.group_id = Some(group_id.to_owned());
            let key = bulk::key(
                item.host.trim(),
                item.port.filter(|p| *p != 0).unwrap_or(3389),
                item.username.trim(),
            );
            if let Some(pos) = existing.iter().position(|c| c.key() == key) {
                let mut c = existing.remove(pos);
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
                next.push(c);
                outcome.kept += 1;
            } else if let Some(c) = self.build(item, now)? {
                next.push(c);
                outcome.added += 1;
            }
        }
        outcome.removed = existing.into_iter().map(|c| c.id).collect();

        // The group's connections take the place of its first member (or the end).
        let at = first_index
            .unwrap_or(self.data.connections.len())
            .min(self.data.connections.len());
        self.data.connections.splice(at..at, next);
        Ok(outcome)
    }

    pub fn save_settings(&mut self, settings: Map<String, Value>) -> anyhow::Result<()> {
        self.data.settings = settings;
        self.save()
    }

    pub fn api_token(&self) -> String {
        dpapi::unprotect(&self.data.api_token).unwrap_or_default()
    }

    pub fn regenerate_api_token(&mut self) -> anyhow::Result<String> {
        let token = random_token();
        self.data.api_token = dpapi::protect(&token)?;
        self.save()?;
        Ok(token)
    }

    pub fn credentials(&self, id: &str) -> anyhow::Result<Credentials> {
        let c = self
            .data
            .connections
            .iter()
            .find(|c| c.id == id)
            .context("connection not found")?;
        let password = dpapi::unprotect(&c.password)?;
        let group_gateway = self
            .data
            .groups
            .iter()
            .find(|g| g.id == c.group())
            .and_then(|g| g.gateway.as_ref());
        let gateway = match c.gateway.as_ref().or(group_gateway) {
            Some(gw) if gw.enabled && !gw.host.is_empty() => Some(if gw.same_credentials {
                GatewayCredentials {
                    host: gw.host.clone(),
                    username: c.username.clone(),
                    domain: c.domain.clone(),
                    password: password.clone(),
                }
            } else {
                GatewayCredentials {
                    host: gw.host.clone(),
                    username: gw.username.clone(),
                    domain: gw.domain.clone(),
                    password: dpapi::unprotect(&gw.password)?,
                }
            }),
            _ => None,
        };
        Ok(Credentials {
            host: c.host.clone(),
            port: c.port,
            username: c.username.clone(),
            domain: c.domain.clone(),
            password,
            gateway,
            audio: c.audio.clone(),
        })
    }

    /// The connection's audio override (`None` = follow the app setting).
    pub fn audio_override(&self, id: &str) -> Option<String> {
        self.data.connections.iter().find(|c| c.id == id)?.audio.clone()
    }
}

fn random_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rdp-app-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir).unwrap(), dir)
    }

    pub(crate) fn nc(host: &str, user: &str, pass: &str, group: Option<&str>) -> NewConnection {
        NewConnection {
            host: host.into(),
            username: user.into(),
            password: Some(pass.into()).filter(|p: &String| !p.is_empty()),
            group_id: group.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn builtin_group_and_migration() {
        let dir = std::env::temp_dir().join(format!("rdp-app-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // An old (v1) file: ungrouped connection, a group, no built-in group.
        std::fs::write(
            dir.join("library.json"),
            r#"{"version":1,"groups":[{"id":"g1","name":"Prod"}],
               "connections":[{"id":"c1","host":"a","groupId":null},{"id":"c2","host":"b","groupId":"g1"},
                              {"id":"c3","host":"c","groupId":"gone"}]}"#,
        )
        .unwrap();
        let store = Store::open(&dir).unwrap();
        let lib = store.library();
        assert_eq!(lib.groups[0].id, DEFAULT_GROUP);
        assert!(lib.groups[0].builtin);
        assert_eq!(lib.groups[1].id, "g1");
        let groups: Vec<_> = lib.connections.iter().map(|c| c.group_id.as_str()).collect();
        assert_eq!(groups, vec!["all", "g1", "all"]);
        assert!(!store.api_token().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_update_reorder_round_trip() {
        let (mut store, dir) = temp_store();
        let added = store
            .add(vec![
                nc("10.0.0.1", "admin", "s:e:c", None),
                nc("10.0.0.2", r"bob", "", None),
            ])
            .unwrap();
        assert_eq!(added.len(), 2);
        assert!(added[0].has_password && !added[1].has_password);
        assert_eq!(added[0].group_id, DEFAULT_GROUP);

        let raw = std::fs::read_to_string(dir.join("library.json")).unwrap();
        assert!(!raw.contains("s:e:c"), "password must not be stored in clear text");

        let g = store.create_group("Prod", None).unwrap();
        store
            .reorder(vec![
                OrderEntry {
                    id: added[1].id.clone(),
                    group_id: Some(g.id.clone()),
                },
                OrderEntry {
                    id: added[0].id.clone(),
                    group_id: None,
                },
            ])
            .unwrap();
        let lib = Store::open(&dir).unwrap().library();
        assert_eq!(lib.connections[0].id, added[1].id);
        assert_eq!(lib.connections[0].group_id, g.id);
        assert_eq!(store.credentials(&added[0].id).unwrap().password, "s:e:c");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleting_a_group_moves_connections_to_all() {
        let (mut store, dir) = temp_store();
        let g = store.create_group("Lab", None).unwrap();
        let added = store.add(vec![nc("a", "u", "p", Some(&g.id))]).unwrap();
        assert_eq!(added[0].group_id, g.id);
        store
            .save_groups(vec![GroupInput {
                id: DEFAULT_GROUP.into(),
                name: "renamed".into(),
                collapsed: true,
                gateway: None,
            }])
            .unwrap();
        let lib = store.library();
        assert_eq!(lib.groups.len(), 1);
        assert_eq!(lib.groups[0].name, "All", "built-in name is fixed");
        assert!(lib.groups[0].collapsed);
        assert_eq!(lib.connections[0].group_id, DEFAULT_GROUP);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replace_keeps_matches_adds_and_removes() {
        let (mut store, dir) = temp_store();
        let g = store.create_group("Prod", None).unwrap();
        let before = store
            .add(vec![
                nc("other", "u", "p", None),
                nc("a", "admin", "old", Some(&g.id)),
                nc("b", "admin", "old", Some(&g.id)),
            ])
            .unwrap();
        let outcome = store
            .replace_group(
                &g.id,
                vec![nc("c", "admin", "new", None), nc("A", "ADMIN", "new", None)],
            )
            .unwrap();
        assert_eq!((outcome.kept, outcome.added), (1, 1));
        assert_eq!(outcome.removed, vec![before[2].id.clone()]);

        let lib = store.library();
        let hosts: Vec<_> = lib
            .connections
            .iter()
            .map(|c| (c.host.as_str(), c.group_id.as_str()))
            .collect();
        assert_eq!(
            hosts,
            vec![("other", "all"), ("c", g.id.as_str()), ("a", g.id.as_str())]
        );
        // The kept connection keeps its id and gets the new password.
        assert_eq!(lib.connections[2].id, before[1].id);
        assert_eq!(store.credentials(&before[1].id).unwrap().password, "new");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn audio_override_is_validated_and_clearable() {
        let (mut store, dir) = temp_store();
        let id = store.add(vec![nc("h", "u", "p", None)]).unwrap()[0].id.clone();
        assert_eq!(store.audio_override(&id), None);
        let patch = |audio: Option<&str>| ConnectionPatch {
            audio: Some(audio.map(Into::into)),
            ..Default::default()
        };
        store.update(&id, patch(Some("off"))).unwrap();
        assert_eq!(store.audio_override(&id).as_deref(), Some("off"));
        assert_eq!(store.credentials(&id).unwrap().audio.as_deref(), Some("off"));
        assert!(store.update(&id, patch(Some("loud"))).is_err());
        store.update(&id, patch(None)).unwrap();
        assert_eq!(store.audio_override(&id), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gateway_inheritance_and_same_credentials() {
        let (mut store, dir) = temp_store();
        let g = store
            .create_group(
                "Behind GW",
                Some(GatewayInput {
                    enabled: true,
                    host: "rdg.example.com".into(),
                    same_credentials: false,
                    username: "gwuser".into(),
                    domain: "CORP".into(),
                    password: Some("gwpass".into()),
                }),
            )
            .unwrap();
        assert!(g.gateway.as_ref().unwrap().has_password);
        let added = store
            .add(vec![nc("a", "u", "p", Some(&g.id)), nc("b", "u2", "p2", Some(&g.id))])
            .unwrap();
        let creds = store.credentials(&added[0].id).unwrap();
        let gw = creds.gateway.unwrap();
        assert_eq!(
            (gw.host.as_str(), gw.username.as_str(), gw.password.as_str()),
            ("rdg.example.com", "gwuser", "gwpass")
        );

        // A connection can opt out, or reuse its own credentials.
        store
            .update(
                &added[1].id,
                ConnectionPatch {
                    gateway: Some(Some(GatewayInput {
                        enabled: true,
                        host: "other-gw".into(),
                        same_credentials: true,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )
            .unwrap();
        let gw = store.credentials(&added[1].id).unwrap().gateway.unwrap();
        assert_eq!(
            (gw.host.as_str(), gw.username.as_str(), gw.password.as_str()),
            ("other-gw", "u2", "p2")
        );
        store
            .update(
                &added[1].id,
                ConnectionPatch {
                    gateway: Some(Some(GatewayInput::default())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            store.credentials(&added[1].id).unwrap().gateway.is_none(),
            "disabled = direct"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
