//! Frame surfaces backed by WebView2 shared buffers.
//!
//! The engine writes decoded pixels straight into memory that the page maps as
//! an `ArrayBuffer` (via `PostSharedBufferToScript`), so frames reach the
//! renderer with zero copies and zero IPC messages. See
//! `rdp_engine::surface` for the memory layout the page polls.
//!
//! A session's full-size surface goes to the window that shows it (the main
//! window, or a window its tab was dragged into); grid thumbnails always go to the
//! main window. All windows share one WebView2 environment, so a buffer can be
//! handed to another window when its tab moves.
//!
//! WebView2 objects live on the UI thread; session threads ask for buffers
//! through `with_webview` and wait for the result.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use parking_lot::Mutex;
use rdp_engine::surface::surface_len;
use rdp_engine::{SurfaceKind, SurfaceMemory, SurfaceProvider};
use serde::Serialize;
use tauri::{AppHandle, Emitter as _, Manager as _};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY, ICoreWebView2_17, ICoreWebView2Environment12,
    ICoreWebView2SharedBuffer,
};
use windows_core::{HSTRING, Interface as _};

pub const MAIN_WINDOW: &str = "main";

/// Live buffers, only touched on the UI thread (inside `with_webview`).
/// Deliberately a never-dropped static: buffers must not be unmapped during
/// process teardown while a session thread may still be writing.
static REGISTRY: Mutex<Option<HashMap<(String, SurfaceKind), Entry>>> = Mutex::new(None);

struct Entry {
    buffer: UiThreadOnly<ICoreWebView2SharedBuffer>,
    meta: String,
    generation: u64,
    /// The window the buffer was last posted to.
    window: String,
}

/// COM pointer that is only ever used on the UI thread.
struct UiThreadOnly<T>(T);
// SAFETY: values are created, used and released exclusively inside UI-thread closures.
unsafe impl<T> Send for UiThreadOnly<T> {}

struct SharedSurface {
    ptr: usize,
    len: usize,
}

impl SurfaceMemory for SharedSurface {
    fn as_ptr(&self) -> *mut u8 {
        self.ptr as *mut u8
    }
    fn len(&self) -> usize {
        self.len
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SurfaceReleased {
    session: String,
    kind: &'static str,
    generation: u64,
}

pub struct WebviewSurfaces {
    app: AppHandle,
    next_generation: AtomicU64,
    /// Window showing each session's full surface (absent = the main window).
    hosts: Mutex<HashMap<String, String>>,
}

impl WebviewSurfaces {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            next_generation: AtomicU64::new(1),
            hosts: Mutex::new(HashMap::new()),
        })
    }

    fn window_for(&self, session: &str, kind: SurfaceKind) -> String {
        match kind {
            SurfaceKind::Full => self
                .hosts
                .lock()
                .get(session)
                .cloned()
                .unwrap_or_else(|| MAIN_WINDOW.to_owned()),
            SurfaceKind::Thumb => MAIN_WINDOW.to_owned(),
        }
    }

    /// Re-sends the live buffers that belong to `label` (after its page loaded or reloaded).
    pub fn repost_all(&self, label: &str) -> anyhow::Result<()> {
        let window = self.app.get_webview_window(label).context("window")?;
        let label = label.to_owned();
        window.with_webview(move |pw| {
            let Ok(core) = core_webview(&pw) else { return };
            let registry = REGISTRY.lock();
            if let Some(map) = registry.as_ref() {
                for entry in map.values().filter(|e| e.window == label) {
                    unsafe {
                        let _ = core.PostSharedBufferToScript(
                            &entry.buffer.0,
                            COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                            &HSTRING::from(entry.meta.as_str()),
                        );
                    }
                }
            }
        })?;
        Ok(())
    }

    /// Shows a session's full surface in another window (its tab moved there). The
    /// buffer itself stays; the old window is told to let go of its view of it.
    pub fn set_host(&self, session: &str, label: &str) {
        {
            let mut hosts = self.hosts.lock();
            let current = hosts.get(session).map(String::as_str).unwrap_or(MAIN_WINDOW);
            if current == label {
                return;
            }
            if label == MAIN_WINDOW {
                hosts.remove(session);
            } else {
                hosts.insert(session.to_owned(), label.to_owned());
            }
        }
        let Some(window) = self.app.get_webview_window(label) else {
            return;
        };
        let key = (session.to_owned(), SurfaceKind::Full);
        let target = label.to_owned();
        let app = self.app.clone();
        let _ = window.with_webview(move |pw| {
            let Ok(core) = core_webview(&pw) else { return };
            let (previous, generation) = {
                let mut registry = REGISTRY.lock();
                let Some(entry) = registry.as_mut().and_then(|m| m.get_mut(&key)) else {
                    return;
                };
                unsafe {
                    let _ = core.PostSharedBufferToScript(
                        &entry.buffer.0,
                        COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                        &HSTRING::from(entry.meta.as_str()),
                    );
                }
                (std::mem::replace(&mut entry.window, target), entry.generation)
            };
            // A window that closed has nothing left to let go of.
            if app.get_webview_window(&previous).is_some() {
                emit_later(
                    app,
                    Some(previous),
                    SurfaceReleased {
                        session: key.0,
                        kind: key.1.as_str(),
                        generation,
                    },
                );
            }
        });
    }
}

