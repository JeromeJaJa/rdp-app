//! OS clipboard integration: one native clipboard monitor per connected session.
//!
//! IronRDP's Windows backend owns a hidden window that must live on a thread
//! with a message loop, so monitors are created and destroyed on the UI thread
//! and kept in a UI-thread-only registry.

use std::collections::HashMap;
use std::time::Duration;

use ironrdp_cliprdr::backend::CliprdrBackendFactory;
use parking_lot::Mutex;
use rdp_clipboard::WinClipboard;
use rdp_engine::{ClipboardHost, SessionClipboard};
use tauri::AppHandle;
use tracing::warn;

/// Only touched on the UI thread.
struct UiThreadOnly<T>(T);
// SAFETY: created, used and dropped exclusively inside `run_on_main_thread` closures.
unsafe impl<T> Send for UiThreadOnly<T> {}

static MONITORS: Mutex<Option<HashMap<String, UiThreadOnly<WinClipboard>>>> = Mutex::new(None);

pub struct NativeClipboard {
    app: AppHandle,
}

impl NativeClipboard {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ClipboardHost for NativeClipboard {
    fn attach(&self, session: &str, proxy: SessionClipboard) -> Option<Box<dyn CliprdrBackendFactory + Send>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let session = session.to_owned();
        let scheduled = self.app.run_on_main_thread(move || {
            let result = WinClipboard::new(proxy).map(|monitor| {
                let factory = monitor.backend_factory();
                MONITORS
                    .lock()
                    .get_or_insert_with(HashMap::new)
                    .insert(session, UiThreadOnly(monitor));
                factory
            });
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        if scheduled.is_err() {
            return None;
        }
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(factory)) => Some(factory),
            Ok(Err(e)) => {
                warn!(error = %e, "clipboard redirection unavailable");
                None
            }
            Err(_) => None,
        }
    }

    fn detach(&self, session: &str) {
        let session = session.to_owned();
        let _ = self.app.run_on_main_thread(move || {
            // Dropping the monitor destroys its hidden window.
            let removed = MONITORS.lock().as_mut().and_then(|m| m.remove(&session));
            drop(removed);
        });
    }
}
