//! The clipboard on a Mac: plain text, both ways, through the general pasteboard.
//!
//! - **Here → remote.** One thread watches the pasteboard's change count (reading it isn't
//!   reading the clipboard, so macOS doesn't flag it). A change is offered as text to the
//!   sessions on screen, and to the rest when they come on screen. The text itself is read only
//!   when a remote pastes it.
//! - **Remote → here.** Text a session on screen copies is fetched at once and put on the
//!   pasteboard. Sessions off screen are ignored: nobody is copying in them, and each one's
//!   remote announces its clipboard as it connects, which would overwrite yours dozens of times.
//!
//! Files and images stay on Windows for now (`rdp-clipboard`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy as _, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use ironrdp_core::{IntoOwned as _, impl_as_any};
use parking_lot::Mutex;
use rdp_engine::{ClipboardHost, SessionClipboard};

/// How often the pasteboard's change count is checked.
const WATCH_EVERY: Duration = Duration::from_millis(250);

mod pasteboard {
    //! The general pasteboard. Safe to use from any thread.
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    /// Bumped by every change to the pasteboard (ours too).
    pub fn change_count() -> isize {
        autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount())
    }

    /// The pasteboard holds text (without reading it).
    pub fn has_text() -> bool {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            let wanted = unsafe { NSPasteboardTypeString };
            board.types().is_some_and(|types| types.containsObject(wanted))
        })
    }

    pub fn text() -> Option<String> {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            board
                .stringForType(unsafe { NSPasteboardTypeString })
                .map(|s| s.to_string())
        })
    }

    /// Replaces the pasteboard's contents with `text`; returns the change count it now has.
    pub fn set_text(text: &str) -> isize {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            board.clearContents();
            board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
            board.changeCount()
        })
    }
}

struct Session {
    proxy: SessionClipboard,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    /// The channel is set up: copies can be offered.
    ready: bool,
    /// The pasteboard change last offered to this session.
    offered: Option<isize>,
    /// A paste of the remote's text is on its way.
    fetching: bool,
}

impl Session {
    /// Tells the remote what the pasteboard holds now (text or nothing), unless it knows.
    fn offer(&self, change: isize, force: bool) {
        let mut state = self.state.lock();
        if !force && (!state.ready || state.offered == Some(change)) {
            return;
        }
        state.offered = Some(change);
        drop(state);
        let formats = if pasteboard::has_text() {
            vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
        } else {
            Vec::new()
        };
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendInitiateCopy(formats));
    }
}

#[derive(Default)]
struct Registry {
    sessions: HashMap<String, Arc<Session>>,
    /// Sessions on screen: shown full-size in some window.
    on_screen: HashSet<String>,
    /// The pasteboard change last seen.
    change: isize,
    watching: bool,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    f(REGISTRY.lock().get_or_insert_with(Registry::default))
}

/// Offers each pasteboard change to the sessions on screen. Runs while any session has the
/// clipboard.
fn watch() {
    loop {
        std::thread::sleep(WATCH_EVERY);
        let change = pasteboard::change_count();
        let targets: Vec<Arc<Session>> = {
            let mut guard = REGISTRY.lock();
            let Some(registry) = guard.as_mut() else { return };
            if registry.sessions.is_empty() {
                registry.watching = false;
                return;
            }
            if registry.change == change {
                continue;
            }
            registry.change = change;
            registry
                .on_screen
                .iter()
                .filter_map(|id| registry.sessions.get(id).cloned())
                .collect()
        };
        for session in targets {
            session.offer(change, false);
        }
    }
}

/// The sessions now shown full-size (in any window). Copies are offered to them as they
/// happen; a session joining the set gets the latest copy then.
pub fn set_on_screen(sessions: &[String]) {
    let (joined, change) = with_registry(|registry| {
        let now: HashSet<String> = sessions.iter().cloned().collect();
        let joined: Vec<Arc<Session>> = now
            .difference(&registry.on_screen)
            .filter_map(|id| registry.sessions.get(id).cloned())
            .collect();
        registry.on_screen = now;
        (joined, registry.change)
    });
    for session in joined {
        session.offer(change, false);
    }
}

pub struct NativeClipboard;

impl NativeClipboard {
    /// The companion's clipboard (files) is Windows-only for now.
    pub fn new(_companions: Arc<crate::companion_link::Companions>) -> Self {
        Self
    }
}

impl ClipboardHost for NativeClipboard {
    fn attach(&self, session: &str, proxy: SessionClipboard) -> Option<Box<dyn CliprdrBackendFactory + Send>> {
        let entry = Arc::new(Session {
            proxy,
            state: Mutex::new(SessionState::default()),
        });
        let start = with_registry(|registry| {
            registry.sessions.insert(session.to_owned(), Arc::clone(&entry));
            if registry.watching {
                return false;
            }
            registry.watching = true;
            registry.change = pasteboard::change_count();
            true
        });
        if start && let Err(e) = std::thread::Builder::new().name("clipboard-watch".into()).spawn(watch) {
            tracing::warn!(error = %e, "can't watch the clipboard");
            with_registry(|registry| registry.watching = false);
        }
        Some(Box::new(Factory {
            id: session.to_owned(),
            session: entry,
        }))
    }

    fn detach(&self, session: &str) {
        with_registry(|registry| registry.sessions.remove(session));
    }
}

struct Factory {
    id: String,
    session: Arc<Session>,
}

impl CliprdrBackendFactory for Factory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(Backend {
            id: self.id.clone(),
            session: Arc::clone(&self.session),
        })
    }
}

struct Backend {
    id: String,
    session: Arc<Session>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacClipboard").field("session", &self.id).finish()
    }
}

impl_as_any!(Backend);

impl CliprdrBackend for Backend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        self.session.state.lock().ready = true;
    }

    /// The channel's setup (and a server asking again): answered with what's here now.
    fn on_request_format_list(&mut self) {
        let change = with_registry(|registry| registry.change);
        self.session.offer(change, true);
    }

    fn on_process_negotiated_capabilities(&mut self, _capabilities: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        let on_screen = with_registry(|registry| registry.on_screen.contains(&self.id));
        let text = available_formats
            .iter()
            .any(|f| f.id == ClipboardFormatId::CF_UNICODETEXT);
        if !on_screen || !text {
            return;
        }
        self.session.state.lock().fetching = true;
        self.session
            .proxy
            .send_clipboard_message(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let text = (request.format == ClipboardFormatId::CF_UNICODETEXT)
            .then(pasteboard::text)
            .flatten();
        let response: OwnedFormatDataResponse = match text {
            Some(text) => FormatDataResponse::new_unicode_string(&text).into_owned(),
            None => FormatDataResponse::new_error().into_owned(),
        };
        self.session
            .proxy
            .send_clipboard_message(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if !std::mem::take(&mut self.session.state.lock().fetching) || response.is_error() {
            return;
        }
        let Ok(text) = response.to_unicode_string() else {
            return;
        };
        let change = pasteboard::set_text(&text);
        // The remote that copied it has it: don't offer it back there.
        self.session.state.lock().offered = Some(change);
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}
