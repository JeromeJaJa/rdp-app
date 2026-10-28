//! Installing the companion on a remote host (experimental).
//!
//! Pure RDP can't run code on the remote, so the first-run install uses an admin
//! side-channel. Where the host has **WinRM** reachable (most of a managed fleet), we
//! push over it with the connection's own credentials: copy the companion in and launch
//! it as the logged-on user in their interactive session (WinRM itself runs in session
//! 0, so a scheduled task bridges to the session). Once running, the companion persists
//! itself (see `rdp-companion`), so this is one-time per host.
//!
//! Hosts without WinRM (e.g. internet-facing, 3389-only) fall back to an in-session
//! prompt — not built yet. We never auto-enable WinRM.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};

/// A `powershell.exe` command that runs without flashing a console window.
fn powershell() -> Command {
    let mut command = Command::new("powershell.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// A host to install the companion on, with the credentials to reach it.
pub struct Target {
    pub host: String,
    pub username: String,
    pub domain: String,
    pub password: String,
}

/// Outcome of an install attempt, so the UI can respond appropriately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Pushed and launched over WinRM.
    Installed,
    /// WinRM reached the host but this PC doesn't trust it yet (TrustedHosts): fixable
    /// with a one-time elevated step, then retry.
    NeedsTrust,
    /// WinRM wasn't reachable at all (refused, timed out, no listener): needs the
    /// in-session fallback.
    WinrmUnavailable,
}

const DEPLOY_PS1: &str = include_str!("companion_deploy.ps1");

/// Finds the companion binary to deploy: an explicit override, then next to the app exe
/// (bundled), then the dev build output.
pub fn companion_exe() -> anyhow::Result<PathBuf> {
    if let Some(p) = std::env::var_os("RDPAPP_COMPANION_EXE") {
        return Ok(PathBuf::from(p));
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("rdp-companion.exe"));
        // Dev: src-tauri's exe sits in target/debug alongside the companion.
        candidates.push(dir.join("..").join("debug").join("rdp-companion.exe"));
    }
    candidates
        .into_iter()
        .find(|p| p.exists())
        .context("companion binary not found (set RDPAPP_COMPANION_EXE)")
}

/// Fingerprint of the companion this app carries (see `rdp_companion_proto::build_id`), if
/// it can find one.
pub fn bundled_build() -> Option<String> {
    static BUILD: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILD
        .get_or_init(|| {
            let bytes = std::fs::read(companion_exe().ok()?).ok()?;
            Some(rdp_companion_proto::build_id(&bytes))
        })
        .clone()
}

/// Installs the companion on `target` over WinRM. Blocking (runs PowerShell); call off
/// the UI thread.
pub fn install_over_winrm(target: &Target, exe: &Path) -> anyhow::Result<Outcome> {
    anyhow::ensure!(exe.exists(), "companion binary missing: {}", exe.display());
    let user = if target.domain.is_empty() || target.username.contains('\\') {
        target.username.clone()
    } else {
        format!("{}\\{}", target.domain, target.username)
    };

    // Credentials go through the environment, never the command line (process args are
    // world-readable); the script builds a PSCredential from them.
    let output = powershell()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            DEPLOY_PS1,
        ])
        .env("RDPAPP_HOST", &target.host)
        .env("RDPAPP_USER", &user)
        .env("RDPAPP_PASS", &target.password)
        .env("RDPAPP_EXE", exe)
        .output()
        .context("run powershell")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() && stdout.contains("RDPAPP_OK") {
        tracing::info!(host = %target.host, "companion installed over WinRM");
        return Ok(Outcome::Installed);
    }
    let detail = format!("{stdout}\n{stderr}").to_lowercase();
    // The client doesn't trust the host yet — fixable with a one-time elevated step.
    if detail.contains("trustedhosts") || detail.contains("0x8009030e") {
        tracing::info!(host = %target.host, "WinRM needs this host trusted");
        return Ok(Outcome::NeedsTrust);
    }
    // WinRM isn't reachable at all.
    if [
        "cannot connect",
        "winrm",
        "timed out",
        "actively refused",
        "no logon servers",
    ]
    .iter()
    .any(|needle| detail.contains(needle))
    {
        tracing::info!(host = %target.host, "WinRM not available; fallback needed");
        return Ok(Outcome::WinrmUnavailable);
    }
    bail!("companion install failed: {}", detail.trim());
}

/// Adds a host to the WinRM client's TrustedHosts via an elevated prompt (UAC), so WinRM
/// can reach a workgroup host by IP. One-time per client; adds the specific host, not `*`.
/// Blocking (waits for the elevated process).
pub fn trust_host(host: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:_".contains(c)),
        "invalid host"
    );
    // The elevated command starts the WinRM client and appends this host if not present.
    let inner = format!(
        "Start-Service WinRM -ErrorAction SilentlyContinue; \
         $cur = (Get-Item WSMan:\\localhost\\Client\\TrustedHosts -ErrorAction SilentlyContinue).Value; \
         $hosts = @($cur -split ',' | ForEach-Object {{ $_.Trim() }} | Where-Object {{ $_ }}); \
         if ($hosts -notcontains '*' -and $hosts -notcontains '{host}') {{ \
           Set-Item WSMan:\\localhost\\Client\\TrustedHosts -Value '{host}' -Concatenate -Force }}"
    );
    // Handed over encoded (UTF-16LE, base64), so no shell on the way expands its `$`
    // variables or splits its quotes.
    let encoded = {
        use base64::Engine as _;
        let utf16: Vec<u8> = inner.encode_utf16().flat_map(u16::to_le_bytes).collect();
        base64::engine::general_purpose::STANDARD.encode(utf16)
    };
    let script = format!(
        "$p = Start-Process powershell -Verb RunAs -PassThru -Wait -WindowStyle Hidden \
         -ArgumentList @('-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'); exit $p.ExitCode"
    );
    let status = powershell()
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .context("run powershell")?;
    anyhow::ensure!(
        status.success(),
        "the elevation prompt was declined or the command failed"
    );
    tracing::info!(host, "host added to WinRM TrustedHosts");
    Ok(())
}

const UNINSTALL_PS1: &str = include_str!("companion_uninstall.ps1");

/// Removes the companion from a host over WinRM: stops it, drops its autostart entry,
/// and deletes its files. Blocking.
pub fn uninstall_over_winrm(target: &Target) -> anyhow::Result<()> {
    let user = if target.domain.is_empty() || target.username.contains('\\') {
        target.username.clone()
    } else {
        format!("{}\\{}", target.domain, target.username)
    };
    let output = powershell()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            UNINSTALL_PS1,
        ])
        .env("RDPAPP_HOST", &target.host)
        .env("RDPAPP_USER", &user)
        .env("RDPAPP_PASS", &target.password)
        .output()
        .context("run powershell")?;
    if !output.status.success() {
        bail!(
            "companion uninstall failed: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    tracing::info!(host = %target.host, "companion removed over WinRM");
    Ok(())
}
