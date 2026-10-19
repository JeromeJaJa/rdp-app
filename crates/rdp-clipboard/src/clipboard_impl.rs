use core::ptr::with_exposed_provenance_mut;
use core::time::Duration;
use std::collections::HashSet;
use std::sync::{Arc, mpsc};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FormatDataRequest, FormatDataResponse,
};
use tracing::{debug, info};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::GetClipboardOwner;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, WM_CLIPBOARDUPDATE, WM_DESTROY, WM_RENDERALLFORMATS, WM_RENDERFORMAT, WM_TIMER,
};

use crate::clipboard_data_ref::ClipboardDataRef;
use crate::os_clipboard::OwnedOsClipboard;
use crate::remote_files::{FILE_LIST_FORMAT_NAME, OleClipboard, Remote, SharedProxy, is_file_format};
use crate::remote_format_registry::RemoteClipboardFormatRegistry;
use crate::utils::render_format;
use crate::{BackendEvent, WM_CLIPRDR_BACKEND_EVENT, WinCliprdrError, WinCliprdrResult};

/// How long a paste waits for the remote's data. Windows needs it before
/// `WM_RENDERFORMAT` returns, so the UI thread is blocked meanwhile; large text or
/// images over a slow link can take a while.
const RENDER_FORMAT_TIMEOUT_SECS: u64 = 45;
const IDT_CLIPBOARD_RETRY: usize = 1;

/// Internal implementation of the clipboard processing logic.
pub(crate) struct WinClipboardImpl {
    window: HWND,
    message_proxy: Box<dyn ClipboardMessageProxy>,
    backend_rx: mpsc::Receiver<BackendEvent>,
    /// Replies to this thread's paste requests.
    paste_rx: mpsc::Receiver<FormatDataResponse<'static>>,
    /// The server accepted file streaming (CB_STREAM_FILECLIP_ENABLED).
    file_transfer: bool,
    /// Serves the files last offered to the server.
    files: Arc<crate::files::FileServer>,
    /// Reaches the server from the thread serving remote files to local apps.
    remote: Remote,
    /// That thread, started the first time the remote copies files.
    ole: Option<OleClipboard>,
    // Number of attempts spent to process current clipboard message
    attempt: u32,
    // Message to retry
    retry_message: Option<BackendEvent>,
    // Formats available on the remote (represented as LOCAL format ids)
    available_formats_on_remote: Vec<ClipboardFormatId>,
    remote_format_registry: RemoteClipboardFormatRegistry,
}

impl WinClipboardImpl {
    pub(crate) fn new(
        window: HWND,
        proxy: SharedProxy,
        backend_rx: mpsc::Receiver<BackendEvent>,
        paste_rx: mpsc::Receiver<FormatDataResponse<'static>>,
        direct: crate::Direct,
    ) -> Self {
        Self {
            window,
            message_proxy: Box::new(proxy.clone()),
            backend_rx,
            paste_rx,
            file_transfer: false,
            files: direct.files,
            remote: Remote {
                inbox: direct.inbox,
                proxy,
            },
            ole: None,
            attempt: 0,
            retry_message: None,
            available_formats_on_remote: Vec::new(),
            remote_format_registry: Default::default(),
        }
    }

    fn on_format_data_request(&mut self, request: &FormatDataRequest) -> WinCliprdrResult<Option<ClipboardMessage>> {
        // Get data from the clipboard and send event to the main event loop
        let clipboard = OwnedOsClipboard::new(self.window)?;

        let buffer = ClipboardDataRef::get(&clipboard, request.format).map(|borrowed| borrowed.data().to_vec());

        let response = match buffer {
            Some(data) => FormatDataResponse::new_data(data),
            None => {
                // No data available for this format
                FormatDataResponse::new_error()
            }
        };

        Ok(Some(ClipboardMessage::SendFormatData(response)))
    }

    fn on_format_data_response(
        requested_local_format: ClipboardFormatId,
        response: &FormatDataResponse<'_>,
    ) -> WinCliprdrResult<()> {
        if response.is_error() {
            // No data available for this format anymore
            return Ok(());
        }

        // SAFETY: `on_format_data_response` is only called in a context of processing
        // `WM_RENDERFORMAT` and `WM_RENDERALLFORMATS` messages, so we can safely assume that
        // calling `ClipboardData::render` is safe.
        unsafe {
            render_format(requested_local_format, response.data())?;
        }

        Ok(())
    }

