//! Local files on the clipboard (CF_HDROP) offered to the server as a CLIPRDR file
//! list; the server then pulls sizes and byte ranges by list index.
//!
//! Requests are served by [`FileServer`] on its own thread, straight from the session's
//! CLIPRDR backend: the server typically keeps a single request in flight, so every hop
//! (the UI thread, reopening the file) would add to each round trip.

use std::fs::{File, Metadata};
use std::os::windows::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant, UNIX_EPOCH};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, FileContentsFlags, FileContentsRequest, FileContentsResponse, FileDescriptor,
};
use tracing::{info, warn};

use crate::remote_files::SharedProxy;

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

/// Up to `len` bytes from `position` (fewer at the end of the file).
fn read_at(file: &File, position: u64, len: u32) -> std::io::Result<Vec<u8>> {
    let mut data = vec![0u8; len.min(MAX_RANGE) as usize];
    let mut filled = 0;
    while filled < data.len() {
        match file.seek_read(&mut data[filled..], position + filled as u64)? {
            0 => break,
            n => filled += n,
        }
    }
    data.truncate(filled);
    Ok(data)
}

/// The files last offered to the server, with a version so cached handles never
/// outlive the list they were opened for.
#[derive(Default)]
struct Offered {
    version: u64,
    paths: Vec<PathBuf>,
}

/// Serves the server's size and range requests for offered files on a worker thread.
pub(crate) struct FileServer {
    offered: Arc<Mutex<Offered>>,
    requests: Mutex<Option<mpsc::Sender<FileContentsRequest>>>,
    proxy: SharedProxy,
}

impl FileServer {
    pub(crate) fn new(proxy: SharedProxy) -> Self {
        Self {
            offered: Arc::default(),
            requests: Mutex::new(None),
            proxy,
        }
    }

    /// Replaces the offered list (`paths[i]` answers requests for index `i`).
    pub(crate) fn offer(&self, paths: Vec<PathBuf>) {
        let mut offered = self.offered.lock().unwrap_or_else(PoisonError::into_inner);
        offered.version += 1;
        offered.paths = paths;
    }

    /// Queues a request; the reply goes straight to the session.
    pub(crate) fn serve(&self, request: FileContentsRequest) {
        let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = requests.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let offered = Arc::clone(&self.offered);
            let proxy = self.proxy.clone();
            let spawned = std::thread::Builder::new()
                .name("clipboard-file-server".into())
                .spawn(move || run(&offered, &proxy, &rx));
            if let Err(error) = spawned {
                warn!(%error, "could not start the clipboard file server");
            }
            tx
        });
        if let Err(mpsc::SendError(request)) = tx.send(request) {
            *requests = None;
            self.proxy
                .send_clipboard_message(ClipboardMessage::SendFileContentsResponse(
                    FileContentsResponse::new_error(request.stream_id),
                ));
        }
    }
}

/// An open file being read, and how the transfer is going (for the log).
struct Current {
    version: u64,
    index: i32,
    file: File,
    size: u64,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    bytes: u64,
    requests: u32,
    started: Option<Instant>,
    /// Time spent reading and replying.
    busy: Duration,
    /// Time between our reply and the server's next request (round trip + server).
    waiting: Duration,
    last_reply: Option<Instant>,
}

impl Stats {
    fn log(&self, path: &Path, size: u64) {
        let Some(started) = self.started else { return };
        let secs = started.elapsed().as_secs_f64().max(0.001);
        let requests = self.requests.max(1);
        info!(
            file = %path.file_name().unwrap_or_default().to_string_lossy(),
            size,
            sent = self.bytes,
            requests = self.requests,
            avg_request_kib = self.bytes / 1024 / u64::from(requests),
            secs = format!("{secs:.2}"),
            mib_per_s = format!("{:.2}", self.bytes as f64 / 1_048_576.0 / secs),
            avg_server_wait_ms = format!("{:.1}", self.waiting.as_secs_f64() * 1000.0 / f64::from(requests)),
            avg_local_ms = format!("{:.2}", self.busy.as_secs_f64() * 1000.0 / f64::from(requests)),
            "file sent to the remote"
        );
    }
}

