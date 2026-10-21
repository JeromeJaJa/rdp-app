//! Attached companions: host stats, file sends, and fast clipboard copies.
//!
//! The engine hands each companion that attaches to a session to [`Companions`] (its
//! [`CompanionHost`]). From then on this module talks to it: it asks for host stats at
//! the interval the settings say, streams files to it ("Send files to…", and copies made
//! on this PC), and receives the files copied on the remote when they're pasted here.
//! Transfers use [`rdp_companion_proto::transfer`] on both ends.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rdp_companion_proto::transfer::{self, DiskFiles, Inflow, Listing, Note, Outbox, Pump, Source};
use rdp_companion_proto::{Entry, HostStats, Intent, Message};
use rdp_engine::{CompanionHandler, CompanionHost, CompanionLink};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// How long a paste waits for the companion to list what the remote copied.
const LIST_WAIT: Duration = Duration::from_secs(30);

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
    attached: Mutex<HashMap<String, Arc<Attached>>>,
    stats: Mutex<HashMap<String, StatsView>>,
    transfers: Arc<Mutex<HashMap<u64, TransferView>>>,
    next_id: AtomicU64,
    interval: Mutex<Option<IntervalSource>>,
}

/// The stats interval (seconds, 0 = off) a session's settings ask for.
pub type IntervalSource = Arc<dyn Fn(&str) -> u32 + Send + Sync>;

impl Companions {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            app,
            attached: Mutex::default(),
            stats: Mutex::default(),
            transfers: Arc::default(),
            next_id: AtomicU64::new(2),
            interval: Mutex::default(),
        })
    }

    /// Where to look up a session's stats interval.
    pub fn set_interval_source(&self, source: IntervalSource) {
        *self.interval.lock() = Some(source);
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
        self.stats.lock().clone()
    }

    /// Sends in progress or recently finished.
    pub fn transfers(&self) -> Vec<TransferView> {
        self.transfers.lock().values().cloned().collect()
    }

    fn interval_for(&self, session: &str) -> u32 {
        let source = self.interval.lock().clone();
        source.map_or(0, |f| f(session))
    }

    /// Re-sends each attached companion its stats interval (settings changed).
    pub fn refresh_intervals(&self) {
        let attached: Vec<Arc<Attached>> = self.attached.lock().values().cloned().collect();
        for a in attached {
            let secs = self.interval_for(&a.session);
            a.link.send(&Message::StatsInterval { secs });
            if secs == 0 && self.stats.lock().remove(&a.session).is_some() {
                let _ = self
                    .app
                    .emit("host-stats", serde_json::json!({ "id": a.session, "stats": null }));
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
        if !attached.link.send(&Message::Offer {
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
}

impl CompanionHost for Companions {
    fn attached(&self, session: &str, link: CompanionLink, _version: &str, _host: &str) -> Arc<dyn CompanionHandler> {
        let transfers = Arc::clone(&self.transfers);
        let app = self.app.clone();
        let sources: Arc<Mutex<HashSet<u64>>> = Arc::default();
        let pump_sources = Arc::clone(&sources);
        let sender = link.clone();
        let pump = Pump::spawn(
            &format!("companion-{session}"),
            move |message| sender.send(&message),
            move |note| match note {
                Note::Progress { id, consumed } => {
                    Companions::update(&transfers, &app, id, |t| t.sent = consumed.min(t.total));
                }
                Note::Closed { id, error } => {
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
        });
        let replaced = self.attached.lock().insert(session.to_owned(), Arc::clone(&attached));
        if let Some(old) = replaced {
            old.abort("the companion reattached");
        }
        let secs = self.interval_for(session);
        if secs > 0 {
            attached.link.send(&Message::StatsInterval { secs });
        }
        tracing::info!(session, "companion ready for stats and transfers");
        Arc::new(Handler {
            attached,
            owner: self.me.clone(),
        })
    }
}

impl Companions {
    fn on_message(&self, attached: &Arc<Attached>, message: Message) {
        match message {
            Message::Stats(stats) => {
                let view = StatsView::new(stats);
                self.stats.lock().insert(attached.session.clone(), view.clone());
                let _ = self.app.emit(
                    "host-stats",
                    serde_json::json!({ "id": attached.session, "stats": view }),
                );
            }
            Message::Fetch { .. } | Message::Ack { .. } => {
                attached.pump.receive(&message);
            }
            Message::Offer { id, entries, intent } => attached.offered(id, entries, intent),
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
                    serde_json::json!({ "id": attached.session, "stats": null }),
                );
            }
        }
        attached.abort("the connection closed");
        let sending: Vec<u64> = attached.sources.lock().drain().collect();
        for id in sending {
            self.fail(id, "The connection closed before it finished");
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
}

impl Attached {
    fn outbox(&self) -> Outbox {
        let link = self.link.clone();
        Arc::new(move |message| {
            link.send(&message);
        })
    }

    /// The companion offered files: the answer to a paste's query.
    fn offered(&self, id: u64, entries: Vec<Entry>, intent: Intent) {
        let waiter = self.queries.lock().remove(&id);
        let unsafe_name = entries.iter().any(|e| transfer::safe_relative(&e.path).is_none());
        let (Some(waiter), Intent::Clipboard, false) = (waiter, intent, unsafe_name) else {
            if unsafe_name {
                tracing::warn!("the remote offered a file with an unsafe name; ignored");
            }
            self.link.send(&Message::Close {
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
                attached.link.send(&Message::Offer {
                    id,
                    entries: entries.to_vec(),
                    intent: Intent::Clipboard,
                });
            });
        spawned.is_ok()
    }

    fn remote_files(&self) -> Option<Arc<Inflow>> {
        let companions = self.companions.upgrade()?;
        let attached = companions.get(&self.session)?;
        let id = companions.next_id();
        let (tx, rx) = mpsc::sync_channel(1);
        attached.queries.lock().insert(id, tx);
        if !attached.link.send(&Message::ClipboardQuery { id }) {
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
