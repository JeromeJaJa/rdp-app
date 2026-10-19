//! Local files on the clipboard (CF_HDROP) offered to the server as a CLIPRDR file
//! list; the server then pulls sizes and byte ranges by list index.

use std::fs::{File, Metadata};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ironrdp_cliprdr::pdu::{ClipboardFileAttributes, FileDescriptor};
use tracing::warn;

/// IronRDP drops entries whose wire name (`relative\name`) is longer than this, which
/// would shift every later index; skip them here instead so indices stay aligned.
const MAX_WIRE_NAME_CHARS: usize = 259;
/// Upper bound on entries offered at once (copying a whole drive should not hang).
const MAX_ENTRIES: usize = 50_000;
/// Largest single range served (the server asks in chunks; this only caps a bad request).
const MAX_RANGE: u32 = 16 * 1024 * 1024;

/// A file list in wire order: `descriptors[i]` describes `paths[i]`.
#[derive(Default)]
pub(crate) struct LocalFiles {
    pub descriptors: Vec<FileDescriptor>,
    pub paths: Vec<PathBuf>,
}

/// Lists the copied files and folders (recursively, folders before their contents).
pub(crate) fn collect(roots: &[PathBuf]) -> LocalFiles {
    let mut out = LocalFiles::default();
    for root in roots {
        add(&mut out, root, None);
    }
    out
}

fn add(out: &mut LocalFiles, path: &Path, parent: Option<&str>) {
    if out.paths.len() >= MAX_ENTRIES {
        return;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return; // a drive root has no name to paste under
    };
    let wire_chars = parent.map_or(0, |p| p.chars().count() + 1) + name.chars().count();
    if name.is_empty() || wire_chars > MAX_WIRE_NAME_CHARS {
        warn!(path = %path.display(), "skipping file: name too long for clipboard transfer");
        return;
    }
    // Do not follow links into folders (loops); a linked file is sent as its target.
    let Ok(link_meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let Ok(meta) = std::fs::metadata(path) else { return };
    let is_dir = meta.is_dir();

    let mut descriptor = FileDescriptor::new(name).with_attributes(if is_dir {
        ClipboardFileAttributes::DIRECTORY
    } else {
        ClipboardFileAttributes::ARCHIVE
    });
    if let Some(time) = filetime(&meta) {
        descriptor = descriptor.with_last_write_time(time);
    }
    if !is_dir {
        descriptor = descriptor.with_file_size(meta.len());
    }
    if let Some(parent) = parent {
        descriptor = descriptor.with_relative_path(parent);
    }
    out.descriptors.push(descriptor);
    out.paths.push(path.to_owned());

    if is_dir && !link_meta.file_type().is_symlink() {
        let relative = match parent {
            Some(parent) => format!("{parent}\\{name}"),
            None => name.to_owned(),
        };
        let Ok(entries) = std::fs::read_dir(path) else { return };
        let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            add(out, &child, Some(&relative));
        }
    }
}

/// Last write time as a Windows FILETIME (100 ns ticks since 1601-01-01).
fn filetime(meta: &Metadata) -> Option<u64> {
    const UNIX_TO_FILETIME_SECS: u64 = 11_644_473_600;
    let since_unix = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let secs = since_unix.as_secs().checked_add(UNIX_TO_FILETIME_SECS)?;
    secs.checked_mul(10_000_000)?
        .checked_add(u64::from(since_unix.subsec_nanos() / 100))
}

pub(crate) fn size(path: &Path) -> std::io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

/// Up to `len` bytes from `position` (fewer at the end of the file).
pub(crate) fn read_range(path: &Path, position: u64, len: u32) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(position))?;
    let mut data = Vec::with_capacity(len.min(MAX_RANGE) as usize);
    file.take(u64::from(len.min(MAX_RANGE))).read_to_end(&mut data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_before_contents_with_relative_paths() {
        let root = std::env::temp_dir().join(format!("rdp-clipboard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs/sub")).unwrap();
        std::fs::write(root.join("docs/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("docs/sub/b.bin"), [7u8; 10]).unwrap();
        std::fs::write(root.join("top.txt"), b"x").unwrap();

        let files = collect(&[root.join("docs"), root.join("top.txt")]);
        let names: Vec<(Option<&str>, &str)> = files
            .descriptors
            .iter()
            .map(|d| (d.relative_path.as_deref(), d.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                (None, "docs"),
                (Some("docs"), "a.txt"),
                (Some("docs"), "sub"),
                (Some("docs\\sub"), "b.bin"),
                (None, "top.txt"),
            ]
        );
        assert_eq!(files.descriptors[1].file_size, Some(5));
        assert_eq!(files.descriptors[0].file_size, None, "folders carry no size");
        assert_eq!(read_range(&files.paths[3], 8, 100).unwrap(), vec![7u8; 2]);
        assert_eq!(size(&files.paths[1]).unwrap(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }
}
