//! Tabs torn out of the main window into windows of their own, Chrome-style.
//!
//! Every connection's tab lives in exactly one window: the main window (the default)
//! or a detached window. A session's full-size frames go to the window that shows it,
//! and it is sized to that window. Closing a detached window hands its tabs back to the
//! main window; their sessions stay connected.

use std::collections::HashMap;

use anyhow::{Context as _, bail};
use parking_lot::Mutex;
use rdp_engine::{Engine, Viewport};
use serde::Serialize;
use tauri::{AppHandle, Emitter as _, Manager as _, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

use crate::frames::MAIN_WINDOW;

/// Labels of detached windows start with this.
pub const PREFIX: &str = "win-";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    /// Detached windows in creation order, each with its tabs in order.
    pub windows: Vec<WindowTabs>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTabs {
    pub label: String,
    pub tabs: Vec<String>,
}

#[derive(Default)]
struct State {
    layout: Layout,
    /// The session each window shows full size right now.
    active: HashMap<String, Option<String>>,
    /// Sessions shown as grid thumbnails (main window only).
    thumbnails: Vec<String>,
    /// Each window's session-area size.
    viewports: HashMap<String, Viewport>,
}

pub struct Tearoff {
    app: AppHandle,
    engine: Engine,
    #[cfg(windows)]
    surfaces: std::sync::Arc<crate::frames::WebviewSurfaces>,
    state: Mutex<State>,
}

impl Tearoff {
    pub fn new(
        app: AppHandle,
        engine: Engine,
        #[cfg(windows)] surfaces: std::sync::Arc<crate::frames::WebviewSurfaces>,
    ) -> Self {
        Self {
            app,
            engine,
            #[cfg(windows)]
            surfaces,
            state: Mutex::new(State::default()),
        }
    }

    pub fn layout(&self) -> Layout {
        self.state.lock().layout.clone()
    }

    fn broadcast(&self) {
        let _ = self.app.emit("window-layout", self.layout());
    }

    /// Moves a tab into `target` (the main window or a detached one) at `index` (end if
    /// absent). A detached window left without tabs closes.
    pub fn move_tab(&self, session: &str, target: &str, index: Option<usize>) -> anyhow::Result<()> {
        let emptied = {
            let mut state = self.state.lock();
            if target != MAIN_WINDOW && !state.layout.windows.iter().any(|w| w.label == target) {
                bail!("no such window");
            }
            let mut emptied = None;
            for w in &mut state.layout.windows {
                let before = w.tabs.len();
                w.tabs.retain(|t| t != session);
                if w.tabs.is_empty() && before > 0 && w.label != target {
                    emptied = Some(w.label.clone());
                }
            }
            if let Some(w) = state.layout.windows.iter_mut().find(|w| w.label == target) {
                let at = index.unwrap_or(w.tabs.len()).min(w.tabs.len());
                w.tabs.insert(at, session.to_owned());
            }
            // Its old window no longer shows it.
            for active in state.active.values_mut() {
                if active.as_deref() == Some(session) {
                    *active = None;
                }
            }
            emptied
        };
        self.apply_host(session, target);
        self.apply_visibility();
        self.broadcast();
        if let Some(label) = emptied
            && let Some(window) = self.app.get_webview_window(&label)
        {
            let _ = window.close();
        }
        Ok(())
    }

    /// Reorders a detached window's tabs (same set, new order).
    pub fn reorder(&self, label: &str, tabs: Vec<String>) {
        let mut state = self.state.lock();
        if let Some(w) = state.layout.windows.iter_mut().find(|w| w.label == label) {
            let mut a = w.tabs.clone();
            let mut b = tabs.clone();
            a.sort();
            b.sort();
            if a == b {
                w.tabs = tabs;
            }
        }
        drop(state);
        self.broadcast();
    }

    /// Opens a new window at the pointer holding `session`'s tab.
    pub fn detach(&self, session: &str) -> anyhow::Result<String> {
        let label = format!("{PREFIX}{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let cursor = self.app.cursor_position().ok();
        let size = self
            .app
            .get_webview_window(MAIN_WINDOW)
            .and_then(|w| w.inner_size().ok().zip(w.scale_factor().ok()))
            .map(|(s, scale)| (f64::from(s.width) / scale * 0.8, f64::from(s.height) / scale * 0.8))
            .unwrap_or((1280.0, 800.0));
        let window = WebviewWindowBuilder::new(&self.app, &label, WebviewUrl::App("index.html".into()))
            .title("RDP App")
            .inner_size(size.0.max(720.0), size.1.max(480.0))
            .min_inner_size(480.0, 320.0)
            .decorations(false)
            .shadow(true)
            .disable_drag_drop_handler()
            .background_color(tauri::window::Color(9, 9, 11, 255))
            // Shown by the page after its first paint (no white flash).
            .visible(false)
            .build()
            .context("open window")?;
        #[cfg(windows)]
        crate::configure_webview(&window);
        // In case the page fails to load.
        let late = window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if !late.is_visible().unwrap_or(true) {
                let _ = late.show();
            }
        });
        if let Some(cursor) = cursor {
            place_at(&window, cursor);
        }
        self.state.lock().layout.windows.push(WindowTabs {
            label: label.clone(),
            tabs: Vec::new(),
        });
        self.move_tab(session, &label, None)?;
        Ok(label)
    }

    /// A tab was dropped outside `source`: onto another of our windows (moves there) or
    /// onto empty space (opens a new window there). Returns the tab's window.
    pub fn drop_tab(&self, session: &str, source: &str) -> anyhow::Result<String> {
        let cursor = self.app.cursor_position().context("pointer position")?;
        let layout = self.layout();
        let mut labels = vec![MAIN_WINDOW.to_owned()];
        labels.extend(layout.windows.iter().map(|w| w.label.clone()));
        labels.retain(|l| l != source);
        if let Some(label) = self.window_at(cursor, &labels) {
            self.move_tab(session, &label, None)?;
            self.focus(&label);
            return Ok(label);
        }
        // A window's only tab: move that window rather than open another.
        let alone = layout
            .windows
            .iter()
            .any(|w| w.label == source && w.tabs.len() == 1 && w.tabs[0] == session);
        if alone && let Some(window) = self.app.get_webview_window(source) {
            let _ = window.unmaximize();
            place_at(&window, cursor);
            return Ok(source.to_owned());
        }
        self.detach(session)
    }

    /// Which of `labels` is the window under the pointer. Z-order aware: a window
    /// covering ours there (even another app's) wins, so nothing matches.
    #[cfg(windows)]
    fn window_at(&self, cursor: PhysicalPosition<f64>, labels: &[String]) -> Option<String> {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, WindowFromPoint};
        // SAFETY: plain Win32 queries; no pointers are kept.
        let hit = unsafe {
            GetAncestor(
                WindowFromPoint(POINT {
                    x: cursor.x as i32,
                    y: cursor.y as i32,
                }),
                GA_ROOT,
            )
        };
        if hit.is_invalid() {
            return None;
        }
        labels
            .iter()
            .find(|l| {
                self.app
                    .get_webview_window(l)
                    .and_then(|w| w.hwnd().ok())
                    .is_some_and(|h| std::ptr::eq(h.0, hit.0))
            })
            .cloned()
    }

    #[cfg(not(windows))]
    fn window_at(&self, cursor: PhysicalPosition<f64>, labels: &[String]) -> Option<String> {
        labels
            .iter()
            .find(|l| {
                let Some(window) = self.app.get_webview_window(l) else {
                    return false;
                };
                if window.is_minimized().unwrap_or(false) {
                    return false;
                }
                let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
                    return false;
                };
                cursor.x >= f64::from(pos.x)
                    && cursor.y >= f64::from(pos.y)
                    && cursor.x < f64::from(pos.x) + f64::from(size.width)
                    && cursor.y < f64::from(pos.y) + f64::from(size.height)
            })
            .cloned()
    }

    /// A detached window closed: its tabs go back to the main window.
    pub fn window_closed(&self, label: &str) {
        let tabs = {
            let mut state = self.state.lock();
            state.active.remove(label);
            state.viewports.remove(label);
            let Some(pos) = state.layout.windows.iter().position(|w| w.label == label) else {
                return;
            };
            state.layout.windows.remove(pos).tabs
        };
        for tab in &tabs {
            self.apply_host(tab, MAIN_WINDOW);
        }
        self.apply_visibility();
        self.broadcast();
    }

    /// What a window shows: its full-size session and (main window) grid thumbnails.
    pub fn set_visibility(&self, label: &str, active: Option<String>, thumbnails: Vec<String>) {
        {
            let mut state = self.state.lock();
            state.active.insert(label.to_owned(), active);
            if label == MAIN_WINDOW {
                state.thumbnails = thumbnails;
            }
        }
        self.apply_visibility();
    }

    /// A window's session area was resized.
    pub fn set_viewport(&self, label: &str, viewport: Viewport) {
        let tabs = {
            let mut state = self.state.lock();
            state.viewports.insert(label.to_owned(), viewport);
            state
                .layout
                .windows
                .iter()
                .find(|w| w.label == label)
                .map(|w| w.tabs.clone())
        };
        match tabs {
            None if label == MAIN_WINDOW => self.engine.set_viewport(viewport),
            None => {}
            Some(tabs) => {
                for tab in tabs {
                    self.engine.set_session_viewport(&tab, Some(viewport));
                }
            }
        }
    }

    fn apply_visibility(&self) {
        let (active, thumbnails) = {
            let state = self.state.lock();
            // Only count a window's session if its tab really lives there.
            let host = |s: &str| {
                state
                    .layout
                    .windows
                    .iter()
                    .find(|w| w.tabs.iter().any(|t| t == s))
                    .map_or(MAIN_WINDOW, |w| w.label.as_str())
            };
            let active: Vec<String> = state
                .active
                .iter()
                .filter_map(|(label, a)| a.as_ref().filter(|s| host(s) == label.as_str()).cloned())
                .collect();
            (active, state.thumbnails.clone())
        };
        self.engine.set_visibility(active, thumbnails);
    }

    fn apply_host(&self, session: &str, label: &str) {
        #[cfg(windows)]
        self.surfaces.set_host(session, label);
        let viewport = if label == MAIN_WINDOW {
            None
        } else {
            self.state.lock().viewports.get(label).copied()
        };
        if label == MAIN_WINDOW || viewport.is_some() {
            self.engine.set_session_viewport(session, viewport);
        }
    }

    /// Brings the window holding `session`'s tab to the front and selects the tab there.
    pub fn show_tab(&self, session: &str) {
        let label = {
            let state = self.state.lock();
            state
                .layout
                .windows
                .iter()
                .find(|w| w.tabs.iter().any(|t| t == session))
                .map_or_else(|| MAIN_WINDOW.to_owned(), |w| w.label.clone())
        };
        self.focus(&label);
        let _ = self.app.emit_to(label.as_str(), "select-tab", session);
    }

    /// Brings a window to the front.
    pub fn focus(&self, label: &str) {
        if let Some(window) = self.app.get_webview_window(label) {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

/// Moves a window so its tab strip sits under the pointer.
fn place_at(window: &tauri::WebviewWindow, cursor: PhysicalPosition<f64>) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let _ = window.set_position(PhysicalPosition::new(cursor.x - 140.0 * scale, cursor.y - 18.0 * scale));
}
