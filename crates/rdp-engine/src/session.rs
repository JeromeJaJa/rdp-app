//! One RDP session: its own thread, its own single-threaded runtime.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use ironrdp_connector::ConnectionResult;
use ironrdp_connector::connection_activation::{ConnectionActivationFactory, ConnectionActivationState};
use ironrdp_core::WriteBuf;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::pointer::DecodedPointer;
use ironrdp_input::{MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::rdp::headers::ShareDataPdu;
use ironrdp_pdu::rdp::refresh_rectangle::RefreshRectanglePdu;
use ironrdp_pdu::rdp::suppress_output::SuppressOutputPdu;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::{FramedWrite as _, TokioFramed};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::connect::{self, AsyncReadWrite, Connected, clamp_size};
use crate::cursor::CursorCache;
use crate::errors::friendly;
use crate::gfx::{GfxOutput, GfxStats, SharedOutput};
use crate::surface::{SurfaceKind, SurfaceWriter, ThumbWriter, surface_len};
use crate::{
    Command, ConnectParams, EngineEvent, EngineSettings, InputEvent, SessionStatus, Shared, ViewState, Viewport,
};

type Stream = Box<dyn AsyncReadWrite + Unpin + Send + Sync>;
type Reader = TokioFramed<ReadHalf<Stream>>;
type Writer = TokioFramed<WriteHalf<Stream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_RECONNECT_ATTEMPTS: u32 = 8;
/// A session that stays up this long starts a fresh round of reconnect attempts if it
/// drops; one that fails sooner keeps counting, so a failure that repeats right after
/// connecting (a bad shared folder, a server that refuses the session) ends instead
/// of looping.
const STABLE_SESSION: Duration = Duration::from_secs(60);
/// Keep a hidden session's full-resolution surface around briefly so quick tab
/// switches don't reallocate.
const FULL_SURFACE_GRACE: Duration = Duration::from_secs(8);
/// Wait this long after a session leaves the screen before pausing its stream.
const PAUSE_DELAY: Duration = Duration::from_secs(2);
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Windows drops a monitor layout that arrives while it is still applying the previous
/// one (a tab dragged into a window and straight back): this long after asking, the
/// session checks it got the size and asks again, up to `RESIZE_TRIES` times.
const RESIZE_VERIFY: Duration = Duration::from_secs(3);
const RESIZE_TRIES: u8 = 3;

pub(crate) struct SessionInit {
    pub id: String,
    pub clipboard: crate::SessionClipboard,
    pub commands: mpsc::UnboundedSender<Command>,
    pub generation: u64,
    pub params: ConnectParams,
    pub viewport: Viewport,
    pub settings: EngineSettings,
    pub view: ViewState,
    /// Set for each connection attempt: whether it negotiates audio playback.
    pub audio_channel: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) fn spawn(
    shared: Arc<Shared>,
    cleanup: crate::Cleanup,
    init: SessionInit,
    rx: mpsc::UnboundedReceiver<Command>,
) {
    let id = init.id.clone();
    let generation = init.generation;
    let thread_name = format!("rdp-{}", id.chars().take(12).collect::<String>());
    let spawned = std::thread::Builder::new()
        .name(thread_name)
        .stack_size(8 * 1024 * 1024)
        .spawn({
            let shared = Arc::clone(&shared);
            move || {
                let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("tokio runtime");
                    rt.block_on(run(Arc::clone(&shared), init, rx))
                }));
                let (status, message) =
                    outcome.unwrap_or_else(|_| (SessionStatus::Error, Some("Session crashed unexpectedly".to_owned())));
                if let Some(host) = shared.clipboard.lock().clone() {
                    host.detach(&id);
                }
                // Remove the handle first so a reconnect triggered by this event starts fresh.
                cleanup(&id, generation);
                shared.emit(EngineEvent::Status { id, status, message });
            }
        });
    if let Err(e) = spawned {
        warn!(error = %e, "failed to spawn session thread");
    }
}

enum Exit {
    /// Local disconnect request.
    User,
    /// The server ended the session (logoff, admin disconnect, …).
    Server(String),
    /// Transport or protocol failure; may be retried.
    Failed(anyhow::Error),
}

struct Session {
    shared: Arc<Shared>,
    id: String,
    params: ConnectParams,
    viewport: Viewport,
    settings: EngineSettings,
    view: ViewState,
    hidden_since: Option<Instant>,

    full: Option<SurfaceWriter>,
    full_idle_since: Option<Instant>,
    thumb: Option<ThumbWriter>,
    /// The view changed: surfaces must be (re)synchronised with the framebuffer.
    views_dirty: bool,

    cursors: CursorCache,
    last_cursor: String,
    last_status: Option<(SessionStatus, Option<String>)>,
    /// Audio playback gate (see `audio`).
    audible: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.release_full();
        self.release_thumb();
    }
}

impl Session {
    fn status(&mut self, status: SessionStatus, message: Option<String>) {
        let next = (status, message);
        if self.last_status.as_ref() == Some(&next) {
            return;
        }
        self.last_status = Some(next.clone());
        self.shared.emit(EngineEvent::Status {
            id: self.id.clone(),
            status: next.0,
            message: next.1,
        });
    }

    fn cursor(&mut self, css: String) {
        if css != self.last_cursor {
            self.last_cursor = css.clone();
            self.shared.emit(EngineEvent::Cursor {
                id: self.id.clone(),
                cursor: css,
            });
        }
    }

    fn cursor_bitmap(&mut self, pointer: &DecodedPointer) {
        let css = self.cursors.css_for(pointer);
        self.cursor(css);
    }

    /// Applies a command received while not connected.
    fn apply_offline(&mut self, cmd: Command) {
        match cmd {
            Command::View(v) => self.set_view(v),
            Command::Resize(vp) => self.viewport = vp,
            Command::ThumbnailWidth(w) => self.settings.thumbnail_width = w,
            Command::PauseHidden(p) => self.settings.pause_hidden = p,
            Command::Audio(mode) => {
                self.params.audio = mode;
                self.update_audible();
            }
            Command::Screenshot(reply) => {
                let _ = reply.send(Err(anyhow::anyhow!("session is not connected")));
            }
            Command::Input(_)
            | Command::CtrlAltDel
            | Command::Clipboard(_)
            | Command::SendDvc(_)
            | Command::Companion(_)
            | Command::Disconnect => {}
        }
    }

