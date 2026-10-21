//! Holds a window's remote resizes while Windows moves or sizes it.
//!
//! Dragging a maximized window by its title bar restores it (smaller) at once, and crossing to
//! a monitor with another scale changes its size too; a pause mid-drag then resized the remote
//! desktop while the button was still down. Windows brackets every such drag (title bar or
//! edge) with WM_ENTERSIZEMOVE / WM_EXITSIZEMOVE: the window's sessions take its size once the
//! drag ends.

use tauri::{Manager as _, WebviewWindow};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_NCDESTROY};

const SUBCLASS_ID: usize = 0x5253_4D4F; // "RSMO"

struct Watch {
    app: tauri::AppHandle,
    label: String,
}

/// Starts watching `window`'s move/size drags.
pub fn watch(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = hwnd.0 as isize;
    let watch = Watch {
        app: window.app_handle().clone(),
        label: window.label().to_owned(),
    };
    // Subclassing has to happen on the window's own thread.
    let _ = window.run_on_main_thread(move || {
        let data = Box::into_raw(Box::new(watch));
        // SAFETY: our window, on its thread; the data box is freed when the window goes.
        let ok = unsafe { SetWindowSubclass(HWND(hwnd as _), Some(proc), SUBCLASS_ID, data as usize) };
        if !ok.as_bool() {
            // SAFETY: not handed over, so still ours.
            drop(unsafe { Box::from_raw(data) });
        }
    });
}

unsafe extern "system" fn proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the box set in `watch`, alive until WM_NCDESTROY below.
    let watch = unsafe { &*(data as *const Watch) };
    match msg {
        WM_ENTERSIZEMOVE => {
            if let Some(state) = watch.app.try_state::<crate::AppState>() {
                state.tearoff.size_move_started(&watch.label);
            }
        }
        WM_EXITSIZEMOVE => {
            if let Some(state) = watch.app.try_state::<crate::AppState>() {
                state.tearoff.size_move_ended(&watch.label);
            }
        }
        WM_NCDESTROY => {
            // SAFETY: removing our own subclass and freeing its data as the window goes.
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(proc), id);
                drop(Box::from_raw(data as *mut Watch));
            }
        }
        _ => {}
    }
    // SAFETY: the rest of the chain handles the message as before.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
