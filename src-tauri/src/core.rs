//! Application core shared by the window (Tauri commands) and the REST API, so
//! both behave identically: every operation a user can perform lives here.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use parking_lot::Mutex;
use rdp_engine::{
    AudioMode, CertPolicy, CodecPreference, ConnectParams, DecoderPreference, DeviceRedirection, Engine, EngineEvent,
    EngineSettings, GatewayParams, SessionStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Emitter as _, Manager as _};
use tokio::sync::broadcast;

use crate::certs::CertStore;
use crate::profile::Overrides;
use crate::store::{
    self, Connection, ConnectionPatch, ExportOptions, GatewayInput, Group, GroupInput, ImportOptions, ImportPreview,
    ImportSource, Library, NewConnection, OrderEntry, Store,
};

/// Who asked for a change: the window already applied UI-originated changes
/// optimistically, so only API-originated ones are pushed back to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Ui,
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    pub selected_id: Option<String>,
    #[serde(default = "default_view")]
    pub view: String,
    #[serde(default)]
    pub fullscreen: bool,
}

fn default_view() -> String {
    "session".into()
}

/// Replace / import outcome as returned to callers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceResult {
    pub kept: usize,
    pub added: usize,
    pub removed: usize,
    pub library: Library,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub groups_created: usize,
    pub library: Library,
}

/// One session's state, for API callers.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiInfo {
    pub enabled: bool,
    pub url: String,
    pub token: String,
    pub docs_url: String,
    pub llms_url: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Default)]