    fn set_view(&mut self, v: ViewState) {
        if v == self.view {
            return;
        }
        let visible = v.full || v.thumb;
        let was_visible = self.view.full || self.view.thumb;
        if visible {
            self.hidden_since = None;
        } else if was_visible {
            self.hidden_since = Some(Instant::now());
        }
        self.view = v;
        self.views_dirty = true;
        self.update_audible();
    }

    fn update_audible(&self) {
        let audible = match self.params.audio {
            crate::AudioMode::Off => false,
            crate::AudioMode::All => true,
            crate::AudioMode::Active => self.view.full,
        };
        self.audible.store(audible, std::sync::atomic::Ordering::Relaxed);
    }

    fn should_pause(&self) -> bool {
        self.settings.pause_hidden
            && !(self.view.full || self.view.thumb)
            && self.hidden_since.is_none_or(|t| t.elapsed() >= PAUSE_DELAY)
    }

    fn release_full(&mut self) {
        if self.full.take().is_some() {
            self.shared.provider.release(&self.id, SurfaceKind::Full);
        }
        self.full_idle_since = None;
    }

    fn release_thumb(&mut self) {
        if self.thumb.take().is_some() {
            self.shared.provider.release(&self.id, SurfaceKind::Thumb);
        }
    }

    /// Makes the surfaces match the current view and framebuffer size.
    fn sync_views(&mut self, frame: Frame<'_>) {
        let (w, h) = (u32::from(frame.width), u32::from(frame.height));
        if w == 0 || h == 0 {
            return;
        }

        if self.view.full {
            self.full_idle_since = None;
            let fits = self.full.as_ref().is_some_and(|f| f.width() == w && f.height() == h);
            if !fits || self.views_dirty {
                if !fits {
                    self.release_full();
                    match self.allocate(SurfaceKind::Full, w, h) {
                        Some(writer) => self.full = Some(writer),
                        None => return,
                    }
                }
                // (Re)shown: the surface may hold an older frame — repaint everything.
                if let Some(full) = self.full.as_mut() {
                    let all = full_rect(w, h);
                    full.copy_rect(frame.data, frame.stride, &all);
                    full.publish(&all);
                }
            }
        } else if self.full.is_some() {
            let since = *self.full_idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= FULL_SURFACE_GRACE {
                self.release_full();
            }
        }

        if self.view.thumb {
            let target = self.settings.thumbnail_width;
            let fits = self.thumb.as_ref().is_some_and(|t| t.matches(w, h, target));
            if !fits {
                self.release_thumb();
                let (tw, th) = ThumbWriter::dims(w, h, target);
                if let Some(writer) = self.allocate(SurfaceKind::Thumb, tw, th) {
                    self.thumb = Some(ThumbWriter::new(writer, w, h));
                }
            }
            if let Some(thumb) = self.thumb.as_mut()
                && (!fits || self.views_dirty)
            {
                thumb.mark_all();
                thumb.flush(frame.data);
            }
        } else {
            self.release_thumb();
        }

        self.views_dirty = false;
    }

    fn allocate(&self, kind: SurfaceKind, w: u32, h: u32) -> Option<SurfaceWriter> {
        let mem = match self.shared.provider.allocate(&self.id, kind, w, h) {
            Ok(mem) => mem,
            Err(e) => {
                warn!(session = %self.id, ?kind, error = %e, "surface allocation failed");
                return None;
            }
        };
        debug_assert!(mem.len() >= surface_len(w, h));
        SurfaceWriter::new(mem, w, h)
            .inspect_err(|e| warn!(error = %e, "invalid surface"))
            .ok()
    }

    /// Pushes freshly decoded damage to whatever is on screen.
    fn present(&mut self, frame: Frame<'_>, damage: &[InclusiveRectangle]) {
        if damage.is_empty() {
            return;
        }
        if self.view.full
            && let Some(full) = self.full.as_mut()
        {
            for rect in damage {
                full.copy_rect(frame.data, frame.stride, rect);
                full.publish(rect);
            }
        }
        if self.view.thumb
            && let Some(thumb) = self.thumb.as_mut()
        {
            for rect in damage {
                thumb.mark(rect);
            }
        }
    }

    fn flush_thumb(&mut self, frame: Frame<'_>) {
        if let Some(thumb) = self.thumb.as_mut()
            && thumb.has_pending()
        {
            thumb.flush(frame.data);
        }
    }
}

/// The current desktop image: the graphics pipeline output when active, else the
/// classic (fast-path bitmap) framebuffer.
#[derive(Clone, Copy)]
pub(crate) struct Frame<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: u16,
    pub height: u16,
}

