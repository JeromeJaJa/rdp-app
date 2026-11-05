//! The remote clipboard, for fast copies.
//!
//! Files copied on the client are put here as an OLE data object whose streams pull
//! through the companion transfer as Explorer pastes them ([`rdp_vfiles`]). Anything else
//! copied there (text, HTML, images…) is put here format by format, each rendered when
//! something pastes it by fetching it from the client (Windows' delayed rendering). Both
//! carry [`CLIPBOARD_MARKER`], so when the session's clipboard redirection mirrors the
//! change back, the client knows it's its own.
//!
//! The other way, the clipboard is watched: each copy made here is announced to the client
//! (its formats, and whether it holds files), and read when the client pastes it: files
//! as paths (`CF_HDROP`), anything else as bytes ([`read_format`]).

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use rdp_companion_proto::transfer::{CHUNK, Inflow};
use rdp_companion_proto::{CLIPBOARD_MARKER, ClipFormat, Message};
use rdp_vfiles::{InflowFiles, Options, data_object};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{OleInitialize, OleSetClipboard, OleUninitialize};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowThreadProcessId, HWND_MESSAGE, MSG,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_CLIPBOARDUPDATE, WM_DESTROYCLIPBOARD, WM_RENDERFORMAT, WM_USER, WNDCLASSW,
};
use windows::core::{HSTRING, w};

const CF_HDROP: u32 = 15;
/// How long a paste waits for more of the client's data before giving up.
const RENDER_STALL: Duration = Duration::from_secs(30);

enum Command {
    Offer(Arc<Inflow>),
    OfferFormats(Arc<Inflow>, Vec<ClipFormat>),
    ClearIfOurs,
}

/// Where announcements of copies made here go (the attached client), if anywhere.
pub(crate) type Sink = Arc<dyn Fn(Message) + Send + Sync>;

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// The thread that owns the clipboard objects (OLE needs an STA with a message loop).
pub(crate) struct Clipboard {
    thread_id: u32,
    commands: mpsc::Sender<Command>,
    /// The clipboard is watched: copies made here are announced.
    watching: bool,
}

impl Clipboard {
    pub(crate) fn spawn() -> Option<Self> {
        let (commands, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || run(&rx, &ready_tx))
            .ok()?;
        let (thread_id, watching) = ready_rx.recv_timeout(Duration::from_secs(5)).ok()??;
        Some(Self {
            thread_id,
            commands,
            watching,
        })
    }

    /// Whether copies made here are announced ([`Clipboard::set_sink`]).
    pub(crate) fn watching(&self) -> bool {
        self.watching
    }

    /// Where to announce copies made here (the attached client), or nowhere.
    pub(crate) fn set_sink(&self, sink: Option<Sink>) {
        *SINK.lock().unwrap_or_else(PoisonError::into_inner) = sink;
    }

    /// Puts a transfer's files on the clipboard.
    pub(crate) fn offer(&self, inflow: Arc<Inflow>) {
        self.send(Command::Offer(inflow));
    }

    /// Puts the client's copy on the clipboard: `formats`, entry by entry of `inflow`.
    pub(crate) fn offer_formats(&self, inflow: Arc<Inflow>, formats: Vec<ClipFormat>) {
        self.send(Command::OfferFormats(inflow, formats));
    }

