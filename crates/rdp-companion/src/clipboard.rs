//! The remote clipboard, for fast file copies.
//!
//! Files copied on the client are put here as an OLE data object whose streams pull
//! through the companion transfer as Explorer pastes them ([`rdp_vfiles`]). It carries
//! [`CLIPBOARD_MARKER`], so when the session's clipboard redirection mirrors the change
//! back, the client knows it's its own. Files copied here are read off the clipboard as
//! paths (`CF_HDROP`) when the client wants them.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use rdp_companion_proto::CLIPBOARD_MARKER;
use rdp_companion_proto::transfer::Inflow;
use rdp_vfiles::{InflowFiles, Options, data_object};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows::Win32::System::Ole::{OleInitialize, OleSetClipboard, OleUninitialize};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowThreadProcessId, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    TranslateMessage, WM_APP, WM_USER,
};
use windows::core::HSTRING;

const CF_HDROP: u32 = 15;

enum Command {
    Offer(Arc<Inflow>),
    ClearIfOurs,
}

/// The thread that owns the clipboard objects (OLE needs an STA with a message loop).
pub(crate) struct Clipboard {
    thread_id: u32,
    commands: mpsc::Sender<Command>,
}

impl Clipboard {
    pub(crate) fn spawn() -> Option<Self> {
        let (commands, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || run(&rx, &ready_tx))
            .ok()?;
        let thread_id = ready_rx.recv_timeout(Duration::from_secs(5)).ok()??;
        Some(Self { thread_id, commands })
    }

    /// Puts a transfer's files on the clipboard.
    pub(crate) fn offer(&self, inflow: Arc<Inflow>) {
        self.send(Command::Offer(inflow));
    }

    /// The client is gone: take our files off the clipboard (they can't be read any
    /// more), unless something else was copied since.
    pub(crate) fn clear_if_ours(&self) {
        self.send(Command::ClearIfOurs);
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // SAFETY: plain thread message post; the queue exists before `spawn` returns.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_APP, Default::default(), Default::default()) };
        }
    }
}

fn run(commands: &mpsc::Receiver<Command>, ready: &mpsc::SyncSender<Option<u32>>) {
    // SAFETY: initializes OLE for this thread; paired with `OleUninitialize` below.
    if let Err(error) = unsafe { OleInitialize(None) } {
        tracing::warn!(%error, "OLE unavailable; fast clipboard copies are off");
        let _ = ready.send(None);
        return;
    }
    let mut msg = MSG::default();
    // SAFETY: creates this thread's message queue before anyone posts to it.
    let thread_id = unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        GetCurrentThreadId()
    };
    let _ = ready.send(Some(thread_id));
    loop {
        // SAFETY: standard message loop on this thread.
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            break;
        }
        if msg.hwnd.is_invalid() && msg.message == WM_APP {
            while let Ok(command) = commands.try_recv() {
                match command {
                    Command::Offer(inflow) => {
                        let files = inflow.entries().len();
                        let object = data_object(
                            Arc::new(InflowFiles::new(inflow)),
                            Options {
                                marker: Some(CLIPBOARD_MARKER),
                            },
                        );
                        // SAFETY: called on this STA thread, which pumps messages for OLE.
                        match unsafe { OleSetClipboard(&object) } {
                            Ok(()) => tracing::info!(files, "files from the client are on the clipboard"),
                            Err(error) => tracing::warn!(%error, "couldn't put the client's files on the clipboard"),
                        }
                    }
                    Command::ClearIfOurs => {
                        if owned_by(thread_id) {
                            // SAFETY: OLE call on the thread that set the clipboard.
                            let _ = unsafe { OleSetClipboard(None) };
                        }
                    }
                }
            }
            continue;
        }
        // SAFETY: dispatching a message retrieved by `GetMessageW` on this thread.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: paired with `OleInitialize` above.
    unsafe { OleUninitialize() };
}

/// Whether the clipboard's owner is a window on `thread` (OLE's clipboard window).
fn owned_by(thread: u32) -> bool {
    // SAFETY: plain queries; an invalid handle yields 0.
    unsafe { GetClipboardOwner().is_ok_and(|owner| GetWindowThreadProcessId(owner, None) == thread) }
}

/// The files copied here (Explorer's `CF_HDROP`), or `None` if the clipboard holds no
/// real files (text, virtual files) or holds our own.
pub(crate) fn copied_files() -> Option<Vec<PathBuf>> {
    // SAFETY: registering a format by name.
    let marker = unsafe { RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER)) };
    // Someone else may have it open for a moment.
    let mut opened = false;
    for _ in 0..10 {
        // SAFETY: opening the clipboard for this thread; closed below.
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !opened {
        tracing::warn!("the clipboard stayed busy");
        return None;
    }
    // SAFETY: the clipboard is open; the HDROP stays valid until it's closed.
    let paths = unsafe {
        if IsClipboardFormatAvailable(marker).is_ok() || IsClipboardFormatAvailable(CF_HDROP).is_err() {
            None
        } else {
            GetClipboardData(CF_HDROP).ok().map(|handle| drop_paths(handle))
        }
    };
    // SAFETY: closing what we opened above.
    let _ = unsafe { CloseClipboard() };
    paths.filter(|p| !p.is_empty())
}

/// The paths in a `CF_HDROP` handle.
///
/// # Safety
///
/// `handle` must be a `CF_HDROP` clipboard handle, valid for the duration of the call.
unsafe fn drop_paths(handle: HANDLE) -> Vec<PathBuf> {
    let hdrop = HDROP(handle.0);
    // SAFETY: counting, then reading each name into a buffer of the reported length.
    unsafe {
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        (0..count)
            .filter_map(|i| {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                (got > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..got.min(len)])))
            })
            .collect()
    }
}
