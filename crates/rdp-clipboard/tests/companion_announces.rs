//! With a companion that announces what the remote copies, the clipboard channel's own
//! announcement of the same copy is ignored, and the companion's is offered here instead,
//! fetched from the companion when pasted.
//!
//! On a window station of its own (its own clipboard, not the user's); one test per file,
//! as the station is per process.
#![cfg(windows)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{ClipboardFormat, ClipboardFormatId};
use rdp_clipboard::{Companion, WinClipboard};
use rdp_companion_proto::ClipFormat;
use rdp_companion_proto::transfer::Inflow;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, CreateWindowStationW, DESKTOP_CONTROL_FLAGS, HDESK, SetProcessWindowStation, SetThreadDesktop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::w;

const CF_UNICODETEXT: u32 = 13;

#[derive(Debug)]
struct NoServer;

impl ClipboardMessageProxy for NoServer {
    fn send_clipboard_message(&self, _message: ClipboardMessage) {}
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// A companion that announces copies and serves text.
struct Announcing;

impl Companion for Announcing {
    fn offer_files(&self, _paths: Vec<PathBuf>) -> bool {
        false
    }

    fn remote_files(&self) -> Option<Arc<Inflow>> {
        None
    }

    fn offer_formats(&self, _formats: Vec<ClipFormat>) -> bool {
        false
    }

    fn remote_format(&self, format: ClipFormat) -> Option<Vec<u8>> {
        (format.id == CF_UNICODETEXT).then(|| utf16("from the companion"))
    }

    fn announces_copies(&self) -> bool {
        true
    }
}

fn isolate() -> HDESK {
    // SAFETY: creating and switching to a window station and desktop of our own, before
    // this process makes any window.
    unsafe {
        let station = CreateWindowStationW(None, 0, 0x37F /* WINSTA_ALL_ACCESS */, None).unwrap();
        SetProcessWindowStation(station).unwrap();
        let desktop = CreateDesktopW(
            w!("Default"),
            None,
            None,
            DESKTOP_CONTROL_FLAGS(0),
            0x01FF | 0x000F_0000, // DESKTOP_ALL_ACCESS
            None,
        )
        .unwrap();
        SetThreadDesktop(desktop).unwrap();
        desktop
    }
}

/// Empties the clipboard (the window station is shared by test runs, which leave things on
/// it), owned by a window that goes straight after.
fn empty_clipboard() {
    // SAFETY: a message-only window of a system class owns the emptied clipboard, briefly.
    unsafe {
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            None,
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
        .unwrap();
        OpenClipboard(Some(window)).unwrap();
        EmptyClipboard().unwrap();
        CloseClipboard().unwrap();
        DestroyWindow(window).unwrap();
    }
}

fn pump_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    let mut msg = MSG::default();
    while Instant::now() < deadline {
        // SAFETY: standard message pumping on this thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn text_offered() -> bool {
    // SAFETY: plain query.
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() }
}

/// Pastes text on another thread, as another program would, while this one serves.
fn paste(desktop: HDESK) -> Vec<u8> {
    let desktop = desktop.0 as isize;
    let paster = std::thread::spawn(move || {
        // SAFETY: joining the test's desktop; open, read, copy out, close.
        unsafe {
            SetThreadDesktop(HDESK(desktop as _)).unwrap();
            for _ in 0..50 {
                if OpenClipboard(None).is_ok() {
                    let bytes = GetClipboardData(CF_UNICODETEXT).ok().map(|handle| {
                        let handle = windows::Win32::Foundation::HGLOBAL(handle.0);
                        let ptr = GlobalLock(handle).cast::<u8>().cast_const();
                        let bytes = core::slice::from_raw_parts(ptr, GlobalSize(handle)).to_vec();
                        let _ = GlobalUnlock(handle);
                        bytes
                    });
                    let _ = CloseClipboard();
                    return bytes.unwrap_or_default();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("couldn't open the clipboard");
        }
    });
    assert!(pump_until(Duration::from_secs(10), || paster.is_finished()));
    paster.join().unwrap()
}

#[test]
fn the_companions_announcement_wins() {
    let desktop = isolate();
    empty_clipboard();
    let monitor = WinClipboard::new(NoServer, Some(Arc::new(Announcing))).unwrap();
    let mut backend = monitor.backend_factory().build_cliprdr_backend();

    // The clipboard channel announces a copy: ignored, the companion announces it.
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    assert!(
        !pump_until(Duration::from_millis(500), text_offered),
        "the channel's announcement was used"
    );

    // The companion's announcement is offered, and pasting reads it from the companion.
    monitor.announcer().remote_copy(
        vec![ClipFormat {
            id: CF_UNICODETEXT,
            name: None,
        }],
        false,
    );
    assert!(pump_until(Duration::from_secs(5), text_offered));
    let expected = utf16("from the companion");
    let pasted = paste(desktop);
    assert_eq!(pasted[..expected.len()], expected[..]);
    drop(backend);
    drop(monitor);
}
