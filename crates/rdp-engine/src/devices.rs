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
        for (i, path) in devices.drives.iter().filter(|p| p.exists()).enumerate() {
            drives.push(
                RedirectedDrive::new(i as u32 + 1, display_name(path), path.clone(), false)
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
    fn names_drives_and_folders() {
        assert_eq!(display_name(std::path::Path::new("C:\\")), "C");
        assert_eq!(display_name(std::path::Path::new("d:")), "D");
        assert_eq!(display_name(std::path::Path::new("D:\\Work\\Shared")), "Shared");
    }
}
