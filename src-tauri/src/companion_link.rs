//! Attached companions: host stats, file sends, and fast clipboard copies.
//!
//! The engine hands each companion that attaches to a session to [`Companions`] (its
//! [`CompanionHost`]). From then on this module talks to it: it asks for host stats at
//! the interval the settings say, streams files to it ("Send files to…", and copies made
//! on this PC), and receives the files copied on the remote when they're pasted here.
//! Transfers use [`rdp_companion_proto::transfer`] on both ends.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rdp_companion_proto::transfer::{self, CHUNK, DiskFiles, Inflow, Listing, Note, Outbox, Produced, Pump, Source};
use rdp_companion_proto::{ClipFormat, Entry, HostStats, Intent, Message};
use rdp_direct::{DirectSlot, Peer, Prepared};
use rdp_engine::{CompanionHandler, CompanionHost, CompanionLink, Engine, InputEvent};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Samples of host stats kept per session (10 minutes at the companion's 5 s sampling).
const STATS_HISTORY: usize = 120;

/// How long a paste waits for the companion to list what the remote copied.
const LIST_WAIT: Duration = Duration::from_secs(30);
/// A paste through the companion falls back to the clipboard channel if nothing of the
/// remote's copy arrives this soon...
const PASTE_FIRST_WAIT: Duration = Duration::from_secs(5);
/// ...and fails if it then stalls this long.
const PASTE_STALL: Duration = Duration::from_secs(30);
/// How long a drag onto a session waits for the companion to get ready.
const DRAG_READY_WAIT: Duration = Duration::from_secs(3);

/// What a drag onto a session is doing, as the UI hears it (`remote-drag` events).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDragView {
    pub session: String,
    pub id: u64,
    /// What dropping where the pointer is would do on the remote (`DROPEFFECT` bits).
    pub effect: u32,
    /// The remote drag is over (dropped, or cancelled).
    pub ended: bool,
}

/// A drag of local files onto a session, driven through its companion.
struct RemoteDrag {
    id: u64,
    /// Answered when the companion has its window under the pointer.
    ready: Option<mpsc::SyncSender<()>>,
    /// The button is down on the remote (we pressed it).
    pressed: bool,
    cancelled: bool,
    at: (u16, u16),
}

/// Host stats as the UI gets them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsView {
    /// When the sample arrived (ms since the Unix epoch).
    pub at: u64,
    pub span: f32,
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_read: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_write: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_busy: Option<f32>,
    pub volumes: Vec<VolumeView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeView {
    pub name: String,
    pub free: u64,
    pub total: u64,
}

impl StatsView {
    fn new(stats: HostStats) -> Self {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            at,
            span: stats.span,
            cpu: stats.cpu,
            mem_used: stats.mem_used,
            mem_total: stats.mem_total,
            net_rx: stats.net_rx,
            net_tx: stats.net_tx,
            disk_read: stats.disk.as_ref().map(|d| d.read),
            disk_write: stats.disk.as_ref().map(|d| d.write),
            disk_busy: stats.disk.as_ref().map(|d| d.busy),
            volumes: stats
                .volumes
                .into_iter()
                .map(|v| VolumeView {
                    name: v.name,
                    free: v.free,
                    total: v.total,
                })
                .collect(),
        }
    }
}

/// Where a send is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferState {
    /// Waiting for the session to connect and its companion to attach.
    Waiting,
    Sending,
    Done,
    Failed,
    Cancelled,
}

/// A "Send files to…" transfer, as the UI shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    /// The connection it goes to.
    pub session: String,
    pub id: u64,
    /// "report.pdf", or "3 items".
    pub label: String,
    /// Where it's saved on the remote, as asked.
    pub folder: String,
    pub files: usize,
    pub total: u64,
    pub sent: u64,
    pub state: TransferState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// "report.pdf", or "3 items".
pub fn label(listing: &Listing) -> String {
    let top: Vec<&Entry> = listing.entries.iter().filter(|e| !e.path.contains('\\')).collect();
    match top.as_slice() {
        [one] => one.path.clone(),
        many => format!("{} items", many.len()),
    }
}

/// Every attached companion, and every send in progress.
pub struct Companions {
    me: Weak<Companions>,
    app: AppHandle,
    /// For pressing and moving the remote pointer during drags.
    engine: Engine,
    attached: Mutex<HashMap<String, Arc<Attached>>>,
    /// Recent host stats per session, oldest first (enough for the trend lines).
    stats: Mutex<HashMap<String, VecDeque<StatsView>>>,
    transfers: Arc<Mutex<HashMap<u64, TransferView>>>,
    next_id: AtomicU64,
    prefs: Mutex<Option<PrefsSource>>,
}

