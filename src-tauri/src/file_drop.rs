//! Files dragged into a window from Explorer (or any other app).
//!
//! WebView2 handles drops itself and only gives the page `File` objects, without their
//! paths. So each window gets our own OLE drop target instead: the page hears where a file
//! drag is (`file-drag` events, CSS px in the window) and says what a drop there would do
//! ([`set_effect`]); we hand that back to the drag source, so the cursor shows copy or
//! no-drop as the pointer crosses tabs, groups and sessions. Drags that aren't files (text
//! dragged within the page) go on to WebView2's own target, untouched.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, EventTarget, Manager, WebviewWindow};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, POINTL};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, RegisterDragDrop,
    ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GA_ROOT, GetAncestor, GetPropW};
use windows::core::{BOOL, Interface as _, Ref, implement, w};

/// Per window: what a drop at the pointer would do, as the page last said.
static EFFECTS: Mutex<Option<HashMap<String, Arc<AtomicU32>>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileDrag {
    /// enter | over | leave | drop
    phase: &'static str,
    x: f64,
    y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    paths: Option<Vec<String>>,
}

/// Takes over file drops for `window`'s webview.
pub fn install(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else { return };
    let app = window.app_handle().clone();
    let label = window.label().to_owned();
    let effect = Arc::new(AtomicU32::new(DROPEFFECT_NONE.0));
    EFFECTS
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(label.clone(), Arc::clone(&effect));
    let root = hwnd.0 as isize;
    // On the UI thread, once the webview (and its windows) exist.
    let _ = window.with_webview(move |_| {
        // SAFETY: the window handle belongs to this thread's window.
        let replaced = unsafe { take_over(HWND(root as _), &app, &label, &effect) };
        tracing::debug!(window = %label, replaced, "file drops handled by the app");
    });
}

/// The page says whether dropping at the pointer would do something.
pub fn set_effect(label: &str, copy: bool) {
    if let Some(effect) = EFFECTS.lock().as_ref().and_then(|m| m.get(label)) {
        let value = if copy { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        effect.store(value.0, Ordering::Relaxed);
    }
}

/// The window is gone.
pub fn forget(label: &str) {
    if let Some(map) = EFFECTS.lock().as_mut() {
        map.remove(label);
    }
}

/// Puts our target on every window under `root` that can take drops, keeping the one it
/// had to pass other drags on to. Returns how many were replaced.
///
/// # Safety
///
/// Call on the thread that owns `root`.
unsafe fn take_over(root: HWND, app: &tauri::AppHandle, label: &str, effect: &Arc<AtomicU32>) -> usize {
    let mut children: Vec<HWND> = Vec::new();
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the Vec passed below, alive for the enumeration.
        let children = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        children.push(hwnd);
        true.into()
    }
    // SAFETY: the callback only pushes into the Vec that outlives the call.
    let _ = unsafe { EnumChildWindows(Some(root), Some(collect), LPARAM(&mut children as *mut _ as isize)) };

    let mut replaced = 0;
    for hwnd in children {
        // The target OLE registered for this window, if any (ole32 keeps it in this property).
        // SAFETY: reading a window property; the pointer, if set, is a live IDropTarget.
        let previous = unsafe {
            let raw = GetPropW(hwnd, w!("OleDropTargetInterface"));
            (!raw.0.is_null())
                .then(|| IDropTarget::from_raw_borrowed(&raw.0).cloned())
                .flatten()
        };
        // SAFETY: (re)registering on a window of this thread.
        unsafe {
            let _ = RevokeDragDrop(hwnd);
            let target: IDropTarget = FileDropTarget {
                app: app.clone(),
                label: label.to_owned(),
                effect: Arc::clone(effect),
                previous,
                state: std::cell::RefCell::new(State::Idle),
            }
            .into();
            if RegisterDragDrop(hwnd, &target).is_ok() {
                replaced += 1;
            }
        }
    }
    replaced
}

enum State {
    Idle,
    /// A file drag we handle.
    Files {
        last: (i32, i32),
    },
    /// Something else, passed on.
    Passed,
}