fn run(offered: &Mutex<Offered>, proxy: &SharedProxy, requests: &mpsc::Receiver<FileContentsRequest>) {
    let mut current: Option<Current> = None;
    while let Ok(request) = requests.recv() {
        let arrived = Instant::now();
        let (version, path) = {
            let offered = offered.lock().unwrap_or_else(PoisonError::into_inner);
            let path = usize::try_from(request.index)
                .ok()
                .and_then(|i| offered.paths.get(i))
                .cloned();
            (offered.version, path)
        };
        let Some(path) = path else {
            reply(proxy, FileContentsResponse::new_error(request.stream_id));
            continue;
        };

        let reuse = current
            .as_ref()
            .is_some_and(|c| c.version == version && c.index == request.index);
        if !reuse {
            // A file left part-way (the user cancelled, or the server skipped ahead).
            if let Some(done) = current.take()
                && done.stats.requests > 0
            {
                done.stats.log(
                    &path_of(offered, done.version, done.index).unwrap_or_default(),
                    done.size,
                );
            }
            match File::open(&path).and_then(|f| f.metadata().map(|m| (f, m.len()))) {
                Ok((file, size)) => {
                    current = Some(Current {
                        version,
                        index: request.index,
                        file,
                        size,
                        stats: Stats::default(),
                    });
                }
                Err(error) => {
                    warn!(path = %path.display(), %error, "file requested by the remote could not be opened");
                    reply(proxy, FileContentsResponse::new_error(request.stream_id));
                    continue;
                }
            }
        }
        let c = current.as_mut().expect("opened above");

        let response = if request.flags.contains(FileContentsFlags::SIZE) {
            FileContentsResponse::new_size_response(request.stream_id, c.size)
        } else {
            match read_at(&c.file, request.position, request.requested_size) {
                Ok(data) => {
                    let s = &mut c.stats;
                    s.started.get_or_insert(arrived);
                    if let Some(last) = s.last_reply {
                        s.waiting += arrived.saturating_duration_since(last);
                    }
                    s.requests += 1;
                    s.bytes += data.len() as u64;
                    FileContentsResponse::new_data_response(request.stream_id, data)
                }
                Err(error) => {
                    warn!(path = %path.display(), %error, "file requested by the remote could not be read");
                    FileContentsResponse::new_error(request.stream_id)
                }
            }
        };
        let finished = !request.flags.contains(FileContentsFlags::SIZE)
            && request.position + u64::from(request.requested_size) >= c.size;
        reply(proxy, response);
        let now = Instant::now();
        c.stats.busy += now.saturating_duration_since(arrived);
        c.stats.last_reply = Some(now);
        if finished && c.stats.requests > 0 {
            c.stats.log(&path, c.size);
            c.stats = Stats::default();
        }
    }
}

fn path_of(offered: &Mutex<Offered>, version: u64, index: i32) -> Option<PathBuf> {
    let offered = offered.lock().unwrap_or_else(PoisonError::into_inner);
    (offered.version == version)
        .then(|| usize::try_from(index).ok().and_then(|i| offered.paths.get(i)).cloned())
        .flatten()
}

fn reply(proxy: &SharedProxy, response: FileContentsResponse<'static>) {
    proxy.send_clipboard_message(ClipboardMessage::SendFileContentsResponse(response));
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
        assert_eq!(
            read_at(&File::open(&files.paths[3]).unwrap(), 8, 100).unwrap(),
            vec![7u8; 2]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Collects what the server would receive.
    #[derive(Debug, Clone, Default)]
    struct Sink(Arc<Mutex<Vec<ClipboardMessage>>>);

    impl ClipboardMessageProxy for Sink {
        fn send_clipboard_message(&self, message: ClipboardMessage) {
            self.0.lock().unwrap().push(message);
        }
    }

    #[test]
    fn serves_sizes_and_ranges_off_thread() {
        let dir = std::env::temp_dir().join(format!("rdp-clipboard-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.bin");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let sink = Sink::default();
        let server = FileServer::new(SharedProxy::new(sink.clone()));
        server.offer(vec![path]);
        let request = |stream_id, flags, position, requested_size| FileContentsRequest {
            stream_id,
            index: 0,
            flags,
            position,
            requested_size,
            data_id: None,
        };
        server.serve(request(1, FileContentsFlags::SIZE, 0, 8));
        server.serve(request(2, FileContentsFlags::RANGE, 0, 65_536));
        server.serve(request(3, FileContentsFlags::RANGE, 65_536, 65_536));
        server.serve(FileContentsRequest {
            index: 7,
            ..request(4, FileContentsFlags::RANGE, 0, 10)
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while sink.0.lock().unwrap().len() < 4 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let replies: Vec<(u32, bool, Vec<u8>)> = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|m| match m {
                ClipboardMessage::SendFileContentsResponse(r) => (r.stream_id(), r.is_error(), r.data().to_vec()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(replies[0], (1, false, 100_000u64.to_le_bytes().to_vec()));
        assert_eq!(replies[1].2, data[..65_536]);
        assert_eq!(
            replies[2].2,
            data[65_536..],
            "the last range stops at the end of the file"
        );
        assert_eq!((replies[3].0, replies[3].1), (4, true), "unknown index");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