fn with_frame<R>(image: &DecodedImage, gfx: &SharedOutput, f: impl FnOnce(Frame<'_>) -> R) -> R {
    let out = gfx.lock();
    if out.active {
        f(Frame {
            data: &out.data,
            stride: usize::from(out.width) * 4,
            width: out.width,
            height: out.height,
        })
    } else {
        drop(out);
        f(Frame {
            data: image.data(),
            stride: image.stride(),
            width: image.width(),
            height: image.height(),
        })
    }
}

fn full_rect(w: u32, h: u32) -> InclusiveRectangle {
    InclusiveRectangle {
        left: 0,
        top: 0,
        right: (w - 1) as u16,
        bottom: (h - 1) as u16,
    }
}

async fn run(
    shared: Arc<Shared>,
    init: SessionInit,
    mut rx: mpsc::UnboundedReceiver<Command>,
) -> (SessionStatus, Option<String>) {
    let mut s = Session {
        shared: Arc::clone(&shared),
        id: init.id,
        params: init.params,
        viewport: init.viewport,
        settings: init.settings,
        view: init.view,
        hidden_since: (!(init.view.full || init.view.thumb)).then(Instant::now),
        full: None,
        full_idle_since: None,
        thumb: None,
        views_dirty: true,
        cursors: CursorCache::default(),
        last_cursor: String::new(),
        last_status: None,
        audible: Arc::default(),
    };

    let init_clipboard = init.clipboard;
    let init_commands = init.commands;
    s.update_audible();
    let mut attempt: u32 = 0;
    let mut ever_connected = false;
    let mut last_error: Option<String> = None;

    loop {
        let waiting = if ever_connected {
            SessionStatus::Reconnecting
        } else {
            SessionStatus::Queued
        };
        s.status(waiting, last_error.clone());

        // Wait for a handshake slot (connect-all queue).
        let permit = loop {
            tokio::select! {
                permit = Arc::clone(&shared.connect_slots).acquire_owned() => break permit.expect("semaphore never closes"),
                cmd = rx.recv() => match cmd {
                    None | Some(Command::Disconnect) => return (SessionStatus::Idle, None),
                    Some(cmd) => s.apply_offline(cmd),
                },
            }
        };

        s.status(
            if ever_connected {
                SessionStatus::Reconnecting
            } else {
                SessionStatus::Connecting
            },
            last_error.clone(),
        );

        let stats = Arc::new(GfxStats::default());
        let cert_issue: Arc<parking_lot::Mutex<Option<crate::certs::CertIssue>>> = Arc::default();
        let gfx_out: SharedOutput = Arc::new(parking_lot::Mutex::new(GfxOutput::new()));
        // Per connection: a link to an earlier connection's companion stops working.
        let (bulk_tx, bulk_rx) = mpsc::unbounded_channel::<Vec<ironrdp_svc::SvcMessage>>();
        let started = Instant::now();
        let attempt_result = {
            let params = s.params.clone();
            init.audio_channel.store(
                params.audio != crate::AudioMode::Off,
                std::sync::atomic::Ordering::Relaxed,
            );
            let fut = tokio::time::timeout(
                CONNECT_TIMEOUT,
                connect::connect(
                    &params,
                    s.viewport,
                    connect::ConnectContext {
                        gfx_stats: Arc::clone(&stats),
                        gfx_output: Arc::clone(&gfx_out),
                        clipboard: params
                            .clipboard
                            .then(|| clipboard_backend(&shared, &s.id, &init_clipboard))
                            .flatten(),
                        audible: Arc::clone(&s.audible),
                        commands: init_commands.clone(),
                        session: s.id.clone(),
                        bulk: bulk_tx,
                        companion_host: shared.companion.lock().clone(),
                        cert_store: shared.certificates.lock().clone(),
                        cert_issue: Arc::clone(&cert_issue),
                    },
                ),
            );
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    res = &mut fut => break res,
                    cmd = rx.recv() => match cmd {
                        None | Some(Command::Disconnect) => return (SessionStatus::Idle, None),
                        Some(cmd) => s.apply_offline(cmd),
                    },
                }
            }
        };
        drop(permit);

        let connected = match attempt_result {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                if let Some(issue) = cert_issue.lock().take() {
                    warn!(session = %s.id, endpoint = %issue.endpoint, changed = issue.changed, "certificate refused");
                    let msg = if issue.changed {
                        "The server's certificate changed since you last connected. Verify it before trusting it."
                    } else {
                        "The server's certificate isn't trusted yet."
                    };
                    shared.emit(EngineEvent::Certificate {
                        id: s.id.clone(),
                        endpoint: issue.endpoint,
                        fingerprint: issue.fingerprint,
                        reason: if issue.changed { "changed" } else { "unknown" },
                        previous_fingerprint: issue.previous,
                        subject: issue.subject,
                    });
                    return (SessionStatus::Error, Some(msg.to_owned()));
                }
                let msg = friendly(&e);
                warn!(session = %s.id, error = %format!("{e:#}"), "connect failed");
                if ever_connected && s.params.auto_reconnect && attempt < MAX_RECONNECT_ATTEMPTS {
                    attempt += 1;
                    last_error = Some(msg);
                    if !backoff(&mut s, &mut rx, attempt).await {
                        return (SessionStatus::Idle, None);
                    }
                    continue;
                }
                return (SessionStatus::Error, Some(msg));
            }
            Err(_) => {
                let msg = "Connection timed out during sign-in".to_owned();
                if ever_connected && s.params.auto_reconnect && attempt < MAX_RECONNECT_ATTEMPTS {
                    attempt += 1;
                    last_error = Some(msg);
                    if !backoff(&mut s, &mut rx, attempt).await {
                        return (SessionStatus::Idle, None);
                    }
                    continue;
                }
                return (SessionStatus::Error, Some(msg));
            }
        };

        info!(session = %s.id, elapsed_ms = started.elapsed().as_millis() as u64, "connected");
        ever_connected = true;
        let connected_at = Instant::now();
        s.status(SessionStatus::Connected, None);

        let exit = active(&mut s, connected, stats, gfx_out, &mut rx, bulk_rx).await;
        if let Some(host) = shared.clipboard.lock().clone() {
            host.detach(&s.id);
        }
        match exit {
            Exit::User => return (SessionStatus::Idle, None),
            Exit::Server(reason) => return (SessionStatus::Idle, Some(reason)),
            Exit::Failed(e) => {
                let msg = friendly(&e);
                warn!(session = %s.id, error = %format!("{e:#}"), "session failed");
                if connected_at.elapsed() >= STABLE_SESSION {
                    attempt = 0;
                }
                if s.params.auto_reconnect && attempt < MAX_RECONNECT_ATTEMPTS {
                    attempt += 1;
                    last_error = Some(msg);
                    if !backoff(&mut s, &mut rx, attempt).await {
                        return (SessionStatus::Idle, None);
                    }
                    continue;
                }
                return (SessionStatus::Error, Some(msg));
            }
        }
    }
}

/// Asks the host for an OS clipboard backend for the next connection, if clipboard
/// redirection is enabled.
fn clipboard_backend(
    shared: &Shared,
    id: &str,
    proxy: &crate::SessionClipboard,
) -> Option<Box<dyn ironrdp_cliprdr::backend::CliprdrBackend>> {
    let host = shared.clipboard.lock().clone()?;
    let factory = host.attach(id, proxy.clone())?;
    Some(factory.build_cliprdr_backend())
}

