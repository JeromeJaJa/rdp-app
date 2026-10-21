#![cfg_attr(windows, windows_subsystem = "windows")]
//! Remote-side companion for RDP App.
//!
//! A background agent with no UI: runs without a console window (it logs to a file).
//!
//! Runs inside the remote session, opens the companion dynamic virtual channel back to
//! the client, and performs the [`rdp_companion_proto`] handshake. On the client the
//! channel registers only when the "Companion" setting is on and only accepts a channel
//! the remote opens, so this program is inert unless the user enabled it.
//!
//! Once attached it reports host load ([`stats`]) and moves files: copies made on either
//! side ([`clipboard`]) and "Send files to…" ([`transfers`]), streamed rather than
//! fetched a chunk at a time. It must run inside an RDP session (there is no virtual
//! channel otherwise).

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
mod apps;
#[cfg(windows)]
mod channel;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod drag;
#[cfg(windows)]
mod dragout;
#[cfg(windows)]
mod stats;
#[cfg(windows)]
mod transfers;

#[cfg(windows)]
mod windows_impl {
    use std::sync::Arc;

    use anyhow::{Context as _, bail};
    use rdp_companion_proto::{Decoder, Message, PROTOCOL_VERSION};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    use windows::Win32::System::Threading::GetCurrentProcessId;

    use crate::channel::Channel;
    use crate::clipboard::Clipboard;
    use crate::stats::Stats;
    use crate::transfers::Transfers;

    /// Stays resident for the life of the session, (re)attaching whenever the client is
    /// connected. The channel only exists while an RDP connection is up, so a disconnect
    /// ends an attach and a reconnect starts a new one, without re-launching, since the
    /// process survives a disconnect. The OS kills it at logoff; the logon autostart
    /// brings it back next time.
    const RETRY: std::time::Duration = std::time::Duration::from_secs(5);
    /// Give up if the channel never opens for this long (the client doesn't want us).
    const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(120);

    /// How an attach ended.
    enum Ended {
        /// The connection went away (or said goodbye): wait for the next one.
        Detached,
        /// The client runs a different protocol and will deploy its own companion.
        Refused,
    }

    pub(super) fn run() -> anyhow::Result<()> {
        // Window positions in the session's own pixels (what the client's pointer uses).
        // SAFETY: process-wide setting, made before any window exists.
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        // Persist first (best-effort), so this host auto-starts the companion on every
        // future connection without any delivery step. Then attach for this session.
        install::ensure();
        let clipboard = Clipboard::spawn().map(Arc::new);
        let started = std::time::Instant::now();
        let mut ever_attached = false;
        loop {
            match Channel::open() {
                Ok(channel) => {
                    ever_attached = true;
                    match serve(Arc::new(channel), clipboard.clone()) {
                        Ok(Ended::Detached) => {}
                        Ok(Ended::Refused) => return Ok(()),
                        Err(error) => tracing::warn!(%error, "attach ended"),
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

    /// One attach: handshake, then serve until the channel closes or the client says bye.
    fn serve(channel: Arc<Channel>, clipboard: Option<Arc<Clipboard>>) -> anyhow::Result<Ended> {
        let host = computer_name();
        let session = current_session();
        tracing::info!(%host, session, "attached, saying hello");
        if !channel.send(&Message::Hello {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            host,
            session,
            build: build(),
            watches_clipboard: clipboard.as_ref().is_some_and(|c| c.watching()),
        }) {
            bail!("couldn't say hello");
        }

        let stats = Arc::new(Stats::start(Arc::clone(&channel)));
        let transfers = match clipboard {
            Some(clipboard) => Some(Arc::new(Transfers::new(
                Arc::clone(&channel),
                clipboard,
                Arc::clone(&stats),
            )?)),
            None => None,
        };
        let ended = attached(&channel, &stats, transfers.as_ref());
        if let Some(transfers) = &transfers {
            transfers.detach();
        }
        ended
    }

    fn attached(channel: &Channel, stats: &Stats, transfers: Option<&Arc<Transfers>>) -> anyhow::Result<Ended> {
        let mut decoder = Decoder::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let Some(chunk) = channel.read(&mut buffer) else {
                tracing::info!("channel closed (client disconnected)");
                return Ok(Ended::Detached);
            };
            decoder.feed(chunk);
            loop {
                let message = match decoder.next_message() {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => bail!("protocol error from client: {error}"),
                };
                match message {
                    Message::Welcome { version, .. } => tracing::info!(client = %version, "attached to the client"),
                    Message::Refused { reason } => {
                        tracing::warn!(%reason, "client refused the companion");
                        return Ok(Ended::Refused);
                    }
                    Message::Ping { nonce } => {
                        channel.send(&Message::Pong { nonce });
                    }
                    Message::Bye => {
                        tracing::info!("client said goodbye");
                        return Ok(Ended::Detached);
                    }
                    Message::StatsInterval { secs } => stats.set_interval(secs),
                    // The rest: the same as over the direct link.
                    message => {
                        if let Some(transfers) = transfers {
                            transfers.handle(message);
                        }
                    }
                }
            }
        }
    }

    /// This executable's fingerprint (so the client can tell an outdated companion).
    fn build() -> String {
        static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        BUILD
            .get_or_init(|| {
                std::env::current_exe()
                    .and_then(std::fs::read)
                    .map(|bytes| rdp_companion_proto::build_id(&bytes))
                    .unwrap_or_default()
            })
            .clone()
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
