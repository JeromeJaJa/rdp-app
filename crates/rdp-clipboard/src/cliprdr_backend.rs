use std::sync::mpsc as mpsc_sync;

use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse, FileDescriptor,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::{IntoOwned as _, impl_as_any};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::{BackendEvent, Direct, WM_CLIPRDR_BACKEND_EVENT};

#[derive(Debug)]
pub(crate) struct WinCliprdrBackend {
    backend_event_tx: mpsc_sync::Sender<BackendEvent>,
    window: HWND,
    direct: Direct,
}

// SAFETY: window handle is thread safe for PostMessageW usage
unsafe impl Send for WinCliprdrBackend {}

impl_as_any!(WinCliprdrBackend);

impl WinCliprdrBackend {
    pub(crate) fn new(window: HWND, backend_event_tx: mpsc_sync::Sender<BackendEvent>, direct: Direct) -> Self {
        Self {
            window,
            backend_event_tx,
            direct,
        }
    }

    fn send_event(&self, event: BackendEvent) {
        if self.backend_event_tx.send(event).is_err() {
            // Channel is closed, backend is dead
            return;
        }
        // Wake up subproc event loop; Dont wait for result
        //
        // SAFETY: it is safe to call PostMessageW from any thread with a valid window handle
        if let Err(err) = unsafe { PostMessageW(Some(self.window), WM_CLIPRDR_BACKEND_EVENT, WPARAM(0), LPARAM(0)) } {
            tracing::error!("Failed to post message to wake up subproc event loop: {}", err);
        }
    }
}

impl CliprdrBackend for WinCliprdrBackend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // Files copied locally are offered as a list of relative names and streamed on request.
        ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {}

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        self.send_event(BackendEvent::DowngradedCapabilities(capabilities))
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.send_event(BackendEvent::RemoteFormatList(available_formats.to_vec()));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.send_event(BackendEvent::FormatDataRequest(request));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        // A refused file list arrives here rather than in `on_remote_file_list`.
        if response.is_error() && self.direct.inbox.list_failed() {
            return;
        }
        let _ = self.direct.paste.send(response.into_owned());
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.direct.files.serve(request);
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.direct.inbox.file_contents(&response);
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], _clip_data_id: Option<u32>) {
        if !self.direct.inbox.file_list(files) {
            tracing::debug!("file list arrived after its request was given up");
        }
    }

    // Clipboard locking (CB_CAN_LOCK_CLIPDATA) is not advertised.
    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}

    fn on_request_format_list(&mut self) {
        self.send_event(BackendEvent::RemoteRequestsFormatList);
    }

    fn now_ms(&self) -> u64 {
        crate::native_now_ms()
    }

    fn elapsed_ms(&self, since: u64) -> u64 {
        self.now_ms().saturating_sub(since)
    }
}
