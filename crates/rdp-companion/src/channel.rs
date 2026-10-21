//! The companion's dynamic virtual channel, opened from inside the remote session.
//!
//! I/O goes through the channel's file handle with overlapped reads and writes (the way
//! Microsoft's DVC sample does it): the read loop waits on its own thread while stats,
//! transfers and drags write from theirs. The plain `WTSVirtualChannelRead`/`Write` calls
//! don't mix like that: a write while a read was waiting ended the read, which looked like
//! the client disconnecting, so the companion reattached every few seconds.

use std::sync::{Mutex, PoisonError};

use anyhow::Context as _;
use rdp_companion_proto::{CHANNEL_NAME, Message, encode};
use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_IO_PENDING, HANDLE, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::RemoteDesktop::{
    WTS_CHANNEL_OPTION_DYNAMIC, WTS_CHANNEL_OPTION_DYNAMIC_PRI_LOW, WTS_CURRENT_SESSION, WTSFreeMemory,
    WTSVirtualChannelClose, WTSVirtualChannelOpenEx, WTSVirtualChannelQuery, WTSVirtualFileHandle,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess};
use windows::core::PCSTR;

/// Bytes of CHANNEL_PDU_HEADER (`length: u32, flags: u32`) that precede each read.
const CHANNEL_PDU_HEADER: usize = 8;
/// Largest piece written at once (our frames carry their own length, so the reader
/// doesn't care where writes split).
const MAX_WRITE: usize = 64 * 1024;

/// The open companion channel; closes on drop. One thread reads; any may write (writes
/// are serialized).
pub(crate) struct Channel {
    file: HANDLE,
    read_event: HANDLE,
    /// The write event, held while a write is in progress.
    write: Mutex<HANDLE>,
}

// SAFETY: the handles may be used from any thread; writes are serialized by the mutex and
// only the read loop reads.
unsafe impl Send for Channel {}
// SAFETY: as above.
unsafe impl Sync for Channel {}

fn pending(error: &windows::core::Error) -> bool {
    error.code() == WIN32_ERROR(ERROR_IO_PENDING.0).to_hresult()
}

impl Channel {
    pub(crate) fn open() -> anyhow::Result<Self> {
        let name = std::ffi::CString::new(CHANNEL_NAME).expect("channel name has no NUL");
        // Low priority: the session's own graphics go ahead of transfers on the wire.
        // SAFETY: a null-terminated name; the handle is closed below once the file handle
        // is duplicated out of it.
        let wts = unsafe {
            WTSVirtualChannelOpenEx(
                WTS_CURRENT_SESSION,
                PCSTR(name.as_ptr().cast()),
                WTS_CHANNEL_OPTION_DYNAMIC | WTS_CHANNEL_OPTION_DYNAMIC_PRI_LOW,
            )
        }
        .context("open companion channel (is this an RDP session with the client's setting on?)")?;

        // SAFETY: querying the channel's file handle, duplicating it (the query's buffer is
        // freed, and the WTS handle closed, right after), and creating the I/O events.
        unsafe {
            let mut buffer: *mut core::ffi::c_void = core::ptr::null_mut();
            let mut len = 0u32;
            let queried = WTSVirtualChannelQuery(wts, WTSVirtualFileHandle, &mut buffer, &mut len);
            let mut file = HANDLE::default();
            let duplicated = queried.and_then(|()| {
                let source = *(buffer as *const HANDLE);
                DuplicateHandle(
                    GetCurrentProcess(),
                    source,
                    GetCurrentProcess(),
                    &mut file,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )
            });
            if !buffer.is_null() {
                WTSFreeMemory(buffer);
            }
            let _ = WTSVirtualChannelClose(wts);
            duplicated.context("get the companion channel's file handle")?;
            let read_event = CreateEventW(None, true, false, None).context("create an event")?;
            let write_event = CreateEventW(None, true, false, None).context("create an event")?;
            Ok(Self {
                file,
                read_event,
                write: Mutex::new(write_event),
            })
        }
    }

    /// Sends one message. False if the channel is gone.
    pub(crate) fn send(&self, message: &Message) -> bool {
        let bytes = encode(message);
        let event = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        bytes.chunks(MAX_WRITE).all(|piece| self.write_all(*event, piece))
    }

    fn write_all(&self, event: HANDLE, mut piece: &[u8]) -> bool {
        while !piece.is_empty() {
            let mut overlapped = OVERLAPPED {
                hEvent: event,
                ..Default::default()
            };
            let mut written = 0u32;
            // SAFETY: the buffer and OVERLAPPED outlive the operation (we wait for it below).
            let done = unsafe {
                match WriteFile(self.file, Some(piece), None, Some(&mut overlapped)) {
                    Err(error) if !pending(&error) => Err(error),
                    _ => GetOverlappedResult(self.file, &overlapped, &mut written, true),
                }
            };
            match done {
                Ok(()) if written > 0 => piece = &piece[(written as usize).min(piece.len())..],
                Ok(()) => return false,
                Err(error) => {
                    tracing::debug!(%error, "write to the companion channel failed");
                    return false;
                }
            }
        }
        true
    }

    /// Blocks until the next chunk arrives and returns its payload. `None` means the
    /// channel closed (the client disconnected).
    pub(crate) fn read<'b>(&self, buffer: &'b mut [u8]) -> Option<&'b [u8]> {
        let mut overlapped = OVERLAPPED {
            hEvent: self.read_event,
            ..Default::default()
        };
        let mut read = 0u32;
        // SAFETY: the buffer and OVERLAPPED outlive the operation (we wait for it below).
        let done = unsafe {
            match ReadFile(self.file, Some(buffer), None, Some(&mut overlapped)) {
                Err(error) if !pending(&error) => Err(error),
                _ => GetOverlappedResult(self.file, &overlapped, &mut read, true),
            }
        };
        match done {
            // Each read is one chunk behind a CHANNEL_PDU_HEADER (length, flags). Our own
            // frames are length-prefixed and reassembled by the decoder, so only the header
            // needs dropping.
            Ok(()) if read as usize > CHANNEL_PDU_HEADER => Some(&buffer[CHANNEL_PDU_HEADER..read as usize]),
            Ok(()) if read > 0 => Some(&[]),
            Ok(()) => {
                tracing::debug!("read returned 0 bytes");
                None
            }
            Err(error) => {
                tracing::debug!(%error, "read failed");
                None
            }
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // SAFETY: closing the handles this channel owns.
        unsafe {
            let _ = CloseHandle(self.file);
            let _ = CloseHandle(self.read_event);
            let _ = CloseHandle(*self.write.get_mut().unwrap_or_else(PoisonError::into_inner));
        }
    }
}
