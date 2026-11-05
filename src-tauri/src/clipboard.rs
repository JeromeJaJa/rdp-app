//! OS clipboard integration: one native clipboard monitor per connected session.
//!
//! Each monitor runs on a thread of its own ([`ClipboardMonitor`]): pasting what a remote
//! copied waits for the remote's data inside `WM_RENDERFORMAT`, and on the UI thread that
//! wait froze the whole app (with dozens of sessions, often).
//!
//! Copies made on this PC, or on one session's remote, are offered at once only to the
//! sessions on screen ([`set_on_screen`]); the rest get the latest copy when they come on
//! screen. Offering every copy to every session sent each one to dozens of hosts, whose
//! clipboards then changed and read it back, keeping this PC's clipboard busy.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ironrdp_cliprdr::backend::CliprdrBackendFactory;
use parking_lot::Mutex;
use rdp_clipboard::ClipboardMonitor;
use rdp_engine::{ClipboardHost, SessionClipboard};
use tracing::warn;

struct Registry {
    monitors: HashMap<String, ClipboardMonitor>,
    /// Sessions on screen: shown full-size in some window.
    on_screen: HashSet<String>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut guard = REGISTRY.lock();
    f(guard.get_or_insert_with(|| Registry {
        monitors: HashMap::new(),
        on_screen: HashSet::new(),
    }))
}

/// The sessions now shown full-size (in any window). Copies are offered to them as they
/// happen; a session joining the set gets the latest copy then.
pub fn set_on_screen(sessions: &[String]) {
    let now: HashSet<String> = sessions.iter().cloned().collect();
    with_registry(|registry| {
        for id in registry.on_screen.symmetric_difference(&now) {
            if let Some(monitor) = registry.monitors.get(id) {
                monitor.set_focused(now.contains(id));
            }
        }
        registry.on_screen = now;
    });
}

pub struct NativeClipboard {
    /// Copies files through the session's companion when one is attached.
    companions: Arc<crate::companion_link::Companions>,
}

impl NativeClipboard {
    pub fn new(companions: Arc<crate::companion_link::Companions>) -> Self {
        Self { companions }
    }
}

impl ClipboardHost for NativeClipboard {
    fn attach(&self, session: &str, proxy: SessionClipboard) -> Option<Box<dyn CliprdrBackendFactory + Send>> {
        let hook = self.companions.clipboard_hook(session);
        let name = format!("clipboard-{}", session.get(..8).unwrap_or(session));
        // Holding the registry while starting the monitor: a change of what's on screen
        // meanwhile can't miss it.
        with_registry(|registry| {
            let focused = registry.on_screen.contains(session);
            match ClipboardMonitor::spawn(&name, proxy, Some(hook), focused) {
                Ok((monitor, factory)) => {
                    // What the remote copies, as its companion announces it, goes to this monitor.
                    self.companions.set_announcer(session, Some(monitor.announcer()));
                    registry.monitors.insert(session.to_owned(), monitor);
                    Some(factory)
                }
                Err(e) => {
                    warn!(error = %e, "clipboard redirection unavailable");
                    None
                }
            }
        })
    }

    fn detach(&self, session: &str) {
        self.companions.set_announcer(session, None);
        // Dropping the monitor ends its thread, which destroys its window.
        let removed = with_registry(|registry| registry.monitors.remove(session));
        drop(removed);
    }
}
