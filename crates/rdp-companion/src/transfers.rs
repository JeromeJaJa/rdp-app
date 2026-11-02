//! File transfers with the client, on the companion's side.
//!
//! Incoming offers are saved into a folder ("Send files to…") or put on the clipboard (a
//! copy on the client); the client streams their contents ([`rdp_companion_proto::transfer`]).
//! The other way, what was copied here is served when the client pastes it.
//!
//! Everything rides a direct link to the client when one is up ([`rdp_direct`]): the RDP
//! connection caps what the client can send here at 64 KiB per round trip.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rdp_companion_proto::transfer::{self, DiskFiles, Inflow, Note, Outbox, Produced, Pump, Source};
use rdp_companion_proto::{ClipFormat, Entry, Intent, Message};
use rdp_direct::{Peer, Prepared};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::HSTRING;

use crate::channel::Channel;
use crate::clipboard::{self, Clipboard};
use crate::drag::Drags;
use crate::stats::Stats;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One attach's transfers.
pub(crate) struct Transfers {
    channel: Arc<Channel>,
    clipboard: Arc<Clipboard>,
    /// Serves files this side offers.
    pump: Pump,
    /// Ids of transfers this side is the source of.
    sources: Mutex<HashSet<u64>>,
    /// Transfers being received.
    inflows: Arc<Mutex<HashMap<u64, Arc<Inflow>>>>,
    /// Files being dragged onto the session.
    drags: Drags,
    /// Host stats (their interval can arrive over the direct link).
    stats: Arc<Stats>,
}

impl Transfers {
    pub(crate) fn new(channel: Arc<Channel>, clipboard: Arc<Clipboard>, stats: Arc<Stats>) -> std::io::Result<Self> {
        let sender = Arc::clone(&channel);
        let pump = Pump::spawn(
            "transfers",
            move |message| sender.send(&message),
            |note| match note {
                Note::Closed { id, error, throughput } => {
                    tracing::info!(id, ?error, %throughput, "transfer to the client ended");
                }
                Note::Progress { .. } => {}
            },
        )?;
        Ok(Self {
            channel,
            clipboard,
            pump,
            sources: Mutex::default(),
            inflows: Arc::default(),
            drags: Drags::default(),
            stats,
        })
        .inspect(|transfers| {
            // Copies made here go to this client.
            let channel = Arc::clone(&transfers.channel);
            transfers.clipboard.set_sink(Some(Arc::new(move |message| {
                channel.send(&message);
            })));
        })
    }

    fn outbox(&self) -> Outbox {
        let channel = Arc::clone(&self.channel);
        Arc::new(move |message| {
            channel.send(&message);
        })
    }

    /// A message from the client (over either path), other than the handshake.
    pub(crate) fn handle(self: &Arc<Self>, message: Message) {
        match message {
            Message::StatsInterval { secs } => self.stats.set_interval(secs),
            Message::Offer { id, entries, intent } => self.offer(id, entries, intent),
            Message::Data { id, .. } | Message::End { id, .. } | Message::Unreadable { id, .. } => {
                self.incoming(id, message);
            }
            Message::Fetch { .. } | Message::Ack { .. } => self.outgoing(&message),
            Message::Close { id, error } => self.close(id, error),
            Message::ClipboardQuery { id } => self.clipboard_query(id),
            Message::ClipboardOffer { id, formats } => self.clipboard_offer(id, formats),
            Message::ClipboardRead { id, format } => self.clipboard_read(id, format),
            Message::DirectOffer {
                fingerprint,
                candidates,
            } => self.direct_offer(fingerprint, candidates),
            Message::DragIn { id, entries, x, y } => self.drag_in(id, entries, x, y),
            Message::DragCancel { id } => self.drag_cancel(id),
            Message::DragOutProbe { id, x, y } => self.drag_out_probe(id, x, y),
            Message::AppControl { id, action, path } => self.app_control(id, action, path),
            Message::Run { id, spec } => self.run(id, spec),
            // Not sent to the companion.
            _ => {}
        }
    }