/// What a session's settings ask of its companion.
#[derive(Debug, Clone, Default)]
pub struct CompanionPrefs {
    /// Seconds between host stats reports (0 = off).
    pub stats_interval: u32,
    /// Open a direct link for transfers.
    pub direct: bool,
    /// The host the session connects to (one more address to try for the direct link).
    pub host: String,
}

/// Looks up a session's [`CompanionPrefs`].
pub type PrefsSource = Arc<dyn Fn(&str) -> CompanionPrefs + Send + Sync>;

impl Companions {
    pub fn new(app: AppHandle, engine: Engine) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            app,
            engine,
            attached: Mutex::default(),
            stats: Mutex::default(),
            transfers: Arc::default(),
            next_id: AtomicU64::new(2),
            prefs: Mutex::default(),
        })
    }

    /// Where to look up what a session's settings ask of its companion.
    pub fn set_prefs_source(&self, source: PrefsSource) {
        *self.prefs.lock() = Some(source);
    }

    fn prefs_for(&self, session: &str) -> CompanionPrefs {
        let source = self.prefs.lock().clone();
        source.map(|f| f(session)).unwrap_or_default()
    }

    /// Transfer ids this side picks are even (the companion's are odd).
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(2, Ordering::Relaxed)
    }

    fn get(&self, session: &str) -> Option<Arc<Attached>> {
        self.attached.lock().get(session).cloned()
    }

    pub fn is_attached(&self, session: &str) -> bool {
        self.attached.lock().contains_key(session)
    }

    /// Latest host stats per session.
    pub fn stats(&self) -> HashMap<String, StatsView> {
        self.stats
            .lock()
            .iter()
            .filter_map(|(id, history)| Some((id.clone(), history.back()?.clone())))
            .collect()
    }

    /// Recent host stats per session, oldest first.
    pub fn stats_history(&self) -> HashMap<String, Vec<StatsView>> {
        self.stats
            .lock()
            .iter()
            .map(|(id, history)| (id.clone(), history.iter().cloned().collect()))
            .collect()
    }

    /// Sends in progress or recently finished.
    pub fn transfers(&self) -> Vec<TransferView> {
        self.transfers.lock().values().cloned().collect()
    }

    fn interval_for(&self, session: &str) -> u32 {
        self.prefs_for(session).stats_interval
    }

    /// Re-sends each attached companion its stats interval (settings changed).
    pub fn refresh_intervals(&self) {
        let attached: Vec<Arc<Attached>> = self.attached.lock().values().cloned().collect();
        for a in attached {
            let secs = self.interval_for(&a.session);
            a.send(&Message::StatsInterval { secs });
            if secs == 0 && self.stats.lock().remove(&a.session).is_some() {
                let _ = self
                    .app
                    .emit("host-stats", serde_json::json!({ "id": a.session, "samples": null }));
            }
        }
    }

    fn update(
        transfers: &Mutex<HashMap<u64, TransferView>>,
        app: &AppHandle,
        id: u64,
        f: impl FnOnce(&mut TransferView),
    ) {
        let view = {
            let mut transfers = transfers.lock();
            let Some(view) = transfers.get_mut(&id) else { return };
            f(view);
            view.clone()
        };
        let _ = app.emit("transfer", &view);
    }

    /// Registers a send that will start once the session's companion is attached.
    pub fn register(&self, session: &str, listing: &Listing, folder: &str) -> u64 {
        let id = self.next_id();
        let view = TransferView {
            session: session.to_owned(),
            id,
            label: label(listing),
            folder: folder.to_owned(),
            files: listing.entries.iter().filter(|e| !e.dir).count(),
            total: listing.total(),
            sent: 0,
            state: TransferState::Waiting,
            error: None,
        };
        self.transfers.lock().insert(id, view.clone());
        let _ = self.app.emit("transfer", &view);
        id
    }

    /// Starts a registered send: streams `listing` into `folder` on the remote.
    pub fn start(&self, id: u64, entries: Arc<[Entry]>, paths: Vec<PathBuf>, folder: &str) -> anyhow::Result<()> {
        let session = self
            .transfers
            .lock()
            .get(&id)
            .filter(|t| t.state == TransferState::Waiting)
            .map(|t| t.session.clone())
            .ok_or_else(|| anyhow::anyhow!("the send was cancelled"))?;
        let attached = self
            .get(&session)
            .ok_or_else(|| anyhow::anyhow!("the companion isn't attached"))?;
        attached.sources.lock().insert(id);
        attached
            .pump
            .start(Source::new(id, Arc::clone(&entries), Box::new(DiskFiles::new(paths))));
        if !attached.send(&Message::Offer {
            id,
            entries: entries.to_vec(),
            intent: Intent::Save {
                folder: folder.to_owned(),
            },
        }) {
            anyhow::bail!("the connection closed");
        }
        Self::update(&self.transfers, &self.app, id, |t| t.state = TransferState::Sending);
        Ok(())
    }

    /// Marks a send failed.
    pub fn fail(&self, id: u64, error: &str) {
        Self::update(&self.transfers, &self.app, id, |t| {
            if matches!(t.state, TransferState::Waiting | TransferState::Sending) {
                t.state = TransferState::Failed;
                t.error = Some(error.to_owned());
            }
        });
    }

    /// Stops a send (the remote deletes what it had saved of the file in progress).
    pub fn cancel(&self, id: u64) {
        let session = self.transfers.lock().get(&id).map(|t| t.session.clone());
        if let Some(attached) = session.and_then(|s| self.get(&s))
            && attached.sources.lock().remove(&id)
        {
            attached.pump.withdraw(id);
        }
        Self::update(&self.transfers, &self.app, id, |t| {
            if matches!(t.state, TransferState::Waiting | TransferState::Sending) {
                t.state = TransferState::Cancelled;
            }
        });
    }

    /// Forgets finished sends (the UI's "clear").
    pub fn clear_finished(&self) {
        self.transfers
            .lock()
            .retain(|_, t| matches!(t.state, TransferState::Waiting | TransferState::Sending));
    }

    /// A send's state, if it's still known.
    pub fn state(&self, id: u64) -> Option<TransferState> {
        self.transfers.lock().get(&id).map(|t| t.state)
    }

    // ── Dragging files onto a session ───────────────────────────────────────

    /// Starts dragging `listing` onto `session` at (`x`, `y`) (remote pixels): the
    /// companion gets a window under the pointer, then we press the button there, which
    /// starts a real drag on the remote. Blocking (waits for the companion, briefly).
    pub fn drag_in(&self, session: &str, listing: Listing, x: u16, y: u16) -> anyhow::Result<u64> {
        let attached = self
            .get(session)
            .ok_or_else(|| anyhow::anyhow!("no companion is attached to this session"))?;
        self.drag_cancel(session);
        let id = self.next_id();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        *attached.drag.lock() = Some(RemoteDrag {
            id,
            ready: Some(ready_tx),
            pressed: false,
            cancelled: false,
            at: (x, y),
        });
        let entries: Arc<[Entry]> = listing.entries.into();
        attached.sources.lock().insert(id);
        attached.pump.start(Source::new(
            id,
            Arc::clone(&entries),
            Box::new(DiskFiles::new(listing.paths)),
        ));
        attached.send(&Message::DragIn {
            id,
            entries: entries.to_vec(),
            x: i32::from(x),
            y: i32::from(y),
        });
        if ready_rx.recv_timeout(DRAG_READY_WAIT).is_err() {
            attached.drag.lock().take();
            anyhow::bail!("the companion didn't take the drag");
        }
        let mut drag = attached.drag.lock();
        match drag.as_mut() {
            Some(d) if d.id == id && !d.cancelled => {
                let (x, y) = d.at;
                d.pressed = true;
                self.engine.input(
                    session,
                    vec![InputEvent::Move { x, y }, InputEvent::Button { button: 0, down: true }],
                );
                Ok(id)
            }
            _ => anyhow::bail!("the drag was cancelled"),
        }
    }

    /// The pointer moved over the session during a drag.
    pub fn drag_move(&self, session: &str, x: u16, y: u16) {
        let Some(attached) = self.get(session) else { return };
        let mut drag = attached.drag.lock();
        let Some(d) = drag.as_mut().filter(|d| !d.cancelled) else {
            return;
        };
        d.at = (x, y);
        if d.pressed {
            self.engine.input(session, vec![InputEvent::Move { x, y }]);
        }
    }

    /// The files were dropped on the session: let go of the button there.
    pub fn drag_drop(&self, session: &str, x: u16, y: u16) {
        let Some(attached) = self.get(session) else { return };
        let mut drag = attached.drag.lock();
        let Some(d) = drag.as_mut().filter(|d| !d.cancelled) else {
            return;
        };
        d.at = (x, y);
        if d.pressed {
            d.pressed = false;
            self.engine.input(
                session,
                vec![InputEvent::Move { x, y }, InputEvent::Button { button: 0, down: false }],
            );
        }
    }

    /// The drag left the session (or was cancelled): end the remote drag without a drop.
    /// The button comes up once the companion says the drag is over.
    pub fn drag_cancel(&self, session: &str) {
        let Some(attached) = self.get(session) else { return };
        let mut drag = attached.drag.lock();
        let Some(d) = drag.as_mut().filter(|d| !d.cancelled) else {
            return;
        };
        d.cancelled = true;
        attached.send(&Message::DragCancel { id: d.id });
        if d.pressed {
            // The remote drag notices the cancel on its next input.
            let (x, y) = d.at;
            self.engine.input(session, vec![InputEvent::Move { x, y }]);
        } else {
            drag.take();
        }
    }

    // ── Dragging files out of a session ────────────────────────────────────

    /// The pointer left `session` with the button held, at (`x`, `y`) (remote pixels, at
    /// the edge): if the remote is dragging files, carry the drag on here.
    pub fn drag_out_probe(&self, session: &str, x: u16, y: u16) {
        let Some(attached) = self.get(session) else { return };
        let id = self.next_id();
        *attached.drag_out.lock() = Some((id, (x, y)));
        attached.send(&Message::DragOutProbe {
            id,
            x: i32::from(x),
            y: i32::from(y),
        });
    }

    /// The remote is dragging these files out: start a drag of them here.
    fn dragged_out(&self, attached: &Attached, id: u64, entries: Vec<Entry>) {
        let probe = attached.drag_out.lock().take();
        if probe.map(|(probe, _)| probe) != Some(id)
            || entries.iter().any(|e| transfer::safe_relative(&e.path).is_none())
        {
            attached.send(&Message::Close { id, error: None });
            return;
        }
        attached.inflows.lock().retain(|_, inflow| !inflow.is_closed());
        let inflow = Arc::new(Inflow::new(id, entries.into(), attached.outbox()));
        attached.inflows.lock().insert(id, Arc::clone(&inflow));
        // The remote drag is done with: its button comes up (over the companion's target,
        // which takes nothing). The drag goes on here, under the user's own button.
        self.engine
            .input(&attached.session, vec![InputEvent::Button { button: 0, down: false }]);
        #[cfg(windows)]
        {
            if !primary_button_down() {
                // Already let go: a drag now would drop wherever the pointer happens to be.
                inflow.finish(None);
                return;
            }
            let files = Arc::new(rdp_vfiles::InflowFiles::new(inflow));
            let started = rdp_vfiles::drag(files, rdp_vfiles::Options::default(), move |effect| {
                tracing::info!(effect, "files dragged out of a session were dropped");
            });
            if let Err(error) = started {
                tracing::warn!(%error, "couldn't start dragging the files out");
            }
        }
        #[cfg(not(windows))]
        inflow.finish(None);
    }

    fn drag_event(&self, session: &str, id: u64, effect: u32, ended: bool) {
        let view = RemoteDragView {
            session: session.to_owned(),
            id,
            effect,
            ended,
        };
        let _ = self.app.emit("remote-drag", &view);
    }
}