    fn on_clipboard_update(&mut self) -> WinCliprdrResult<Option<ClipboardMessage>> {
        let mut clipboard = OwnedOsClipboard::new(self.window)?;
        if self.file_transfer
            && let Some(paths) = clipboard.dropped_files()
        {
            drop(clipboard); // walking folders can take a moment; don't hold the clipboard
            let files = crate::files::collect(&paths);
            if !files.descriptors.is_empty() {
                self.files.offer(files.paths);
                return Ok(Some(ClipboardMessage::SendInitiateFileCopy(files.descriptors)));
            }
            clipboard = OwnedOsClipboard::new(self.window)?;
        }
        let mut formats = clipboard.enum_available_formats()?;

        let mut filtered_format_ids = formats.iter().map(|f| f.id()).collect::<HashSet<_, _>>();
        filter_format_ids(&mut filtered_format_ids);
        // File lists without CF_HDROP are virtual files (another session's remote, Outlook
        // attachments...) whose contents this side cannot serve.
        formats.retain(|format| filtered_format_ids.contains(&format.id()) && !is_file_format(format));

        Ok(Some(ClipboardMessage::SendInitiateCopy(formats)))
    }

    fn get_remote_format_data(
        &mut self,
        format: ClipboardFormatId,
    ) -> WinCliprdrResult<Option<FormatDataResponse<'static>>> {
        let mapped_format = self.remote_format_registry.local_to_remote(format);

        let remote_format = if let Some(format) = mapped_format {
            format
        } else {
            // Format is unknown or not supported on the local machine
            return Ok(None);
        };