    /// The client wants a program launched, restarted or closed here (off the message
    /// thread: closing waits for the program to exit).
    fn app_control(&self, id: u64, action: rdp_companion_proto::AppAction, path: String) {
        let channel = Arc::clone(&self.channel);
        let spawned = std::thread::Builder::new().name("app-control".into()).spawn(move || {
            let result = crate::apps::control(action, &path);
            if let Err(error) = &result {
                tracing::info!(?action, %path, %error, "app control failed");
            }
            channel.send(&Message::AppControlDone {
                id,
                error: result.as_ref().err().cloned(),
                was_running: result.unwrap_or(false),
            });
        });
        if spawned.is_err() {
            self.channel.send(&Message::AppControlDone {
                id,
                error: Some("couldn't start".into()),
                was_running: false,
            });
        }
    }

    /// The client wants a command run here (off the message thread: it may run a while).
    /// The command itself isn't logged: it can hold a password.
    fn run(&self, id: u64, spec: rdp_companion_proto::RunSpec) {
        let channel = Arc::clone(&self.channel);
        let spawned = std::thread::Builder::new().name("run".into()).spawn(move || {
            let result = crate::run::run(&spec);
            match &result {
                Ok(r) => tracing::info!(
                    shell = ?spec.shell,
                    visible = spec.visible,
                    pid = r.pid,
                    exit_code = ?r.exit_code,
                    timed_out = r.timed_out,
                    "ran a command"
                ),
                Err(error) => tracing::info!(%error, "couldn't run a command"),
            }
            channel.send(&Message::RunDone { id, result });
        });
        if spawned.is_err() {
            self.channel.send(&Message::RunDone {
                id,
                result: Err("couldn't start".into()),
            });
        }
    }

    /// The client offers a direct link: answer with this side's addresses, then wait for
    /// it to connect (sending openers its way meanwhile).
    pub(crate) fn direct_offer(self: &Arc<Self>, fingerprint: Vec<u8>, candidates: Vec<String>) {
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("direct".into()).spawn(move || {
            let unavailable = |reason: String| {
                tracing::info!(%reason, "no direct link");
                this.channel.send(&Message::DirectUnavailable { reason });
            };
            let peer = match rdp_direct::parse_fingerprint(&fingerprint) {
                Ok(fingerprint) => Peer {
                    fingerprint,
                    candidates: rdp_direct::parse_candidates(&candidates),
                },
                Err(error) => return unavailable(error.to_string()),
            };
            let prepared = match Prepared::new(rdp_direct::STUN_SERVERS) {
                Ok(prepared) => prepared,
                Err(error) => return unavailable(format!("{error:#}")),
            };
            tracing::info!(here = ?prepared.candidates(), client = ?peer.candidates, "opening a direct link");
            this.channel.send(&Message::DirectAnswer {
                fingerprint: prepared.fingerprint().to_vec(),
                candidates: prepared.candidates().iter().map(ToString::to_string).collect(),
            });
            let weak = Arc::downgrade(&this);
            let on_message = {
                let weak = weak.clone();
                Box::new(move |message| {
                    if let Some(this) = weak.upgrade() {
                        this.handle(message);
                    }
                })
            };
            let on_closed = Box::new(move |reason: String| {
                if let Some(this) = weak.upgrade() {
                    this.direct_lost(&reason);
                }
            });
            match rdp_direct::accept(prepared, peer, rdp_direct::CONNECT_TIMEOUT, on_message, on_closed) {
                Ok(link) => {
                    tracing::info!(remote = %link.remote(), "direct link to the client; transfers go over it");
                    this.channel.direct().set(Some(link));
                }
                Err(error) => {
                    tracing::info!(error = %format!("{error:#}"), "no direct link; transfers stay on the RDP channel")
                }
            }
        });
        if spawned.is_err() {
            self.channel.send(&Message::DirectUnavailable {
                reason: "couldn't start".into(),
            });
        }
    }