/// Sleeps before a reconnect attempt. Returns false if the user cancelled.
async fn backoff(s: &mut Session, rx: &mut mpsc::UnboundedReceiver<Command>, attempt: u32) -> bool {
    let delay = Duration::from_millis(match attempt {
        1 => 500,
        2 => 1500,
        3 => 3000,
        _ => 6000,
    });
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            cmd = rx.recv() => match cmd {
                None | Some(Command::Disconnect) => return false,
                Some(cmd) => s.apply_offline(cmd),
            },
        }
    }
}

struct Live {
    stage: ActiveStage,
    image: DecodedImage,
    gfx: SharedOutput,
    input: ironrdp_input::Database,
    writer: Writer,
    suppress_supported: bool,
    refresh_rect_supported: bool,
    suppressed: bool,
    resize_to: Option<(Viewport, Instant)>,
    /// Requests sent for the current target size without it taking effect.
    resize_tries: u8,
    resize_unavailable_logged: bool,
    /// Bytes written and time spent writing, since the last perf report.
    sent_bytes: u64,
    send_us: u64,
}

impl Live {
    /// Size of the current desktop image.
    fn dims(&self) -> (u16, u16) {
        let out = self.gfx.lock();
        if out.active {
            (out.width, out.height)
        } else {
            (self.image.width(), self.image.height())
        }
    }

    async fn send(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        if !frame.is_empty() {
            let t = Instant::now();
            self.writer.write_all(frame).await.context("write to server")?;
            self.send_us += t.elapsed().as_micros() as u64;
            self.sent_bytes += frame.len() as u64;
        }
        Ok(())
    }

    async fn send_outputs(&mut self, outputs: Vec<ActiveStageOutput>) -> anyhow::Result<()> {
        for out in outputs {
            if let ActiveStageOutput::ResponseFrame(frame) = out {
                self.send(&frame).await?;
            }
        }
        Ok(())
    }

    async fn apply_ops(&mut self, ops: Vec<Operation>) -> anyhow::Result<()> {
        let events = typematic_input(&mut self.input, ops);
        if events.is_empty() {
            return Ok(());
        }
        let outputs = self
            .stage
            .process_fastpath_input(&mut self.image, &events)
            .context("encode input")?;
        self.send_outputs(outputs).await
    }

    async fn set_suppressed(&mut self, suppress: bool) -> anyhow::Result<()> {
        if suppress == self.suppressed || !self.suppress_supported {
            return Ok(());
        }
        let (w, h) = self.dims();
        let desktop_rect = (!suppress).then(|| full_rect(u32::from(w), u32::from(h)));
        let mut buf = WriteBuf::new();
        self.stage
            .encode_static(
                &mut buf,
                ShareDataPdu::SuppressOutput(SuppressOutputPdu { desktop_rect }),
            )
            .context("encode suppress output")?;
        let frame = buf.into_inner();
        self.send(&frame).await?;
        self.suppressed = suppress;
        debug!(suppress, "display output toggled");
        Ok(())
    }

    /// Asks the server to resend the whole desktop (used to recover from decode errors).
    async fn request_repaint(&mut self) -> anyhow::Result<()> {
        let (w, h) = self.dims();
        if w == 0 || h == 0 || self.suppressed {
            return Ok(());
        }
        // Only a Refresh Rect. IronRDP's `request_full_redraw` prefers toggling Suppress
        // Output off and on, which Windows treats like minimize + restore: open menus close,
        // drags are cancelled and windows flash inactive.
        if !self.refresh_rect_supported {
            return Ok(());
        }
        let mut buf = WriteBuf::new();
        self.stage
            .encode_static(
                &mut buf,
                ShareDataPdu::RefreshRectangle(RefreshRectanglePdu {
                    areas_to_refresh: vec![full_rect(u32::from(w), u32::from(h))],
                }),
            )
            .context("encode repaint request")?;
        let frame = buf.into_inner();
        self.send(&frame).await
    }

    async fn resize_now(&mut self, vp: Viewport) -> anyhow::Result<ResizeStep> {
        let (w, h) = clamp_size(vp.width, vp.height);
        if (w, h) == self.dims() {
            return Ok(ResizeStep::Done);
        }
        match self
            .stage
            .encode_resize(u32::from(w), u32::from(h), Some(vp.scale.clamp(100, 500)), None)
        {
            Some(frame) => {
                let frame = frame.context("encode resize")?;
                self.send(&frame).await?;
                debug!(w, h, "requested dynamic resize");
                Ok(ResizeStep::Sent)
            }
            None => Ok(ResizeStep::NotReady),
        }
    }
}

enum ResizeStep {
    /// The session already has the size.
    Done,
    /// Asked the server for it.
    Sent,
    /// The display control channel isn't up yet.
    NotReady,
}

/// A batch of input, split where the order of non-key/mouse events matters.
/// Applies input operations, sending a key that is already down again as a plain
/// press: an auto-repeat, the way a physical keyboard reports it. (The input database
/// would release and press it again; held Shift then reads as repeated taps, which
/// opens the Sticky Keys prompt.)
fn typematic_input(
    input: &mut ironrdp_input::Database,
    ops: Vec<Operation>,
) -> Vec<ironrdp_pdu::input::fast_path::FastPathInputEvent> {
    use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};

    let mut events = Vec::new();
    let mut batch = Vec::with_capacity(ops.len());
    for op in ops {
        let repeat = match op {
            Operation::KeyPressed(sc) if input.is_key_pressed(sc) => {
                let (extended, code) = sc.as_u8();
                let flags = if extended {
                    KeyboardFlags::EXTENDED
                } else {
                    KeyboardFlags::empty()
                };
                Some(FastPathInputEvent::KeyboardEvent(flags, code))
            }
            _ => None,
        };
        match repeat {
            Some(event) => {
                events.extend(input.apply(std::mem::take(&mut batch)));
                events.push(event);
            }
            None => batch.push(op),
        }
    }
    events.extend(input.apply(batch));
    events
}

