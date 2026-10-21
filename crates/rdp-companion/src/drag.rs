//! Files dragged onto the session from the client, dropped here like any local drag.
//!
//! A drag on Windows needs the mouse button down where it starts. So for each drag the
//! companion shows a tiny, almost invisible window under the pointer; the client presses
//! the button there through the session (its own input, not faked here); the window then
//! starts a real OLE drag of the client's files (streamed from the client as the drop
//! target reads them), and the client moves the pointer and releases the button through
//! the session as the user drags on their side. Remote targets react the way they always
//! do; what they'd accept goes back to the client to show on its cursor.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rdp_companion_proto::Message;
use rdp_companion_proto::transfer::Inflow;
use rdp_vfiles::{InflowFiles, Options, VirtualFiles, data_object};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize,
    OleUninitialize,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    KillTimer, LWA_ALPHA, MSG, PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW, SW_HIDE,
    SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_CLOSE,
    WM_LBUTTONDOWN, WM_QUIT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows::core::{BOOL, HRESULT, implement, w};

use crate::channel::Channel;

/// Side of the window the button is pressed in (it only has to be under the pointer).
const SIZE: i32 = 48;
/// How long the window waits for the client's button press before giving up.
const PRESS_WAIT: Duration = Duration::from_secs(10);
/// After the drop, how long the thread keeps serving the files to a target still reading.
const SERVE_LIMIT: Duration = Duration::from_secs(4 * 60 * 60);
const TIMER_PRESS: usize = 1;

const DRAGDROP_S_DROP: HRESULT = HRESULT(0x0004_0100);
const DRAGDROP_S_CANCEL: HRESULT = HRESULT(0x0004_0101);
const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = HRESULT(0x0004_0102);

/// The drag in progress (one at a time: there's one pointer).
#[derive(Default)]
pub(crate) struct Drags {
    current: Mutex<Option<Current>>,
}

struct Current {
    id: u64,
    cancel: Arc<AtomicBool>,
    /// The drag window, until the drag starts (0 after).
    window: Arc<AtomicIsize>,
}

impl Drags {
    /// Gets a drag of `inflow`'s files ready at (`x`, `y`).
    pub(crate) fn start(&self, channel: Arc<Channel>, inflow: Arc<Inflow>, x: i32, y: i32) {
        let id = inflow.id();
        self.cancel_current();
        let cancel = Arc::new(AtomicBool::new(false));
        let window = Arc::new(AtomicIsize::new(0));
        *lock(&self.current) = Some(Current {
            id,
            cancel: Arc::clone(&cancel),
            window: Arc::clone(&window),
        });
        let spawned = std::thread::Builder::new().name("drag".into()).spawn(move || {
            run(&channel, inflow, x, y, &cancel, &window);
        });
        if let Err(error) = spawned {
            tracing::warn!(%error, "couldn't start a drag");
        }
    }

    /// The client cancelled drag `id` (it left the session, or Esc).
    pub(crate) fn cancel(&self, id: u64) {
        if lock(&self.current).as_ref().is_some_and(|c| c.id == id) {
            self.cancel_current();
        }
    }