    /// The direct link closed: what was in flight on it is lost, so sources forget it and
    /// sinks ask again, over the RDP channel.
    fn direct_lost(&self, reason: &str) {
        self.channel.direct().set(None);
        self.pump.rewind();
        for inflow in lock(&self.inflows).values() {
            inflow.resume();
        }
        tracing::info!(%reason, "direct link closed; transfers carry on over the RDP channel");
    }

    /// The client offers files.
    pub(crate) fn offer(&self, id: u64, entries: Vec<Entry>, intent: Intent) {
        // A clipboard object that let go of its files closed its transfer: forget it.
        lock(&self.inflows).retain(|_, inflow| !inflow.is_closed());
        if let Some(bad) = entries.iter().find(|e| transfer::safe_relative(&e.path).is_none()) {
            tracing::warn!(path = %bad.path, "refusing an offer with an unsafe name");
            self.channel.send(&Message::Close {
                id,
                error: Some(format!("unsafe file name: {}", bad.path)),
            });
            return;
        }
        let inflow = Arc::new(Inflow::new(id, entries.into(), self.outbox()));
        lock(&self.inflows).insert(id, Arc::clone(&inflow));
        match intent {
            Intent::Clipboard => self.clipboard.offer(inflow),
            Intent::Save { folder } => {
                let inflows = Arc::clone(&self.inflows);
                let spawned = std::thread::Builder::new().name("save".into()).spawn(move || {
                    let result = resolve_folder(&folder)
                        .and_then(|folder| transfer::save(&inflow, &folder, |_| {}).map(|r| (folder, r)));
                    let error = match result {
                        Ok((folder, report)) => {
                            tracing::info!(
                                folder = %folder.display(),
                                files = report.files,
                                failed = report.failed.len(),
                                "files received"
                            );
                            summarize_failures(&report.failed)
                        }
                        Err(error) => Some(error),
                    };
                    inflow.finish(error);
                    lock(&inflows).remove(&inflow.id());
                });
                if let Err(error) = spawned {
                    tracing::warn!(%error, "couldn't start saving");
                }
            }
        }
    }

    /// The client drags files onto the session at (`x`, `y`): get a drag ready there.
    pub(crate) fn drag_in(&self, id: u64, entries: Vec<Entry>, x: i32, y: i32) {
        lock(&self.inflows).retain(|_, inflow| !inflow.is_closed());
        if entries.iter().any(|e| transfer::safe_relative(&e.path).is_none()) {
            tracing::warn!("refusing a drag with an unsafe name");
            self.channel.send(&Message::DragEnded { id, effect: 0 });
            return;
        }
        let inflow = Arc::new(Inflow::new(id, entries.into(), self.outbox()));
        lock(&self.inflows).insert(id, Arc::clone(&inflow));
        self.drags.start(Arc::clone(&self.channel), inflow, x, y);
    }

    /// The client's pointer left the session with the button held: if files are being
    /// dragged here, offer them so the drag carries on on the client.
    pub(crate) fn drag_out_probe(self: &Arc<Self>, id: u64, x: i32, y: i32) {
        let this = Arc::clone(self);
        let serve: crate::dragout::Serve = Arc::new(move |id, paths| this.serve_dragged(id, paths));
        crate::dragout::probe(Arc::clone(&self.channel), id, x, y, serve);
    }

    /// Lists and serves files dragged out of the session (off the drag's thread: listing a
    /// big folder takes a moment, and the remote drag waits on that thread).
    fn serve_dragged(self: &Arc<Self>, id: u64, paths: Vec<PathBuf>) {
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("drag-out-list".into()).spawn(move || {
            let listing = transfer::list(&paths);
            if listing.entries.is_empty() {
                this.channel.send(&Message::DragOutNone { id });
                return;
            }
            let entries: Arc<[Entry]> = listing.entries.into();
            lock(&this.sources).insert(id);
            this.pump.start(Source::new(
                id,
                Arc::clone(&entries),
                Box::new(DiskFiles::new(listing.paths)),
            ));
            this.channel.send(&Message::DragOut {
                id,
                entries: entries.to_vec(),
            });
        });
        if spawned.is_err() {
            self.channel.send(&Message::DragOutNone { id });
        }
    }

