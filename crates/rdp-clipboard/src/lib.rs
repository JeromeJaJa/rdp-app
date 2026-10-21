//! Windows clipboard redirection backend for CLIPRDR.
//!
//! Adapted from IronRDP's `ironrdp-cliprdr-native` Windows backend (MIT/Apache-2.0,
//! Copyright Devolutions; see LICENSE-MIT / LICENSE-APACHE). Changes for this app:
//!
//! - local clipboard changes are forwarded to the server whenever this monitor did not
//!   make them itself (upstream only did so while its never-activated hidden window was
//!   "inactive", so local copies never reached the remote);
//! - files copied locally (CF_HDROP) are offered to the server, which then pulls their
//!   contents (MS-RDPECLIP file streams);
//! - files copied on the remote are offered locally as an OLE data object whose streams
//!   pull their contents from the server as they are read;
//! - file requests, file data and paste replies go straight between the session thread
//!   and the threads that need them, not through the UI thread's event queue (which is
//!   also unbounded now, so a busy UI thread never stalls the session's network loop);
//! - file transfer capabilities are advertised;
//! - dropping the monitor destroys its window and state (upstream leaked both);
//! - with a companion attached, copies go through it instead (see [`Companion`]).
#![cfg(windows)]
#![warn(unsafe_op_in_unsafe_fn)]

mod clipboard_data_ref;
mod clipboard_impl;
mod cliprdr_backend;
mod files;
mod os_clipboard;
mod remote_files;
mod remote_format_registry;
mod utils;

use std::sync::Arc;
use std::sync::mpsc as mpsc_sync;

use ironrdp_cliprdr::backend::{ClipboardMessageProxy, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FormatDataRequest, FormatDataResponse,
};
use tracing::error;
pub use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::{E_ACCESSDENIED, FALSE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::UI::Shell::SetWindowSubclass;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExA, DefWindowProcA, DestroyWindow, GetClassInfoA, RegisterClassA, WINDOW_EX_STYLE,
    WM_USER, WNDCLASSA, WS_POPUP,
};
use windows::core::{Error, s};

use self::clipboard_impl::{WinClipboardImpl, clipboard_subproc};
use self::cliprdr_backend::WinCliprdrBackend;
use self::files::FileServer;
use self::remote_files::{Inbox, SharedProxy};

const WM_CLIPRDR_BACKEND_EVENT: u32 = WM_USER;

pub type WinCliprdrResult<T> = Result<T, WinCliprdrError>;

#[derive(Debug)]
pub enum WinCliprdrError {
    AddClipboardFormatListener,
    FormatsEnumeration,
    ClipboardAccessDenied,
    ClipboardOpen,
    ClipboardEmpty,
    Utf16Conversion,
    ClipboardData,
    SetClipboardData,
    WindowSubclass,
    Alloc,
    DataReceiveTimeout,
    RenderFormat,
    WinAPI(Error),
}

impl core::fmt::Display for WinCliprdrError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WinCliprdrError::AddClipboardFormatListener => write!(f, "failed to register clipboard format listener"),
            WinCliprdrError::FormatsEnumeration => {
                write!(f, "failed to enumerate formats available in the current clipboard")
            }
            WinCliprdrError::ClipboardAccessDenied => write!(f, "clipboard is busy or denied"),
            WinCliprdrError::ClipboardOpen => write!(f, "failed to open the clipboard"),
            WinCliprdrError::ClipboardEmpty => write!(f, "failed to empty the clipboard"),
            WinCliprdrError::Utf16Conversion => write!(f, "failed to convert UTF-16 string to UTF-8"),
            WinCliprdrError::ClipboardData => write!(f, "failed to get current clipboard data"),
            WinCliprdrError::SetClipboardData => write!(f, "failed to set clipboard data"),
            WinCliprdrError::WindowSubclass => write!(f, "failed to subclass window"),
            WinCliprdrError::Alloc => write!(f, "failed to allocate global memory"),
            WinCliprdrError::DataReceiveTimeout => write!(f, "failed to receive data from remote clipboard"),
            WinCliprdrError::RenderFormat => write!(f, "failed to render clipboard format"),
            WinCliprdrError::WinAPI(_error) => write!(f, "WinAPI error"),
        }
    }
}