impl CompanionHost for Companions {
    fn attached(&self, session: &str, link: CompanionLink, _version: &str, _host: &str) -> Arc<dyn CompanionHandler> {
        let transfers = Arc::clone(&self.transfers);
        let app = self.app.clone();
        let sources: Arc<Mutex<HashSet<u64>>> = Arc::default();
        let pump_sources = Arc::clone(&sources);
        let sender = link.clone();
        // Transfer traffic takes the direct link while it's up.
        let direct = DirectSlot::default();
        let route = direct.clone();
        let pump = Pump::spawn(
            &format!("companion-{session}"),
            move |message| route.try_send(&message) || sender.send(&message),
            move |note| match note {
                Note::Progress { id, consumed } => {
                    Companions::update(&transfers, &app, id, |t| t.sent = consumed.min(t.total));
                }
                Note::Closed { id, error, throughput } => {
                    if throughput.bytes > 0 {
                        tracing::info!(id, ?error, %throughput, "transfer to the remote ended");
                    }
                    pump_sources.lock().remove(&id);
                    Companions::update(&transfers, &app, id, |t| {
                        if t.state == TransferState::Sending {
                            t.sent = t.total;
                            t.state = if error.is_some() {
                                TransferState::Failed
                            } else {
                                TransferState::Done
                            };
                            t.error = error;
                        }
                    });
                }
            },
        )
        .expect("spawn a transfer thread");

        let attached = Arc::new(Attached {
            session: session.to_owned(),
            link,
            pump,
            sources,
            inflows: Mutex::default(),
            queries: Mutex::default(),
            drag: Mutex::default(),
            drag_out: Mutex::default(),
            direct,
            preparing: Mutex::default(),
        });
        let replaced = self.attached.lock().insert(session.to_owned(), Arc::clone(&attached));
        if let Some(old) = replaced {
            old.abort("the companion reattached");
        }
        let prefs = self.prefs_for(session);
        if prefs.stats_interval > 0 {
            attached.send(&Message::StatsInterval {
                secs: prefs.stats_interval,
            });
        }
        tracing::info!(session, "companion ready for stats and transfers");
        if prefs.direct {
            Self::start_direct(&attached);
        }
        Arc::new(Handler {
            attached,
            owner: self.me.clone(),
        })
    }
}