pub struct ApiRuntime {
    pub port: u16,
    pub running: bool,
    pub error: Option<String>,
    pub shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Backend defaults for settings the UI hasn't saved yet (mirror of DEFAULT_SETTINGS in `src/lib/types.ts`).
pub fn default_settings() -> Map<String, Value> {
    let v = json!({
        "theme": "dark",
        "windowMaterial": "solid",
        "topTabs": "active",
        "sidebarWidth": 248,
        "gridTileWidth": 320,
        "gridFilter": "all",
        "connectOnSelect": false,
        "maxConcurrentConnects": 6,
        "pauseHiddenSessions": true,
        "codec": "auto",
        "h264Quality": "sharp",
        "videoDecoder": "auto",
        "autoReconnect": true,
        "companion": false,
        "hostStatsInterval": 30,
        "clipboardSync": true,
        "audio": "active",
        "microphone": false,
        "driveRedirection": "none",
        "drivePaths": [],
        "printerRedirection": false,
        "smartcardRedirection": false,
        "certificatePolicy": "tofu",
        "apiEnabled": true,
        "apiPort": 47123
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// Rejects wrongly typed or out-of-range values for known settings keys.
/// Unknown keys (UI layout values) pass through untouched.
pub fn validate_settings(settings: &Map<String, Value>) -> anyhow::Result<()> {
    const CHOICES: &[(&str, &[&str])] = &[
        ("theme", &["dark", "light", "system"]),
        ("windowMaterial", &["solid", "acrylic"]),
        ("topTabs", &["active", "opened"]),
        ("codec", &["auto", "h264", "remotefx"]),
        ("h264Quality", &["balanced", "sharp"]),
        ("videoDecoder", &["auto", "hardware", "software"]),
        ("audio", &["off", "active", "all"]),
        ("driveRedirection", &["none", "all", "custom"]),
        ("certificatePolicy", &["tofu", "prompt", "ignore"]),
    ];
    const RANGES: &[(&str, u64, u64)] = &[
        ("maxConcurrentConnects", 1, 32),
        ("apiPort", 1024, 65535),
        ("hostStatsInterval", 0, 3600),
    ];
    // Layout sizes from pointer drags may be fractional.
    const SIZES: &[&str] = &["sidebarWidth", "gridTileWidth"];
    let defaults = default_settings();
    for (key, value) in settings {
        let Some(default) = defaults.get(key) else { continue };
        if let Some((_, allowed)) = CHOICES.iter().find(|(k, _)| k == key) {
            match value.as_str() {
                Some(v) if allowed.contains(&v) => {}
                _ => bail!("{key} must be one of {}", allowed.join(", ")),
            }
        } else if let Some(&(_, lo, hi)) = RANGES.iter().find(|(k, ..)| k == key) {
            match value.as_u64() {
                Some(v) if (lo..=hi).contains(&v) => {}
                _ => bail!("{key} must be an integer from {lo} to {hi}"),
            }
        } else if SIZES.contains(&key.as_str()) {
            if !value.as_f64().is_some_and(|v| (0.0..=4096.0).contains(&v)) {
                bail!("{key} must be a number from 0 to 4096");
            }
        } else if default.is_boolean() && !value.is_boolean() {
            bail!("{key} must be true or false");
        } else if default.is_string() && !value.is_string() {
            bail!("{key} must be a string");
        } else if default.is_array() && !value.as_array().is_some_and(|a| a.iter().all(Value::is_string)) {
            bail!("{key} must be an array of strings");
        }
    }
    Ok(())
}

/// After a companion-enabled session connects, how long to wait for an already-installed
/// companion to attach on its own before installing one.
const COMPANION_INSTALL_DELAY: u64 = 8;

pub struct Core {
    store: Mutex<Store>,
    pub engine: Engine,
    app: AppHandle,
    pub certs: Arc<CertStore>,
    ui: Mutex<UiState>,
    bus: broadcast::Sender<Value>,
    pub api: Mutex<ApiRuntime>,
    /// Sessions with a companion attached, and sessions whose auto-install we've already
    /// kicked off this connection. Ephemeral — we never persist "installed"; each connect
    /// re-checks by whether the companion attaches (hosts get reset, IPs change).
    companion: Mutex<CompanionState>,
    /// Attached companions: host stats, file sends, fast clipboard copies.
    pub companions: Arc<crate::companion_link::Companions>,
}

#[derive(Default)]
struct CompanionState {
    attached: std::collections::HashSet<String>,
    pending: std::collections::HashSet<String>,
}

impl Core {
    pub fn new(
        store: Store,
        engine: Engine,
        app: AppHandle,
        certs: Arc<CertStore>,
        bus: broadcast::Sender<Value>,
    ) -> Self {
        let companions = crate::companion_link::Companions::new(app.clone());
        engine.set_companion_host(Some(Arc::clone(&companions) as Arc<dyn rdp_engine::CompanionHost>));
        Self {
            store: Mutex::new(store),
            engine,
            app,
            companions,
            certs,
            ui: Mutex::new(UiState {
                view: default_view(),
                ..Default::default()
            }),
            bus,
            api: Mutex::new(ApiRuntime::default()),
            companion: Mutex::new(CompanionState::default()),
        }
    }

    /// Observes engine events for the companion: tracks attach state and, when a
    /// companion-enabled connection comes up without one attaching shortly, installs it.
    pub fn on_engine_event(self: &Arc<Self>, event: &EngineEvent) {
        match event {
            EngineEvent::CompanionConnected { id, .. } => {
                self.companion.lock().attached.insert(id.clone());
            }
            EngineEvent::CompanionDisconnected { id } => {
                self.companion.lock().attached.remove(id);
            }
            EngineEvent::Status { id, status, .. } => match status {
                SessionStatus::Connected => self.maybe_install_companion(id.clone()),
                // Session ended: forget it, so a later connect re-checks from scratch.
                SessionStatus::Idle | SessionStatus::Error => {
                    let mut c = self.companion.lock();
                    c.attached.remove(id);
                    c.pending.remove(id);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// If the connection wants a companion, wait briefly for one (an already-installed
    /// companion self-starts), then install over WinRM if none attached.
    fn maybe_install_companion(self: &Arc<Self>, id: String) {
        let enabled = self.effective_settings(&id).get("companion").and_then(Value::as_bool) == Some(true);
        if !enabled {
            return;
        }
        if !self.companion.lock().pending.insert(id.clone()) {
            return; // already handling this session
        }
        let core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(COMPANION_INSTALL_DELAY)).await;
            let attached = core.companion.lock().attached.contains(&id);
            // Gone already, or the resident companion attached on its own: nothing to do.
            if attached || !core.engine.running().contains(&id) {
                return;
            }
            core.emit_companion("installing", &id, None);
            let outcome = {
                let core = Arc::clone(&core);
                let id = id.clone();
                tauri::async_runtime::spawn_blocking(move || core.install_companion(&id)).await
            };
            match outcome {
                Ok(Ok(crate::companion::Outcome::Installed)) => {} // the attach event will follow
                Ok(Ok(crate::companion::Outcome::NeedsTrust)) => {
                    let host = core.store.lock().credentials(&id).map(|c| c.host).unwrap_or_default();
                    core.emit_companion("needs-trust", &id, Some(&host));
                }
                Ok(Ok(crate::companion::Outcome::WinrmUnavailable)) => {
                    core.emit_companion("winrm-unavailable", &id, None)
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %format!("{e:#}"), "companion auto-install failed");
                    core.emit_companion("failed", &id, None);
                }
                Err(_) => {}
            }
        });
    }

    fn emit_companion(&self, status: &str, id: &str, host: Option<&str>) {
        let _ = self
            .app
            .emit("companion-status", json!({ "id": id, "status": status, "host": host }));
    }

    /// Adds a host to the WinRM client's TrustedHosts (elevated). Blocking.
    pub fn trust_host(&self, host: &str) -> anyhow::Result<()> {
        crate::companion::trust_host(host)
    }

    /// Event stream for API subscribers (session events, library changes, UI state).
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.bus.subscribe()
    }

    pub fn publish(&self, event: Value) {
        let _ = self.bus.send(event);
    }

    fn changed(&self, origin: Origin) {
        // The window that made a UI change already shows it; other windows don't.
        if origin == Origin::Api || self.app.webview_windows().len() > 1 {
            let _ = self.app.emit("library-changed", self.library());
        }
        self.publish(json!({ "type": "libraryChanged" }));
    }

    // ── Library ─────────────────────────────────────────────────────────────

    pub fn library(&self) -> Library {
        self.store.lock().library()
    }

    pub fn connection(&self, id: &str) -> anyhow::Result<Connection> {
        self.store.lock().connection(id).context("connection not found")
    }

    pub fn group(&self, id: &str) -> anyhow::Result<Group> {
        self.store.lock().group(id).context("group not found")
    }

    pub fn add_connections(&self, items: Vec<NewConnection>, origin: Origin) -> anyhow::Result<Vec<Connection>> {
        let added = self.store.lock().add(items)?;
        self.changed(origin);
        Ok(added)
    }

    pub fn update_connection(&self, id: &str, patch: ConnectionPatch, origin: Origin) -> anyhow::Result<Connection> {
        let audio_changed = patch.settings.is_some() || patch.group_id.is_some();
        let c = self.store.lock().update(id, patch)?;
        if audio_changed {
            self.apply_audio(id);
            self.companions.refresh_intervals();
        }
        self.changed(origin);
        Ok(c)
    }

    /// Replaces a connection's setting overrides and applies what can change live (sound).
    /// Returns false when its session must reconnect to hear it (it connected with sound
    /// off, so has no audio channel).
    pub fn set_connection_settings(&self, id: &str, settings: Overrides, origin: Origin) -> anyhow::Result<bool> {
        self.store.lock().update(
            id,
            ConnectionPatch {
                settings: Some(settings),
                ..Default::default()
            },
        )?;
        let live = self.apply_audio(id);
        self.companions.refresh_intervals();
        self.changed(origin);
        Ok(live)
    }

    /// Replaces a group's setting overrides; running sessions pick up what can change live.
    pub fn set_group_settings(&self, id: &str, settings: Overrides, origin: Origin) -> anyhow::Result<Group> {
        let group = self.store.lock().set_group_settings(id, settings)?;
        for session in self.engine.running() {
            self.apply_audio(&session);
        }
        self.companions.refresh_intervals();
        self.changed(origin);
        Ok(group)
    }

    /// A connection's settings: the app's, overridden by its group's, then its own.
    pub fn effective_settings(&self, id: &str) -> Map<String, Value> {
        let app = self.settings();
        match self.store.lock().layers(id) {
            Some((group, conn)) => crate::profile::resolve(&app, &[&group, &conn]),
            None => app,
        }
    }

    /// A connection's host stats interval in seconds (0: off), from its settings.
    pub fn stats_interval(&self, id: &str) -> u32 {
        let s = self.effective_settings(id);
        let on = s.get("companion").and_then(Value::as_bool) == Some(true);
        let secs = s.get("hostStatsInterval").and_then(Value::as_u64).unwrap_or(30);
        if on { u32::try_from(secs).unwrap_or(u32::MAX) } else { 0 }
    }

    /// Sends files and folders to connections, saving them into `folder` on each remote
    /// (`Desktop`, `Downloads`, `Documents`, optionally with a subfolder, or a full path).
    /// A connection that isn't connected is connected first; each send starts once its
    /// companion attaches. Progress arrives as `transfer` events. Blocking (lists the
    /// files).
    pub fn send_files(
        self: &Arc<Self>,
        ids: &[String],
        paths: &[std::path::PathBuf],
        folder: &str,
    ) -> anyhow::Result<Vec<u64>> {
        use rdp_companion_proto::Entry;

        anyhow::ensure!(!ids.is_empty(), "no connections to send to");
        let listing = rdp_companion_proto::transfer::list(paths);
        anyhow::ensure!(!listing.entries.is_empty(), "nothing to send");
        let entries: Arc<[Entry]> = listing.entries.clone().into();
        let folder = if folder.trim().is_empty() {
            "Desktop"
        } else {
            folder.trim()
        };
        let mut started = Vec::new();
        for id in ids {
            self.connection(id)?;
            let transfer = self.companions.register(id, &listing, folder);
            started.push(transfer);
            if self.effective_settings(id).get("companion").and_then(Value::as_bool) != Some(true) {
                self.companions.fail(
                    transfer,
                    "Turn on the companion for this connection to send files to it",
                );
                continue;
            }
            if self.companions.is_attached(id) {
                if let Err(e) = self
                    .companions
                    .start(transfer, Arc::clone(&entries), listing.paths.clone(), folder)
                {
                    self.companions.fail(transfer, &e.to_string());
                }
                continue;
            }
            if !self.engine.running().contains(id) {
                self.connect(std::slice::from_ref(id));
            }
            let (core, id, entries, paths, folder) = (
                Arc::clone(self),
                id.clone(),
                Arc::clone(&entries),
                listing.paths.clone(),
                folder.to_owned(),
            );
            tauri::async_runtime::spawn(async move {
                // Connecting can include installing the companion; give it a while.
                let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
                loop {
                    if core.companions.state(transfer) != Some(crate::companion_link::TransferState::Waiting) {
                        return; // cancelled meanwhile
                    }
                    if core.companions.is_attached(&id) {
                        if let Err(e) = core.companions.start(transfer, entries, paths, &folder) {
                            core.companions.fail(transfer, &e.to_string());
                        }
                        return;
                    }
                    if tokio::time::Instant::now() >= deadline || !core.engine.running().contains(&id) {
                        core.companions
                            .fail(transfer, "The companion didn't attach (is it installed on that host?)");
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            });
        }
        Ok(started)
    }

    /// Installs the companion on a connection's host over WinRM (experimental). Blocking.
    pub fn install_companion(&self, id: &str) -> anyhow::Result<crate::companion::Outcome> {
        let creds = self.store.lock().credentials(id)?;
        let target = crate::companion::Target {
            host: creds.host,
            username: creds.username,
            domain: creds.domain,
            password: creds.password,
        };
        let exe = crate::companion::companion_exe()?;
        crate::companion::install_over_winrm(&target, &exe)
    }

    /// Removes the companion from a connection's host over WinRM. Blocking.
    pub fn uninstall_companion(&self, id: &str) -> anyhow::Result<()> {
        let creds = self.store.lock().credentials(id)?;
        let target = crate::companion::Target {
            host: creds.host,
            username: creds.username,
            domain: creds.domain,
            password: creds.password,
        };
        crate::companion::uninstall_over_winrm(&target)
    }

    /// The audio mode a connection should use.
    fn audio_mode(&self, id: &str) -> AudioMode {
        match self.effective_settings(id).get("audio").and_then(Value::as_str) {
            Some("off") => AudioMode::Off,
            Some("all") => AudioMode::All,
            _ => AudioMode::Active,
        }
    }

    fn apply_audio(&self, id: &str) -> bool {
        self.engine.set_audio(id, self.audio_mode(id))
    }

    pub fn delete_connections(&self, ids: &[String], origin: Origin) -> anyhow::Result<()> {
        for id in ids {
            self.engine.disconnect(id);
        }
        self.store.lock().delete(ids)?;
        self.changed(origin);
        Ok(())
    }

    pub fn reorder(&self, order: Vec<OrderEntry>, origin: Origin) -> anyhow::Result<()> {
        self.store.lock().reorder(order)?;
        self.changed(origin);
        Ok(())
    }

    pub fn save_groups(&self, groups: Vec<GroupInput>, origin: Origin) -> anyhow::Result<Library> {
        let lib = {
            let mut store = self.store.lock();
            store.save_groups(groups)?;
            store.library()
        };
        self.changed(origin);
        Ok(lib)
    }

    pub fn create_group(&self, name: &str, gateway: Option<GatewayInput>, origin: Origin) -> anyhow::Result<Group> {
        let g = self.store.lock().create_group(name, gateway)?;
        self.changed(origin);
        Ok(g)
    }

    /// Updates one group (name / collapsed / gateway) without touching the others.
    pub fn update_group(
        &self,
        id: &str,
        name: Option<String>,
        collapsed: Option<bool>,
        gateway: Option<Option<GatewayInput>>,
        origin: Origin,
    ) -> anyhow::Result<Group> {
        let group = {
            let mut store = self.store.lock();
            let lib = store.library();
            let Some(current) = lib.groups.iter().find(|g| g.id == id) else {
                bail!("group not found");
            };
            if current.builtin && name.as_deref().is_some_and(|n| n.trim() != current.name) {
                bail!("the built-in \"All\" group cannot be renamed");
            }
            let inputs = lib
                .groups
                .iter()
                .map(|g| {
                    if g.id == id {
                        GroupInput {
                            id: g.id.clone(),
                            name: name.clone().unwrap_or_else(|| g.name.clone()),
                            collapsed: collapsed.unwrap_or(g.collapsed),
                            gateway: gateway.clone(),
                            settings: None,
                        }
                    } else {
                        GroupInput {
                            id: g.id.clone(),
                            name: g.name.clone(),
                            collapsed: g.collapsed,
                            gateway: None,
                            settings: None,
                        }
                    }
                })
                .collect();
            store.save_groups(inputs)?;
            store.group(id).context("group not found")?
        };
        self.changed(origin);
        Ok(group)
    }

    /// Deletes a custom group; its connections move to "All" or are deleted.
    pub fn delete_group(&self, id: &str, delete_connections: bool, origin: Origin) -> anyhow::Result<()> {
        if id == store::DEFAULT_GROUP {
            bail!("the built-in \"All\" group cannot be deleted");
        }
        {
            let mut store = self.store.lock();
            let lib = store.library();
            if !lib.groups.iter().any(|g| g.id == id) {
                bail!("group not found");
            }
            if delete_connections {
                let ids = store.connection_ids_in(id);
                for cid in &ids {
                    self.engine.disconnect(cid);
                }
                store.delete(&ids)?;
            }
            let inputs = lib
                .groups
                .iter()
                .filter(|g| g.id != id)
                .map(|g| GroupInput {
                    id: g.id.clone(),
                    name: g.name.clone(),
                    collapsed: g.collapsed,
                    gateway: None,
                    settings: None,
                })
                .collect();
            store.save_groups(inputs)?;
        }
        self.changed(origin);
        Ok(())
    }

    /// Reorders groups (the built-in group stays first).
    pub fn reorder_groups(&self, ids: &[String], origin: Origin) -> anyhow::Result<Library> {
        let lib = self.library();
        let mut ordered: Vec<&Group> = ids
            .iter()
            .filter_map(|id| lib.groups.iter().find(|g| &g.id == id))
            .collect();
        for g in &lib.groups {
            if !ordered.iter().any(|o| o.id == g.id) {
                ordered.push(g);
            }
        }
        let inputs = ordered
            .into_iter()
            .map(|g| GroupInput {
                id: g.id.clone(),
                name: g.name.clone(),
                collapsed: g.collapsed,
                gateway: None,
                settings: None,
            })
            .collect();
        self.save_groups(inputs, origin)
    }

    pub fn replace_group(&self, id: &str, items: Vec<NewConnection>, origin: Origin) -> anyhow::Result<ReplaceResult> {
        let (outcome, library) = {
            let mut store = self.store.lock();
            let outcome = store.replace_group(id, items)?;
            (outcome, store.library())
        };
        for removed in &outcome.removed {
            self.engine.disconnect(removed);
        }
        self.changed(origin);
        Ok(ReplaceResult {
            kept: outcome.kept,
            added: outcome.added,
            removed: outcome.removed.len(),
            library,
        })
    }

    pub fn export(&self, options: &ExportOptions) -> anyhow::Result<String> {
        self.store.lock().export(options)
    }

    pub fn preview_import(&self, source: &ImportSource, options: &ImportOptions) -> anyhow::Result<ImportPreview> {
        self.store.lock().preview_import(source, options)
    }

    pub fn apply_import(
        &self,
        source: &ImportSource,
        options: &ImportOptions,
        origin: Origin,
    ) -> anyhow::Result<ImportResult> {
        let (result, library) = {
            let mut store = self.store.lock();
            let result = store.apply_import(source, options)?;
            (result, store.library())
        };
        for removed in &result.removed {
            self.engine.disconnect(removed);
        }
        self.changed(origin);
        Ok(ImportResult {
            added: result.added,
            updated: result.updated,
            removed: result.removed.len(),
            groups_created: result.groups_created,
            library,
        })
    }

    // ── Settings ────────────────────────────────────────────────────────────

    /// Effective settings: valid stored values over backend defaults.
    pub fn settings(&self) -> Map<String, Value> {
        let mut out = default_settings();
        for (k, v) in self.store.lock().settings() {
            let single = Map::from_iter([(k.clone(), v.clone())]);
            if validate_settings(&single).is_ok() {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    pub fn save_settings(self: &Arc<Self>, settings: Map<String, Value>, origin: Origin) -> anyhow::Result<()> {
        validate_settings(&settings)?;
        self.store.lock().save_settings(settings)?;
        self.apply_settings();
        self.changed(origin);
        Ok(())
    }

    /// The effective settings with `patch` applied (null resets a key), validated but not saved.
    pub fn merged_settings(&self, patch: Map<String, Value>) -> anyhow::Result<Map<String, Value>> {
        // Start from the effective settings so a first API patch doesn't drop UI defaults.
        let mut next = self.settings();
        let defaults = default_settings();
        for (k, v) in patch {
            match (v.is_null(), defaults.get(&k)) {
                (true, Some(d)) => next.insert(k, d.clone()),
                (true, None) => next.remove(&k),
                (false, _) => next.insert(k, v),
            };
        }
        validate_settings(&next)?;
        Ok(next)
    }

    pub fn patch_settings(
        self: &Arc<Self>,
        patch: Map<String, Value>,
        origin: Origin,
    ) -> anyhow::Result<Map<String, Value>> {
        let next = self.merged_settings(patch)?;
        self.save_settings(next, origin)?;
        Ok(self.settings())
    }

    /// Pushes settings into the engine, clipboard and API server.
    pub fn apply_settings(self: &Arc<Self>) {
        let s = self.settings();
        let current = self.engine.settings();
        self.engine.update_settings(EngineSettings {
            max_concurrent_connects: s
                .get("maxConcurrentConnects")
                .and_then(Value::as_u64)
                .map(|v| v.clamp(1, 64) as usize)
                .unwrap_or(current.max_concurrent_connects),
            pause_hidden: s.get("pauseHiddenSessions").and_then(Value::as_bool).unwrap_or(true),
            ..current
        });
        // The app-wide audio setting may have changed: running sessions follow it live.
        for id in self.engine.running() {
            self.apply_audio(&id);
        }

        let clipboard = s.get("clipboardSync").and_then(Value::as_bool).unwrap_or(true);
        #[cfg(windows)]
        self.engine.set_clipboard_host(clipboard.then(|| {
            Arc::new(crate::clipboard::NativeClipboard::new(
                self.app.clone(),
                Arc::clone(&self.companions),
            )) as Arc<dyn rdp_engine::ClipboardHost>
        }));
        self.companions.refresh_intervals();
        #[cfg(not(windows))]
        let _ = clipboard;

        crate::api::ensure_running(self);
    }

    // ── Sessions ────────────────────────────────────────────────────────────

    fn connect_params(&self, id: &str) -> anyhow::Result<ConnectParams> {
        let creds = self.store.lock().credentials(id)?;
        let s = crate::profile::resolve(&self.settings(), &[&creds.group_settings, &creds.settings]);
        let str_of = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let bool_of = |k: &str, d: bool| s.get(k).and_then(Value::as_bool).unwrap_or(d);

        let drives = match str_of("driveRedirection").as_str() {
            "all" => local_drives(),
            "custom" => s
                .get("drivePaths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(std::path::PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        Ok(ConnectParams {
            host: creds.host,
            port: creds.port,
            username: creds.username,
            password: creds.password,
            domain: Some(creds.domain).filter(|d| !d.is_empty()),
            codec: match str_of("codec").as_str() {
                "h264" => CodecPreference::H264,
                "remotefx" => CodecPreference::RemoteFx,
                _ => CodecPreference::Auto,
            },
            h264_444: str_of("h264Quality") != "balanced",
            decoder: match str_of("videoDecoder").as_str() {
                "hardware" => DecoderPreference::Hardware,
                "software" => DecoderPreference::Software,
                _ => DecoderPreference::Auto,
            },
            auto_reconnect: bool_of("autoReconnect", true),
            gateway: creds.gateway.map(|g| GatewayParams {
                host: g.host,
                username: g.username,
                domain: Some(g.domain).filter(|d| !d.is_empty()),
                password: g.password,
            }),
            cert_policy: match str_of("certificatePolicy").as_str() {
                "prompt" => CertPolicy::Prompt,
                "ignore" => CertPolicy::Ignore,
                _ => CertPolicy::TrustOnFirstUse,
            },
            clipboard: bool_of("clipboardSync", true),
            audio: match str_of("audio").as_str() {
                "off" => AudioMode::Off,
                "all" => AudioMode::All,
                _ => AudioMode::Active,
            },
            microphone: bool_of("microphone", false),
            companion: bool_of("companion", false),
            devices: DeviceRedirection {
                drives,
                printer: bool_of("printerRedirection", false),
                smartcard: bool_of("smartcardRedirection", false),
            },
        })
    }

    pub fn connect(&self, ids: &[String]) {
        for id in ids {
            match self.connect_params(id) {
                Ok(params) => self.engine.connect(id, params),
                Err(e) => self.emit_session(EngineEvent::Status {
                    id: id.clone(),
                    status: SessionStatus::Error,
                    message: Some(format!("{e:#}")),
                }),
            }
        }
    }

    pub fn disconnect(&self, ids: &[String]) {
        for id in ids {
            self.engine.disconnect(id);
        }
    }

    /// Connects again (after the current session, if any, has fully stopped).
    pub fn reconnect(&self, ids: &[String]) {
        for id in ids {
            match self.connect_params(id) {
                Ok(params) => self.engine.reconnect(id, params),
                Err(e) => self.emit_session(EngineEvent::Status {
                    id: id.clone(),
                    status: SessionStatus::Error,
                    message: Some(format!("{e:#}")),
                }),
            }
        }
    }

    pub fn all_connection_ids(&self) -> Vec<String> {
        self.store.lock().all_connection_ids()
    }

    pub fn connection_ids_in(&self, group: &str) -> anyhow::Result<Vec<String>> {
        let store = self.store.lock();
        if store.group(group).is_none() {
            bail!("group not found");
        }
        Ok(store.connection_ids_in(group))
    }

    /// Session state of every connection (idle ones included).
    pub fn sessions(&self) -> Vec<SessionInfo> {
        let mut infos: Vec<SessionInfo> = self
            .all_connection_ids()
            .into_iter()
            .map(|id| SessionInfo {
                id,
                status: "idle".into(),
                ..Default::default()
            })
            .collect();
        for event in self.engine.snapshot() {
            type Apply = Box<dyn Fn(&mut SessionInfo)>;
            let (id, apply): (&str, Apply) = match &event {
                EngineEvent::Status { id, status, message } => {
                    let status = serde_json::to_value(status)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    let message = message.clone();
                    (
                        id,
                        Box::new(move |i| {
                            i.status = status.clone();
                            i.message = message.clone();
                        }),
                    )
                }
                EngineEvent::Resolution { id, width, height } => {
                    let (w, h) = (*width, *height);
                    (
                        id,
                        Box::new(move |i| {
                            i.width = Some(w);
                            i.height = Some(h);
                        }),
                    )
                }
                EngineEvent::Codec { id, codec } => {
                    let codec = codec.clone();
                    (id, Box::new(move |i| i.codec = Some(codec.clone())))
                }
                _ => continue,
            };
            if let Some(info) = infos.iter_mut().find(|i| i.id == id) {
                apply(info);
            }
        }
        infos
    }

    pub fn session(&self, id: &str) -> anyhow::Result<SessionInfo> {
        self.connection(id)?;
        Ok(self.sessions().into_iter().find(|s| s.id == id).unwrap_or_default())
    }

    /// Waits until each session is connected or failed (or `timeout` passes).
    pub async fn wait_settled(&self, ids: &[String], timeout: Duration) -> Vec<SessionInfo> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let all = self.sessions();
            let mine: Vec<SessionInfo> = all.into_iter().filter(|s| ids.contains(&s.id)).collect();
            let settled = mine
                .iter()
                .all(|s| matches!(s.status.as_str(), "connected" | "error" | "idle"));
            if settled || tokio::time::Instant::now() >= deadline {
                return mine;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn emit_session(&self, event: EngineEvent) {
        let _ = self.app.emit("session-event", &event);
        if let Ok(v) = serde_json::to_value(&event) {
            self.publish(v);
        }
    }

    // ── Certificates ────────────────────────────────────────────────────────

    pub fn trust_certificate(&self, endpoint: &str, fingerprint: &str) {
        self.certs.set(endpoint, fingerprint, "");
        self.publish(json!({ "type": "certificateTrusted", "endpoint": endpoint }));
    }

    // ── UI ──────────────────────────────────────────────────────────────────

    pub fn ui_state(&self) -> UiState {
        self.ui.lock().clone()
    }

    pub fn set_ui_state(&self, state: UiState) {
        *self.ui.lock() = state.clone();
        if let Ok(Value::Object(mut v)) = serde_json::to_value(&state) {
            v.insert("type".into(), "uiState".into());
            self.publish(Value::Object(v));
        }
    }

    /// Sends a command to the window (select a tab, switch view, …).
    pub fn ui_command(&self, command: Value) {
        let _ = self.app.emit("ui-command", command);
    }

    pub fn api_info(&self) -> ApiInfo {
        let s = self.settings();
        let enabled = s.get("apiEnabled").and_then(Value::as_bool).unwrap_or(true);
        let port = s.get("apiPort").and_then(Value::as_u64).unwrap_or(47123);
        let rt = self.api.lock();
        let url = format!("http://127.0.0.1:{port}");
        ApiInfo {
            enabled,
            docs_url: format!("{url}/docs"),
            llms_url: format!("{url}/llms.txt"),
            url,
            token: self.store.lock().api_token(),
            running: rt.running,
            error: rt.error.clone(),
        }
    }

    pub fn api_token(&self) -> String {
        self.store.lock().api_token()
    }

    pub fn regenerate_api_token(&self) -> anyhow::Result<ApiInfo> {
        self.store.lock().regenerate_api_token()?;
        Ok(self.api_info())
    }
}

/// Fixed and removable local drives (`C:\`, `D:\`, …).
fn local_drives() -> Vec<std::path::PathBuf> {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        use windows::core::PCWSTR;
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_FIXED: u32 = 3;
        let mask = unsafe { GetLogicalDrives() };
        (0..26u8)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| format!("{}:\\", (b'A' + i) as char))
            .filter(|root| {
                let wide: Vec<u16> = root.encode_utf16().chain([0]).collect();
                let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
                kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE
            })
            .map(std::path::PathBuf::from)
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(v: Value) -> anyhow::Result<()> {
        validate_settings(v.as_object().unwrap())
    }

    #[test]
    fn settings_validation() {
        assert!(validate_settings(&default_settings()).is_ok());
        assert!(check(json!({"maxConcurrentConnects": 12, "theme": "light", "sidebarWidth": 231.5})).is_ok());
        assert!(
            check(json!({"someUiValue": {"anything": true}})).is_ok(),
            "unknown keys pass through"
        );
        assert!(check(json!({"maxConcurrentConnects": "lots"})).is_err());
        assert!(check(json!({"maxConcurrentConnects": 0})).is_err());
        assert!(check(json!({"apiPort": 80})).is_err());
        assert!(check(json!({"theme": "blue"})).is_err());
        assert!(check(json!({"microphone": "yes"})).is_err());
        assert!(check(json!({"drivePaths": ["C:\\", 3]})).is_err());
    }
}