impl core::error::Error for WinCliprdrError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            WinCliprdrError::AddClipboardFormatListener => None,
            WinCliprdrError::FormatsEnumeration => None,
            WinCliprdrError::ClipboardAccessDenied => None,
            WinCliprdrError::ClipboardOpen => None,
            WinCliprdrError::ClipboardEmpty => None,
            WinCliprdrError::Utf16Conversion => None,
            WinCliprdrError::ClipboardData => None,
            WinCliprdrError::SetClipboardData => None,
            WinCliprdrError::WindowSubclass => None,
            WinCliprdrError::Alloc => None,
            WinCliprdrError::DataReceiveTimeout => None,
            WinCliprdrError::RenderFormat => None,
            WinCliprdrError::WinAPI(error) => Some(error),
        }
    }
}

impl From<Error> for WinCliprdrError {
    fn from(err: Error) -> Self {
        if err.code() == E_ACCESSDENIED {
            WinCliprdrError::ClipboardAccessDenied
        } else {
            WinCliprdrError::WinAPI(err)
        }
    }
}

/// Copies faster through the session's companion, when one is attached.
///
/// The clipboard channel moves file contents one request at a time, and anything else in
/// one uncompressed reply; the companion streams both, compressed. Whatever the companion
/// can't serve still goes over the clipboard channel.
pub trait Companion: Send + Sync {
    /// Files were copied on this PC: put them on the remote clipboard through the
    /// companion. Returns false (use the clipboard channel) if none is attached. Must not
    /// block: list the files elsewhere.
    fn offer_files(&self, paths: Vec<std::path::PathBuf>) -> bool;

    /// The remote copied files: ask the companion for them. `None` if no companion is
    /// attached or it can't serve what the remote copied. Blocks briefly; should let
    /// sent messages through while it waits ([`rdp_vfiles::pump_sent_messages`]).
    fn remote_files(&self) -> Option<Arc<rdp_companion_proto::transfer::Inflow>>;

    /// Something other than files was copied on this PC: put `formats` on the remote
    /// clipboard through the companion, each read from this clipboard ([`read_format`])
    /// when something there pastes it. Returns false (use the clipboard channel) if none is
    /// attached. Must not block.
    fn offer_formats(&self, formats: Vec<rdp_companion_proto::ClipFormat>) -> bool;

    /// Something here is pasting `format` of what the remote copied: read it through the
    /// companion. `None` (use the clipboard channel) if none is attached or it can't. Blocks
    /// until the data is in: it's needed before `WM_RENDERFORMAT` returns.
    fn remote_format(&self, format: rdp_companion_proto::ClipFormat) -> Option<Vec<u8>>;

    /// The companion announces what the remote copies ([`Announcer`]), so the clipboard
    /// channel's announcements are ignored (a copy isn't offered twice).
    fn announces_copies(&self) -> bool {
        false
    }
}

/// Offers what the remote copied, as its companion announced it, on this PC's clipboard:
/// each format fetched when pasted (through the companion first). Any thread.
#[derive(Clone)]
pub struct Announcer {
    tx: mpsc_sync::Sender<BackendEvent>,
    window: HWND,
}

// SAFETY: the window handle is only used with PostMessageW, which any thread may call.
unsafe impl Send for Announcer {}
// SAFETY: as above.
unsafe impl Sync for Announcer {}

impl Announcer {
    /// The remote copied `formats` (its ids, names for registered ones), and files if
    /// `files`.
    pub fn remote_copy(&self, formats: Vec<rdp_companion_proto::ClipFormat>, files: bool) {
        use ironrdp_cliprdr::pdu::ClipboardFormatName;
        let mut list: Vec<ClipboardFormat> = formats
            .into_iter()
            .map(|f| {
                let format = ClipboardFormat::new(ClipboardFormatId::new(f.id));
                match f.name {
                    Some(name) => format.with_name(ClipboardFormatName::new(name)),
                    None => format,
                }
            })
            .collect();
        if files {
            // The file list's id is the companion's business: pastes ask it for the files.
            list.push(
                ClipboardFormat::new(ClipboardFormatId::new(0xC0FF))
                    .with_name(ClipboardFormatName::new(remote_files::FILE_LIST_FORMAT_NAME)),
            );
        }
        if self.tx.send(BackendEvent::CompanionCopy(list)).is_ok() {
            // SAFETY: posting to the monitor's window from any thread.
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(self.window),
                    WM_CLIPRDR_BACKEND_EVENT,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    }
}

/// The bytes of one format on this PC's clipboard (plain-memory formats: see
/// [`rdp_companion_proto::ClipFormat::is_memory`]). Any thread; waits briefly if another
/// program has the clipboard open.
pub fn read_format(format: u32) -> std::io::Result<Vec<u8>> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    let failed = |what: &str| std::io::Error::other(what.to_owned());
    let mut opened = false;
    for _ in 0..25 {
        // SAFETY: opening the clipboard for this thread; closed below.
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        return Err(failed("the clipboard stayed busy"));
    }
    // SAFETY: the clipboard is open; the handle stays valid until it closes, and the
    // locked bytes are copied out before unlocking.
    let data = unsafe {
        GetClipboardData(format).ok().and_then(|handle| {
            let handle = HGLOBAL(handle.0);
            let ptr = GlobalLock(handle).cast::<u8>().cast_const();
            if ptr.is_null() {
                return None;
            }
            let bytes = core::slice::from_raw_parts(ptr, GlobalSize(handle)).to_vec();
            let _ = GlobalUnlock(handle);
            Some(bytes)
        })
    };
    // SAFETY: closing what was opened above.
    let _ = unsafe { CloseClipboard() };
    data.ok_or_else(|| failed("the clipboard no longer has that format"))
}