impl Companions {
    fn on_message(&self, attached: &Arc<Attached>, message: Message) {
        match message {
            Message::Stats(samples) => {
                let views: Vec<StatsView> = samples.into_iter().map(StatsView::new).collect();
                {
                    let mut stats = self.stats.lock();
                    let history = stats.entry(attached.session.clone()).or_default();
                    history.extend(views.iter().cloned());
                    while history.len() > STATS_HISTORY {
                        history.pop_front();
                    }
                }
                let _ = self.app.emit(
                    "host-stats",
                    serde_json::json!({ "id": attached.session, "samples": views }),
                );
            }
            Message::Fetch { .. } | Message::Ack { .. } => {
                attached.pump.receive(&message);
            }
            Message::Offer { id, entries, intent } => attached.offered(id, entries, intent),
            Message::DirectAnswer {
                fingerprint,
                candidates,
            } => self.direct_answer(attached, fingerprint, candidates),
            Message::DirectUnavailable { reason } => {
                attached.preparing.lock().take();
                tracing::info!(session = %attached.session, %reason, "no direct link; transfers stay on the RDP channel");
            }
            Message::NoFiles { id } => {
                if let Some(waiter) = attached.queries.lock().remove(&id) {
                    let _ = waiter.send(None);
                }
            }
            Message::Data { id, .. } | Message::End { id, .. } | Message::Unreadable { id, .. } => {
                let inflow = attached.inflows.lock().get(&id).cloned();
                if let Some(inflow) = inflow {
                    inflow.receive(message);
                }
            }
            Message::DragReady { id } => {
                if let Some(d) = attached.drag.lock().as_mut().filter(|d| d.id == id)
                    && let Some(ready) = d.ready.take()
                {
                    let _ = ready.send(());
                }
            }
            Message::DragFeedback { id, effect } => self.drag_event(&attached.session, id, effect, false),
            Message::DragOutReady { id } => {
                // Nudge the pointer so a remote drag under way finds the companion's target.
                let at = attached
                    .drag_out
                    .lock()
                    .filter(|(probe, _)| *probe == id)
                    .map(|(_, at)| at);
                if let Some((x, y)) = at {
                    let nudge = if x > 0 { x - 1 } else { x + 1 };
                    self.engine.input(
                        &attached.session,
                        vec![InputEvent::Move { x: nudge, y }, InputEvent::Move { x, y }],
                    );
                }
            }
            Message::DragOut { id, entries } => self.dragged_out(attached, id, entries),
            Message::DragOutNone { id } => {
                let mut probe = attached.drag_out.lock();
                if probe.is_some_and(|(p, _)| p == id) {
                    probe.take();
                }
            }
            Message::DragEnded { id, effect } => {
                let ended = {
                    let mut drag = attached.drag.lock();
                    let ours = drag.as_ref().is_some_and(|d| d.id == id);
                    if ours { drag.take() } else { None }
                };
                // Cancelled with the button still down on the remote: let it go now
                // that nothing would take it as a drop.
                if let Some(d) = ended
                    && d.pressed
                {
                    self.engine
                        .input(&attached.session, vec![InputEvent::Button { button: 0, down: false }]);
                }
                self.drag_event(&attached.session, id, effect, true);
            }
            Message::Close { id, error } => {
                if attached.sources.lock().contains(&id) {
                    attached.pump.closed(id, error);
                } else if let Some(inflow) = attached.inflows.lock().remove(&id) {
                    inflow.receive(Message::Close { id, error });
                }
            }
            _ => {}
        }
    }

