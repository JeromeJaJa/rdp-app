//! Multi-session RDP client engine.
//!
//! Each session runs on its own OS thread with a single-threaded Tokio runtime,
//! so one busy or misbehaving session can never stall the others (or the UI).
//! Decoded frames are written straight into host-provided shared memory
//! ([`SurfaceMemory`]) using a small lock-free layout the renderer polls; see
//! [`surface`] for the exact format.
//!
//! The engine knows nothing about Tauri or WebView2: the host plugs in a
//! [`SurfaceProvider`] for pixel memory and an event callback.

mod audio;
mod autodetect;
pub mod certs;
mod clipboard;
mod connect;
mod cursor;
mod devices;
mod errors;
mod gfx;
mod session;
pub mod surface;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, mpsc};
use tracing::{info, warn};

pub use certs::{CertDecision, CertPolicy, CertificateStore};
pub use clipboard::{ClipboardHost, SessionClipboard};
pub use surface::{HeapSurface, SurfaceKind, SurfaceMemory, SurfaceProvider};

/// Identifier of a session; the host uses its connection id.
pub type SessionId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Idle,
    Queued,
    Connecting,
    Connected,
    Reconnecting,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    Status {
        id: SessionId,
        status: SessionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Resolution {
        id: SessionId,
        width: u16,
        height: u16,
    },
    Codec {
        id: SessionId,
        codec: String,
    },
    /// CSS `cursor` value for the session (data-URL image or keyword).
    Cursor {
        id: SessionId,
        cursor: String,
    },
    /// A server certificate needs a decision; the connection attempt was refused.
    #[serde(rename_all = "camelCase")]
    Certificate {
        id: SessionId,
        endpoint: String,
        fingerprint: String,
        /// "unknown" or "changed".
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_fingerprint: Option<String>,
        #[serde(skip_serializing_if = "String::is_empty")]
        subject: String,
    },
}

pub type EventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CodecPreference {
    /// H.264 when the server supports it, otherwise the best graphics pipeline codec.
    #[default]
    Auto,
    H264,
    /// Classic RemoteFX surface commands: no graphics pipeline, no H.264.
    RemoteFx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DecoderPreference {
    /// GPU (Media Foundation / DXVA) when available, else software.
    #[default]
    Auto,
    Hardware,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioMode {
    Off,
    /// Only the session currently on screen is audible.
    #[default]
    Active,
    All,
}

/// RD Gateway (MS-TSGU) to tunnel through.
#[derive(Clone)]
pub struct GatewayParams {
    /// `host` or `host:port` (default 443).
    pub host: String,
    pub username: String,
    pub domain: Option<String>,
    pub password: String,
}

/// Local devices shared with the remote session.
#[derive(Debug, Clone, Default)]
pub struct DeviceRedirection {
    /// Folders or drive roots, e.g. `C:\\` or `D:\\Work`.
    pub drives: Vec<std::path::PathBuf>,
    pub printer: bool,
    pub smartcard: bool,
}

impl DeviceRedirection {
    pub fn any(&self) -> bool {
        !self.drives.is_empty() || self.printer || self.smartcard
    }
}

#[derive(Clone)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub domain: Option<String>,
    pub codec: CodecPreference,
    /// Ask for H.264 4:4:4 (AVC444, sharper text) when the server supports it.
    pub h264_444: bool,
    pub decoder: DecoderPreference,
    pub auto_reconnect: bool,
    /// Share the clipboard with this session (when the app has a clipboard host).
    pub clipboard: bool,
    pub gateway: Option<GatewayParams>,
    pub cert_policy: CertPolicy,
    pub audio: AudioMode,
    pub microphone: bool,
    pub devices: DeviceRedirection,
}

impl ConnectParams {
    /// Plain connection with default options.
    pub fn new(host: impl Into<String>, port: u16, username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            password: password.into(),
            domain: None,
            codec: CodecPreference::Auto,
            h264_444: true,
            decoder: DecoderPreference::Auto,
            auto_reconnect: true,
            clipboard: true,
            gateway: None,
            cert_policy: CertPolicy::Ignore,
            audio: AudioMode::Off,
            microphone: false,
            devices: DeviceRedirection::default(),
        }
    }
}

impl std::fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("codec", &self.codec)
            .field("gateway", &self.gateway.as_ref().map(|g| &g.host))
            .finish_non_exhaustive()
    }
}

