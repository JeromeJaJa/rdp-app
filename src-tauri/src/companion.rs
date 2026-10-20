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

/// A host to install the companion on, with the credentials to reach it.
pub struct Target {
    pub host: String,
    pub username: String,
    pub domain: String,
    pub password: String,
}

/// Outcome of an install attempt, so the UI can tell "done" from "WinRM unreachable".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Pushed and launched over WinRM.
    Installed,
    /// WinRM wasn't reachable (refused, timed out, not trusted): needs the fallback.
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
    let output = Command::new("powershell.exe")
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
    let detail = format!("{stdout}\n{stderr}");
    if looks_like_no_winrm(&detail) {
        tracing::info!(host = %target.host, "WinRM not available; fallback needed");
        return Ok(Outcome::WinrmUnavailable);
    }
    bail!("companion install failed: {}", detail.trim());
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
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", UNINSTALL_PS1])
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

/// Whether the failure is "WinRM can't be reached/used" (so we fall back) rather than a
/// real deployment error worth surfacing.
fn looks_like_no_winrm(detail: &str) -> bool {
    let d = detail.to_lowercase();
    [
        "cannot connect",
        "winrm",
        "the client cannot connect",
        "access is denied", // auth/trust issues on the WinRM channel
        "trustedhosts",
        "timed out",
        "no logon servers",
        "actively refused",
    ]
    .iter()
    .any(|needle| d.contains(needle))
}