    fn cancel_current(&self) {
        if let Some(current) = lock(&self.current).take() {
            current.cancel.store(true, Ordering::SeqCst);
            let hwnd = current.window.load(Ordering::SeqCst);
            if hwnd != 0 {
                // Not pressed yet: close the window, which ends the drag.
                // SAFETY: posting to a window handle (stale handles just fail).
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The dragged files; when every app lets go of them, the drag thread can end.
struct DragFiles {
    files: InflowFiles,
    thread: u32,
}

impl VirtualFiles for DragFiles {
    fn entries(&self) -> Result<Arc<[rdp_companion_proto::Entry]>, String> {
        self.files.entries()
    }

    fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
        self.files.read(index, position, out)
    }
}

impl Drop for DragFiles {
    fn drop(&mut self) {
        // SAFETY: posting to our own drag thread's queue.
        let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

/// What the window procedure needs, behind GWLP_USERDATA.
struct Context {
    channel: Arc<Channel>,
    id: u64,
    data: Option<windows::Win32::System::Com::IDataObject>,
    cancel: Arc<AtomicBool>,
    window: Arc<AtomicIsize>,
}

fn run(channel: &Arc<Channel>, inflow: Arc<Inflow>, x: i32, y: i32, cancel: &Arc<AtomicBool>, slot: &Arc<AtomicIsize>) {
    let id = inflow.id();
    // SAFETY: OLE for this thread; paired with OleUninitialize below.
    if let Err(error) = unsafe { OleInitialize(None) } {
        tracing::warn!(%error, "OLE unavailable; can't take the drag");
        channel.send(&Message::DragEnded { id, effect: 0 });
        return;
    }
    // SAFETY: plain thread query.
    let thread = unsafe { GetCurrentThreadId() };
    let data = data_object(
        Arc::new(DragFiles {
            files: InflowFiles::new(inflow),
            thread,
        }),
        Options::default(),
    );
    let context = Box::new(Context {
        channel: Arc::clone(channel),
        id,
        data: Some(data),
        cancel: Arc::clone(cancel),
        window: Arc::clone(slot),
    });

    // SAFETY: creating, showing and pumping a window owned by this thread; the context box
    // lives until the loop below ends.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: w!("RdpAppCompanionDrag"),
            ..Default::default()
        };
        RegisterClassW(&class); // fails harmlessly once registered
        let context = Box::into_raw(context);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("RdpAppCompanionDrag"),
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
        );
        let Ok(hwnd) = hwnd else {
            drop(Box::from_raw(context));
            channel.send(&Message::DragEnded { id, effect: 0 });
            OleUninitialize();
            return;
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, context as isize);
        // Nearly transparent, but still there to be clicked (fully transparent isn't).
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        slot.store(hwnd.0 as isize, Ordering::SeqCst);
        SetTimer(Some(hwnd), TIMER_PRESS, PRESS_WAIT.as_millis() as u32, None);
        if cancel.load(Ordering::SeqCst) {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        } else {
            channel.send(&Message::DragReady { id });
        }

        // Pump until the drag is over and nobody holds the files any more (DragFiles
        // posts WM_QUIT when released), serving a target that reads them after the drop.
        let started = std::time::Instant::now();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if started.elapsed() > SERVE_LIMIT {
                break;
            }
        }
        // The window may still be up if the loop ended early.
        let hwnd_left = slot.swap(0, Ordering::SeqCst);
        if hwnd_left != 0 {
            let _ = DestroyWindow(HWND(hwnd_left as _));
        }
        drop(Box::from_raw(context));
        OleUninitialize();
    }
}

/// The drag window: the client's button press starts the drag.
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the context pointer is set right after creation and outlives the window.
    let context = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Context).as_mut() };
    let Some(context) = context else {
        // SAFETY: default handling before the context is set.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_LBUTTONDOWN => {
            // SAFETY: timer and window calls on our own window.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_PRESS);
            }
            context.window.store(0, Ordering::SeqCst);
            let effect = drag(hwnd, context);
            context.channel.send(&Message::DragEnded {
                id: context.id,
                effect: effect.0,
            });
            // Our reference goes; a target still reading keeps the files alive.
            context.data = None;
            // SAFETY: destroying our own window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        // No press came (or the client cancelled first): give up.
        WM_TIMER | WM_CLOSE => {
            context.window.store(0, Ordering::SeqCst);
            context.channel.send(&Message::DragEnded {
                id: context.id,
                effect: 0,
            });
            context.data = None;
            // SAFETY: destroying our own window and ending this thread's loop.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_PRESS);
                let _ = DestroyWindow(hwnd);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Runs the OLE drag (modal until the button comes up or it's cancelled).
fn drag(hwnd: HWND, context: &Context) -> DROPEFFECT {
    let Some(data) = context.data.clone() else {
        return DROPEFFECT_NONE;
    };
    let source: IDropSource = Source {
        window: hwnd,
        hidden: Cell::new(false),
        cancel: Arc::clone(&context.cancel),
        channel: Arc::clone(&context.channel),
        id: context.id,
        last: Cell::new(u32::MAX),
    }
    .into();
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: a modal OLE drag on this STA thread, with objects that live through it.
    let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    tracing::info!(
        id = context.id,
        ?result,
        effect = effect.0,
        "drag from the client ended"
    );
    if result == DRAGDROP_S_DROP {
        effect
    } else {
        DROPEFFECT_NONE
    }
}

#[implement(IDropSource)]
struct Source {
    window: HWND,
    hidden: Cell<bool>,
    cancel: Arc<AtomicBool>,
    channel: Arc<Channel>,
    id: u64,
    last: Cell<u32>,
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if !self.hidden.replace(true) {
            // The drag has its own capture now; get out of the way of what's underneath.
            // SAFETY: hiding our own window.
            unsafe {
                let _ = ShowWindow(self.window, SW_HIDE);
            }
        }
        if escape.as_bool() || self.cancel.load(Ordering::SeqCst) {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            HRESULT(0)
        }
    }

    fn GiveFeedback(&self, effect: DROPEFFECT) -> HRESULT {
        if self.last.replace(effect.0) != effect.0 {
            self.channel.send(&Message::DragFeedback {
                id: self.id,
                effect: effect.0,
            });
        }
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