/// Input from the UI, already translated to RDP terms.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum InputEvent {
    /// Scancode; extended keys carry the 0xE000 prefix (e.g. 0xE04B = left arrow).
    Key {
        code: u16,
        down: bool,
    },
    Unicode {
        ch: char,
        down: bool,
    },
    /// Pointer position in remote desktop pixels.
    Move {
        x: u16,
        y: u16,
    },
    /// Web `MouseEvent.button` numbering: 0 left, 1 middle, 2 right, 3 back, 4 forward.
    Button {
        button: u8,
        down: bool,
    },
    /// Wheel rotation in RDP units (120 per notch, positive = away from user / right).
    Wheel {
        delta: i16,
        horizontal: bool,
    },
    /// Release every pressed key and button (focus loss).
    ReleaseAll,
    /// Set the remote Caps/Num/Scroll Lock state to match the local keyboard.
    Sync {
        caps: bool,
        num: bool,
        scroll: bool,
    },
}

/// Desktop size requested for new connections and resizes, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub width: u16,
    pub height: u16,
    /// Scale factor in percent (100 = 96 DPI).
    pub scale: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineSettings {
    pub max_concurrent_connects: usize,
    /// Suppress server output for sessions that are not on screen.
    pub pause_hidden: bool,
    /// Grid thumbnail width in device pixels.
    pub thumbnail_width: u32,
    /// Minimum interval between thumbnail refreshes per session.
    pub thumbnail_interval_ms: u64,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            max_concurrent_connects: 6,
            pause_hidden: true,
            thumbnail_width: 480,
            thumbnail_interval_ms: 150,
        }
    }
}

/// What the UI currently shows for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ViewState {
    pub full: bool,
    pub thumb: bool,
}

pub(crate) enum Command {
    Input(Vec<InputEvent>),
    Resize(Viewport),
    View(ViewState),
    ThumbnailWidth(u32),
    PauseHidden(bool),
    CtrlAltDel,
    Clipboard(ironrdp_cliprdr::backend::ClipboardMessage),
    /// New audio mode for this session (live when its connection has an audio channel).
    Audio(AudioMode),
    /// Encoded DVC messages produced off-thread (microphone).
    SendDvc(Vec<ironrdp_svc::SvcMessage>),
    Screenshot(tokio::sync::oneshot::Sender<anyhow::Result<Screenshot>>),
    Disconnect,
}

/// A captured frame (RGBA, tightly packed).
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

struct SessionHandle {
    tx: mpsc::UnboundedSender<Command>,
    generation: u64,
    /// Whether the current connection negotiated audio playback.
    audio_channel: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) struct Shared {
    pub provider: Arc<dyn SurfaceProvider>,
    pub events: EventSink,
    pub connect_slots: Arc<Semaphore>,
    /// Latest state per session, so a reloaded UI can catch up.
    snapshot: Mutex<HashMap<SessionId, Snapshot>>,
    pub clipboard: Mutex<Option<Arc<dyn ClipboardHost>>>,
    pub certificates: Mutex<Option<Arc<dyn CertificateStore>>>,
}

#[derive(Default)]
struct Snapshot {
    status: Option<EngineEvent>,
    resolution: Option<EngineEvent>,
    codec: Option<EngineEvent>,
    cursor: Option<EngineEvent>,
}

impl Shared {
    pub(crate) fn emit(&self, event: EngineEvent) {
        {
            let mut snap = self.snapshot.lock();
            match &event {
                EngineEvent::Status { id, status, .. } => {
                    if *status == SessionStatus::Idle {
                        snap.remove(id);
                    } else {
                        snap.entry(id.clone()).or_default().status = Some(event.clone());
                    }
                }
                EngineEvent::Resolution { id, .. } => {
                    snap.entry(id.clone()).or_default().resolution = Some(event.clone())
                }
                EngineEvent::Codec { id, .. } => snap.entry(id.clone()).or_default().codec = Some(event.clone()),
                EngineEvent::Cursor { id, .. } => snap.entry(id.clone()).or_default().cursor = Some(event.clone()),
                // One-off prompts are not part of a session's lasting state.
                EngineEvent::Certificate { .. } => {}
            }
        }
        (self.events)(event);
    }
}

struct State {
    sessions: HashMap<SessionId, SessionHandle>,
    viewport: Viewport,
    settings: EngineSettings,
    active: Option<SessionId>,
    thumbnails: Vec<SessionId>,
    next_generation: u64,
    /// Sessions to start again as soon as their current thread has exited (reconnect).
    restarts: HashMap<SessionId, ConnectParams>,
}

/// Owns every session. Cheap to clone.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
    state: Arc<Mutex<State>>,
}