    fn on_detached(&self, attached: &Arc<Attached>) {
        let mut map = self.attached.lock();
        if map.get(&attached.session).is_some_and(|a| Arc::ptr_eq(a, attached)) {
            map.remove(&attached.session);
            drop(map);
            if self.stats.lock().remove(&attached.session).is_some() {
                let _ = self.app.emit(
                    "host-stats",
                    serde_json::json!({ "id": attached.session, "samples": null }),
                );
            }
        }
        attached.abort("the connection closed");
        attached.direct.close();
        attached.preparing.lock().take();
        let sending: Vec<u64> = attached.sources.lock().drain().collect();
        for id in sending {
            self.fail(id, "The connection closed before it finished");
        }
    }

    /// Starts a direct link for this companion's transfers: gathers this side's addresses
    /// (asking a STUN server for the public one) and offers them; the companion answers
    /// with its own ([`Self::direct_answer`]).
    fn start_direct(attached: &Arc<Attached>) {
        let attached = Arc::clone(attached);
        let spawned = std::thread::Builder::new()
            .name("direct-prepare".into())
            .spawn(move || {
                let prepared = match Prepared::new(rdp_direct::STUN_SERVERS) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        tracing::info!(session = %attached.session, error = %format!("{error:#}"), "no direct link");
                        return;
                    }
                };
                let offer = Message::DirectOffer {
                    fingerprint: prepared.fingerprint().to_vec(),
                    candidates: prepared.candidates().iter().map(ToString::to_string).collect(),
                };
                *attached.preparing.lock() = Some(prepared);
                attached.send(&offer);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "couldn't start a direct link");
        }
    }

    /// The direct link closed: transfers carry on over the companion channel, requests
    /// waiting for an answer that may have been lost give up (their callers fall back),
    /// and the companion hears its stats interval again in case that was lost too.
    fn direct_lost(&self, attached: &Arc<Attached>, reason: &str) {
        attached.direct_lost(reason);
        for (_, waiter) in attached.queries.lock().drain() {
            let _ = waiter.send(None);
        }
        let secs = self.interval_for(&attached.session);
        attached.send(&Message::StatsInterval { secs });
    }

    /// The companion answered our offer and is waiting: connect to it.
    fn direct_answer(&self, attached: &Arc<Attached>, fingerprint: Vec<u8>, candidates: Vec<String>) {
        let Some(prepared) = attached.preparing.lock().take() else {
            return;
        };
        let fingerprint = match rdp_direct::parse_fingerprint(&fingerprint) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                tracing::warn!(%error, "the companion's direct-link answer is malformed");
                return;
            }
        };
        let host = self.prefs_for(&attached.session).host;
        let owner = self.me.clone();
        let weak = Arc::downgrade(attached);
        let attached = Arc::clone(attached);
        // Off the session thread: resolving the host and connecting take a moment.
        let spawned = std::thread::Builder::new().name("direct-connect".into()).spawn(move || {
            let mut addrs = rdp_direct::parse_candidates(&candidates);
            // The address this session reaches the host at, with each port the companion
            // listed (a host behind a one-to-one NAT keeps its ports).
            let ports: Vec<u16> = addrs.iter().map(|a| a.port()).collect();
            let resolved = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), 0)).map(|a| a.collect::<Vec<_>>());
            for ip in resolved.unwrap_or_default().into_iter().filter(|a| a.is_ipv4()).map(|a| a.ip()) {
                for &port in &ports {
                    let addr = std::net::SocketAddr::new(ip, port);
                    if !addrs.contains(&addr) {
                        addrs.push(addr);
                    }
                }
            }
            tracing::info!(session = %attached.session, here = ?prepared.candidates(), companion = ?addrs, "opening a direct link");
            let on_message = {
                let (owner, weak) = (owner.clone(), weak.clone());
                Box::new(move |message| {
                    if let (Some(owner), Some(attached)) = (owner.upgrade(), weak.upgrade()) {
                        owner.on_message(&attached, message);
                    }
                })
            };
            let on_closed = {
                Box::new(move |reason: String| {
                    if let (Some(owner), Some(attached)) = (owner.upgrade(), weak.upgrade()) {
                        owner.direct_lost(&attached, &reason);
                    }
                })
            };
            let peer = Peer {
                fingerprint,
                candidates: addrs,
            };
            match rdp_direct::connect(prepared, peer, rdp_direct::CONNECT_TIMEOUT, on_message, on_closed) {
                Ok(link) => {
                    tracing::info!(
                        session = %attached.session,
                        remote = %link.remote(),
                        rtt_ms = link.rtt().as_millis() as u64,
                        "direct link to the companion; transfers go over it"
                    );
                    attached.direct.set(Some(link));
                }
                Err(error) => tracing::info!(
                    session = %attached.session,
                    error = %format!("{error:#}"),
                    "no direct link to the companion; transfers stay on the RDP channel"
                ),
            }
        });
        if let Err(error) = spawned {
            tracing::warn!(%error, "couldn't connect a direct link");
        }
    }

    /// The fast clipboard path for one session (whether or not a companion is attached
    /// right now).
    #[cfg(windows)]
    pub fn clipboard_hook(self: &Arc<Self>, session: &str) -> Arc<dyn rdp_clipboard::Companion> {
        Arc::new(ClipboardHook {
            session: session.to_owned(),
            companions: Arc::downgrade(self),
        })
    }
}