enum InputStep {
    Ops(Vec<Operation>),
    ReleaseAll,
    Sync { caps: bool, num: bool, scroll: bool },
}

/// Converts input to steps, keeping the original order (a release or lock sync in the
/// middle of a batch applies exactly there).
fn to_steps(events: Vec<InputEvent>) -> Vec<InputStep> {
    let mut steps = Vec::new();
    let mut ops = Vec::with_capacity(events.len());
    for ev in events {
        match ev {
            InputEvent::Key { code, down } => {
                let sc = Scancode::from_u16(code);
                ops.push(if down {
                    Operation::KeyPressed(sc)
                } else {
                    Operation::KeyReleased(sc)
                });
            }
            InputEvent::Unicode { ch, down } => ops.push(if down {
                Operation::UnicodeKeyPressed(ch)
            } else {
                Operation::UnicodeKeyReleased(ch)
            }),
            InputEvent::Move { x, y } => ops.push(Operation::MouseMove(MousePosition { x, y })),
            InputEvent::Button { button, down } => {
                if let Some(b) = MouseButton::from_web_button(button) {
                    ops.push(if down {
                        Operation::MouseButtonPressed(b)
                    } else {
                        Operation::MouseButtonReleased(b)
                    });
                }
            }
            InputEvent::Wheel { delta, horizontal } => ops.push(Operation::WheelRotations(WheelRotations {
                is_vertical: !horizontal,
                rotation_units: delta,
            })),
            InputEvent::ReleaseAll => {
                if !ops.is_empty() {
                    steps.push(InputStep::Ops(std::mem::take(&mut ops)));
                }
                steps.push(InputStep::ReleaseAll);
            }
            InputEvent::Sync { caps, num, scroll } => {
                if !ops.is_empty() {
                    steps.push(InputStep::Ops(std::mem::take(&mut ops)));
                }
                steps.push(InputStep::Sync { caps, num, scroll });
            }
        }
    }
    if !ops.is_empty() {
        steps.push(InputStep::Ops(ops));
    }
    steps
}

