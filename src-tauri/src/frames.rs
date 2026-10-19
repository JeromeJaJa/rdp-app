//! Frame surfaces backed by WebView2 shared buffers.
//!
//! The engine writes decoded pixels straight into memory that the page maps as
//! an `ArrayBuffer` (via `PostSharedBufferToScript`), so frames reach the
//! renderer with zero copies and zero IPC messages. See
//! `rdp_engine::surface` for the memory layout the page polls.
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
}

impl WebviewSurfaces {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            next_generation: AtomicU64::new(1),
        })
    }

    /// Re-sends every live buffer to the page (after a reload the page has lost them).
    pub fn repost_all(&self) -> anyhow::Result<()> {
        let window = self.app.get_webview_window(MAIN_WINDOW).context("main window")?;
        window.with_webview(|pw| {
            let Ok(core) = core_webview(&pw) else { return };
            let registry = REGISTRY.lock();
            if let Some(map) = registry.as_ref() {
                for entry in map.values() {
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
        let window = self.app.get_webview_window(MAIN_WINDOW).context("main window")?;
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
                let _ = app.emit(
                    "surface-released",
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