/// One attached companion.
struct Attached {
    session: String,
    link: CompanionLink,
    /// Serves the files this side offers.
    pump: Pump,
    /// Ids of transfers this side is the source of.
    sources: Arc<Mutex<HashSet<u64>>>,
    /// Transfers being received (the remote's copied files, being pasted here).
    inflows: Mutex<HashMap<u64, Arc<Inflow>>>,
    /// Pastes waiting for the companion to offer what the remote copied.
    queries: Mutex<HashMap<u64, mpsc::SyncSender<Option<Arc<Inflow>>>>>,
    /// A drag of local files onto the session, if one is under way.
    drag: Mutex<Option<RemoteDrag>>,
    /// Looking for a drag out of the session: (transfer id, where the pointer left).
    drag_out: Mutex<Option<(u64, (u16, u16))>>,
    /// The direct link for transfers, while it's up.
    direct: DirectSlot,
    /// This side of a direct link being set up, until the companion answers.
    preparing: Mutex<Option<Prepared>>,
}

impl Attached {
    /// Sends to the companion: over the direct link while it's up (all but the
    /// channel's own handshake), else the companion channel. False if neither took it.
    fn send(&self, message: &Message) -> bool {
        self.direct.try_send(message) || self.link.send(message)
    }

    fn outbox(&self) -> Outbox {
        let link = self.link.clone();
        let direct = self.direct.clone();
        Arc::new(move |message| {
            if !direct.try_send(&message) {
                link.send(&message);
            }
        })
    }

