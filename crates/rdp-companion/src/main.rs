#![cfg_attr(windows, windows_subsystem = "windows")]
//! Remote-side companion for RDP App.
//!
//! A background agent with no UI — runs without a console window (it logs to a file).
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
    // Launched with no console, so also log to a file we can read back over WinRM.
    let path = std::env::var_os("LOCALAPPDATA")
        .map(|b| std::path::PathBuf::from(b).join("RdpApp"))
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join("companion.log"))
        .ok();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("RDP_LOG").unwrap_or_else(|_| "info".into()))
        .compact();
    match file {
        Some(file) => builder.with_writer(std::sync::Mutex::new(file)).init(),
        None => builder.with_writer(std::io::stderr).init(),
    }
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
    /// Block indefinitely on a read; the channel closing (disconnect) wakes it.
    const READ_WAIT: u32 = u32::MAX;
    /// Bytes of CHANNEL_PDU_HEADER (`length: u32, flags: u32`) that precede each read.
    const CHANNEL_PDU_HEADER: usize = 8;

    /// The open companion channel; closes on drop.
    struct Channel(HANDLE);

    impl Channel {
        fn open() -> anyhow::Result<Self> {
            let name = std::ffi::CString::new(CHANNEL_NAME).expect("channel name has no NUL");
            // SAFETY: a null-terminated name; the returned handle is owned here.
            let handle = unsafe {
                WTSVirtualChannelOpenEx(
                    WTS_CURRENT_SESSION,
                    PCSTR(name.as_ptr().cast()),
                    WTS_CHANNEL_OPTION_DYNAMIC,
                )
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

        /// Blocks until the next chunk arrives. `None` means the channel closed (the
        /// client disconnected). Blocking (rather than a short timeout that looks like a
        /// close every interval) keeps one attach stable until a real disconnect.
        fn read<'b>(&self, buffer: &'b mut [u8]) -> anyhow::Result<Option<&'b [u8]>> {
            let mut read = 0u32;
            // SAFETY: reads into `buffer`, which outlives the call.
            let result = unsafe { WTSVirtualChannelRead(self.0, READ_WAIT, buffer, &mut read) };
            match result {
                // With no timeout, a zero-length read means end of channel.
                Ok(()) if read > 0 => Ok(Some(&buffer[..read as usize])),
                Ok(()) => {
                    tracing::debug!("read returned 0 bytes");
                    Ok(None)
                }
                Err(error) => {
                    tracing::debug!(%error, code = format!("{:#010x}", error.code().0), "read failed");
                    Ok(None)
                }
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

    /// Stays resident for the life of the session, (re)attaching whenever the client is
    /// connected. The channel only exists while an RDP connection is up, so a disconnect
    /// ends an attach and a reconnect starts a new one — without re-launching, since the
    /// process survives a disconnect. The OS kills it at logoff; the logon autostart
    /// brings it back next time.
    const RETRY: std::time::Duration = std::time::Duration::from_secs(5);
    /// Give up if the channel never opens for this long (the client doesn't want us).
    const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(120);

    pub(super) fn run() -> anyhow::Result<()> {
        // Persist first (best-effort), so this host auto-starts the companion on every
        // future connection without any delivery step. Then attach for this session.
        install::ensure();
        let started = std::time::Instant::now();
        let mut ever_attached = false;
        loop {
            match Channel::open() {
                Ok(channel) => {
                    ever_attached = true;
                    if let Err(error) = serve(&channel) {
                        tracing::warn!(%error, "attach ended");
                    }
                }
                Err(error) => {
                    // Not connected, or companion disabled on the client: wait and retry.
                    if !ever_attached && started.elapsed() >= GIVE_UP_AFTER {
                        return Err(error).context("companion channel never became available");
                    }
                }
            }
            std::thread::sleep(RETRY);
        }
    }

    /// One attach: handshake, then relay until the channel closes or the client says bye.
    fn serve(channel: &Channel) -> anyhow::Result<()> {
        let host = computer_name();
        let session = current_session();
        tracing::info!(%host, session, "attached, saying hello");
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
                tracing::info!("channel closed (client disconnected)");
                return Ok(());
            };
            if chunk.is_empty() {
                continue; // nothing this read; keep waiting
            }
            // WTSVirtualChannelRead prepends an 8-byte CHANNEL_PDU_HEADER (length, flags)
            // on each read; our own length-prefixed frames sit after it. Our decoder
            // reassembles from our prefix, so we only need to drop the header. (Our
            // messages fit one chunk; larger ones will need the header's FIRST/LAST flags.)
            let payload = if chunk.len() >= CHANNEL_PDU_HEADER {
                &chunk[CHANNEL_PDU_HEADER..]
            } else {
                chunk
            };
            decoder.feed(payload);
            loop {
                match decoder.next_message() {
                    Ok(Some(message)) => {
                        if handle(channel, message)? {
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
        let ok = unsafe {
            GetComputerNameExW(
                ComputerNameDnsHostname,
                Some(windows::core::PWSTR(buf.as_mut_ptr())),
                &mut size,
            )
        };
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

    /// Installs the companion so it starts on every future logon, once per server.
    ///
    /// The first run may be from anywhere (a redirected drive, a download). It copies
    /// itself to a stable per-user location and registers a logon scheduled task for the
    /// current user, which runs in their interactive session — no admin, no service, no
    /// delivery step on later connects. Idempotent and best-effort: a failure here never
    /// stops the companion attaching for this session.
    mod install {
        use std::os::windows::process::CommandExt as _;
        use std::path::{Path, PathBuf};
        use std::process::Command;

        /// Scheduled-task name; also what an uninstall would remove.
        const TASK: &str = "RdpAppCompanion";
        /// So schtasks doesn't flash a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        pub(super) fn ensure() {
            if let Err(error) = try_ensure() {
                tracing::warn!(%error, "companion self-install skipped");
            }
        }

        fn try_ensure() -> anyhow::Result<()> {
            let target = target_path()?;
            let current = std::env::current_exe()?;
            if !same_file(&current, &target) {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                // If the target is in use (already installed and running), keep it.
                if let Err(error) = std::fs::copy(&current, &target) {
                    tracing::debug!(%error, "keeping the existing installed companion");
                }
            }
            register_logon_task(&target)
        }

        fn target_path() -> anyhow::Result<PathBuf> {
            let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA is not set"))?;
            Ok(PathBuf::from(base).join("RdpApp").join("rdp-companion.exe"))
        }

        fn same_file(a: &Path, b: &Path) -> bool {
            match (a.canonicalize(), b.canonicalize()) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        }

        /// Adds a per-user autostart entry so `exe` runs at logon. HKCU needs no admin
        /// (a logon scheduled task would); reconnects without a fresh logon are covered
        /// by the companion staying resident and re-opening its channel.
        fn register_logon_task(exe: &Path) -> anyhow::Result<()> {
            let value = format!("\"{}\"", exe.display());
            let status = Command::new("reg")
                .args([
                    "add",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    TASK,
                    "/t",
                    "REG_SZ",
                    "/d",
                    &value,
                    "/f",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            anyhow::ensure!(status.success(), "reg add exited with {status}");
            tracing::info!("companion registered to start on logon");
            Ok(())
        }
    }
}