#[implement(IDropTarget)]
struct FileDropTarget {
    app: tauri::AppHandle,
    label: String,
    effect: Arc<AtomicU32>,
    previous: Option<IDropTarget>,
    /// OLE calls a drop target on its window's thread only.
    state: std::cell::RefCell<State>,
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: core::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// The dragged files' paths, if the data carries files.
fn paths(data: &IDataObject) -> Option<Vec<String>> {
    // SAFETY: COM calls on the data object we were handed; the medium is released below.
    unsafe {
        let mut medium = data.GetData(&hdrop_format()).ok()?;
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let paths = (0..count)
            .filter_map(|i| {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                (got > 0).then(|| String::from_utf16_lossy(&buf[..got.min(len)]))
            })
            .collect::<Vec<_>>();
        ReleaseStgMedium(&mut medium);
        (!paths.is_empty()).then_some(paths)
    }
}

impl FileDropTarget_Impl {
    /// Screen pixels to CSS px in the window.
    fn point(&self, pt: &POINTL) -> (f64, f64, (i32, i32)) {
        let Some(window) = self.app.get_webview_window(&self.label) else {
            return (0.0, 0.0, (pt.x, pt.y));
        };
        let Ok(hwnd) = window.hwnd() else {
            return (0.0, 0.0, (pt.x, pt.y));
        };
        let hwnd = HWND(hwnd.0);
        let mut p = POINT { x: pt.x, y: pt.y };
        // SAFETY: plain coordinate conversion and DPI query on our window.
        let scale = unsafe {
            let root = GetAncestor(hwnd, GA_ROOT);
            let _ = ScreenToClient(if root.is_invalid() { hwnd } else { root }, &mut p);
            f64::from(GetDpiForWindow(hwnd).max(96)) / 96.0
        };
        (f64::from(p.x) / scale, f64::from(p.y) / scale, (pt.x, pt.y))
    }

    /// Tells the page, off this call (an emit runs script in the webviews).
    fn tell(&self, drag: FileDrag) {
        let app = self.app.clone();
        let label = self.label.clone();
        tauri::async_runtime::spawn(async move {
            let _ = app.emit_to(EventTarget::webview_window(label), "file-drag", drag);
        });
    }

    fn current(&self) -> DROPEFFECT {
        DROPEFFECT(self.effect.load(Ordering::Relaxed))
    }
}

impl IDropTarget_Impl for FileDropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = data.as_ref().and_then(paths);
        let Some(files) = files else {
            *self.state.borrow_mut() = State::Passed;
            return match &self.previous {
                // SAFETY: forwarding the call as received.
                Some(previous) => unsafe { previous.DragEnter(data.as_ref(), keys, *pt, effect) },
                None => {
                    // SAFETY: OLE passes a valid out pointer.
                    unsafe { *effect = DROPEFFECT_NONE };
                    Ok(())
                }
            };
        };
        let (x, y, at) = self.point(pt);
        *self.state.borrow_mut() = State::Files { last: at };
        // Nothing is known yet about where it may land; the page answers on its first look.
        self.effect.store(DROPEFFECT_NONE.0, Ordering::Relaxed);
        self.tell(FileDrag {
            phase: "enter",
            x,
            y,
            paths: Some(files),
        });
        // SAFETY: OLE passes a valid out pointer.
        unsafe { *effect = DROPEFFECT_NONE };
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let moved = match &mut *self.state.borrow_mut() {
            State::Files { last } => {
                let moved = *last != (pt.x, pt.y);
                *last = (pt.x, pt.y);
                Some(moved)
            }
            State::Passed => None,
            State::Idle => Some(false),
        };
        match moved {
            None => match &self.previous {
                // SAFETY: forwarding the call as received.
                Some(previous) => unsafe { previous.DragOver(keys, *pt, effect) },
                None => {
                    // SAFETY: OLE passes a valid out pointer.
                    unsafe { *effect = DROPEFFECT_NONE };
                    Ok(())
                }
            },
            Some(moved) => {
                if moved {
                    let (x, y, _) = self.point(pt);
                    self.tell(FileDrag {
                        phase: "over",
                        x,
                        y,
                        paths: None,
                    });
                }
                // SAFETY: OLE passes a valid out pointer.
                unsafe { *effect = self.current() };
                Ok(())
            }
        }
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        let state = std::mem::replace(&mut *self.state.borrow_mut(), State::Idle);
        match state {
            State::Passed => match &self.previous {
                // SAFETY: forwarding the call.
                Some(previous) => unsafe { previous.DragLeave() },
                None => Ok(()),
            },
            State::Files { .. } => {
                self.tell(FileDrag {
                    phase: "leave",
                    x: 0.0,
                    y: 0.0,
                    paths: None,
                });
                Ok(())
            }
            State::Idle => Ok(()),
        }
    }

    fn Drop(
        &self,
        data: Ref<'_, IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let state = std::mem::replace(&mut *self.state.borrow_mut(), State::Idle);
        match state {
            State::Passed => match &self.previous {
                // SAFETY: forwarding the call as received.
                Some(previous) => unsafe { previous.Drop(data.as_ref(), keys, *pt, effect) },
                None => {
                    // SAFETY: OLE passes a valid out pointer.
                    unsafe { *effect = DROPEFFECT_NONE };
                    Ok(())
                }
            },
            _ => {
                let chosen = self.current();
                let (x, y, _) = self.point(pt);
                let files = (chosen != DROPEFFECT_NONE)
                    .then(|| data.as_ref().and_then(paths))
                    .flatten();
                self.tell(FileDrag {
                    phase: if files.is_some() { "drop" } else { "leave" },
                    x,
                    y,
                    paths: files,
                });
                // The files are only read (copied); the source keeps them.
                // SAFETY: OLE passes a valid out pointer.
                unsafe { *effect = chosen };
                Ok(())
            }
        }
    }
}