        // We need to receive data from the remote clipboard immediately, because Windows
        // expects us to set clipboard data before returning from `WM_RENDERFORMAT` handler.
        //
        // Sadly, this will block the GUI thread, while data is being received
        while self.paste_rx.try_recv().is_ok() {} // a late reply to an earlier paste
        let started = std::time::Instant::now();
        self.message_proxy
            .send_clipboard_message(ClipboardMessage::SendInitiatePaste(remote_format));
        let response = self
            .paste_rx
            .recv_timeout(Duration::from_secs(RENDER_FORMAT_TIMEOUT_SECS))
            .map_err(|_| WinCliprdrError::DataReceiveTimeout)?;
        let bytes = response.data().len();
        let ms = started.elapsed().as_millis();
        if bytes >= 1024 * 1024 {
            info!(bytes, ms, "remote clipboard data received");
        } else {
            debug!(bytes, ms, "remote clipboard data received");
        }
        Ok(Some(response))
    }

    fn on_render_format(&mut self, format: ClipboardFormatId) -> WinCliprdrResult<Option<ClipboardMessage>> {
        // Owning clipboard is not required when processing `WM_RENDERFORMAT` message
        if let Some(response) = self.get_remote_format_data(format)? {
            Self::on_format_data_response(format, &response)?;
        }

        Ok(None)
    }

    fn on_render_all_formats(&mut self) -> WinCliprdrResult<Option<ClipboardMessage>> {
        // We need to be clipboard owner to be able to set all clipboard formats
        let _clipboard = match OwnedOsClipboard::new(self.window) {
            Ok(clipboard) => {
                // SAFETY: `GetClipboardOwner` is always safe to call
                if self.window != unsafe { GetClipboardOwner()? } {
                    // As per MSDN, we need to validate clipboard owner after opening clipboard
                    return Ok(None);
                }

                clipboard
            }
            Err(WinCliprdrError::ClipboardAccessDenied) => {
                // We don't own the clipboard anymore, we don't need to do anything
                return Ok(None);
            }
            Err(err) => {
                return Err(err);
            }
        };

        let formats = core::mem::take(&mut self.available_formats_on_remote);

        // Clearing clipboard is not required, just render all available formats

        for format in formats {
            if let Some(response) = self.get_remote_format_data(format)? {
                Self::on_format_data_response(format, &response)?;
            }
        }

        Ok(None)
    }

    fn on_remote_format_list(&mut self, formats: &[ClipboardFormat]) -> WinCliprdrResult<Option<ClipboardMessage>> {
        let generation = self.remote.inbox.next_generation();
        self.available_formats_on_remote.clear();

        // Clear previous format mapping
        self.remote_format_registry.clear();

        // Files: offered through OLE so Explorer can stream them.
        let file_list = formats
            .iter()
            .find(|f| f.name().is_some_and(|n| n.value() == FILE_LIST_FORMAT_NAME));
        if let (true, Some(file_list)) = (self.file_transfer, file_list) {
            if self.ole.is_none() {
                self.ole = OleClipboard::spawn(self.remote.clone());
            }
            if let Some(ole) = &self.ole {
                ole.offer(generation, file_list.id());
                return Ok(None);
            }
        }

        let mut local_format_ids = formats
            .iter()
            .filter(|format| !is_file_format(format))
            .filter_map(|format| {
                // `register` will return None if format is unknown/unsupported on the local machine,
                // so we need to filter them out.
                self.remote_format_registry.register(format)
            })
            .collect::<HashSet<_>>();

        filter_format_ids(&mut local_format_ids);

        if local_format_ids.is_empty() {
            return Ok(None);
        }

        // We need to be clipboard owner to be able to set clipboard data
        let mut clipboard = OwnedOsClipboard::new(self.window)?;
        // Before sending delayed-rendered data, we need to clear clipboard first.
        clipboard.clear()?;

        for format in local_format_ids.into_iter() {
            clipboard.delay_render(format)?;
            // Store available format for `WM_RENDERALLFORMATS` processing.
            self.available_formats_on_remote.push(format);
        }

        Ok(None)
    }

    fn handle_event(&mut self, event: BackendEvent) {
        let result = match &event {
            BackendEvent::FormatDataRequest(request) => self.on_format_data_request(request),
            BackendEvent::RemoteFormatList(formats) => self.on_remote_format_list(formats),

            BackendEvent::ClipboardUpdated | BackendEvent::RemoteRequestsFormatList => self.on_clipboard_update(),
            BackendEvent::RenderFormat(format) => self.on_render_format(*format),
            BackendEvent::RenderAllFormats => self.on_render_all_formats(),

            BackendEvent::DowngradedCapabilities(flags) => {
                self.file_transfer = flags.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
                Ok(None)
            }
        };

        let retry_err = match result {
            Ok(Some(message)) => {
                self.message_proxy.send_clipboard_message(message);
                None
            }
            Ok(None) => {
                // No message to send
                None
            }
            Err(err) => {
                // Some errors are retryable (e.g. access to clipboard is temporarily denied)
                if let WinCliprdrError::ClipboardAccessDenied = &err {
                    Some(err)
                } else {
                    self.message_proxy
                        .send_clipboard_message(ClipboardMessage::Error(Box::new(err)));
                    None
                }
            }
        };

        match retry_err {
            None => {
                self.attempt = 0;
            }
            Some(err) => {
                const MAX_PROCESSING_ATTEMPTS: u32 = 10;
                const PROCESSING_TIMEOUT_MS: u32 = 100;

                #[expect(clippy::arithmetic_side_effects)]
                // self.attempt can’t be greater than MAX_PROCESSING_ATTEMPTS, so the arithmetic is safe here
                if self.attempt < MAX_PROCESSING_ATTEMPTS {
                    self.attempt += 1;

                    self.retry_message = Some(event);

                    // SAFETY: `SetTimer` is always safe to call when `hwnd` is a valid window handle
                    unsafe {
                        SetTimer(
                            Some(self.window),
                            IDT_CLIPBOARD_RETRY,
                            self.attempt * PROCESSING_TIMEOUT_MS,
                            None,
                        )
                    };
                } else {
                    // Send error, retries limit exceeded
                    self.message_proxy
                        .send_clipboard_message(ClipboardMessage::Error(Box::new(err)));
                }
            }
        }
    }
}

impl Drop for WinClipboardImpl {
    fn drop(&mut self) {
        // Fail transfers still waiting on the server; the file thread then quits.
        self.remote.inbox.close();
    }
}