    /// The direct link closed: what was in flight on it is lost, so sources forget it and
    /// sinks ask again, over the RDP channel.
    fn direct_lost(&self, reason: &str) {
        self.direct.set(None);
        self.pump.rewind();
        for inflow in self.inflows.lock().values() {
            inflow.resume();
        }
        tracing::info!(session = %self.session, %reason, "direct link closed; transfers carry on over the RDP channel");
    }

    /// The companion offered files: the answer to a paste's query.
    fn offered(&self, id: u64, entries: Vec<Entry>, intent: Intent) {
        let waiter = self.queries.lock().remove(&id);
        let unsafe_name = entries.iter().any(|e| transfer::safe_relative(&e.path).is_none());
        let (Some(waiter), Intent::Clipboard, false) = (waiter, intent, unsafe_name) else {
            if unsafe_name {
                tracing::warn!("the remote offered a file with an unsafe name; ignored");
            }
            self.send(&Message::Close {
                id,
                error: Some("not expected".into()),
            });
            return;
        };
        self.inflows.lock().retain(|_, inflow| !inflow.is_closed());
        let inflow = Arc::new(Inflow::new(id, entries.into(), self.outbox()));
        self.inflows.lock().insert(id, Arc::clone(&inflow));
        if waiter.send(Some(Arc::clone(&inflow))).is_err() {
            // The paste gave up waiting.
            inflow.finish(None);
            self.inflows.lock().remove(&id);
        }
    }

    /// The companion went away (or was replaced): fail what's in flight.
    fn abort(&self, reason: &str) {
        for (_, inflow) in self.inflows.lock().drain() {
            inflow.abort(reason);
        }
        self.queries.lock().clear();
    }
}

/// An attached companion's messages, from the session thread.
struct Handler {
    attached: Arc<Attached>,
    owner: Weak<Companions>,
}

impl CompanionHandler for Handler {
    fn message(&self, message: Message) {
        if let Some(owner) = self.owner.upgrade() {
            owner.on_message(&self.attached, message);
        }
    }

    fn detached(&self) {
        match self.owner.upgrade() {
            Some(owner) => owner.on_detached(&self.attached),
            None => self.attached.abort("the app is closing"),
        }
    }
}

/// Whether the user's primary mouse button is physically down right now.
#[cfg(windows)]
fn primary_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    // SAFETY: plain state queries. GetAsyncKeyState reads physical buttons, so with the
    // buttons swapped the primary one is the right.
    unsafe {
        let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        GetAsyncKeyState(i32::from(key.0)) < 0
    }
}

/// Copies files through a session's companion, when one is attached.
#[cfg(windows)]
struct ClipboardHook {
    session: String,
    companions: Weak<Companions>,
}

#[cfg(windows)]
impl rdp_clipboard::Companion for ClipboardHook {
    fn offer_files(&self, paths: Vec<PathBuf>) -> bool {
        let Some(companions) = self.companions.upgrade() else {
            return false;
        };
        let Some(attached) = companions.get(&self.session) else {
            return false;
        };
        let id = companions.next_id();
        // Listing a big folder takes a moment: not on the clipboard thread.
        let spawned = std::thread::Builder::new()
            .name("clipboard-offer".into())
            .spawn(move || {
                let started = Instant::now();
                let listing = transfer::list(&paths);
                if listing.entries.is_empty() {
                    return;
                }
                tracing::info!(
                    entries = listing.entries.len(),
                    bytes = listing.total(),
                    ms = started.elapsed().as_millis() as u64,
                    "offering copied files through the companion"
                );
                let entries: Arc<[Entry]> = listing.entries.into();
                attached.sources.lock().insert(id);
                attached.pump.start(Source::new(
                    id,
                    Arc::clone(&entries),
                    Box::new(DiskFiles::new(listing.paths)),
                ));
                attached.send(&Message::Offer {
                    id,
                    entries: entries.to_vec(),
                    intent: Intent::Clipboard,
                });
            });
        spawned.is_ok()
    }