/// Emits `surface-released` (to one window, or all) from outside a `with_webview`
/// closure: run on the UI thread, such a closure holds its webview's lock, and emitting
/// evaluates script in every listening webview, that one included: a deadlock.
fn emit_later(app: AppHandle, window: Option<String>, released: SurfaceReleased) {
    tauri::async_runtime::spawn(async move {
        let _ = match window {
            Some(label) => app.emit_to(label.as_str(), "surface-released", released),
            None => app.emit("surface-released", released),
        };
    });
}

fn core_webview(pw: &tauri::webview::PlatformWebview) -> windows_core::Result<ICoreWebView2_17> {
    unsafe { pw.controller().CoreWebView2()?.cast::<ICoreWebView2_17>() }
}

impl SurfaceProvider for WebviewSurfaces {
    fn allocate(
        &self,
        session: &str,
        kind: SurfaceKind,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Arc<dyn SurfaceMemory>> {
        let label = self.window_for(session, kind);
        let window = match self.app.get_webview_window(&label) {
            Some(w) => w,
            None => self.app.get_webview_window(MAIN_WINDOW).context("main window")?,
        };
        let label = window.label().to_owned();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let key = (session.to_owned(), kind);
        let len = surface_len(width, height);
        let meta = serde_json::json!({
            "session": session,
            "kind": kind.as_str(),
            "width": width,
            "height": height,
            "generation": generation,
        })
        .to_string();

        let (tx, rx) = std::sync::mpsc::sync_channel::<anyhow::Result<usize>>(1);
        window.with_webview(move |pw| {
            let result = (|| -> anyhow::Result<usize> {
                let env: ICoreWebView2Environment12 = pw.environment().cast().context("WebView2 runtime too old")?;
                let buffer = unsafe { env.CreateSharedBuffer(len as u64) }.context("create shared buffer")?;
                let mut ptr: *mut u8 = std::ptr::null_mut();
                unsafe { buffer.Buffer(&mut ptr) }.context("map shared buffer")?;
                let core = core_webview(&pw).context("WebView2 runtime too old")?;
                unsafe {
                    core.PostSharedBufferToScript(
                        &buffer,
                        COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                        &HSTRING::from(meta.as_str()),
                    )
                }
                .context("post shared buffer")?;

                let mut registry = REGISTRY.lock();
                let map = registry.get_or_insert_with(HashMap::new);
                if let Some(old) = map.insert(
                    key,
                    Entry {
                        buffer: UiThreadOnly(buffer),
                        meta,
                        generation,
                        window: label,
                    },
                ) {
                    unsafe {
                        let _ = old.buffer.0.Close();
                    }
                }
                Ok(ptr as usize)
            })();
            let _ = tx.send(result);
        })?;

        let ptr = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("UI thread did not answer the surface request"))??;
        anyhow::ensure!(ptr % 8 == 0, "shared buffer misaligned");
        Ok(Arc::new(SharedSurface { ptr, len }))
    }

    fn release(&self, session: &str, kind: SurfaceKind) {
        let Some(window) = self.app.get_webview_window(MAIN_WINDOW) else {
            return;
        };
        let key = (session.to_owned(), kind);
        let app = self.app.clone();
        let _ = window.with_webview(move |_| {
            let removed = REGISTRY.lock().as_mut().and_then(|m| m.remove(&key));
            if let Some(entry) = removed {
                unsafe {
                    let _ = entry.buffer.0.Close();
                }
                emit_later(
                    app,
                    None,
                    SurfaceReleased {
                        session: key.0,
                        kind: key.1.as_str(),
                        generation: entry.generation,
                    },
                );
            }
        });
    }
}