/// Sent from the clipboard backend shim to the actual WinAPI subproc event loop
#[derive(Debug)]
pub(crate) enum BackendEvent {
    // Events generated by OS event loop
    ClipboardUpdated,
    RenderFormat(ClipboardFormatId),
    RenderAllFormats,

    // PDU processing events
    DowngradedCapabilities(ClipboardGeneralCapabilityFlags),
    /// The channel finished setting up (the server took our first format list).
    Ready,
    RemoteFormatList(Vec<ClipboardFormat>),
    /// What the remote copied, as its companion announced it.
    CompanionCopy(Vec<ClipboardFormat>),
    FormatDataRequest(FormatDataRequest),
    RemoteRequestsFormatList,
}

/// Where a CLIPRDR backend (on the session thread) delivers directly, bypassing the UI
/// thread's event queue.
#[derive(Clone)]
pub(crate) struct Direct {
    /// Backends alive (0 once the session's connection is gone): a paste then fails at once
    /// instead of waiting for an answer that can't come.
    pub alive: Arc<core::sync::atomic::AtomicUsize>,
    /// Remote file lists and file data, for the thread serving them to local apps.
    pub inbox: Arc<Inbox>,
    /// Replies to the UI thread's paste requests (`WM_RENDERFORMAT`).
    pub paste: mpsc_sync::Sender<FormatDataResponse<'static>>,
    /// The server's requests for files this side offered.
    pub files: Arc<FileServer>,
}

impl core::fmt::Debug for Direct {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Direct")
    }
}

/// Windows RDP client clipboard implementation.
///
/// IronRDP client implementation should provide raw window handle and message proxy to send
/// messages from the backend to `CLIPRDR` SVC.
///
/// [`WinClipboard`] instance holds ownership of the actual clipboard backend processing logic
/// and should be kept alive during the whole lifetime of the application.
///
/// This type is not thread safe, it should be used only in the main thread with a windows event
/// loop. (GUI thread). However, backend factory returned by [`WinClipboard::backend_factory`]
/// can be safely used in other threads.
pub struct WinClipboard {
    window: HWND,
    backend_tx: mpsc_sync::Sender<BackendEvent>,
    direct: Direct,
    /// Set as the monitor is torn down (its session is over).
    closing: Arc<core::sync::atomic::AtomicBool>,

    /// From MS docs:
    /// ```text
    /// You cannot use the subclassing helper functions to subclass a window across threads
    /// ```
    ///
    /// Therefore this type should be non-Send and non-Sync to prevent incorrect use.
    _thread_marker: core::marker::PhantomData<*const ()>,
}

impl WinClipboard {
    /// Creates new clipboard instance.
    ///
    /// Under the hood, a hidden window is created for capturing the clipboard events.
    /// `companion` (if given) speeds up file copies while one is attached.
    pub fn new(
        message_proxy: impl ClipboardMessageProxy + 'static,
        companion: Option<Arc<dyn Companion>>,
    ) -> WinCliprdrResult<Self> {
        extern "system" fn wndproc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            // SAFETY: default handler
            unsafe { DefWindowProcA(window, message, wparam, lparam) }
        }

        // SAFETY: low-level WinAPI call
        let instance = unsafe { GetModuleHandleA(None)? };
        let window_class = s!("IronRDPClipboardMonitor");

        let mut existing_wc = WNDCLASSA::default();
        // SAFETY: `instance` is a valid module handle, `window_class` is a valid null-terminated string,
        // and `existing_wc` is a valid mutable reference to a WNDCLASSA structure.
        let class_exists = unsafe { GetClassInfoA(Some(instance.into()), window_class, &mut existing_wc).is_ok() };