async fn active(
    s: &mut Session,
    connected: Connected,
    stats: Arc<GfxStats>,
    gfx: SharedOutput,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    mut bulk_rx: mpsc::UnboundedReceiver<Vec<ironrdp_svc::SvcMessage>>,
) -> Exit {
    let Connected { result, framed, socket } = connected;
    let ConnectionResult {
        io_channel_id,
        user_channel_id,
        message_channel_id,
        share_id,
        static_channels,
        desktop_size,
        enable_server_pointer,
        pointer_software_rendering,
        suppress_output_support,
        refresh_rect_support,
        activation_factory,
        compression_type,
        ..
    } = result;

    let (mut reader, writer): (Reader, Writer) = ironrdp_tokio::split_tokio_framed(framed);
    let mut bandwidth = crate::autodetect::BandwidthProbe::new(user_channel_id, message_channel_id);
    let mut live = Live {
        stage: ActiveStageBuilder {
            static_channels,
            user_channel_id,
            io_channel_id,
            message_channel_id,
            share_id,
            compression_type,
            enable_server_pointer,
            pointer_software_rendering,
        }
        .build(),
        image: DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height),
        gfx,
        input: ironrdp_input::Database::new(),
        writer,
        suppress_supported: suppress_output_support,
        refresh_rect_supported: refresh_rect_support,
        suppressed: false,
        resize_to: None,
        resize_tries: 0,
        resize_unavailable_logged: false,
        sent_bytes: 0,
        send_us: 0,
    };

    let mut perf = Perf::new();
    perf.socket = socket;
    let mut shots: Vec<(tokio::sync::oneshot::Sender<anyhow::Result<crate::Screenshot>>, Instant)> = Vec::new();
    let mut shots_ready_at: Option<Instant> = None;
    let mut rdpdr_tick = tokio::time::interval(Duration::from_millis(50));
    rdpdr_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let has_rdpdr = live.stage.get_svc_processor::<ironrdp_rdpdr::Rdpdr>().is_some();
    let mut frame_size = (0u16, 0u16);
    // Once the graphics pipeline drives the display, the classic framebuffer is
    // unused: shrink it to save a full desktop's worth of memory per session.
    let mut classic_image_shrunk = false;
    let mut last_refresh_request: Option<Instant> = None;
    let mut codec_label = String::new();
    let mut frames_since_label = 0u32;
    s.views_dirty = true;

    // If the requested size was clamped/changed by the server, ask again once the
    // display control channel is up.
    if (desktop_size.width, desktop_size.height) != clamp_size(s.viewport.width, s.viewport.height) {
        live.resize_to = Some((s.viewport, Instant::now() + Duration::from_secs(1)));
    }

    let mut thumb_tick = tokio::time::interval(Duration::from_millis(s.settings.thumbnail_interval_ms.max(33)));
    thumb_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut housekeeping = tokio::time::interval(Duration::from_millis(500));
    housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let result: anyhow::Result<Exit> = async {
        loop {
            // Keep surfaces in sync with resolution changes and view changes.
            if live.dims() != frame_size {
                frame_size = live.dims();
                s.views_dirty = true;
                s.shared.emit(EngineEvent::Resolution {
                    id: s.id.clone(),
                    width: frame_size.0,
                    height: frame_size.1,
                });
            }
            if s.views_dirty {
                with_frame(&live.image, &live.gfx, |f| s.sync_views(f));
                live.set_suppressed(s.should_pause()).await?;
            }

            let resize_deadline = live.resize_to.as_ref().map(|(_, at)| *at);
            let shots_deadline = shots
                .iter()
                .map(|(_, d)| *d)
                .chain(shots_ready_at)
                .min();

            tokio::select! {
                biased;

                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { return Ok(Exit::User) };
                    match cmd {
                        Command::Disconnect => return Ok(Exit::User),
                        Command::Input(events) => {
                            // Mouse buttons, releases and lock syncs (never keys or text) go to the
                            // log: they are what to look at when a drag or menu misbehaves.
                            for ev in &events {
                                match ev {
                                    InputEvent::Button { button, down } => info!(session = %s.id, button, down, "mouse button"),
                                    InputEvent::ReleaseAll => info!(session = %s.id, "release all input"),
                                    InputEvent::Sync { caps, num, scroll } => info!(session = %s.id, caps, num, scroll, "lock keys synced"),
                                    _ => {}
                                }
                            }
                            for step in to_steps(events) {
                                match step {
                                    InputStep::Ops(ops) => live.apply_ops(ops).await?,
                                    InputStep::ReleaseAll => {
                                        let events = live.input.release_all();
                                        let outputs = live.stage.process_fastpath_input(&mut live.image, &events).context("encode input")?;
                                        live.send_outputs(outputs).await?;
                                    }
                                    InputStep::Sync { caps, num, scroll } => {
                                        let event = ironrdp_input::synchronize_event(scroll, num, caps, false);
                                        let outputs = live.stage.process_fastpath_input(&mut live.image, &[event]).context("encode lock sync")?;
                                        live.send_outputs(outputs).await?;
                                    }
                                }
                            }
                        }
                        Command::SendDvc(messages) => {
                            let frame = live.stage.encode_dvc_messages(messages).context("encode DVC messages")?;
                            live.send(&frame).await?;
                        }
                        Command::Companion(update) => {
                            let event = match update {
                                crate::companion::Update::Connected { version, host, build } => {
                                    EngineEvent::CompanionConnected { id: s.id.clone(), version, host, build }
                                }
                                crate::companion::Update::Refused { protocol } => {
                                    EngineEvent::CompanionRefused { id: s.id.clone(), protocol }
                                }
                                crate::companion::Update::Disconnected => {
                                    EngineEvent::CompanionDisconnected { id: s.id.clone() }
                                }
                            };
                            s.shared.emit(event);
                        }
                        Command::Screenshot(reply) => {
                            if live.suppressed {
                                // Paused: resume and wait for a fresh frame before capturing.
                                live.set_suppressed(false).await?;
                                live.request_repaint().await?;
                                shots.push((reply, Instant::now() + Duration::from_millis(1500)));
                            } else {
                                let _ = reply.send(Ok(capture(&live)));
                            }
                        }
                        Command::Clipboard(message) => {
                            // A clipboard hiccup must never end the session.
                            match crate::clipboard::encode(&mut live.stage, message) {
                                Ok(Some(frame)) => live.send(&frame).await?,
                                Ok(None) => {}
                                Err(e) => warn!(session = %s.id, error = %format!("{e:#}"), "clipboard message dropped"),
                            }
                        }
                        Command::CtrlAltDel => {
                            let ctrl = Scancode::from_u16(0x1D);
                            let alt = Scancode::from_u16(0x38);
                            let del = Scancode::from_u16(0xE053);
                            live.apply_ops(vec![
                                Operation::KeyPressed(ctrl),
                                Operation::KeyPressed(alt),
                                Operation::KeyPressed(del),
                                Operation::KeyReleased(del),
                                Operation::KeyReleased(alt),
                                Operation::KeyReleased(ctrl),
                            ]).await?;
                        }
                        Command::Resize(vp) => {
                            s.viewport = vp;
                            live.resize_to = Some((vp, Instant::now() + RESIZE_DEBOUNCE));
                            live.resize_tries = 0;
                        }
                        Command::View(v) => {
                            s.set_view(v);
                        }
                        Command::ThumbnailWidth(w) => {
                            s.settings.thumbnail_width = w;
                            s.views_dirty = true;
                        }
                        Command::PauseHidden(p) => {
                            s.settings.pause_hidden = p;
                            s.views_dirty = true;
                        }
                        Command::Audio(mode) => {
                            s.params.audio = mode;
                            s.update_audible();
                        }
                    }
                }

                pdu = reader.read_pdu() => {
                    let (action, payload) = pdu.context("read from server")?;
                    perf.bytes_in += payload.len() as u64;
                    if let Some(reply) = bandwidth.on_frame(action, &payload) {
                        live.send(&reply).await?;
                    }
                    let t0 = Instant::now();
                    let outputs = live.stage.process(&mut live.image, action, &payload).context("process server data")?;
                    perf.process_us += t0.elapsed().as_micros() as u64;
                    perf.pdus += 1;
                    for out in outputs {
                        match out {
                            ActiveStageOutput::ResponseFrame(frame) => live.send(&frame).await?,
                            ActiveStageOutput::PointerDefault => s.cursor("default".to_owned()),
                            ActiveStageOutput::PointerHidden => s.cursor("none".to_owned()),
                            ActiveStageOutput::PointerBitmap(p) => s.cursor_bitmap(&p),
                            ActiveStageOutput::Terminate(reason) => {
                                return Ok(Exit::Server(describe_termination(&reason.description())));
                            }
                            ActiveStageOutput::AutoDetect(result) => {
                                debug!(session = %s.id, ?result, "network characteristics from server");
                            }
                            ActiveStageOutput::DeactivateAll => {
                                reactivate(&mut reader, &mut live, &activation_factory).await?;
                                classic_image_shrunk = false;
                                s.views_dirty = true;
                            }
                            _ => {}
                        }
                    }

                    if stats.needs_refresh.swap(false, std::sync::atomic::Ordering::Relaxed)
                        && last_refresh_request.is_none_or(|t: Instant| t.elapsed() >= Duration::from_secs(1))
                    {
                        last_refresh_request = Some(Instant::now());
                        live.request_repaint().await?;
                    }

                    if !classic_image_shrunk && live.gfx.lock().active {
                        live.image = DecodedImage::new(PixelFormat::RgbA32, 8, 8);
                        classic_image_shrunk = true;
                    }

                    let image_damage = live.stage.take_damage_regions();
                    let damage = {
                        let mut out = live.gfx.lock();
                        if out.active { out.take_damage() } else { image_damage }
                    };
                    if live.dims() != frame_size {
                        // Resolution changed mid-stream (graphics reset); handled at loop top.
                        continue;
                    }
                    if !shots.is_empty() && !damage.is_empty() && shots_ready_at.is_none() {
                        shots_ready_at = Some(Instant::now() + Duration::from_millis(250));
                    }
                    let t1 = Instant::now();
                    with_frame(&live.image, &live.gfx, |f| s.present(f, &damage));
                    perf.present_us += t1.elapsed().as_micros() as u64;

                    if !damage.is_empty() {
                        frames_since_label += 1;
                        if codec_label.is_empty() || frames_since_label >= 10 {
                            frames_since_label = 0;
                            let label = stats.label().unwrap_or_else(|| {
                                if stats.confirmed.load(std::sync::atomic::Ordering::Relaxed) {
                                    "Graphics Pipeline".to_owned()
                                } else {
                                    "Classic (RemoteFX/Bitmap)".to_owned()
                                }
                            });
                            if label != codec_label {
                                codec_label = label.clone();
                                s.shared.emit(EngineEvent::Codec { id: s.id.clone(), codec: label });
                            }
                        }
                    }
                    // A busy screen mustn't starve companion transfers: one batch per server PDU.
                    if let Ok(messages) = bulk_rx.try_recv() {
                        let frame = live.stage.encode_dvc_messages(messages).context("encode companion data")?;
                        live.send(&frame).await?;
                    }
                }

                // Companion transfers: after input and server data (both above).
                Some(messages) = bulk_rx.recv() => {
                    let frame = live.stage.encode_dvc_messages(messages).context("encode companion data")?;
                    live.send(&frame).await?;
                }

                _ = thumb_tick.tick(), if s.thumb.is_some() => {
                    let t = Instant::now();
                    with_frame(&live.image, &live.gfx, |f| s.flush_thumb(f));
                    perf.thumb_us += t.elapsed().as_micros() as u64;
                }

                _ = sleep_until_opt(resize_deadline), if resize_deadline.is_some() => {
                    if let Some((vp, _)) = live.resize_to.take() {
                        match live.resize_now(vp).await? {
                            ResizeStep::Done => live.resize_tries = 0,
                            ResizeStep::Sent if live.resize_tries < RESIZE_TRIES => {
                                live.resize_tries += 1;
                                live.resize_to = Some((vp, Instant::now() + RESIZE_VERIFY));
                            }
                            ResizeStep::Sent => {
                                debug!("server kept its size after repeated resize requests");
                                live.resize_tries = 0;
                            }
                            // Display control not ready yet; retry shortly (gives up quietly
                            // on servers without the channel).
                            ResizeStep::NotReady if !live.resize_unavailable_logged => {
                                live.resize_unavailable_logged = true;
                                debug!("display control not ready; will retry resize");
                                live.resize_to = Some((vp, Instant::now() + Duration::from_secs(2)));
                            }
                            ResizeStep::NotReady => {}
                        }
                    }
                }

                _ = sleep_until_opt(shots_deadline), if shots_deadline.is_some() => {
                    for (reply, _) in shots.drain(..) {
                        let _ = reply.send(Ok(capture(&live)));
                    }
                    shots_ready_at = None;
                }

                _ = rdpdr_tick.tick(), if has_rdpdr => {
                    let messages = match live.stage.get_svc_processor_mut::<ironrdp_rdpdr::Rdpdr>() {
                        Some(rdpdr) => rdpdr.poll_deferred_messages().map_err(|e| anyhow::anyhow!("device redirection: {e}"))?,
                        None => Vec::new(),
                    };
                    if !messages.is_empty() {
                        let frame = live
                            .stage
                            .process_svc_messages_by_name(&ironrdp_rdpdr::Rdpdr::NAME, messages)
                            .context("encode device redirection")?;
                        live.send(&frame).await?;
                    }
                }

                _ = housekeeping.tick() => {
                    if perf.since.elapsed() >= Duration::from_secs(5) {
                        perf.report(&s.id, &stats, std::mem::take(&mut live.sent_bytes), std::mem::take(&mut live.send_us));
                    }
                    // Delayed pause, full-surface release after the grace period.
                    if s.full.is_some() && !s.view.full {
                        s.views_dirty = true;
                    }
                    let pause = s.should_pause() && shots.is_empty();
                    if pause != live.suppressed {
                        live.set_suppressed(pause).await?;
                    }
                    match crate::clipboard::drive_timeouts(&mut live.stage) {
                        Ok(Some(frame)) => live.send(&frame).await?,
                        Ok(None) => {}
                        Err(e) => warn!(session = %s.id, error = %e, "clipboard timeouts"),
                    }
                }
            }
        }
    }
    .await;

    match result {
        Ok(exit) => exit,
        Err(e) => Exit::Failed(e),
    }
}