    fn offer_formats(&self, formats: Vec<ClipFormat>) -> bool {
        let Some(companions) = self.companions.upgrade() else {
            return false;
        };
        let Some(attached) = companions.get(&self.session) else {
            return false;
        };
        let id = companions.next_id();
        let entries: Arc<[Entry]> = formats
            .iter()
            .map(|f| Entry {
                path: f.name.clone().unwrap_or_else(|| format!("#{}", f.id)),
                dir: false,
                size: 0,
                modified: 0,
            })
            .collect();
        // Each format is read off this clipboard when something on the remote pastes it.
        let local: Vec<u32> = formats.iter().map(|f| f.id).collect();
        let files = Produced::new(move |index: u32| {
            let format = local
                .get(index as usize)
                .copied()
                .ok_or_else(|| std::io::Error::other("no such format"))?;
            rdp_clipboard::read_format(format)
        });
        attached.sources.lock().insert(id);
        attached.pump.start(Source::new(id, entries, Box::new(files)));
        if !attached.send(&Message::ClipboardOffer { id, formats }) {
            attached.sources.lock().remove(&id);
            return false;
        }
        true
    }

    fn remote_format(&self, format: ClipFormat) -> Option<Vec<u8>> {
        let companions = self.companions.upgrade()?;
        let attached = companions.get(&self.session)?;
        let id = companions.next_id();
        let entries: Arc<[Entry]> = vec![Entry {
            path: "clipboard".into(),
            dir: false,
            size: 0,
            modified: 0,
        }]
        .into();
        // The companion starts streaming as soon as it has read its clipboard.
        let inflow = Arc::new(Inflow::per_entry(id, entries, attached.outbox()).streaming());
        attached.inflows.lock().insert(id, Arc::clone(&inflow));
        let started = Instant::now();
        let result = if attached.send(&Message::ClipboardRead {
            id,
            format: format.clone(),
        }) {
            read_whole(&inflow)
        } else {
            Err("the companion went away".to_owned())
        };
        attached.inflows.lock().remove(&id);
        match result {
            Ok(data) => {
                inflow.finish(None);
                let secs = started.elapsed().as_secs_f64();
                tracing::info!(
                    format = format.id,
                    bytes = data.len(),
                    ms = (secs * 1000.0) as u64,
                    mib_per_s = format!("{:.2}", data.len() as f64 / 1_048_576.0 / secs.max(0.001)),
                    wire = %inflow.throughput(),
                    "pasted the remote's copy through the companion"
                );
                Some(data)
            }
            Err(error) => {
                inflow.finish(Some(error.clone()));
                tracing::info!(format = format.id, %error, "pasting over the clipboard channel instead");
                None
            }
        }
    }

    fn remote_files(&self) -> Option<Arc<Inflow>> {
        let companions = self.companions.upgrade()?;
        let attached = companions.get(&self.session)?;
        let id = companions.next_id();
        let (tx, rx) = mpsc::sync_channel(1);
        attached.queries.lock().insert(id, tx);
        if !attached.send(&Message::ClipboardQuery { id }) {
            attached.queries.lock().remove(&id);
            return None;
        }
        let started = Instant::now();
        loop {
            match rx.recv_timeout(rdp_vfiles::PUMP_INTERVAL) {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() < LIST_WAIT => {
                    // This runs on the clipboard's STA thread: let sent messages through.
                    rdp_vfiles::pump_sent_messages();
                }
                Err(_) => {
                    attached.queries.lock().remove(&id);
                    tracing::warn!("the companion didn't list the remote's copied files in time");
                    return None;
                }
            }
        }
    }
}

/// All of a single-entry transfer (a clipboard format being pasted).
#[cfg(windows)]
fn read_whole(inflow: &Inflow) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; CHUNK * 4];
    loop {
        let wait = if data.is_empty() { PASTE_FIRST_WAIT } else { PASTE_STALL };
        match inflow.read(0, data.len() as u64, &mut buf, wait) {
            Ok(0) => return Ok(data),
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(error) => return Err(error.to_string()),
        }
    }
}