fn filter_format_ids(formats: &mut HashSet<ClipboardFormatId>) {
    let has_non_utf16_formats =
        formats.contains(&ClipboardFormatId::CF_TEXT) || formats.contains(&ClipboardFormatId::CF_OEMTEXT);

    let has_utf16_formats = formats.contains(&ClipboardFormatId::CF_UNICODETEXT);

    // Windows could implicitly convert `CF_UNICODETEXT` to `CF_TEXT`, so to avoid
    // character encoding issues with application which prefer non-unicode text, we need to remove
    // `CF_TEXT` and `CF_OEMTEXT` from the list of formats.
    if has_utf16_formats && has_non_utf16_formats {
        formats.remove(&ClipboardFormatId::CF_TEXT);
        formats.remove(&ClipboardFormatId::CF_OEMTEXT);
    }
}

/// WinAPI event loop for clipboard processing
///
/// # Safety
///
/// This function should only be used for windows subclassing api via `SetWindowSubclass`.
pub(crate) unsafe extern "system" fn clipboard_subproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_DESTROY {
        // Later messages (WM_NCDESTROY) must not reach this procedure with freed state.
        //
        // SAFETY: removing this very subclass from its own window, on its thread.
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(clipboard_subproc), _id) };

        // Transfer ownership and drop previously allocated context

        // SAFETY: `data` is a valid pointer, returned by `Box::into_raw`, transferred to OS earlier
        // via `SetWindowSubclass` call.
        let _ = unsafe { Box::from_raw(with_exposed_provenance_mut::<WinClipboardImpl>(data)) };
        return LRESULT(0);
    }

    // SAFETY: `data` is a valid pointer, returned by `Box::into_raw`, transferred to OS earlier
    // via `SetWindowSubclass` call.
    let ctx = unsafe { &mut *(with_exposed_provenance_mut::<WinClipboardImpl>(data)) };

    match msg {
        // Sent by the OS when OS clipboard content is changed
        WM_CLIPBOARDUPDATE => {
            // SAFETY: `GetClipboardOwner` is always safe to call.
            let clipboard_owner = unsafe { GetClipboardOwner() };
            // Our own change (this session's remote copy being made available locally, by
            // this window or by its OLE file thread) must not echo back. Anything else,
            // including another session's copy, is offered to this session's remote.
            let own = clipboard_owner
                .as_ref()
                .is_ok_and(|&owner| owner == hwnd || ctx.remote.inbox.owns(owner));
            if !own {
                ctx.handle_event(BackendEvent::ClipboardUpdated);
            }
        }
        // Sent by the OS when delay-rendered data is requested for rendering.
        WM_RENDERFORMAT => {
            ctx.handle_event(BackendEvent::RenderFormat(ClipboardFormatId::new(
                u32::try_from(wparam.0).expect("should never truncate in practice"),
            )));
        }
        // Sent by the OS when all delay-rendered data is requested for rendering.
        WM_RENDERALLFORMATS => {
            ctx.handle_event(BackendEvent::RenderAllFormats);
        }
        // User event, a message was sent by the `Cliprdr` backend shim
        WM_CLIPRDR_BACKEND_EVENT => {
            let message = if let Ok(message) = ctx.backend_rx.try_recv() {
                message
            } else {
                // No message has been received, spurious event
                return LRESULT(0);
            };

            ctx.handle_event(message);
        }
        // Retry timer. Some operations clipboard operations such as `OpenClipboard` are
        // fallible. We need to retry them a few times before giving up.
        WM_TIMER => {
            if wparam.0 == IDT_CLIPBOARD_RETRY {
                // Timer is one-shot, we need to stop it immediately

                // SAFETY: `KillTimer` is always safe to call when `hwnd` is a valid window handle.
                if let Err(err) = unsafe { KillTimer(Some(hwnd), IDT_CLIPBOARD_RETRY) } {
                    tracing::error!("Failed to kill timer: {}", err);
                }

                if let Some(event) = ctx.retry_message.take() {
                    ctx.handle_event(event);
                }
            }
        }
        _ => {
            // Call next event handler in the subclass chain

            // SAFETY: `DefSubclassProc` is always safe to call in context of subclass event loop
            return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        }
    };

    LRESULT(0) // SUCCESS
}
