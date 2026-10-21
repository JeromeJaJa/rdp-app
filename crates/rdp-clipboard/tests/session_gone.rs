//! A clipboard monitor whose session has ended mustn't block the UI thread: a paste of
//! what the remote copied, and tearing the monitor down (Windows asks it to render
//! everything it still owes), used to wait 45 s per format for an answer that couldn't
//! come. This froze the window after the last tab closed.
//!
//! The test runs on a window station of its own, so it has its own clipboard and never
//! touches the user's. (One test per file: the station is per process.)
#![cfg(windows)]

use std::time::{Duration, Instant};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{ClipboardFormat, ClipboardFormatId};
use rdp_clipboard::WinClipboard;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
};
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, CreateWindowStationW, DESKTOP_CONTROL_FLAGS, HDESK, SetProcessWindowStation, SetThreadDesktop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::w;

const CF_UNICODETEXT: u32 = 13;

/// Sends nowhere: no server answers.
#[derive(Debug)]
struct NoServer;

impl ClipboardMessageProxy for NoServer {
    fn send_clipboard_message(&self, _message: ClipboardMessage) {}
}

/// Moves the process to a new window station (its own clipboard), and this thread to a
/// desktop there.
fn isolate() -> HDESK {
    // SAFETY: creating and switching to a window station and desktop of our own, before
    // this process makes any window.
    unsafe {
        // Unnamed: the system names it after the logon session.
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

/// Runs this thread's messages until `done`, for at most `limit`.
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

/// A monitor holding a remote copy of text (delay-rendered: fetched when pasted).
fn monitor_with_remote_copy() -> (WinClipboard, Box<dyn ironrdp_cliprdr::backend::CliprdrBackend>) {
    let monitor = WinClipboard::new(NoServer, None).unwrap();
    let mut backend = monitor.backend_factory().build_cliprdr_backend();
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    // SAFETY: plain query.
    let offered = pump_until(Duration::from_secs(5), || unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok()
    });
    assert!(offered, "the remote's copy should be on the clipboard");
    (monitor, backend)
}

/// Pastes text on another thread (as another program would) while this one serves the
/// clipboard; returns how long it took.
fn paste_elapsed(desktop: HDESK) -> Duration {
    let started = Instant::now();
    let desktop = desktop.0 as isize;
    let paster = std::thread::spawn(move || {
        // SAFETY: joining the test's desktop, then open, read (the owner renders on its own
        // thread), close.
        unsafe {
            SetThreadDesktop(HDESK(desktop as _)).unwrap();
            for _ in 0..50 {
                if OpenClipboard(None).is_ok() {
                    let _ = GetClipboardData(CF_UNICODETEXT);
                    let _ = CloseClipboard();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("couldn't open the clipboard");
        }
    });
    assert!(pump_until(Duration::from_secs(60), || paster.is_finished()));
    paster.join().unwrap();
    started.elapsed()
}

#[test]
fn a_monitor_whose_session_ended_never_blocks() {
    let desktop = isolate();
    empty_clipboard();

    // The session's connection is gone: a paste gives up at once.
    let (monitor, backend) = monitor_with_remote_copy();
    drop(backend);
    let paste = paste_elapsed(desktop);
    assert!(paste < Duration::from_secs(2), "paste took {paste:?}");
    // And tearing the monitor down doesn't wait to render what it owed.
    let started = Instant::now();
    drop(monitor);
    let teardown = started.elapsed();
    assert!(teardown < Duration::from_secs(2), "teardown took {teardown:?}");

    // Torn down while its backend is still there (the session ending): no wait either.
    let (monitor, backend) = monitor_with_remote_copy();
    let started = Instant::now();
    drop(monitor);
    let teardown = started.elapsed();
    assert!(teardown < Duration::from_secs(2), "teardown took {teardown:?}");
    drop(backend);
}