        if !class_exists {
            let wc = WNDCLASSA {
                hInstance: instance.into(),
                lpszClassName: window_class,
                lpfnWndProc: Some(wndproc),
                ..Default::default()
            };

            // SAFETY: low-level WinAPI call
            let atom = unsafe { RegisterClassA(&wc) };
            if atom == 0 {
                return Err(WinCliprdrError::from(Error::from_thread()));
            }
        }

        // SAFETY: low-level WinAPI call
        let window = unsafe {
            CreateWindowExA(
                WINDOW_EX_STYLE::default(),
                window_class,
                None,
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };

        if window.is_invalid() {
            return Err(WinCliprdrError::from(Error::from_thread()));
        }
        // Init clipboard processing for WinAPI event loop
        //
        // SAFETY: `window` is a valid window handle
        unsafe { AddClipboardFormatListener(window)? };

        let (backend_tx, backend_rx) = mpsc_sync::channel();
        let (paste_tx, paste_rx) = mpsc_sync::channel();

        let proxy = SharedProxy::new(message_proxy);
        let direct = Direct {
            alive: Arc::default(),
            inbox: Arc::new(Inbox::default()),
            paste: paste_tx,
            files: Arc::new(FileServer::new(proxy.clone())),
        };
        let closing = Arc::new(core::sync::atomic::AtomicBool::new(false));
        let ctx = Box::new(WinClipboardImpl::new(
            window,
            proxy,
            backend_rx,
            paste_rx,
            direct.clone(),
            companion,
            Arc::clone(&closing),
        ));

        // We need to receive winapi messages in the main thread, so we need to add a subclass to
        // the window.
        //
        // SAFETY: `window` is a valid window handle, `clipboard_subproc` is in the static memory,
        // `ctx` is valid and its ownership is transferred to the subclass via `into_raw`.
        let winapi_result = unsafe {
            SetWindowSubclass(
                window,
                Some(clipboard_subproc),
                0,
                Box::into_raw(ctx).expose_provenance(),
            )
        };

        if winapi_result == FALSE {
            return Err(WinCliprdrError::WindowSubclass);
        }

        Ok(Self {
            window,
            backend_tx,
            direct,
            closing,
            _thread_marker: Default::default(),
        })
    }

    /// Offers what the remote copied, as its companion announces it (from any thread).
    pub fn announcer(&self) -> Announcer {
        Announcer {
            tx: self.backend_tx.clone(),
            window: self.window,
        }
    }

    /// Returns clipboard backend factory suitable for making backend instances for `CLIPRDR` SVC.
    pub fn backend_factory(&self) -> Box<dyn CliprdrBackendFactory + Send> {
        Box::new(WinCliprdrBackendFactory {
            tx: self.backend_tx.clone(),
            window: self.window,
            direct: self.direct.clone(),
        })
    }
}

impl Drop for WinClipboard {
    fn drop(&mut self) {
        // Remove clipboard processing from WinAPI event loop

        // SAFETY: Format listener was registered in the `new` method previously.
        if let Err(err) = unsafe { RemoveClipboardFormatListener(self.window) } {
            error!("Failed to remove clipboard listener: {}", err)
        }

        // The session is over, so what it copied can't be fetched any more: destroying the
        // window must not wait to render it (WM_RENDERALLFORMATS, on the UI thread).
        self.closing.store(true, core::sync::atomic::Ordering::SeqCst);

        // The subclass frees its state on WM_DESTROY.
        //
        // SAFETY: the window was created on this thread in `new` and is destroyed only here.
        if let Err(err) = unsafe { DestroyWindow(self.window) } {
            error!("Failed to destroy clipboard window: {}", err)
        }
    }
}

/// Windows-specific clipboard backend factory
struct WinCliprdrBackendFactory {
    tx: mpsc_sync::Sender<BackendEvent>,
    window: HWND,
    direct: Direct,
}

// SAFETY: window handle is thread safe for PostMessageW usage
unsafe impl Send for WinCliprdrBackendFactory {}

impl CliprdrBackendFactory for WinCliprdrBackendFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(WinCliprdrBackend::new(
            self.window,
            self.tx.clone(),
            self.direct.clone(),
        ))
    }
}

/// Monotonic milliseconds since the first call (for `CliprdrBackend::now_ms`).
pub(crate) fn native_now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}