/// Per-session CPU accounting, logged at debug level (`RDP_LOG=rdp_engine=debug`).
struct Perf {
    since: Instant,
    /// The session's TCP socket, and its state at the last report.
    socket: Option<u64>,
    tcp: Option<crate::tcp_info::TcpInfo>,
    pdus: u64,
    bytes_in: u64,
    process_us: u64,
    present_us: u64,
    thumb_us: u64,
    decode_us: u64,
    decoded: u64,
}

impl Perf {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            socket: None,
            tcp: None,
            pdus: 0,
            bytes_in: 0,
            process_us: 0,
            present_us: 0,
            thumb_us: 0,
            decode_us: 0,
            decoded: 0,
        }
    }

    fn report(&mut self, id: &str, stats: &GfxStats, bytes_out: u64, send_us: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let secs = self.since.elapsed().as_secs_f64();
        let decode_us = stats.decode_us.load(Relaxed) - self.decode_us;
        let decoded = stats.decoded.load(Relaxed) - self.decoded;
        let pct = |us: u64| us as f64 / 1e4 / secs;
        // Heavy traffic (file copies, big pastes): how fast it flows and how busy this
        // session's loop is, to tell a slow link or server from a slow client.
        let mib_s = |bytes: u64| bytes as f64 / 1_048_576.0 / secs;
        // The connection's side of it: what held sending back (see `tcp_info`).
        let tcp_now = self.socket.and_then(crate::tcp_info::query);
        let tcp = match (tcp_now, self.tcp) {
            (Some(now), Some(before)) => crate::tcp_info::Interval { now, before }.to_string(),
            _ => String::new(),
        };
        if mib_s(self.bytes_in) >= 0.5 || mib_s(bytes_out) >= 0.5 {
            info!(
                session = id,
                in_mib_s = format!("{:.2}", mib_s(self.bytes_in)),
                out_mib_s = format!("{:.2}", mib_s(bytes_out)),
                pdus_per_s = format!("{:.0}", self.pdus as f64 / secs),
                busy_pct = format!(
                    "{:.0}",
                    pct(self.process_us + self.present_us + self.thumb_us + decode_us + send_us)
                ),
                writing_pct = format!("{:.0}", pct(send_us)),
                %tcp,
                "session traffic"
            );
        }
        debug!(
            session = id,
            pdus_per_s = format!("{:.0}", self.pdus as f64 / secs),
            h264_fps = format!("{:.1}", decoded as f64 / secs),
            process_pct = format!("{:.1}", pct(self.process_us)),
            h264_pct = format!("{:.1}", pct(decode_us)),
            present_pct = format!("{:.1}", pct(self.present_us)),
            thumb_pct = format!("{:.1}", pct(self.thumb_us)),
            "cpu (% of one core)"
        );
        let load = |a: &std::sync::atomic::AtomicU64| a.load(Relaxed);
        debug!(
            session = id,
            avc420 = load(&stats.avc),
            avc444 = load(&stats.avc444),
            progressive = load(&stats.progressive),
            clear = load(&stats.clear),
            planar = load(&stats.planar),
            other = load(&stats.other),
            prog_tiles = ?stats.prog_tiles.each_ref().map(load),
            "codec totals (progressive tiles: simple, first, first@full, upgrade, upgrade@full)"
        );
        *self = Self {
            decode_us: stats.decode_us.load(Relaxed),
            decoded: stats.decoded.load(Relaxed),
            socket: self.socket,
            tcp: tcp_now,
            ..Self::new()
        };
    }
}