    /// The client is gone: take its copy off the clipboard (it can't be read any more),
    /// unless something else was copied since.
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

/// The client's copy this thread's window has on the clipboard.
struct Offered {
    inflow: Arc<Inflow>,
    /// Clipboard format id of each entry (0: not available here).
    formats: Vec<u32>,
}

thread_local! {
    static OFFERED: RefCell<Option<Offered>> = const { RefCell::new(None) };
}

fn run(commands: &mpsc::Receiver<Command>, ready: &mpsc::SyncSender<Option<(u32, bool)>>) {
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
    let window = create_window();
    if window.is_none() {
        tracing::warn!("no clipboard window; copies other than files go the usual way");
    }
    // SAFETY: registering our own window for clipboard change notices.
    let watching = window.is_some_and(|w| unsafe { AddClipboardFormatListener(w) }.is_ok());
    let _ = ready.send(Some((thread_id, watching)));
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
                    Command::OfferFormats(inflow, formats) => match window {
                        Some(window) => put_formats(window, inflow, &formats),
                        None => inflow.finish(Some("the remote clipboard isn't available".into())),
                    },
                    Command::ClearIfOurs => {
                        if window.is_some_and(owns_clipboard) {
                            if open(Some(window.expect("checked above"))) {
                                // SAFETY: open above; WM_DESTROYCLIPBOARD releases the copy.
                                unsafe {
                                    let _ = EmptyClipboard();
                                    let _ = CloseClipboard();
                                }
                            }
                        } else if owned_by(thread_id) {
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

/// A message-only window to own the clipboard while it holds the client's copy.
fn create_window() -> Option<HWND> {
    // SAFETY: registering a class with a static procedure, and creating a message-only
    // window of it on this thread.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = w!("RdpAppCompanionClipboard");
        let wc = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: class,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            None,
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .ok()
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Something is pasting one of the client's formats: fetch it now.
        WM_RENDERFORMAT => {
            let format = wparam.0 as u32;
            let offered = OFFERED.with_borrow(|o| {
                o.as_ref()
                    .and_then(|o| Some((Arc::clone(&o.inflow), o.formats.iter().position(|&f| f == format)?)))
            });
            if let Some((inflow, index)) = offered {
                render(&inflow, index as u32, format);
            }
            LRESULT(0)
        }
        // The clipboard changed: if something here copied, tell the client.
        WM_CLIPBOARDUPDATE => {
            announce(hwnd);
            LRESULT(0)
        }
        // Something else was copied (or the companion is leaving): the client's copy is done.
        WM_DESTROYCLIPBOARD => {
            if let Some(offered) = OFFERED.take() {
                offered.inflow.finish(None);
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else (WM_RENDERALLFORMATS included: the
        // copy can't outlive the companion, so its formats just go).
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Puts the client's copy on the clipboard, each format rendered when it's pasted.
fn put_formats(window: HWND, inflow: Arc<Inflow>, formats: &[ClipFormat]) {
    let ids: Vec<u32> = formats.iter().map(clipboard_id).collect();
    if !open(Some(window)) {
        tracing::warn!("the clipboard stayed busy; the client's copy isn't here");
        inflow.finish(Some("the remote clipboard stayed busy".into()));
        return;
    }
    // SAFETY: the clipboard is open by this window. Emptying it makes the window its owner
    // (WM_DESTROYCLIPBOARD releases a copy it held before); null data defers rendering.
    unsafe {
        let _ = EmptyClipboard();
        for &id in ids.iter().filter(|&&id| id != 0) {
            let _ = SetClipboardData(id, None);
        }
        let marker = RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER));
        if let Some(handle) = global(&[1]) {
            let _ = SetClipboardData(marker, Some(HANDLE(handle.0)));
        }
        let _ = CloseClipboard();
    }
    tracing::info!(formats = ids.len(), "the client's copy is on the clipboard");
    OFFERED.set(Some(Offered { inflow, formats: ids }));
}

/// Announces a copy made here to the client, unless it's the client's own (offered by this
/// thread's windows, or carrying the marker).
fn announce(window: HWND) {
    let sink = SINK.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let Some(sink) = sink else { return };
    // SAFETY: plain query.
    if owned_by(unsafe { GetCurrentThreadId() }) || owns_clipboard(window) {
        return;
    }
    if let Some((formats, files)) = copied_formats()
        && (files || !formats.is_empty())
    {
        tracing::debug!(formats = formats.len(), files, "announcing a copy");
        sink(Message::RemoteCopy { formats, files });
    }
}

/// What the clipboard holds: its plain-memory formats, and whether it has files. `None`
/// if it can't be opened, or holds the client's own copy.
fn copied_formats() -> Option<(Vec<ClipFormat>, bool)> {
    // SAFETY: registering a format by name.
    let marker = unsafe { RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER)) };
    if !open(None) {
        return None;
    }
    let mut formats = Vec::new();
    let mut files = false;
    let mut own = false;
    // SAFETY: the clipboard is open; enumerating it and reading format names into a
    // buffer of the size passed.
    unsafe {
        let mut id = EnumClipboardFormats(0);
        let mut name = [0u16; 256];
        while id != 0 {
            own |= id == marker;
            files |= id == CF_HDROP;
            let format = ClipFormat {
                id,
                name: (id >= ClipFormat::REGISTERED)
                    .then(|| GetClipboardFormatNameW(id, &mut name))
                    .filter(|&n| n > 0)
                    .map(|n| String::from_utf16_lossy(&name[..n as usize])),
            };
            if format.is_memory() {
                formats.push(format);
            }
            id = EnumClipboardFormats(id);
        }
        let _ = CloseClipboard();
    }
    (!own).then_some((formats, files))
}

/// Fetches entry `index` of the client's copy and hands it to the clipboard as `format`.
fn render(inflow: &Inflow, index: u32, format: u32) {
    let started = Instant::now();
    let mut data = Vec::new();
    let mut buf = vec![0u8; CHUNK * 4];
    loop {
        match inflow.read(index, data.len() as u64, &mut buf, RENDER_STALL) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(error) => {
                tracing::warn!(%error, format, "couldn't get the client's copy");
                return;
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    tracing::info!(
        format,
        bytes = data.len(),
        ms = (secs * 1000.0) as u64,
        mib_per_s = format!("{:.2}", data.len() as f64 / 1_048_576.0 / secs.max(0.001)),
        so_far = %inflow.throughput(),
        "pasted the client's copy"
    );
    if let Some(handle) = global(&data) {
        // SAFETY: rendering inside WM_RENDERFORMAT; the clipboard owns the handle on success.
        if unsafe { SetClipboardData(format, Some(HANDLE(handle.0))) }.is_err() {
            // SAFETY: not taken by the clipboard, so still ours.
            let _ = unsafe { GlobalFree(Some(handle)) };
        }
    }
}

/// `data` in movable global memory, for the clipboard.
fn global(data: &[u8]) -> Option<HGLOBAL> {
    // SAFETY: allocating, then copying `data` into the locked block of its size.
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)).ok()?;
        let ptr = GlobalLock(handle).cast::<u8>();
        if ptr.is_null() {
            let _ = GlobalFree(Some(handle));
            return None;
        }
        core::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        let _ = GlobalUnlock(handle);
        Some(handle)
    }
}

/// This machine's id for a format (registered ones by name); 0 if it has none.
fn clipboard_id(format: &ClipFormat) -> u32 {
    if !format.is_memory() {
        return 0;
    }
    if format.id < ClipFormat::REGISTERED {
        return format.id;
    }
    // SAFETY: registering (or looking up) a format by name.
    format
        .name
        .as_deref()
        .map_or(0, |name| unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) })
}

/// Opens the clipboard (for `window`, or this thread), waiting a little if another
/// program has it open.
fn open(window: Option<HWND>) -> bool {
    for _ in 0..25 {
        // SAFETY: opening the clipboard; the caller closes it.
        if unsafe { OpenClipboard(window) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Whether `window` owns the clipboard.
fn owns_clipboard(window: HWND) -> bool {
    // SAFETY: plain query.
    unsafe { GetClipboardOwner() }.is_ok_and(|owner| owner == window)
}

/// Whether the clipboard's owner is a window on `thread` (OLE's clipboard window).
fn owned_by(thread: u32) -> bool {
    // SAFETY: plain queries; an invalid handle yields 0.
    unsafe { GetClipboardOwner().is_ok_and(|owner| GetWindowThreadProcessId(owner, None) == thread) }
}

/// The bytes of one format of what was copied here, for the client to paste. Fails for
/// the client's own copy (it has the original) and formats that aren't plain memory.
pub(crate) fn read_format(format: &ClipFormat) -> Result<Vec<u8>, String> {
    let id = clipboard_id(format);
    if id == 0 {
        return Err("that format can't be copied as bytes".into());
    }
    // SAFETY: registering a format by name.
    let marker = unsafe { RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER)) };
    if !open(None) {
        return Err("the clipboard stayed busy".into());
    }
    // SAFETY: the clipboard is open; the handle stays valid until it closes, and the
    // locked bytes are copied out before unlocking.
    let data = unsafe {
        if IsClipboardFormatAvailable(marker).is_ok() {
            Err(rdp_companion_proto::OWN_COPY.to_owned())
        } else {
            GetClipboardData(id)
                .ok()
                .and_then(|handle| {
                    let handle = HGLOBAL(handle.0);
                    let ptr = GlobalLock(handle).cast::<u8>().cast_const();
                    if ptr.is_null() {
                        return None;
                    }
                    let bytes = core::slice::from_raw_parts(ptr, GlobalSize(handle)).to_vec();
                    let _ = GlobalUnlock(handle);
                    Some(bytes)
                })
                .ok_or_else(|| "the clipboard no longer has that format".to_owned())
        }
    };
    // SAFETY: closing what was opened above.
    let _ = unsafe { CloseClipboard() };
    data
}

/// The files copied here (Explorer's `CF_HDROP`), or `None` if the clipboard holds no
/// real files (text, virtual files) or holds our own.
pub(crate) fn copied_files() -> Option<Vec<PathBuf>> {
    // SAFETY: registering a format by name.
    let marker = unsafe { RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER)) };
    if !open(None) {
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

#[cfg(test)]
mod tests {
    use rdp_companion_proto::Entry;
    use rdp_companion_proto::transfer::{Outbox, Produced, Pump, Source};
    use rdp_companion_proto::{ClipFormat, Message};
    use windows::Win32::System::StationsAndDesktops::{
        CreateDesktopW, CreateWindowStationW, DESKTOP_CONTROL_FLAGS, HDESK, SetProcessWindowStation, SetThreadDesktop,
    };

    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

    use super::*;

    const CF_UNICODETEXT: u32 = 13;

    /// Runs test `name` again in a child process (this test binary, filtered to it) and
    /// checks it passed there; returns false in the child, which runs the test itself. The
    /// child moves to a window station of its own: its own clipboard, not the user's.
    fn in_child(name: &str) -> bool {
        if std::env::var_os("RDP_COMPANION_CLIPBOARD_CHILD").is_some() {
            return false;
        }
        // One child at a time: their window stations are one (it's per logon session), so
        // two at once would share a clipboard.
        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env("RDP_COMPANION_CLIPBOARD_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    /// Moves this process to a new window station, and this thread to a desktop there.
    fn isolate() -> HDESK {
        // SAFETY: creating and switching to a window station and desktop of our own, before
        // this process makes any window.
        unsafe {
            // Unnamed: the system names it after the logon session (as the Chromium sandbox
            // does), so runs share it and a test starts by emptying its clipboard.
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

    /// The clipboard thread, on `desktop`.
    fn spawn_on(desktop: HDESK) -> Clipboard {
        let (commands, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let desktop = desktop.0 as isize;
        std::thread::spawn(move || {
            // SAFETY: a desktop handle that stays open for the test.
            unsafe { SetThreadDesktop(HDESK(desktop as _)).unwrap() };
            run(&rx, &ready_tx);
        });
        let (thread_id, watching) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        Clipboard {
            thread_id,
            commands,
            watching,
        }
    }

    /// The client's side of a copy: a pump serving `contents` (an entry each), joined to
    /// an inflow the way the channel joins them.
    fn client(id: u64, contents: Vec<Vec<u8>>) -> Arc<Inflow> {
        let entries: Arc<[Entry]> = contents
            .iter()
            .map(|c| Entry {
                path: "format".into(),
                dir: false,
                size: c.len() as u64,
                modified: 0,
            })
            .collect();
        let (to_pump, from_sink) = mpsc::channel::<Message>();
        let outbox: Outbox = Arc::new(move |m| {
            let _ = to_pump.send(m);
        });
        let inflow = Arc::new(Inflow::per_entry(id, Arc::clone(&entries), outbox));
        let sink = Arc::clone(&inflow);
        let pump = Arc::new(
            Pump::spawn(
                "test-client",
                move |m| {
                    sink.receive(m);
                    true
                },
                |_| {},
            )
            .unwrap(),
        );
        pump.start(Source::new(
            id,
            entries,
            Box::new(Produced::new(move |i: u32| Ok(contents[i as usize].clone()))),
        ));
        std::thread::spawn(move || {
            for message in from_sink {
                match message {
                    Message::Close { id, error } => pump.closed(id, error),
                    message => {
                        pump.receive(&message);
                    }
                }
            }
        });
        inflow
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    /// What pasting `format` here gets (as any program would).
    fn paste(format: u32) -> Option<Vec<u8>> {
        assert!(open(None));
        // SAFETY: the clipboard is open; the bytes are copied out before it closes.
        let data = unsafe {
            GetClipboardData(format).ok().map(|handle| {
                let handle = HGLOBAL(handle.0);
                let ptr = GlobalLock(handle).cast::<u8>().cast_const();
                let bytes = core::slice::from_raw_parts(ptr, GlobalSize(handle)).to_vec();
                let _ = GlobalUnlock(handle);
                bytes
            })
        };
        // SAFETY: closing what was opened above.
        let _ = unsafe { CloseClipboard() };
        data
    }

    /// Copies `text` here (or empties the clipboard), as another program would. Its window
    /// goes straight after: this thread doesn't pump messages, so a window of its owning
    /// the clipboard would hold up the next program to empty it.
    fn copy_text(text: Option<&str>) {
        // SAFETY: a message-only window of a system class, to own what's copied.
        let window = unsafe {
            CreateWindowExW(
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
            .unwrap()
        };
        assert!(open(Some(window)));
        // SAFETY: the clipboard is open by `window`; it owns the handle once set.
        unsafe {
            EmptyClipboard().unwrap();
            if let Some(text) = text {
                let handle = global(&utf16(text)).unwrap();
                SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0))).unwrap();
            }
            CloseClipboard().unwrap();
            DestroyWindow(window).unwrap();
        }
    }

    #[test]
    fn the_clients_copy_is_fetched_when_pasted() {
        if in_child("clipboard::tests::the_clients_copy_is_fetched_when_pasted") {
            return;
        }
        let desktop = isolate();
        copy_text(None);
        let clipboard = spawn_on(desktop);
        let text = utf16(&"a line of copied text\r\n".repeat(20_000));
        let html = b"Version:0.9\r\nStartHTML:0\r\n<html><body>copied</body></html>".to_vec();
        let inflow = client(2, vec![text.clone(), html.clone()]);
        clipboard.offer_formats(
            Arc::clone(&inflow),
            vec![
                ClipFormat {
                    id: CF_UNICODETEXT,
                    name: None,
                },
                ClipFormat {
                    id: 0xC0FF, // the client's id; this side goes by the name
                    name: Some("HTML Format".into()),
                },
            ],
        );
        // SAFETY: registering a format by name, and plain queries.
        let marker = unsafe { RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER)) };
        wait_for("the copy", || unsafe { IsClipboardFormatAvailable(marker).is_ok() });

        // Pasting fetches each format from the client (the clipboard may round sizes up).
        let pasted = paste(CF_UNICODETEXT).unwrap();
        assert_eq!(pasted[..text.len()], text[..]);
        // SAFETY: registering a format by name.
        let html_id = unsafe { RegisterClipboardFormatW(w!("HTML Format")) };
        assert_eq!(paste(html_id).unwrap()[..html.len()], html[..]);
        // The text was compressed on the way.
        let moved = inflow.throughput();
        assert!(moved.wire * 4 < moved.bytes, "{moved}");
        // The client's own copy isn't served back to it.
        assert!(
            read_format(&ClipFormat {
                id: CF_UNICODETEXT,
                name: None
            })
            .is_err()
        );

        // Copying something else lets go of the client's copy...
        copy_text(Some("copied here"));
        wait_for("the client's copy to be released", || inflow.is_closed());
        // ...and what was copied here is what the client gets.
        let expected = utf16("copied here");
        let served = read_format(&ClipFormat {
            id: CF_UNICODETEXT,
            name: None,
        })
        .unwrap();
        assert_eq!(served[..expected.len()], expected[..]);
        drop(clipboard);
    }

    #[test]
    fn copies_made_here_are_announced() {
        if in_child("clipboard::tests::copies_made_here_are_announced") {
            return;
        }
        let desktop = isolate();
        copy_text(None);
        let clipboard = spawn_on(desktop);
        assert!(clipboard.watching());
        let heard = Arc::new(Mutex::new(Vec::<Message>::new()));
        let into = Arc::clone(&heard);
        clipboard.set_sink(Some(Arc::new(move |m| into.lock().unwrap().push(m))));

        copy_text(Some("copied on the remote"));
        wait_for("the announcement", || !heard.lock().unwrap().is_empty());
        let first = heard.lock().unwrap().remove(0);
        let Message::RemoteCopy { formats, files } = first else {
            panic!("{first:?}")
        };
        assert!(!files);
        assert!(formats.iter().any(|f| f.id == CF_UNICODETEXT), "{formats:?}");

        // The client's own copy (put here by the companion) isn't announced back.
        let inflow = client(4, vec![utf16("from the client")]);
        clipboard.offer_formats(
            inflow,
            vec![ClipFormat {
                id: CF_UNICODETEXT,
                name: None,
            }],
        );
        // SAFETY: registering a format by name, and plain queries.
        let marker = unsafe { RegisterClipboardFormatW(&HSTRING::from(CLIPBOARD_MARKER)) };
        wait_for("the client's copy", || unsafe {
            IsClipboardFormatAvailable(marker).is_ok()
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(heard.lock().unwrap().is_empty(), "{:?}", heard.lock().unwrap());
        clipboard.set_sink(None);
    }
}