    /// The client's drag left the session (or was cancelled).
    pub(crate) fn drag_cancel(&self, id: u64) {
        self.drags.cancel(id);
    }

    /// Contents for a transfer being received.
    pub(crate) fn incoming(&self, id: u64, message: Message) {
        let inflow = lock(&self.inflows).get(&id).cloned();
        if let Some(inflow) = inflow {
            inflow.receive(message);
        }
    }

    /// Fetches and acks for a transfer this side is serving.
    pub(crate) fn outgoing(&self, message: &Message) {
        self.pump.receive(message);
    }

    /// The client closed transfer `id`.
    pub(crate) fn close(&self, id: u64, error: Option<String>) {
        if lock(&self.sources).remove(&id) {
            self.pump.closed(id, error);
        } else if let Some(inflow) = lock(&self.inflows).remove(&id) {
            inflow.receive(Message::Close { id, error });
        }
    }

    /// The client wants the files copied here, as transfer `id`.
    pub(crate) fn clipboard_query(self: &Arc<Self>, id: u64) {
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("clipboard-query".into())
            .spawn(move || {
                let listing = clipboard::copied_files()
                    .map(|paths| transfer::list(&paths))
                    .filter(|l| !l.entries.is_empty());
                let Some(listing) = listing else {
                    this.channel.send(&Message::NoFiles { id });
                    return;
                };
                tracing::info!(
                    entries = listing.entries.len(),
                    bytes = listing.total(),
                    "serving copied files"
                );
                let entries: Arc<[Entry]> = listing.entries.into();
                lock(&this.sources).insert(id);
                this.pump.start(Source::new(
                    id,
                    Arc::clone(&entries),
                    Box::new(DiskFiles::new(listing.paths)),
                ));
                this.channel.send(&Message::Offer {
                    id,
                    entries: entries.to_vec(),
                    intent: Intent::Clipboard,
                });
            });
        if spawned.is_err() {
            self.channel.send(&Message::NoFiles { id });
        }
    }

    /// The client copied something other than files: put it on the clipboard, each
    /// format fetched from the client when something pastes it.
    pub(crate) fn clipboard_offer(&self, id: u64, formats: Vec<ClipFormat>) {
        lock(&self.inflows).retain(|_, inflow| !inflow.is_closed());
        let entries: Arc<[Entry]> = formats
            .iter()
            .map(|f| Entry {
                path: f.name.clone().unwrap_or_else(|| format!("#{}", f.id)),
                dir: false,
                size: 0,
                modified: 0,
            })
            .collect();
        let inflow = Arc::new(Inflow::per_entry(id, entries, self.outbox()));
        lock(&self.inflows).insert(id, Arc::clone(&inflow));
        self.clipboard.offer_formats(inflow, formats);
    }

    /// The client is pasting what was copied here: stream `format` of it as transfer `id`
    /// straight away (or close the transfer if the clipboard can't give it).
    pub(crate) fn clipboard_read(self: &Arc<Self>, id: u64, format: ClipFormat) {
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("clipboard-read".into())
            .spawn(move || {
                let data = match clipboard::read_format(&format) {
                    Ok(data) => data,
                    Err(error) => {
                        tracing::debug!(%error, format = format.id, "can't serve a paste");
                        this.channel.send(&Message::Close { id, error: Some(error) });
                        return;
                    }
                };
                tracing::info!(format = format.id, bytes = data.len(), "serving a paste");
                let entries: Arc<[Entry]> = vec![Entry {
                    path: "clipboard".into(),
                    dir: false,
                    size: data.len() as u64,
                    modified: 0,
                }]
                .into();
                // Read once; the source keeps it while the client reads it.
                let mut data = Some(data);
                let files = Produced::new(move |_| data.take().ok_or_else(|| std::io::Error::other("already sent")));
                let mut source = Source::new(id, entries, Box::new(files));
                source.fetch(0, 0, 0, true);
                lock(&this.sources).insert(id);
                this.pump.start(source);
            });
        if spawned.is_err() {
            self.channel.send(&Message::Close {
                id,
                error: Some("couldn't start".into()),
            });
        }
    }