impl Engine {
    pub fn new(provider: Arc<dyn SurfaceProvider>, events: EventSink, settings: EngineSettings) -> Self {
        // Several crates in the graph enable rustls; pick the provider explicitly.
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            shared: Arc::new(Shared {
                provider,
                events,
                connect_slots: Arc::new(Semaphore::new(settings.max_concurrent_connects.max(1))),
                snapshot: Mutex::new(HashMap::new()),
                clipboard: Mutex::new(None),
                certificates: Mutex::new(None),
            }),
            state: Arc::new(Mutex::new(State {
                sessions: HashMap::new(),
                viewport: Viewport::default(),
                settings,
                active: None,
                thumbnails: Vec::new(),
                restarts: HashMap::new(),
                next_generation: 1,
            })),
        }
    }

    /// Enables clipboard redirection for connections started from now on.
    pub fn set_clipboard_host(&self, host: Option<Arc<dyn ClipboardHost>>) {
        *self.shared.clipboard.lock() = host;
    }

    /// Where pinned server certificates live (required for TOFU / prompt policies).
    pub fn set_certificate_store(&self, store: Option<Arc<dyn CertificateStore>>) {
        *self.shared.certificates.lock() = store;
    }

    /// Captures the session's current frame. Paused sessions are briefly resumed.
    pub async fn screenshot(&self, id: &str) -> anyhow::Result<Screenshot> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let state = self.state.lock();
            let handle = state
                .sessions
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("session is not running"))?;
            handle
                .tx
                .send(Command::Screenshot(tx))
                .map_err(|_| anyhow::anyhow!("session is not running"))?;
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| anyhow::anyhow!("screenshot timed out"))?
            .map_err(|_| anyhow::anyhow!("session ended"))?
    }

    /// Starts (or restarts) a session. Returns immediately; progress arrives as events.
    pub fn connect(&self, id: &str, params: ConnectParams) {
        start(&self.shared, &self.state, id, params);
    }

    /// Disconnects a running session and connects it again once it has fully stopped
    /// (or just connects when it is not running).
    pub fn reconnect(&self, id: &str, params: ConnectParams) {
        let mut state = self.state.lock();
        if let Some(handle) = state.sessions.get(id)
            && !handle.tx.is_closed()
        {
            let _ = handle.tx.send(Command::Disconnect);
            state.restarts.insert(id.to_owned(), params);
            return;
        }
        drop(state);
        self.connect(id, params);
    }

    pub fn disconnect(&self, id: &str) {
        let mut state = self.state.lock();
        state.restarts.remove(id);
        if let Some(handle) = state.sessions.get(id) {
            let _ = handle.tx.send(Command::Disconnect);
        }
    }

    pub fn disconnect_all(&self) {
        let mut state = self.state.lock();
        state.restarts.clear();
        for handle in state.sessions.values() {
            let _ = handle.tx.send(Command::Disconnect);
        }
    }

    pub fn input(&self, id: &str, events: Vec<InputEvent>) {
        if events.is_empty() {
            return;
        }
        let state = self.state.lock();
        if let Some(handle) = state.sessions.get(id) {
            let _ = handle.tx.send(Command::Input(events));
        }
    }

    /// Changes a session's audio mode without reconnecting. Returns false when the
    /// running connection was opened with audio off and so has no audio channel: the
    /// mode then takes effect from its next connection.
    pub fn set_audio(&self, id: &str, mode: AudioMode) -> bool {
        let mut state = self.state.lock();
        if let Some(params) = state.restarts.get_mut(id) {
            params.audio = mode;
        }
        let Some(handle) = state.sessions.get(id) else {
            return true;
        };
        let _ = handle.tx.send(Command::Audio(mode));
        mode == AudioMode::Off || handle.audio_channel.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn ctrl_alt_del(&self, id: &str) {
        let state = self.state.lock();
        if let Some(handle) = state.sessions.get(id) {
            let _ = handle.tx.send(Command::CtrlAltDel);
        }
    }

    /// New main-area size: new connections use it and running sessions resize to it.
    pub fn set_viewport(&self, viewport: Viewport) {
        let mut state = self.state.lock();
        if state.viewport == viewport {
            return;
        }
        state.viewport = viewport;
        for handle in state.sessions.values() {
            let _ = handle.tx.send(Command::Resize(viewport));
        }
    }

    pub fn set_visibility(&self, active: Option<String>, thumbnails: Vec<SessionId>) {
        let mut state = self.state.lock();
        let before: HashMap<SessionId, ViewState> = state
            .sessions
            .keys()
            .map(|id| (id.clone(), view_for(&state, id)))
            .collect();
        state.active = active;
        state.thumbnails = thumbnails;
        for (id, handle) in &state.sessions {
            let now = view_for(&state, id);
            if before.get(id) != Some(&now) {
                let _ = handle.tx.send(Command::View(now));
            }
        }
    }

    pub fn update_settings(&self, settings: EngineSettings) {
        let mut state = self.state.lock();
        let old = state.settings;
        state.settings = settings;

        if settings.max_concurrent_connects != old.max_concurrent_connects {
            let new = settings.max_concurrent_connects.max(1);
            let old = old.max_concurrent_connects.max(1);
            if new > old {
                self.shared.connect_slots.add_permits(new - old);
            } else {
                let forgotten = self.shared.connect_slots.forget_permits(old - new);
                if forgotten < old - new {
                    warn!("connect slot shrink deferred until running handshakes finish");
                }
            }
        }
        for handle in state.sessions.values() {
            if settings.thumbnail_width != old.thumbnail_width {
                let _ = handle.tx.send(Command::ThumbnailWidth(settings.thumbnail_width));
            }
            if settings.pause_hidden != old.pause_hidden {
                let _ = handle.tx.send(Command::PauseHidden(settings.pause_hidden));
            }
        }
    }

    pub fn settings(&self) -> EngineSettings {
        self.state.lock().settings
    }

    /// The latest status/resolution/codec/cursor event of every non-idle session,
    /// for replaying to a UI that (re)loaded after sessions started.
    pub fn snapshot(&self) -> Vec<EngineEvent> {
        let snap = self.shared.snapshot.lock();
        snap.values()
            .flat_map(|s| [&s.status, &s.resolution, &s.codec, &s.cursor])
            .flatten()
            .cloned()
            .collect()
    }

    /// Identifies the session thread currently running for `id` (changes on reconnect).
    pub fn generation(&self, id: &str) -> Option<u64> {
        let state = self.state.lock();
        state
            .sessions
            .get(id)
            .filter(|h| !h.tx.is_closed())
            .map(|h| h.generation)
    }

    /// Ids of sessions whose threads are still alive.
    pub fn running(&self) -> Vec<SessionId> {
        let state = self.state.lock();
        state
            .sessions
            .iter()
            .filter(|(_, h)| !h.tx.is_closed())
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Starts a session thread unless one is already running for `id`.
fn start(shared: &Arc<Shared>, state_lock: &Arc<Mutex<State>>, id: &str, params: ConnectParams) {
    let mut state = state_lock.lock();
    if let Some(existing) = state.sessions.get(id)
        && !existing.tx.is_closed()
    {
        return; // already running
    }

    let generation = state.next_generation;
    state.next_generation += 1;
    let (tx, rx) = mpsc::unbounded_channel();
    let view = view_for(&state, id);
    let audio_channel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let init = session::SessionInit {
        id: id.to_owned(),
        clipboard: SessionClipboard::new(tx.clone()),
        commands: tx.clone(),
        generation,
        params,
        viewport: state.viewport,
        settings: state.settings,
        view,
        audio_channel: Arc::clone(&audio_channel),
    };
    state.sessions.insert(
        id.to_owned(),
        SessionHandle {
            tx,
            generation,
            audio_channel,
        },
    );
    drop(state);

    info!(session = id, "starting session");
    session::spawn(Arc::clone(shared), cleanup(shared, state_lock), init, rx);
}

/// Runs when a session thread exits: forgets it, and starts it again if a reconnect
/// was requested meanwhile.
/// Called with (session id, generation) when a session thread exits.
pub(crate) type Cleanup = Arc<dyn Fn(&str, u64) + Send + Sync>;

fn cleanup(shared: &Arc<Shared>, state: &Arc<Mutex<State>>) -> Cleanup {
    let (shared, state) = (Arc::downgrade(shared), Arc::downgrade(state));
    Arc::new(move |id: &str, generation: u64| {
        let Some(state) = state.upgrade() else { return };
        let restart = {
            let mut st = state.lock();
            if st.sessions.get(id).is_some_and(|h| h.generation == generation) {
                st.sessions.remove(id);
            }
            st.restarts.remove(id)
        };
        if let (Some(params), Some(shared)) = (restart, shared.upgrade()) {
            start(&shared, &state, id, params);
        }
    })
}

fn view_for(state: &State, id: &str) -> ViewState {
    ViewState {
        full: state.active.as_deref() == Some(id),
        thumb: state.thumbnails.iter().any(|t| t == id),
    }
}
