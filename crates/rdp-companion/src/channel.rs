//! The companion's dynamic virtual channel, opened from inside the remote session.

use std::sync::Mutex;

use anyhow::Context as _;
use rdp_companion_proto::{CHANNEL_NAME, Message, encode};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::RemoteDesktop::{
    WTS_CHANNEL_OPTION_DYNAMIC, WTS_CHANNEL_OPTION_DYNAMIC_PRI_LOW, WTS_CURRENT_SESSION, WTSVirtualChannelClose,
    WTSVirtualChannelOpenEx, WTSVirtualChannelRead, WTSVirtualChannelWrite,
};
use windows::core::PCSTR;

/// Block indefinitely on a read; the channel closing (disconnect) wakes it.
const READ_WAIT: u32 = u32::MAX;
/// Bytes of CHANNEL_PDU_HEADER (`length: u32, flags: u32`) that precede each read.
const CHANNEL_PDU_HEADER: usize = 8;

/// The open companion channel; closes on drop. Reads happen on one thread; writes may
/// come from any (they're serialized).
pub(crate) struct Channel {
    handle: HANDLE,
    write: Mutex<()>,
}

// SAFETY: the WTS channel handle may be used from any thread; writes are serialized.
unsafe impl Send for Channel {}
// SAFETY: as above.
unsafe impl Sync for Channel {}

impl Channel {
    pub(crate) fn open() -> anyhow::Result<Self> {
        let name = std::ffi::CString::new(CHANNEL_NAME).expect("channel name has no NUL");
        // Low priority: the session's own graphics go ahead of transfers on the wire.
        // SAFETY: a null-terminated name; the returned handle is owned here.
        let handle = unsafe {
            WTSVirtualChannelOpenEx(
                WTS_CURRENT_SESSION,
                PCSTR(name.as_ptr().cast()),
                WTS_CHANNEL_OPTION_DYNAMIC | WTS_CHANNEL_OPTION_DYNAMIC_PRI_LOW,
            )
        }
        .context("open companion channel (is this an RDP session with the client's setting on?)")?;
        Ok(Self {
            handle,
            write: Mutex::new(()),
        })
    }

    /// Sends one message. False if the channel is gone.
    pub(crate) fn send(&self, message: &Message) -> bool {
        let bytes = encode(message);
        let _guard = self.write.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut written = 0u32;
        // SAFETY: writes `bytes` to our channel handle.
        match unsafe { WTSVirtualChannelWrite(self.handle, &bytes, &mut written) } {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "write to the companion channel failed");
                false
            }
        }
    }

    /// Blocks until the next chunk arrives and returns its payload. `None` means the
    /// channel closed (the client disconnected). Blocking (rather than a short timeout
    /// that looks like a close every interval) keeps one attach stable until a real
    /// disconnect.
    pub(crate) fn read<'b>(&self, buffer: &'b mut [u8]) -> Option<&'b [u8]> {
        let mut read = 0u32;
        // SAFETY: reads into `buffer`, which outlives the call.
        let result = unsafe { WTSVirtualChannelRead(self.handle, READ_WAIT, buffer, &mut read) };
        match result {
            // With no timeout, a zero-length read means end of channel.
            Ok(()) if read > 0 => {
                // Each read is one chunk behind a CHANNEL_PDU_HEADER (length, flags). Our
                // own frames are length-prefixed and reassembled by the decoder, so only
                // the header needs dropping.
                let chunk = &buffer[..read as usize];
                Some(chunk.get(CHANNEL_PDU_HEADER..).unwrap_or_default())
            }
            Ok(()) => {
                tracing::debug!("read returned 0 bytes");
                None
            }
            Err(error) => {
                tracing::debug!(%error, code = format!("{:#010x}", error.code().0), "read failed");
                None
            }
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // SAFETY: closing the channel handle we opened (this also frees the handle; a
        // second CloseHandle could close a handle another thread just got).
        unsafe {
            let _ = WTSVirtualChannelClose(self.handle);
        }
    }
}
