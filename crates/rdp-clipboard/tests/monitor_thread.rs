//! A monitor on a thread of its own serves pastes there (the thread that made it never
//! pumps a message), holds copies back while its session isn't on screen (offering the
//! latest when it comes on screen), and gives up on a copy that's gone from the remote at
//! once instead of waiting on the clipboard channel.
//!
//! On a window station of its own (its own clipboard, not the user's); one test per file,
//! as the station is per process.
#![cfg(windows)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use rdp_clipboard::{ClipboardMonitor, Companion, RemoteFormat};
use rdp_companion_proto::ClipFormat;
use rdp_companion_proto::transfer::Inflow;
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, CreateWindowStationW, DESKTOP_CONTROL_FLAGS, HDESK, SetProcessWindowStation, SetThreadDesktop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::w;

const CF_UNICODETEXT: u32 = 13;

/// What the monitor sent its session, by kind.
#[derive(Default)]
struct Sent {
    copies: usize,
    pastes: usize,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Sent>>);

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recorder")
    }
}

impl ClipboardMessageProxy for Recorder {
    fn send_clipboard_message(&self, message: ClipboardMessage) {
        let mut sent = self.0.lock().unwrap();
        match message {
            ClipboardMessage::SendInitiateCopy(_) => sent.copies += 1,
            ClipboardMessage::SendInitiatePaste(_) => sent.pastes += 1,
            _ => {}
        }
    }
}

impl Recorder {
    fn copies(&self) -> usize {
        self.0.lock().unwrap().copies
    }

    fn pastes(&self) -> usize {
        self.0.lock().unwrap().pastes
    }
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// Announces copies; serves text slowly (as a remote does), or reports it gone.
struct Remote {
    gone: AtomicBool,
}

impl Companion for Remote {
    fn offer_files(&self, _paths: Vec<PathBuf>) -> bool {
        false
    }

    fn remote_files(&self) -> Option<Arc<Inflow>> {
        None
    }

    // Copies made here go over the clipboard channel, where the recorder sees them.
    fn offer_formats(&self, _formats: Vec<ClipFormat>) -> bool {
        false
    }

    fn remote_format(&self, format: ClipFormat) -> RemoteFormat {
        if self.gone.load(Ordering::SeqCst) {
            return RemoteFormat::Gone;
        }
        if format.id != CF_UNICODETEXT {
            return RemoteFormat::UseChannel;
        }
        std::thread::sleep(Duration::from_millis(300));
        RemoteFormat::Data(utf16("from the remote"))
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

/// Puts `text` on the clipboard (or empties it), as a copy made in another program.
fn copy_here(text: Option<&str>) {
    // SAFETY: a message-only window of a system class owns the clipboard while it's set; the
    // global memory is handed to the clipboard, which owns it from then on.
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
        if let Some(text) = text {
            let bytes = utf16(text);
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).unwrap();
            let ptr = GlobalLock(memory).cast::<u8>();
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(memory);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))).unwrap();
        }
        CloseClipboard().unwrap();
        DestroyWindow(window).unwrap();
    }
}

/// Waits for `done`, without pumping this thread's messages.
fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn text_offered() -> bool {
    // SAFETY: plain query.
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() }
}

/// Pastes text on another thread, as another program would. Returns the bytes (empty if
/// none came) and how long it took.
fn paste(desktop: HDESK) -> (Vec<u8>, Duration) {
    let desktop = desktop.0 as isize;
    std::thread::spawn(move || {
        // SAFETY: joining the test's desktop; open, read, copy out, close.
        unsafe {
            SetThreadDesktop(HDESK(desktop as _)).unwrap();
            for _ in 0..100 {
                if OpenClipboard(None).is_ok() {
                    let started = Instant::now();
                    let bytes = GetClipboardData(CF_UNICODETEXT).ok().map(|handle| {
                        let handle = HGLOBAL(handle.0);
                        let ptr = GlobalLock(handle).cast::<u8>().cast_const();
                        let bytes = core::slice::from_raw_parts(ptr, GlobalSize(handle)).to_vec();
                        let _ = GlobalUnlock(handle);
                        bytes
                    });
                    let took = started.elapsed();
                    let _ = CloseClipboard();
                    return (bytes.unwrap_or_default(), took);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("couldn't open the clipboard");
        }
    })
    .join()
    .unwrap()
}

#[test]
fn the_monitor_thread_serves_pastes_defers_copies_and_gives_up_on_gone_ones() {
    let desktop = isolate();
    copy_here(None);
    let sent = Recorder::default();
    let remote = Arc::new(Remote {
        gone: AtomicBool::new(false),
    });
    let station_desktop = desktop.0 as isize;
    let (monitor, factory) = ClipboardMonitor::spawn_with(
        "clipboard-test",
        // SAFETY: the monitor's thread joins the test's desktop, before it makes its window.
        move || unsafe { SetThreadDesktop(HDESK(station_desktop as _)).unwrap() },
        sent.clone(),
        Some(Arc::clone(&remote) as Arc<dyn Companion>),
        false,
    )
    .unwrap();
    let mut backend = factory.build_cliprdr_backend();

    // Off screen, the channel's setup is still answered with a format list…
    backend.on_request_format_list();
    assert!(
        wait_for(Duration::from_secs(5), || sent.copies() == 1),
        "setup unanswered"
    );
    backend.on_ready();

    // …but a copy made here waits.
    copy_here(Some("made here"));
    assert!(
        !wait_for(Duration::from_millis(600), || sent.copies() > 1),
        "offered while off screen"
    );

    // On screen, the latest copy is offered, and copies from then on go at once.
    monitor.set_focused(true);
    assert!(
        wait_for(Duration::from_secs(5), || sent.copies() == 2),
        "not offered on screen"
    );
    copy_here(Some("made here again"));
    assert!(
        wait_for(Duration::from_secs(5), || sent.copies() == 3),
        "not offered at once"
    );

    // A copy made on the remote is offered here and served from the monitor's own thread:
    // this thread never pumps a message.
    monitor.announcer().remote_copy(
        vec![ClipFormat {
            id: CF_UNICODETEXT,
            name: None,
        }],
        false,
    );
    assert!(
        wait_for(Duration::from_secs(5), text_offered),
        "remote copy not offered"
    );
    let (pasted, _) = paste(desktop);
    let expected = utf16("from the remote");
    assert_eq!(pasted[..expected.len()], expected[..]);

    // A copy that's gone from the remote: the paste gets nothing at once, without asking the
    // clipboard channel (which would wait for nothing).
    remote.gone.store(true, Ordering::SeqCst);
    monitor.announcer().remote_copy(
        vec![ClipFormat {
            id: CF_UNICODETEXT,
            name: None,
        }],
        false,
    );
    assert!(
        wait_for(Duration::from_secs(5), text_offered),
        "second remote copy not offered"
    );
    let (pasted, took) = paste(desktop);
    assert!(pasted.is_empty(), "a gone copy pasted something");
    assert!(took < Duration::from_secs(2), "a gone copy took {took:?}");
    assert_eq!(sent.pastes(), 0, "asked the clipboard channel for a gone copy");

    drop(backend);
    drop(monitor);
}
