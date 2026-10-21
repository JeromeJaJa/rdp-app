//! Tabs torn out of the main window into windows of their own, Chrome-style.
//!
//! Every connection's tab lives in exactly one window: the main window (the default)
//! or a detached window. A session's full-size frames go to the window that shows it,
//! and it is sized to that window. Closing a detached window hands its tabs back to the
//! main window; their sessions stay connected.
//!
//! A tab pulled out of its strip mid-drag gets a window sized so its session area
//! matches the session (no resize), which then follows the pointer (our own drag loop,
//! so the rest of the app keeps updating). While it is over another window's strip that
//! window opens a gap where the tab would land, Chrome-style, and the follow window hides;
//! on release the tab joins that window there. Dragging the only tab of a window moves
//! that window the same way. Size changes wait for the drop, and a hidden window is
//! prepared when a drag starts, so tearing a tab off is instant.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use parking_lot::Mutex;
use rdp_engine::{Engine, Viewport};
use serde::Serialize;
use tauri::{
    AppHandle, Emitter as _, Manager as _, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::frames::MAIN_WINDOW;

/// Labels of detached windows start with this.
pub const PREFIX: &str = "win-";

/// A detached window's title bar height in CSS px (`--titlebar-h`).
const TITLEBAR: f64 = 40.0;
/// Where the pointer sits in a torn-off window, in CSS px: on its tab, mid title bar.
const GRAB: (f64, f64) = (110.0, 20.0);
/// A prepared window that no drag used is closed after this long.
const SPARE_TTL: Duration = Duration::from_secs(60);

/// A rectangle in CSS px relative to a window's client area: x, y, width, height.
pub type Rect = [f64; 4];

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    /// The main window's open tabs, in order.
    pub main_tabs: Vec<String>,
    /// Detached windows in creation order, each with its tabs in order.
    pub windows: Vec<WindowTabs>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTabs {
    pub label: String,
    pub tabs: Vec<String>,
}

impl Layout {
    /// A window's tab list (main or detached), mutably.
    fn tabs_mut(&mut self, label: &str) -> Option<&mut Vec<String>> {
        if label == MAIN_WINDOW {
            Some(&mut self.main_tabs)
        } else {
            self.windows.iter_mut().find(|w| w.label == label).map(|w| &mut w.tabs)
        }
    }

    /// Removes a session's tab from every window; returns any detached window it emptied.
    fn remove_tab(&mut self, session: &str) -> Option<String> {
        self.main_tabs.retain(|t| t != session);
        let mut emptied = None;
        for w in &mut self.windows {
            let before = w.tabs.len();
            w.tabs.retain(|t| t != session);
            if w.tabs.is_empty() && before > 0 {
                emptied = Some(w.label.clone());
            }
        }
        emptied
    }

    /// The window currently holding a session's tab.
    fn host(&self, session: &str) -> Option<&str> {
        if let Some(w) = self.windows.iter().find(|w| w.tabs.iter().any(|t| t == session)) {
            Some(w.label.as_str())
        } else if self.main_tabs.iter().any(|t| t == session) {
            Some(MAIN_WINDOW)
        } else {
            None
        }
    }
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
    /// Windows being dragged: their sessions keep their size until the drop.
    held: HashSet<String>,
    /// Where each window shows its tabs (title bar, sidebar): where a dragged tab can join it.
    tab_areas: HashMap<String, Vec<Rect>>,
    /// Each window's individual top-strip tab rects, in tab order: for the live insertion point.
    tab_slots: HashMap<String, Vec<Rect>>,
    /// A loaded, hidden window ready for the next tear-off, with the stamp that expires it.
    spare: Option<(String, u64)>,
    spare_stamp: u64,
}

pub struct Tearoff {
    app: AppHandle,
    engine: Engine,
    #[cfg(windows)]
    surfaces: Arc<crate::frames::WebviewSurfaces>,
    state: Arc<Mutex<State>>,
}

impl Tearoff {
    pub fn new(app: AppHandle, engine: Engine, #[cfg(windows)] surfaces: Arc<crate::frames::WebviewSurfaces>) -> Self {
        Self {
            app,
            engine,
            #[cfg(windows)]
            surfaces,
            state: Arc::default(),
        }
    }

    pub fn layout(&self) -> Layout {
        self.state.lock().layout.clone()
    }

    fn broadcast(&self) {
        let _ = self.app.emit("window-layout", self.layout());
    }

    fn tabs_of(&self, label: &str) -> Vec<String> {
        let mut state = self.state.lock();
        state.layout.tabs_mut(label).map(|t| t.clone()).unwrap_or_default()
    }

    /// Replaces the main window's open tabs (used to restore them on launch). Sessions
    /// already in a detached window are ignored.
    pub fn set_main_tabs(&self, sessions: Vec<String>) {
        let tabs = {
            let mut state = self.state.lock();
            let detached: HashSet<String> =
                state.layout.windows.iter().flat_map(|w| w.tabs.iter().cloned()).collect();
            state.layout.main_tabs = sessions.into_iter().filter(|s| !detached.contains(s)).collect();
            state.layout.main_tabs.clone()
        };
        for tab in &tabs {
            self.apply_host(tab, MAIN_WINDOW);
        }
        self.apply_visibility();
        self.broadcast();
    }

    /// Opens `session` as a tab in the main window if it isn't open anywhere yet, and
    /// (optionally) selects it there. If it's already open elsewhere, brings that window up.
    pub fn open_tab(&self, session: &str, select: bool) {
        let host = {
            let mut state = self.state.lock();
            match state.layout.host(session) {
                Some(label) => label.to_owned(),
                None => {
                    state.layout.main_tabs.push(session.to_owned());
                    drop(state);
                    self.apply_host(session, MAIN_WINDOW);
                    self.apply_visibility();
                    MAIN_WINDOW.to_owned()
                }
            }
        };
        self.broadcast();
        if select {
            self.focus(&host);
            let _ = self.app.emit_to(host.as_str(), "select-tab", session);
        }
    }

    /// Closes `session`'s tab (the caller disconnects the session). A detached window left
    /// without tabs closes.
    pub fn close_tab(&self, session: &str) {
        let emptied = {
            let mut state = self.state.lock();
            let emptied = state.layout.remove_tab(session);
            for active in state.active.values_mut() {
                if active.as_deref() == Some(session) {
                    *active = None;
                }
            }
            emptied
        };
        self.apply_visibility();
        self.broadcast();
        if let Some(label) = emptied
            && let Some(window) = self.app.get_webview_window(&label)
        {
            let _ = window.close();
        }
    }

    /// Moves a tab into `target` (the main window or a detached one) at `index` (end if
    /// absent). A detached window left without tabs closes.
    pub fn move_tab(&self, session: &str, target: &str, index: Option<usize>) -> anyhow::Result<()> {
        let emptied = {
            let mut state = self.state.lock();
            if target != MAIN_WINDOW && !state.layout.windows.iter().any(|w| w.label == target) {
                bail!("no such window");
            }
            let mut emptied = state.layout.remove_tab(session);
            // The target can't be the emptied one (we just inserted into it).
            if emptied.as_deref() == Some(target) {
                emptied = None;
            }
            if let Some(tabs) = state.layout.tabs_mut(target) {
                let at = index.unwrap_or(tabs.len()).min(tabs.len());
                tabs.insert(at, session.to_owned());
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

    /// Reorders a window's tabs (same set, new order).
    pub fn reorder(&self, label: &str, tabs: Vec<String>) {
        let mut state = self.state.lock();
        if let Some(current) = state.layout.tabs_mut(label) {
            let mut a = current.clone();
            let mut b = tabs.clone();
            a.sort();
            b.sort();
            if a == b {
                *current = tabs;
            }
        }
        drop(state);
        self.broadcast();
    }

    // ── Opening windows ─────────────────────────────────────────────────────

    /// Has a hidden window ready for a tear-off: a tab drag started. Blocking.
    pub fn prepare(&self) -> anyhow::Result<()> {
        let existing = {
            let mut state = self.state.lock();
            state.spare_stamp += 1;
            let stamp = state.spare_stamp;
            state.spare.as_mut().map(|(label, s)| {
                *s = stamp;
                (label.clone(), stamp)
            })
        };
        let (label, stamp) = match existing {
            Some(spare) => spare,
            None => {
                let label = new_label();
                self.build(&label)?;
                let mut state = self.state.lock();
                let stamp = state.spare_stamp;
                state.spare = Some((label.clone(), stamp));
                (label, stamp)
            }
        };
        // Closed if no drag uses it.
        let state = Arc::clone(&self.state);
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(SPARE_TTL).await;
            let unused = {
                let mut state = state.lock();
                let unused = state.spare.as_ref().is_some_and(|(l, s)| *l == label && *s == stamp);
                if unused {
                    state.spare = None;
                }
                unused
            };
            if unused && let Some(window) = app.get_webview_window(&label) {
                let _ = window.close();
            }
        });
        Ok(())
    }

    /// A hidden detached window; shown once it holds a tab.
    fn build(&self, label: &str) -> anyhow::Result<WebviewWindow> {
        let window = WebviewWindowBuilder::new(&self.app, label, WebviewUrl::App("index.html".into()))
            .title("RDP App")
            .inner_size(1280.0, 800.0)
            .min_inner_size(480.0, 320.0)
            .decorations(false)
            .shadow(true)
            .transparent(true)
            .disable_drag_drop_handler()
            .background_color(tauri::window::Color(9, 9, 11, 255))
            .visible(false)
            .build()
            .context("open window")?;
        #[cfg(windows)]
        crate::configure_webview(&window);
        Ok(window)
    }

    /// Puts `session`'s tab in a window of its own, sized so the session keeps its size,
    /// placed by `place`, and shows it. `hold` defers size changes (a drag follows).
    fn open(
        &self,
        session: &str,
        source: &str,
        hold: bool,
        place: impl FnOnce(&WebviewWindow),
    ) -> anyhow::Result<(String, WebviewWindow)> {
        let spare = self.state.lock().spare.take().map(|(label, _)| label);
        let (label, window) = match spare.and_then(|l| Some((l.clone(), self.app.get_webview_window(&l)?))) {
            Some(ready) => ready,
            None => {
                let label = new_label();
                let window = self.build(&label)?;
                (label, window)
            }
        };
        let current = {
            let mut state = self.state.lock();
            // Its last size report predates the resize below.
            state.viewports.remove(&label);
            if hold {
                state.held.insert(label.clone());
            }
            state.layout.windows.push(WindowTabs {
                label: label.clone(),
                tabs: Vec::new(),
            });
            state.viewports.get(source).copied()
        };
        if let Some(vp) = current {
            let titlebar = (TITLEBAR * f64::from(vp.scale) / 100.0).round() as u32;
            let _ = window.set_size(PhysicalSize::new(u32::from(vp.width), u32::from(vp.height) + titlebar));
            // Pinned to its current size until the window reports its own.
            self.engine.set_session_viewport(session, Some(vp));
        }
        place(&window);
        self.move_tab(session, &label, None)?;
        let _ = window.show();
        let _ = window.set_focus();
        Ok((label, window))
    }

    /// Opens `session`'s tab in a new window next to `source` (menu command). Blocking.
    pub fn detach(&self, session: &str, source: &str) -> anyhow::Result<String> {
        let anchor = self
            .app
            .get_webview_window(source)
            .and_then(|w| w.outer_position().ok());
        let (label, _) = self.open(session, source, false, |w| {
            if let Some(p) = anchor {
                let _ = w.set_position(PhysicalPosition::new(p.x + 48, p.y + 48));
            }
        })?;
        Ok(label)
    }

    /// Tears `session`'s tab out of `source` mid-drag: its new window follows the pointer
    /// until the button is released. Returns the window the tab ends up in. Blocking.
    pub fn tear(&self, session: &str, source: &str) -> anyhow::Result<String> {
        let cursor = self.app.cursor_position().context("pointer position")?;
        let (label, window) = self.open(session, source, true, |w| place_at(w, cursor))?;
        Ok(self.follow_pointer(&label, &window, cursor))
    }

    /// Drags a detached window by its only tab. Returns the window the tab ends up in.
    /// Blocking.
    pub fn drag(&self, label: &str) -> anyhow::Result<String> {
        let window = self.app.get_webview_window(label).context("window")?;
        let cursor = self.app.cursor_position().context("pointer position")?;
        self.state.lock().held.insert(label.to_owned());
        Ok(self.follow_pointer(label, &window, cursor))
    }

    /// Drags a held window with the pointer, showing the tab land live in whatever window's
    /// strip it is over (Chrome-style), then merges its tabs into that window on release (or
    /// lets its sessions take its own size if it was dropped on nothing).
    fn follow_pointer(&self, label: &str, window: &WebviewWindow, cursor: PhysicalPosition<f64>) -> String {
        // The window the tab is hovering, and where it would land there, as it moves.
        let mut last: Option<(String, usize)> = None;
        #[cfg(windows)]
        {
            let mut on_frame = |at: PhysicalPosition<f64>| {
                let cur = self.drop_target(label, window, at).map(|t| {
                    let idx = self.dock_index(&t, at);
                    (t, idx)
                });
                if cur == last {
                    return;
                }
                // Moved to a different window: clear the gap in the old one.
                if let Some((old, _)) = &last
                    && last.as_ref().map(|(l, _)| l) != cur.as_ref().map(|(l, _)| l)
                {
                    self.emit_dock(old, None);
                }
                match &cur {
                    Some((t, idx)) => self.emit_dock(t, Some(*idx)),
                    None => {}
                }
                // Hide the follow window while it's "in" a strip; show it again out in the open.
                if last.is_some() != cur.is_some() {
                    native::show_window(window, cur.is_none());
                }
                last = cur;
            };
            native::drag_loop(&self.app, window, cursor, &mut on_frame);
        }
        let _ = cursor;
        // Where it ended up.
        let drop = self
            .app
            .cursor_position()
            .ok()
            .and_then(|at| self.drop_target(label, window, at).map(|t| (self.dock_index(&t, at), t)));
        // Clear any live gap before the real tabs arrive.
        if let Some((old, _)) = &last {
            self.emit_dock(old, None);
        }
        let result = match drop {
            Some((index, target)) => {
                // The follow window stays hidden; it empties and closes as the tabs move.
                for (i, tab) in self.tabs_of(label).into_iter().enumerate() {
                    let _ = self.move_tab(&tab, &target, Some(index + i));
                }
                self.focus(&target);
                target
            }
            None => {
                #[cfg(windows)]
                native::show_window(window, true);
                label.to_owned()
            }
        };
        self.release(label);
        result
    }

    /// Ends a hold: the window's sessions take its current size.
    fn release(&self, label: &str) {
        let (viewport, tabs) = {
            let mut state = self.state.lock();
            state.held.remove(label);
            (
                state.viewports.get(label).copied(),
                state
                    .layout
                    .windows
                    .iter()
                    .find(|w| w.label == label)
                    .map(|w| w.tabs.clone()),
            )
        };
        if let (Some(viewport), Some(tabs)) = (viewport, tabs) {
            for tab in tabs {
                self.engine.set_session_viewport(&tab, Some(viewport));
            }
        }
    }

    /// The window (other than `dragged`) whose tab area is under the pointer, if it is
    /// the topmost window there.
    fn drop_target(&self, dragged: &str, window: &WebviewWindow, at: PhysicalPosition<f64>) -> Option<String> {
        #[cfg(windows)]
        let under = native::window_below(window, at)?;
        let _ = window;
        let mut labels = vec![MAIN_WINDOW.to_owned()];
        labels.extend(self.layout().windows.into_iter().map(|w| w.label));
        labels.into_iter().filter(|l| l != dragged).find(|label| {
            let Some(candidate) = self.app.get_webview_window(label) else {
                return false;
            };
            #[cfg(windows)]
            if candidate.hwnd().ok().map(|h| h.0 as isize) != Some(under) {
                return false;
            }
            self.in_tab_area(label, &candidate, at)
        })
    }

    fn in_tab_area(&self, label: &str, window: &WebviewWindow, at: PhysicalPosition<f64>) -> bool {
        let (Ok(origin), Ok(scale)) = (window.inner_position(), window.scale_factor()) else {
            return false;
        };
        let state = self.state.lock();
        state.tab_areas.get(label).is_some_and(|rects| {
            rects.iter().any(|&[x, y, w, h]| {
                let left = f64::from(origin.x) + x * scale;
                let top = f64::from(origin.y) + y * scale;
                at.x >= left && at.x < left + w * scale && at.y >= top && at.y < top + h * scale
            })
        })
    }

    /// Where a window shows its tabs, in CSS px (empty in fullscreen): `areas` are the drop
    /// targets (title bar, sidebar), `slots` the individual top-strip tabs in order.
    pub fn set_tab_area(&self, label: &str, areas: Vec<Rect>, slots: Vec<Rect>) {
        let mut state = self.state.lock();
        state.tab_areas.insert(label.to_owned(), areas);
        state.tab_slots.insert(label.to_owned(), slots);
    }

    /// Where a tab dropped on `label` at `at` would land among its tabs (screen px in → index).
    fn dock_index(&self, label: &str, at: PhysicalPosition<f64>) -> usize {
        let Some(window) = self.app.get_webview_window(label) else {
            return 0;
        };
        let (Ok(origin), Ok(scale)) = (window.inner_position(), window.scale_factor()) else {
            return 0;
        };
        let state = self.state.lock();
        let Some(slots) = state.tab_slots.get(label) else {
            return 0;
        };
        let mut idx = 0;
        for &[x, _y, w, _h] in slots {
            let mid = f64::from(origin.x) + (x + w / 2.0) * scale;
            if at.x >= mid {
                idx += 1;
            } else {
                break;
            }
        }
        idx.min(slots.len())
    }

    /// Tells `label`'s window where a dragged tab would land right now (`None` clears it).
    fn emit_dock(&self, label: &str, index: Option<usize>) {
        let _ = self.app.emit_to(label, "dock-preview", index);
    }

    // ── Closing, visibility, sizes ──────────────────────────────────────────

    /// A detached window closed: its tabs go back to the main window.
    pub fn window_closed(&self, label: &str) {
        let tabs = {
            let mut state = self.state.lock();
            state.active.remove(label);
            state.viewports.remove(label);
            state.held.remove(label);
            state.tab_areas.remove(label);
            state.tab_slots.remove(label);
            if state.spare.as_ref().is_some_and(|(l, _)| l == label) {
                state.spare = None;
            }
            let Some(pos) = state.layout.windows.iter().position(|w| w.label == label) else {
                return;
            };
            let tabs = state.layout.windows.remove(pos).tabs;
            // Its tabs return to the main window.
            state.layout.main_tabs.extend(tabs.iter().cloned());
            tabs
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
            if state.held.contains(label) {
                return;
            }
            // The main view drives the engine's default viewport; detached windows size
            // their own tabs' sessions.
            if label == MAIN_WINDOW {
                self.engine.set_viewport(viewport);
                return;
            }
            state.layout.windows.iter().find(|w| w.label == label).map(|w| w.tabs.clone())
        };
        if let Some(tabs) = tabs {
            for tab in tabs {
                self.engine.set_session_viewport(&tab, Some(viewport));
            }
        }
    }

    fn apply_visibility(&self) {
        let (active, thumbnails) = {
            let state = self.state.lock();
            // Only count a window's session if its tab really lives there.
            let active: Vec<String> = state
                .active
                .iter()
                .filter_map(|(label, a)| {
                    a.as_ref().filter(|s| state.layout.host(s) == Some(label.as_str())).cloned()
                })
                .collect();
            (active, state.thumbnails.clone())
        };
        self.engine.set_visibility(active, thumbnails);
    }

    /// Routes the session's frames to `label` and sizes it for that window: the main
    /// view's size, or the detached window's (unless that one is held or hasn't
    /// reported a size yet: the session keeps its size).
    fn apply_host(&self, session: &str, label: &str) {
        #[cfg(windows)]
        self.surfaces.set_host(session, label);
        let viewport = {
            let state = self.state.lock();
            if label == MAIN_WINDOW {
                None
            } else if state.held.contains(label) {
                return;
            } else {
                match state.viewports.get(label) {
                    Some(vp) => Some(*vp),
                    None => return,
                }
            }
        };
        self.engine.set_session_viewport(session, viewport);
    }

    /// Brings the window holding `session`'s tab to the front and selects the tab there.
    pub fn show_tab(&self, session: &str) {
        let label = self.state.lock().layout.host(session).unwrap_or(MAIN_WINDOW).to_owned();
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

fn new_label() -> String {
    format!("{PREFIX}{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// Moves a window so the pointer is over its tab, mid title bar.
fn place_at(window: &WebviewWindow, cursor: PhysicalPosition<f64>) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let _ = window.set_position(PhysicalPosition::new(
        cursor.x - GRAB.0 * scale,
        cursor.y - GRAB.1 * scale,
    ));
}

#[cfg(windows)]
mod native {
    use std::time::Duration;

    use tauri::{AppHandle, PhysicalPosition, WebviewWindow};
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, VK_LBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_HWNDNEXT, GWL_EXSTYLE, GetCursorPos, GetSystemMetrics, GetWindow, GetWindowLongW, GetWindowRect,
        HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, IsWindowVisible, SM_SWAPBUTTON, SW_HIDE, SW_SHOWNOACTIVATE,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, ShowWindow, WS_EX_TRANSPARENT,
    };

    fn hwnd(window: &WebviewWindow) -> Option<HWND> {
        window.hwnd().ok().map(|h| HWND(h.0))
    }

    fn primary_button_down() -> bool {
        // SAFETY: plain input-state queries.
        unsafe {
            let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
                VK_RBUTTON
            } else {
                VK_LBUTTON
            };
            (GetAsyncKeyState(i32::from(key.0)) as u16) & 0x8000 != 0
        }
    }

    /// Shows or hides `window` without activating it (a dragged tab's follow window hides
    /// while it is "inside" another window's strip).
    pub(super) fn show_window(window: &WebviewWindow, show: bool) {
        let Some(hwnd) = hwnd(window) else { return };
        // SAFETY: toggles our own window's visibility; never steals focus.
        unsafe {
            let _ = ShowWindow(hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }

    /// Drags `window` with the pointer until the button is released, calling `on_frame` with
    /// the pointer each tick (to preview where the tab would land). Our own loop, not the
    /// system move loop, so the rest of the app keeps updating live. Blocks: call off the UI
    /// thread.
    pub(super) fn drag_loop(
        app: &AppHandle,
        window: &WebviewWindow,
        _cursor: PhysicalPosition<f64>,
        on_frame: &mut dyn FnMut(PhysicalPosition<f64>),
    ) {
        let Some(hwnd) = hwnd(window) else { return };
        if !primary_button_down() {
            return;
        }
        let scale = window.scale_factor().unwrap_or(1.0);
        // The press began in another window (the tab strip), which holds the capture.
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = app.run_on_main_thread(move || {
            // SAFETY: releases that window's mouse capture so we can follow the pointer.
            unsafe {
                let _ = ReleaseCapture();
            }
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_secs(1));
        // Keep the dragged window on top so hit-testing finds the window below it.
        loop {
            if !primary_button_down() {
                break;
            }
            let mut pt = POINT::default();
            // SAFETY: reads the global cursor position.
            if unsafe { GetCursorPos(&mut pt) }.is_err() {
                break;
            }
            let at = PhysicalPosition::new(f64::from(pt.x), f64::from(pt.y));
            // SAFETY: moves our own window so the pointer stays on its tab; no activation.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    (at.x - super::GRAB.0 * scale) as i32,
                    (at.y - super::GRAB.1 * scale) as i32,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            on_frame(at);
            std::thread::sleep(Duration::from_millis(8));
        }
        // Back to a normal z-order once dropped (if it survives as its own window).
        // SAFETY: our own window; position and size unchanged.
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    /// The topmost window under `at` beneath `window` (which sits under the pointer
    /// itself), skipping hidden, click-through and cloaked ones.
    pub(super) fn window_below(window: &WebviewWindow, at: PhysicalPosition<f64>) -> Option<isize> {
        let point = POINT {
            x: at.x as i32,
            y: at.y as i32,
        };
        // SAFETY: walks the z-order with plain window queries.
        unsafe {
            let mut next = GetWindow(hwnd(window)?, GW_HWNDNEXT).ok()?;
            loop {
                if hit(next, point) {
                    return Some(next.0 as isize);
                }
                next = GetWindow(next, GW_HWNDNEXT).ok()?;
            }
        }
    }

    unsafe fn hit(hwnd: HWND, point: POINT) -> bool {
        // SAFETY: plain window queries on a handle from the z-order walk.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return false;
            }
            if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0 {
                return false;
            }
            let mut cloaked = 0u32;
            if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4).is_ok() && cloaked != 0 {
                return false;
            }
            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect).is_ok()
                && point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
        }
    }
}
