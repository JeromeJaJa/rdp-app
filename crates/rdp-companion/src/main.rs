//! Remote-side companion for RDP App.
//!
//! Runs inside the remote session, opens the companion dynamic virtual channel back to
//! the client, and performs the [`rdp_companion_proto`] handshake. On the client the
//! channel registers only when the "Companion" setting is on and only accepts a channel
//! the remote opens, so this program is inert unless the user enabled it.
//!
//! This base does the handshake and heartbeat; file transfer and native drag come later.
//! It must be run inside an RDP session (there is no virtual channel otherwise).
//!
//! Status: the client side and this binary have unit-tested framing, but the end-to-end
//! channel has not yet been exercised against a live host — that needs a real session.

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use_logging();
    match windows_impl::run() {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "companion exited");
            Err(error)
        }
    }
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the companion only runs on Windows, inside an RDP session");
}

#[cfg(windows)]
fn use_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("RDP_LOG").unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .compact()
        .init();
}

#[cfg(windows)]
mod windows_impl {
    use anyhow::{Context as _, bail};
    use rdp_companion_proto::{CHANNEL_NAME, Decoder, Message, PROTOCOL_VERSION, encode};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTS_CHANNEL_OPTION_DYNAMIC, WTS_CURRENT_SESSION, WTSVirtualChannelClose,
        WTSVirtualChannelOpenEx, WTSVirtualChannelRead, WTSVirtualChannelWrite,
    };
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::core::PCSTR;

    /// How long a read waits before returning empty so we can re-check (ms).
    const READ_TIMEOUT: u32 = 5_000;

    /// The open companion channel; closes on drop.
    struct Channel(HANDLE);

    impl Channel {
        fn open() -> anyhow::Result<Self> {
            let name = std::ffi::CString::new(CHANNEL_NAME).expect("channel name has no NUL");
            // SAFETY: a null-terminated name; the returned handle is owned here.
            let handle = unsafe {
                WTSVirtualChannelOpenEx(WTS_CURRENT_SESSION, PCSTR(name.as_ptr().cast()), WTS_CHANNEL_OPTION_DYNAMIC)
            }
            .context("open companion channel (is this an RDP session with the client's setting on?)")?;
            Ok(Self(handle))
        }

        fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
            let mut written = 0u32;
            // SAFETY: writes `bytes` to our channel handle.
            unsafe { WTSVirtualChannelWrite(self.0, bytes, &mut written) }.context("write to companion channel")?;
            Ok(())
        }

        /// Reads the next chunk, or an empty slice on timeout. `None` means the channel closed.
        fn read<'b>(&self, buffer: &'b mut [u8]) -> anyhow::Result<Option<&'b [u8]>> {
            let mut read = 0u32;
            // SAFETY: reads into `buffer`, which outlives the call.
            let result = unsafe { WTSVirtualChannelRead(self.0, READ_TIMEOUT, buffer, &mut read) };
            match result {
                Ok(()) => Ok(Some(&buffer[..read as usize])),
                // The client closed the channel.
                Err(_) => Ok(None),
            }
        }
    }

    impl Drop for Channel {
        fn drop(&mut self) {
            // SAFETY: closing the channel handle we opened.
            unsafe {
                let _ = WTSVirtualChannelClose(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub(super) fn run() -> anyhow::Result<()> {
        let channel = Channel::open()?;
        let host = computer_name();
        let session = current_session();
        tracing::info!(%host, session, "companion starting, saying hello");
        channel.write(&encode(&Message::Hello {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            host,
            session,
        }))?;

        let mut decoder = Decoder::new();
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let Some(chunk) = channel.read(&mut buffer)? else {
                tracing::info!("channel closed by the client");
                return Ok(());
            };
            if chunk.is_empty() {
                continue; // read timed out; keep waiting
            }
            decoder.feed(chunk);
            loop {
                match decoder.next_message() {
                    Ok(Some(message)) => {
                        if handle(&channel, message)? {
                            return Ok(());
                        }
                    }
                    Ok(None) => break,
                    Err(error) => bail!("protocol error from client: {error}"),
                }
            }
        }
    }

    /// Handles one message; returns `true` when the companion should stop.
    fn handle(channel: &Channel, message: Message) -> anyhow::Result<bool> {
        match message {
            Message::Welcome { version, .. } => {
                tracing::info!(client = %version, "attached to the client");
                Ok(false)
            }
            Message::Refused { reason } => {
                tracing::warn!(%reason, "client refused the companion");
                Ok(true)
            }
            Message::Ping { nonce } => {
                channel.write(&encode(&Message::Pong { nonce }))?;
                Ok(false)
            }
            Message::Bye => {
                tracing::info!("client said goodbye");
                Ok(true)
            }
            // The client shouldn't send these.
            Message::Hello { .. } | Message::Pong { .. } => Ok(false),
        }
    }

    fn computer_name() -> String {
        let mut size = 0u32;
        // SAFETY: first call asks for the needed length (it fails with the size set).
        unsafe {
            let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut size);
        }
        let mut buf = vec![0u16; size as usize];
        // SAFETY: `buf` has `size` elements, as the previous call reported.
        let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut size) };
        if ok.is_ok() {
            String::from_utf16_lossy(&buf[..size as usize])
        } else {
            "remote".to_owned()
        }
    }

    fn current_session() -> u32 {
        let mut session = 0u32;
        // SAFETY: writes this process's session id into `session`.
        let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
        ok.map(|()| session).unwrap_or(0)
    }
}
