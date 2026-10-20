//! Device redirection (RDPDR): drives / folders, the default printer, smart cards.

use anyhow::Context as _;
use ironrdp_rdpdr::Rdpdr;

use crate::DeviceRedirection;

/// Name shown on the remote side ("C" for `C:\`, the folder name otherwise).
fn display_name(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let trimmed = s.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return trimmed[..1].to_uppercase();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Drive".into())
}

/// The form a configured path is shared in, or `None` (with a warning) if it can't be:
/// volume roots as they are (`C:\`), folders resolved to their real location on a local
/// volume (junctions followed), since the backend refuses to traverse reparse points.
/// Network folders are skipped: an unusable root would fail the whole session.
fn shareable(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let s = path.to_string_lossy();
    let trimmed = s.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return path.is_dir().then(|| std::path::PathBuf::from(format!("{trimmed}\\")));
    }
    // Checked before touching the file system: resolving one can stall on the network.
    if s.starts_with(r"\\") && !s.starts_with(r"\\?\") {
        tracing::warn!(path = %path.display(), "network folders can't be shared; skipped");
        return None;
    }
    let real = match std::fs::canonicalize(path) {
        Ok(real) if real.is_dir() => real,
        Ok(_) => {
            tracing::warn!(path = %path.display(), "shared path is not a folder; skipped");
            return None;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "shared folder not available; skipped");
            return None;
        }
    };
    // `\\?\D:\…`; network locations come back as `\\?\UNC\…`.
    let text = real.to_string_lossy();
    let local = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let bytes = local.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        Some(std::path::PathBuf::from(local))
    } else {
        tracing::warn!(path = %path.display(), "network folders can't be shared; skipped");
        None
    }
}

/// Builds the RDPDR channel for the configured devices, or `None` if nothing is shared.
pub(crate) fn rdpdr(devices: &DeviceRedirection) -> anyhow::Result<Option<Rdpdr>> {
    if !devices.any() {
        return Ok(None);
    }
    #[cfg(windows)]
    {
        use ironrdp_rdpdr::backend::RdpdrBackendFactory as _;
        use ironrdp_rdpdr_native::{RedirectedDrive, WindowsRdpdrBackendFactory};

        let mut drives = Vec::new();
        for (path, root) in devices.drives.iter().filter_map(|p| Some((p, shareable(p)?))) {
            let id = drives.len() as u32 + 1;
            drives.push(
                RedirectedDrive::new(id, display_name(path), root, false)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
            );
        }
        let ids = drives.iter().map(RedirectedDrive::device_id).collect();
        let has_drives = !drives.is_empty();
        let factory = WindowsRdpdrBackendFactory::from_drive_configuration(drives, ids)
            .map_err(|e| anyhow::anyhow!("drive configuration: {e}"))?
            .with_smartcard(devices.smartcard)
            .with_default_printer(devices.printer);
        let product = factory
            .build_rdpdr_backend()
            .map_err(|e| anyhow::anyhow!("device redirection: {e}"))
            .context("build RDPDR backend")?;
        let printer = product.printer().cloned();
        let (backend, initial) = product.into_parts();
        if initial.is_empty() && !devices.smartcard && printer.is_none() {
            return Ok(None);
        }

        let client_name: String = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "rdp-app".into());
        let mut channel = Rdpdr::new(backend, client_name);
        if has_drives {
            channel = channel.with_drives(Some(initial.into_iter().map(|d| d.into_parts()).collect()));
        }
        if let Some(printer) = printer {
            let (device_id, name, driver, network) = printer.into_parts();
            channel = channel.with_printer_driver_and_network(device_id, name, driver, network);
        }
        if devices.smartcard {
            channel = channel.with_smartcard(0);
        }
        Ok(Some(channel))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_local_folders_by_their_real_path() {
        let dir = std::env::temp_dir().join("rdp-app-share-test");
        std::fs::create_dir_all(&dir).unwrap();
        let shared = shareable(&dir).expect("local folder is shareable");
        let text = shared.to_string_lossy().into_owned();
        assert!(!text.starts_with(r"\\?\"), "{text}");
        assert_eq!(text.as_bytes()[1], b':');
        assert_eq!(shareable(&dir.join("missing")), None);
        assert_eq!(shareable(std::path::Path::new(r"\\nonexistent-host\share")), None);
        assert_eq!(shareable(std::path::Path::new("C:")).as_deref(), Some(std::path::Path::new("C:\\")));
    }

    #[test]
    fn names_drives_and_folders() {
        assert_eq!(display_name(std::path::Path::new("C:\\")), "C");
        assert_eq!(display_name(std::path::Path::new("d:")), "D");
        assert_eq!(display_name(std::path::Path::new("D:\\Work\\Shared")), "Shared");
    }
}