/// Copies the current frame (opaque RGBA).
fn capture(live: &Live) -> crate::Screenshot {
    with_frame(&live.image, &live.gfx, |f| {
        let (w, h) = (usize::from(f.width), usize::from(f.height));
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            rgba.extend_from_slice(&f.data[y * f.stride..y * f.stride + w * 4]);
        }
        for px in rgba.as_chunks_mut::<4>().0 {
            px[3] = 0xFF;
        }
        crate::Screenshot {
            width: w as u32,
            height: h as u32,
            rgba,
        }
    })
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Deactivation-Reactivation Sequence (server-side resolution change on servers
/// without the graphics pipeline, reconnection of a disconnected session, …).
async fn reactivate(reader: &mut Reader, live: &mut Live, factory: &ConnectionActivationFactory) -> anyhow::Result<()> {
    debug!("deactivation-reactivation sequence");
    let mut sequence = factory.create();
    let mut buf = WriteBuf::new();
    loop {
        let written = ironrdp_tokio::single_sequence_step_read(reader, &mut sequence, &mut buf)
            .await
            .context("reactivation step")?;
        if written.size().is_some() {
            let frame = buf.filled().to_vec();
            live.send(&frame).await?;
        }
        if let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
            static_channel_chunk_size,
            suppress_output_support,
            refresh_rect_support,
            ..
        } = sequence.connection_activation_state()
        {
            live.refresh_rect_supported = refresh_rect_support;
            live.image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
            live.suppress_supported = suppress_output_support;
            live.suppressed = false;
            if !live.stage.reactivate(
                sequence.io_channel_id(),
                sequence.user_channel_id(),
                share_id,
                enable_server_pointer,
                pointer_software_rendering,
                static_channel_chunk_size,
            ) {
                anyhow::bail!("invalid static channel chunk size after reactivation");
            }
            return Ok(());
        }
    }
}

fn describe_termination(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("user initiated") || lower.contains("server initiated") || lower.contains("provider") {
        return "Disconnected by the remote computer".to_owned();
    }
    if lower.contains("logoff") || lower.contains("logged off") {
        return "Signed out".to_owned();
    }
    if lower.contains("another") || lower.contains("replaced") {
        return "Another user connected to this session".to_owned();
    }
    if lower.contains("idle") {
        return "Disconnected: idle timeout".to_owned();
    }
    let mut msg = raw.to_owned();
    if let Some(first) = msg.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    msg
}

#[cfg(test)]
mod input_order_tests {
    use super::*;

    #[test]
    fn release_and_sync_apply_exactly_where_they_occur() {
        let steps = to_steps(vec![
            InputEvent::Button { button: 0, down: true },
            InputEvent::ReleaseAll,
            InputEvent::Move { x: 5, y: 6 },
            InputEvent::Sync {
                caps: true,
                num: false,
                scroll: false,
            },
            InputEvent::Button { button: 0, down: false },
        ]);
        let shape: Vec<&str> = steps
            .iter()
            .map(|s| match s {
                InputStep::Ops(_) => "ops",
                InputStep::ReleaseAll => "release",
                InputStep::Sync { .. } => "sync",
            })
            .collect();
        assert_eq!(shape, ["ops", "release", "ops", "sync", "ops"]);
    }

    #[test]
    fn held_keys_repeat_without_releasing() {
        use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
        let shift = Scancode::from_u16(0x2A);
        let mut db = ironrdp_input::Database::new();
        let first = typematic_input(&mut db, vec![Operation::KeyPressed(shift)]);
        let repeats = typematic_input(
            &mut db,
            vec![Operation::KeyPressed(shift), Operation::KeyPressed(shift)],
        );
        let up = typematic_input(&mut db, vec![Operation::KeyReleased(shift)]);
        let press = FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x2A);
        assert_eq!(first.as_slice(), [press]);
        assert_eq!(repeats.as_slice(), [press, press], "no release between repeats");
        assert_eq!(
            up.as_slice(),
            [FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x2A)]
        );
    }
}