    /// The client is gone: everything in flight fails, and files it put on the
    /// clipboard can't be pasted any more.
    pub(crate) fn detach(&self) {
        self.clipboard.set_sink(None);
        self.channel.direct().close();
        for (_, inflow) in lock(&self.inflows).drain() {
            inflow.abort("the client disconnected");
        }
        self.clipboard.clear_if_ours();
    }
}

/// "3 files couldn't be saved: a.txt (access denied), …", or `None` if all were.
fn summarize_failures(failed: &[(String, String)]) -> Option<String> {
    let (first, rest) = failed.split_first()?;
    let mut summary = format!(
        "{} file{} couldn't be saved: {} ({})",
        failed.len(),
        if failed.len() == 1 { "" } else { "s" },
        first.0,
        first.1
    );
    if !rest.is_empty() {
        summary.push_str(&format!(" and {} more", rest.len()));
    }
    Some(summary)
}

/// Where "Send files to…" saves: `Desktop`, `Downloads` or `Documents` (optionally with
/// a subfolder: `Desktop\Drop`), or a full path (environment variables expanded).
pub(crate) fn resolve_folder(spec: &str) -> Result<PathBuf, String> {
    let spec = spec.trim();
    let (head, rest) = match spec.find(['\\', '/']) {
        Some(at) => (&spec[..at], spec[at + 1..].trim_start_matches(['\\', '/'])),
        None => (spec, ""),
    };
    let known = match head.to_ascii_lowercase().as_str() {
        "" | "desktop" => Some(FOLDERID_Desktop),
        "downloads" => Some(FOLDERID_Downloads),
        "documents" => Some(FOLDERID_Documents),
        _ => None,
    };
    let folder = match known {
        Some(id) if !spec.starts_with(['\\', '/']) => {
            // SAFETY: the returned string is ours to free with CoTaskMemFree.
            let base = unsafe {
                let path = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).map_err(|e| e.to_string())?;
                let text = path.to_string().map_err(|e| e.to_string());
                CoTaskMemFree(Some(path.0 as *const _));
                text?
            };
            let mut folder = PathBuf::from(base);
            if !rest.is_empty() {
                let rest = transfer::safe_relative(rest).ok_or_else(|| format!("not a folder name: {rest}"))?;
                folder.push(rest);
            }
            folder
        }
        _ => {
            let source = HSTRING::from(spec);
            // SAFETY: sizing call, then expanding into a buffer of that size.
            let expanded = unsafe {
                let len = ExpandEnvironmentStringsW(&source, None) as usize;
                let mut buf = vec![0u16; len.max(1)];
                let written = ExpandEnvironmentStringsW(&source, Some(&mut buf)) as usize;
                String::from_utf16_lossy(&buf[..written.saturating_sub(1).min(buf.len())])
            };
            let folder = PathBuf::from(expanded);
            if !folder.is_absolute() {
                return Err(format!("not a full path: {spec}"));
            }
            folder
        }
    };
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_folders_and_paths() {
        let desktop = resolve_folder("Desktop").unwrap();
        assert!(desktop.is_absolute());
        assert_eq!(resolve_folder("").unwrap(), desktop);
        assert_eq!(resolve_folder("desktop\\Drop").unwrap(), desktop.join("Drop"));
        assert!(resolve_folder("Desktop\\..\\x").is_err());
        let temp = resolve_folder("%TEMP%\\x").unwrap();
        assert!(temp.is_absolute() && temp.ends_with("x"), "{temp:?}");
        assert!(resolve_folder("relative\\path").is_err());
    }

    #[test]
    fn summarizes_failures() {
        assert_eq!(summarize_failures(&[]), None);
        let one = vec![("a.txt".to_owned(), "denied".to_owned())];
        assert_eq!(
            summarize_failures(&one).unwrap(),
            "1 file couldn't be saved: a.txt (denied)"
        );
        let three = vec![one[0].clone(), one[0].clone(), one[0].clone()];
        assert!(summarize_failures(&three).unwrap().ends_with("and 2 more"));
    }
}
