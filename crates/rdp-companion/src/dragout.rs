//! Files dragged out of the session, carried on as a drag on the client.
//!
//! When the client's pointer leaves the session with the button held, it asks us to look
//! ([`Message::DragOutProbe`]). We put a small drop target right under the remote pointer
//! and the client nudges the pointer: if a drag is under way on the remote, it finds our
//! target, and we learn what's being dragged. Real files (Explorer's `CF_HDROP`) are
//! offered to the client, which starts its own drag of them, streamed from here as they're
//! dropped. Our target never accepts the remote drop itself, so the remote source (a move
//! within Explorer, say) never deletes anything.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rdp_companion_proto::Message;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINTL, WPARAM};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, OleInitialize,
    OleUninitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer, LWA_ALPHA, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetTimer, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_DESTROY, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{AsImpl as _, Ref, implement, w};

use crate::channel::Channel;

const SIZE: i32 = 48;
/// No drag found the target this soon after the nudge: nothing is being dragged.
const LOOK_FOR: Duration = Duration::from_millis(700);
/// Once a drag is caught, how long the target stays (the client takes over meanwhile).
const HOLD_FOR: Duration = Duration::from_secs(15);
const TIMER: usize = 1;

/// Called with the dragged files once caught (serve them as transfer `id`).
pub(crate) type Serve = Arc<dyn Fn(u64, Vec<PathBuf>) + Send + Sync>;

/// Looks for a drag under the remote pointer at (`x`, `y`) on a thread of its own.
pub(crate) fn probe(channel: Arc<Channel>, id: u64, x: i32, y: i32, serve: Serve) {
    let looker = Arc::clone(&channel);
    let spawned = std::thread::Builder::new()
        .name("drag-out".into())
        .spawn(move || run(&looker, id, x, y, &serve));
    if spawned.is_err() {
        // Can't look: say so, so the client doesn't wait.
        channel.send(&Message::DragOutNone { id });
    }
}

fn run(channel: &Arc<Channel>, id: u64, x: i32, y: i32, serve: &Serve) {
    // SAFETY: OLE for this thread; paired with OleUninitialize below.
    if unsafe { OleInitialize(None) }.is_err() {
        channel.send(&Message::DragOutNone { id });
        return;
    }
    // SAFETY: a window owned by this thread, registered as a drop target, pumped until it
    // closes; the target object lives (in OLE's registration) until RevokeDragDrop.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: w!("RdpAppCompanionDragOut"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("RdpAppCompanionDragOut"),
            w!(""),
            WS_POPUP,
            x - SIZE / 2,
            y - SIZE / 2,
            SIZE,
            SIZE,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            channel.send(&Message::DragOutNone { id });
            OleUninitialize();
            return;
        };
        let target: IDropTarget = Catcher {
            channel: Arc::clone(channel),
            id,
            window: hwnd,
            serve: Arc::clone(serve),
            answered: Cell::new(false),
        }
        .into();
        if RegisterDragDrop(hwnd, &target).is_err() {
            let _ = DestroyWindow(hwnd);
            channel.send(&Message::DragOutNone { id });
            OleUninitialize();
            return;
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER, LOOK_FOR.as_millis() as u32, None);
        channel.send(&Message::DragOutReady { id });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RevokeDragDrop(hwnd);
        // If no drag ever came, say so (Catcher answers at most once).
        target_answer_none(&target);
        drop(target);
        OleUninitialize();
    }
}

fn target_answer_none(target: &IDropTarget) {
    // SAFETY: the object behind the interface is our Catcher.
    let catcher = unsafe { target.as_impl() };
    if !catcher.answered.replace(true) {
        catcher.channel.send(&Message::DragOutNone { id: catcher.id });
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER | WM_CLOSE => {
            // SAFETY: tearing down our own window.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER);
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ending this thread's loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[implement(IDropTarget)]
struct Catcher {
    channel: Arc<Channel>,
    id: u64,
    window: HWND,
    serve: Serve,
    /// Whether the client has had its answer (files, or none).
    answered: Cell<bool>,
}

/// The real files in a dragged data object (`CF_HDROP`), if any.
fn dropped_paths(data: &IDataObject) -> Option<Vec<PathBuf>> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: core::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: COM calls on the data object we were handed; the medium is released below.
    unsafe {
        let mut medium = data.GetData(&format).ok()?;
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let paths: Vec<PathBuf> = (0..count)
            .filter_map(|i| {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                (got > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..got.min(len)])))
            })
            .collect();
        ReleaseStgMedium(&mut medium);
        (!paths.is_empty()).then_some(paths)
    }
}

impl Catcher_Impl {
    fn hold(&self) {
        // A drag found us: stay a while for the client to take over, then go.
        // SAFETY: re-arming our own window's timer.
        unsafe {
            SetTimer(Some(self.window), TIMER, HOLD_FOR.as_millis() as u32, None);
        }
    }
}

impl IDropTarget_Impl for Catcher_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let mut accept = DROPEFFECT_NONE;
        if !self.answered.replace(true) {
            match data.as_ref().and_then(dropped_paths) {
                Some(paths) => {
                    tracing::info!(items = paths.len(), "files dragged out of the session");
                    (self.serve)(self.id, paths);
                    accept = DROPEFFECT_COPY;
                    self.hold();
                }
                None => {
                    self.channel.send(&Message::DragOutNone { id: self.id });
                    // SAFETY: closing our own window (the loop then ends).
                    let _ = unsafe { PostMessageW(Some(self.window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                }
            }
        }
        // SAFETY: OLE passes a valid out pointer.
        unsafe { *effect = accept };
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        // Looks like a copy on the remote while the client carries the drag on.
        // SAFETY: OLE passes a valid out pointer.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        _data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // The drag lands on the client; here nothing happens (and the source keeps its files).
        // SAFETY: OLE passes a valid out pointer; closing our own window.
        unsafe {
            *effect = DROPEFFECT_NONE;
            let _ = PostMessageW(Some(self.window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}
