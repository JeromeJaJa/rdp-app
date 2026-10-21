//! Files copied on the remote, offered to local apps (Explorer) as an OLE data object.
//!
//! Explorer reads the file list and then one stream per file ([`rdp_vfiles`]). Where a
//! companion is attached and can read the remote clipboard's files, they stream through
//! it; otherwise the list is fetched from the server on first use and each stream pulls
//! byte ranges (MS-RDPECLIP File Contents Request) as it is read. The object lives on its
//! own STA thread because reads block until data arrives, which must not stall the UI
//! thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, FileContentsFlags, FileContentsRequest,
    FileContentsResponse, FileDescriptor,
};
use rdp_companion_proto::Entry;
use rdp_vfiles::{InflowFiles, UNKNOWN_SIZE, VirtualFiles, pump_sent_messages};
use tracing::{debug, info, warn};
use windows::Win32::System::DataExchange::GetClipboardOwner;
use windows::Win32::System::Ole::{OleInitialize, OleSetClipboard, OleUninitialize};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowThreadProcessId, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    TranslateMessage, WM_APP, WM_USER,
};

use crate::Companion;

/// Clipboard format names that only make sense together with file streaming.
pub(crate) const FILE_FORMAT_NAMES: [&str; 3] = ["FileGroupDescriptorW", "FileGroupDescriptor", "FileContents"];
/// Name of the format carrying the file list.
pub(crate) const FILE_LIST_FORMAT_NAME: &str = "FileGroupDescriptorW";

pub(crate) fn is_file_format(format: &ClipboardFormat) -> bool {
    format
        .name()
        .is_some_and(|name| FILE_FORMAT_NAMES.contains(&name.value()))
}

/// Bytes asked for per File Contents Request.
const CHUNK: u32 = 512 * 1024;
/// Smallest request size, however little the server returns per reply.
const MIN_CHUNK: u32 = 16 * 1024;
/// Requests kept in flight per stream, so the link stays busy while a chunk is consumed.
const DEPTH: usize = 4;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const RANGE_TIMEOUT: Duration = Duration::from_secs(60);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The session's clipboard proxy, shared by the UI thread and the transfer thread.
#[derive(Clone, Debug)]
pub(crate) struct SharedProxy(Arc<Mutex<Box<dyn ClipboardMessageProxy>>>);

impl SharedProxy {
    pub(crate) fn new(proxy: impl ClipboardMessageProxy + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(proxy))))
    }
}

impl ClipboardMessageProxy for SharedProxy {
    fn send_clipboard_message(&self, message: ClipboardMessage) {
        lock(&self.0).send_clipboard_message(message);
    }
}

/// Server answers for transfers in flight. Filled on the session thread by the CLIPRDR
/// backend, consumed by the transfer thread.
#[derive(Debug, Default)]
pub(crate) struct Inbox {
    state: Mutex<InboxState>,
    changed: Condvar,
    /// Thread owning this session's OLE clipboard object (0 before it starts). Clipboard
    /// changes made from it are this session's own and must not be echoed to the server.
    owner_thread: AtomicU32,
    /// Bumped for every remote format list, so a late offer of an older list is dropped.
    generation: AtomicU64,
}

#[derive(Debug, Default)]
struct InboxState {
    closed: bool,
    list_pending: bool,
    /// `Some(None)`: the server failed the list request.
    list: Option<Option<Arc<[FileDescriptor]>>>,
    waiting: HashSet<u32>,
    /// `None`: the server failed the request.
    responses: HashMap<u32, Option<Vec<u8>>>,
}

impl Inbox {
    /// The list a pending request asked for. Returns false if nobody is waiting for one.
    pub(crate) fn file_list(&self, files: &[FileDescriptor]) -> bool {
        let mut state = lock(&self.state);
        if !state.list_pending {
            return false;
        }
        state.list_pending = false;
        state.list = Some(Some(files.into()));
        self.changed.notify_all();
        true
    }

    /// A failed format data response; consumed if a file list is being waited for.
    pub(crate) fn list_failed(&self) -> bool {
        let mut state = lock(&self.state);
        if !state.list_pending {
            return false;
        }
        state.list_pending = false;
        state.list = Some(None);
        self.changed.notify_all();
        true
    }

    pub(crate) fn file_contents(&self, response: &FileContentsResponse<'_>) {
        let mut state = lock(&self.state);
        let id = response.stream_id();
        if state.waiting.remove(&id) {
            let data = (!response.is_error()).then(|| response.data().to_vec());
            state.responses.insert(id, data);
            self.changed.notify_all();
        }
    }

    /// The session is gone: fail every wait now and from here on.
    pub(crate) fn close(&self) {
        lock(&self.state).closed = true;
        self.changed.notify_all();
    }

    pub(crate) fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether `owner` (the clipboard owner window) belongs to this session's OLE object.
    pub(crate) fn owns(&self, owner: windows::Win32::Foundation::HWND) -> bool {
        let thread = self.owner_thread.load(Ordering::SeqCst);
        // SAFETY: plain query; an invalid handle yields 0.
        thread != 0 && !owner.is_invalid() && unsafe { GetWindowThreadProcessId(owner, None) } == thread
    }

    /// Waits until `ready` yields a value, the deadline passes, or the inbox closes.
    fn wait<T>(&self, timeout: Duration, mut ready: impl FnMut(&mut InboxState) -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            if let Some(value) = ready(&mut state) {
                return Some(value);
            }
            let now = Instant::now();
            if state.closed || now >= deadline {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, (deadline - now).min(rdp_vfiles::PUMP_INTERVAL))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            drop(state);
            pump_sent_messages();
            state = lock(&self.state);
        }
    }
}

/// What the transfer thread needs to reach the server.
#[derive(Clone, Debug)]
pub(crate) struct Remote {
    pub(crate) inbox: Arc<Inbox>,
    pub(crate) proxy: SharedProxy,
}

impl Remote {
    fn fetch_list(&self, format: ClipboardFormatId) -> Option<Arc<[FileDescriptor]>> {
        {
            let mut state = lock(&self.inbox.state);
            if state.closed {
                return None;
            }
            state.list_pending = true;
            state.list = None;
        }
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendInitiatePaste(format));
        let list = self.inbox.wait(LIST_TIMEOUT, |state| state.list.take());
        lock(&self.inbox.state).list_pending = false;
        match list {
            Some(Some(files)) => {
                debug!(files = files.len(), "remote file list received");
                Some(files)
            }
            Some(None) => {
                warn!("the remote refused the file list");
                None
            }
            None => {
                warn!("no file list from the remote");
                None
            }
        }
    }

    fn request_range(&self, index: i32, position: u64, len: u32) -> u32 {
        self.request(index, FileContentsFlags::RANGE, position, len)
    }

    /// Size of a file the list did not give one for.
    fn fetch_size(&self, index: i32) -> Option<u64> {
        let id = self.request(index, FileContentsFlags::SIZE, 0, 8);
        let data = self.take_response(id)?;
        Some(u64::from_le_bytes(data.get(..8)?.try_into().ok()?))
    }

    fn request(&self, index: i32, flags: FileContentsFlags, position: u64, len: u32) -> u32 {
        static NEXT_STREAM: AtomicU32 = AtomicU32::new(1);
        let stream_id = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
        lock(&self.inbox.state).waiting.insert(stream_id);
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendFileContentsRequest(FileContentsRequest {
                stream_id,
                index,
                flags,
                position,
                requested_size: len,
                data_id: None,
            }));
        stream_id
    }

    fn take_response(&self, stream_id: u32) -> Option<Vec<u8>> {
        let response = self
            .inbox
            .wait(RANGE_TIMEOUT, |state| state.responses.remove(&stream_id));
        if response.is_none() {
            lock(&self.inbox.state).waiting.remove(&stream_id);
        }
        response.flatten()
    }

    fn abandon(&self, stream_ids: impl IntoIterator<Item = u32>) {
        let mut state = lock(&self.inbox.state);
        for id in stream_ids {
            state.waiting.remove(&id);
            state.responses.remove(&id);
        }
    }
}

/// Runs this session's OLE clipboard object on a dedicated STA thread.
pub(crate) struct OleClipboard {
    thread_id: u32,
    commands: mpsc::Sender<Command>,
}

enum Command {
    Offer {
        generation: u64,
        list_format: ClipboardFormatId,
    },
    Quit,
}

impl OleClipboard {
    pub(crate) fn spawn(remote: Remote, companion: Option<Arc<dyn Companion>>) -> Option<Self> {
        let (commands, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("clipboard-files".into())
            .spawn(move || run(remote, companion, rx, ready_tx))
            .ok()?;
        let thread_id = ready_rx.recv_timeout(Duration::from_secs(5)).ok()??;
        Some(Self { thread_id, commands })
    }

    /// Puts the remote's files on the local clipboard (asynchronously: OLE's
    /// `EmptyClipboard` may need the UI thread to answer `WM_DESTROYCLIPBOARD`).
    pub(crate) fn offer(&self, generation: u64, list_format: ClipboardFormatId) {
        self.send(Command::Offer {
            generation,
            list_format,
        });
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // SAFETY: plain thread message post; the queue exists before `spawn` returns.
            if let Err(error) =
                unsafe { PostThreadMessageW(self.thread_id, WM_APP, Default::default(), Default::default()) }
            {
                warn!(%error, "could not wake the clipboard file thread");
            }
        }
    }
}

impl Drop for OleClipboard {
    fn drop(&mut self) {
        self.send(Command::Quit);
    }
}

fn run(
    remote: Remote,
    companion: Option<Arc<dyn Companion>>,
    commands: mpsc::Receiver<Command>,
    ready: mpsc::SyncSender<Option<u32>>,
) {
    // SAFETY: initializes OLE for this thread; paired with `OleUninitialize` below.
    if let Err(error) = unsafe { OleInitialize(None) } {
        warn!(%error, "OLE unavailable; files copied on the remote cannot be pasted locally");
        let _ = ready.send(None);
        return;
    }
    let mut msg = MSG::default();
    // SAFETY: creates this thread's message queue before anyone posts to it.
    let thread_id = unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        GetCurrentThreadId()
    };
    remote.inbox.owner_thread.store(thread_id, Ordering::SeqCst);
    let _ = ready.send(Some(thread_id));

    'pump: loop {
        // SAFETY: standard message loop on this thread.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        if msg.hwnd.is_invalid() && msg.message == WM_APP {
            while let Ok(command) = commands.try_recv() {
                match command {
                    Command::Offer {
                        generation,
                        list_format,
                    } => {
                        if generation != remote.inbox.generation.load(Ordering::SeqCst) {
                            continue; // the remote clipboard changed again meanwhile
                        }
                        let files = PastedFiles {
                            cliprdr: CliprdrFiles::new(remote.clone(), list_format),
                            companion: companion.clone(),
                            via: Mutex::new(None),
                        };
                        let object = rdp_vfiles::data_object(Arc::new(files), rdp_vfiles::Options::default());
                        // SAFETY: called on this STA thread, which pumps messages for OLE.
                        match unsafe { OleSetClipboard(&object) } {
                            Ok(()) => debug!("remote files offered on the local clipboard"),
                            Err(error) => warn!(%error, "could not offer the remote files locally"),
                        }
                    }
                    Command::Quit => break 'pump,
                }
            }
            continue;
        }
        // SAFETY: dispatching a message retrieved by `GetMessageW` on this thread.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // The session is gone and its files with it; leave the clipboard empty rather than
    // holding an object nobody can read (but never touch someone else's clipboard).
    // SAFETY: plain queries and OLE calls on the thread that initialized OLE.
    unsafe {
        if let Ok(owner) = GetClipboardOwner()
            && remote.inbox.owns(owner)
        {
            let _ = OleSetClipboard(None);
        }
        OleUninitialize();
    }
}

/// The remote's copied files: through the companion when it can serve them (decided on
/// first use, when something is pasted), otherwise over the clipboard channel.
struct PastedFiles {
    cliprdr: CliprdrFiles,
    companion: Option<Arc<dyn Companion>>,
    /// Decided on first use: `Some(Some(_))` through the companion.
    via: Mutex<Option<Option<Arc<InflowFiles>>>>,
}

impl PastedFiles {
    /// The companion's files, if it serves this paste. Asks it on first use (without
    /// holding the lock: the wait lets sent messages through, which can call back in).
    fn companion(&self) -> Option<Arc<InflowFiles>> {
        if let Some(chosen) = lock(&self.via).clone() {
            return chosen;
        }
        let inflow = self.companion.as_ref().and_then(|c| c.remote_files());
        match &inflow {
            Some(inflow) => info!(files = inflow.entries().len(), "pasting through the companion"),
            None => debug!("pasting over the clipboard channel"),
        }
        let mut via = lock(&self.via);
        via.get_or_insert_with(|| inflow.map(|i| Arc::new(InflowFiles::new(i))))
            .clone()
    }
}

impl VirtualFiles for PastedFiles {
    fn entries(&self) -> Result<Arc<[Entry]>, String> {
        match self.companion() {
            Some(files) => files.entries(),
            None => self.cliprdr.entries(),
        }
    }

    fn size(&self, index: u32) -> Result<u64, String> {
        match self.companion() {
            Some(files) => files.size(index),
            None => self.cliprdr.size(index),
        }
    }

    fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
        match self.companion() {
            Some(files) => files.read(index, position, out),
            None => self.cliprdr.read(index, position, out),
        }
    }

    fn release(&self, index: u32) {
        self.cliprdr.release(index);
    }
}

/// The remote's files over the clipboard channel: the list on first use, then byte
/// ranges with a few requests kept in flight per stream.
struct CliprdrFiles {
    remote: Remote,
    /// The remote's id for `FileGroupDescriptorW`.
    list_format: ClipboardFormatId,
    entries: Mutex<Option<Arc<[Entry]>>>,
    sizes: Mutex<HashMap<u32, u64>>,
    /// Read-ahead per open stream (taken out while a read blocks).
    readers: Mutex<HashMap<u32, Reader>>,
}

impl CliprdrFiles {
    fn new(remote: Remote, list_format: ClipboardFormatId) -> Self {
        Self {
            remote,
            list_format,
            entries: Mutex::new(None),
            sizes: Mutex::new(HashMap::new()),
            readers: Mutex::new(HashMap::new()),
        }
    }

    fn file_size(&self, index: u32) -> Result<u64, String> {
        let entries = self.entries()?;
        let entry = usize::try_from(index)
            .ok()
            .and_then(|i| entries.get(i))
            .ok_or_else(|| "no such file".to_owned())?;
        if entry.size != UNKNOWN_SIZE {
            return Ok(entry.size);
        }
        if let Some(size) = lock(&self.sizes).get(&index) {
            return Ok(*size);
        }
        let size = self
            .remote
            .fetch_size(i32::try_from(index).map_err(|_| "no such file")?)
            .ok_or_else(|| "the remote didn't report the file's size".to_owned())?;
        lock(&self.sizes).insert(index, size);
        Ok(size)
    }
}

/// A CLIPRDR file descriptor as a transfer entry.
fn entry(file: &FileDescriptor) -> Entry {
    let dir = file
        .attributes
        .is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY));
    Entry {
        path: match &file.relative_path {
            Some(parent) => format!("{parent}\\{}", file.name),
            None => file.name.clone(),
        },
        dir,
        size: if dir { 0 } else { file.file_size.unwrap_or(UNKNOWN_SIZE) },
        modified: file.last_write_time.unwrap_or(0),
    }
}

impl VirtualFiles for CliprdrFiles {
    fn entries(&self) -> Result<Arc<[Entry]>, String> {
        if let Some(entries) = lock(&self.entries).as_ref() {
            return Ok(Arc::clone(entries));
        }
        // Not holding the lock while waiting: a nested call (a sent WM_RENDERFORMAT let
        // through by the wait) may fetch the list too.
        match self.remote.fetch_list(self.list_format) {
            Some(files) => {
                let entries: Arc<[Entry]> = files.iter().map(entry).collect();
                *lock(&self.entries) = Some(Arc::clone(&entries));
                Ok(entries)
            }
            None => lock(&self.entries)
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| "the remote didn't send its file list".to_owned()),
        }
    }

    fn size(&self, index: u32) -> Result<u64, String> {
        self.file_size(index)
    }

    fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
        let size = self.file_size(index)?;
        let wire_index = i32::try_from(index).map_err(|_| "no such file")?;
        let mut reader = lock(&self.readers).remove(&index).unwrap_or_default();
        let result = reader.read(&self.remote, wire_index, size, position, out);
        if reader.stats.started.is_some()
            && reader
                .stats
                .last_progress
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            reader.stats.last_progress = Some(Instant::now());
            if reader.stats.requests > 4 {
                reader.stats.log(index, size, "receiving a file from the remote");
            }
        }
        lock(&self.readers).insert(index, reader);
        result.map_err(|()| "the remote didn't send the file's contents".to_owned())
    }

    fn release(&self, index: u32) {
        if let Some(mut reader) = lock(&self.readers).remove(&index) {
            self.remote.abandon(reader.in_flight.drain(..).map(|r| r.0));
            let size = lock(&self.sizes).get(&index).copied().unwrap_or(0);
            reader.stats.log(index, size, "file received from the remote");
        }
    }
}

#[derive(Default)]
struct Reader {
    /// Bytes at `[buf_start, buf_start + buf.len())`.
    buf_start: u64,
    buf: Vec<u8>,
    /// Requests sent and not consumed yet, in file order: (stream id, position, length, sent at).
    in_flight: VecDeque<(u32, u64, u32, Instant)>,
    /// Where the next request starts.
    next: u64,
    /// Request size: `CHUNK`, lowered to what the server actually returns per reply.
    chunk: Option<u32>,
    stats: ReadStats,
}

#[derive(Default)]
struct ReadStats {
    started: Option<Instant>,
    bytes: u64,
    requests: u32,
    short: u32,
    /// Sum of each request's time from being sent to its reply arriving.
    latency: Duration,
    /// Time spent blocked waiting for replies (vs. the app consuming data).
    waiting: Duration,
    last_progress: Option<Instant>,
}

impl ReadStats {
    fn log(&self, index: u32, size: u64, what: &str) {
        let Some(started) = self.started else { return };
        let secs = started.elapsed().as_secs_f64().max(0.001);
        let requests = self.requests.max(1);
        info!(
            index,
            size,
            received = self.bytes,
            requests = self.requests,
            short_replies = self.short,
            secs = format!("{secs:.2}"),
            mib_per_s = format!("{:.2}", self.bytes as f64 / 1_048_576.0 / secs),
            avg_request_ms = format!("{:.0}", self.latency.as_secs_f64() * 1000.0 / f64::from(requests)),
            waiting_pct = format!("{:.0}", self.waiting.as_secs_f64() * 100.0 / secs),
            in_flight = DEPTH,
            chunk_kib = CHUNK / 1024,
            "{what}"
        );
    }
}

impl Reader {
    fn read(&mut self, remote: &Remote, index: i32, size: u64, mut position: u64, out: &mut [u8]) -> Result<usize, ()> {
        let mut done = 0;
        while done < out.len() && position < size {
            let buf_end = self.buf_start + self.buf.len() as u64;
            if (self.buf_start..buf_end).contains(&position) {
                let offset = usize::try_from(position - self.buf_start).map_err(|_| ())?;
                let take = (self.buf.len() - offset).min(out.len() - done);
                out[done..done + take].copy_from_slice(&self.buf[offset..offset + take]);
                done += take;
                position += take as u64;
                continue;
            }
            if self.in_flight.front().map(|r| r.1) != Some(position) {
                self.restart(remote, position); // first read, or after a seek
            }
            self.fill(remote, index, size);
            let (id, start, len, sent) = self.in_flight.pop_front().ok_or(())?;
            self.stats.started.get_or_insert_with(Instant::now);
            let wait_from = Instant::now();
            let data = remote.take_response(id).ok_or(())?;
            self.stats.waiting += wait_from.elapsed();
            self.stats.latency += sent.elapsed();
            self.stats.requests += 1;
            self.stats.bytes += data.len() as u64;
            if data.is_empty() || data.len() > len as usize {
                warn!(
                    index,
                    start,
                    got = data.len(),
                    "unexpected file contents from the remote"
                );
                return Err(());
            }
            if data.len() < len as usize {
                // The server returns less per reply than asked: fetch the rest of this
                // range next (keeping later requests in flight) and ask for that much from
                // now on.
                self.stats.short += 1;
                let got = u32::try_from(data.len()).unwrap_or(len);
                let tail = start + u64::from(got);
                let id = remote.request_range(index, tail, len - got);
                self.in_flight.push_front((id, tail, len - got, Instant::now()));
                self.chunk = Some(got.max(MIN_CHUNK));
            }
            self.buf_start = start;
            self.buf = data;
        }
        Ok(done)
    }

    fn fill(&mut self, remote: &Remote, index: i32, size: u64) {
        while self.in_flight.len() < DEPTH && self.next < size {
            let chunk = self.chunk.unwrap_or(CHUNK);
            let len = u32::try_from((size - self.next).min(u64::from(chunk))).unwrap_or(chunk);
            let id = remote.request_range(index, self.next, len);
            self.in_flight.push_back((id, self.next, len, Instant::now()));
            self.next += u64::from(len);
        }
    }

    fn restart(&mut self, remote: &Remote, at: u64) {
        remote.abandon(self.in_flight.drain(..).map(|r| r.0));
        self.next = at;
    }
}

#[cfg(test)]
mod tests {
    use core::mem::ManuallyDrop;

    use windows::Win32::Foundation::DV_E_LINDEX;
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IStream, TYMED, TYMED_HGLOBAL, TYMED_ISTREAM};
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::ReleaseStgMedium;
    use windows::Win32::UI::Shell::{CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW};

    use super::*;

    /// Answers requests the way the server's CLIPRDR channel would, synchronously.
    #[derive(Debug)]
    struct FakeServer {
        inbox: Arc<Inbox>,
        files: Vec<FileDescriptor>,
        contents: Vec<Vec<u8>>,
        /// Ranges come back at most this long (servers may answer short).
        max_chunk: usize,
        requests: Arc<AtomicU32>,
    }

    impl ClipboardMessageProxy for FakeServer {
        fn send_clipboard_message(&self, message: ClipboardMessage) {
            match message {
                ClipboardMessage::SendInitiatePaste(_) => {
                    assert!(self.inbox.file_list(&self.files));
                }
                ClipboardMessage::SendFileContentsRequest(request) => {
                    self.requests.fetch_add(1, Ordering::Relaxed);
                    let data = &self.contents[usize::try_from(request.index).unwrap()];
                    let response = if request.flags.contains(FileContentsFlags::SIZE) {
                        FileContentsResponse::new_size_response(request.stream_id, data.len() as u64)
                    } else {
                        let start = usize::try_from(request.position).unwrap();
                        let len = (request.requested_size as usize).min(self.max_chunk);
                        let end = (start + len).min(data.len());
                        FileContentsResponse::new_data_response(request.stream_id, data[start..end].to_vec())
                    };
                    self.inbox.file_contents(&response);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn read_all(stream: &IStream, step: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; step as usize];
        loop {
            let mut read = 0u32;
            // SAFETY: `buf` has `step` writable bytes.
            unsafe { stream.Read(buf.as_mut_ptr().cast(), step, Some(&mut read)) }
                .ok()
                .unwrap();
            if read == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..read as usize]);
        }
    }

    fn format(name: windows::core::PCWSTR) -> u16 {
        // SAFETY: registering a static format name.
        u16::try_from(unsafe { RegisterClipboardFormatW(name) }).unwrap()
    }

    #[test]
    fn serves_the_remote_list_and_streams_file_contents() {
        let big: Vec<u8> = (0..CHUNK as usize * 2 + 12_345).map(|i| (i * 7 % 251) as u8).collect();
        let inbox = Arc::new(Inbox::default());
        let server = FakeServer {
            inbox: Arc::clone(&inbox),
            files: vec![
                FileDescriptor::new("dir").with_attributes(ClipboardFileAttributes::DIRECTORY),
                FileDescriptor::new("big.bin")
                    .with_relative_path("dir")
                    .with_file_size(big.len() as u64),
                FileDescriptor::new("nosize.txt"),
            ],
            contents: vec![Vec::new(), big.clone(), b"hello".to_vec()],
            max_chunk: CHUNK as usize - 1000,
            requests: Arc::default(),
        };
        let files = PastedFiles {
            cliprdr: CliprdrFiles::new(
                Remote {
                    inbox: Arc::clone(&inbox),
                    proxy: SharedProxy::new(server),
                },
                ClipboardFormatId::new(0xC0FE),
            ),
            companion: None,
            via: Mutex::new(None),
        };
        let object = rdp_vfiles::data_object(Arc::new(files), rdp_vfiles::Options::default());
        let etc = |format, lindex, tymed: TYMED| FORMATETC {
            cfFormat: format,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex,
            tymed: tymed.0 as u32,
        };
        let (descriptor, contents) = (format(CFSTR_FILEDESCRIPTORW), format(CFSTR_FILECONTENTS));

        // SAFETY: COM calls on locally owned objects with valid arguments.
        unsafe {
            let mut medium = object.GetData(&etc(descriptor, -1, TYMED_HGLOBAL)).unwrap();
            let block = GlobalLock(medium.u.hGlobal).cast::<u8>();
            let count = u32::from_le_bytes(core::slice::from_raw_parts(block, 4).try_into().unwrap());
            let _ = GlobalUnlock(medium.u.hGlobal);
            ReleaseStgMedium(&mut medium);
            assert_eq!(count, 3);

            let mut medium = object.GetData(&etc(contents, 1, TYMED_ISTREAM)).unwrap();
            let stream = ManuallyDrop::take(&mut medium.u.pstm).unwrap();
            assert_eq!(
                read_all(&stream, 100_000),
                big,
                "short answers and read-ahead keep the order"
            );
            drop(stream);

            let mut medium = object.GetData(&etc(contents, 2, TYMED_ISTREAM)).unwrap();
            let stream = ManuallyDrop::take(&mut medium.u.pstm).unwrap();
            assert_eq!(read_all(&stream, 3), b"hello", "size asked for when the list has none");
            drop(stream);

            let folder = object.GetData(&etc(contents, 0, TYMED_ISTREAM));
            assert_eq!(folder.err().map(|e| e.code()), Some(DV_E_LINDEX));
        }
        assert!(lock(&inbox.state).waiting.is_empty(), "nothing left in flight");
    }

    #[test]
    fn short_replies_shrink_requests_instead_of_resending() {
        const CAP: usize = 64 * 1024;
        let data: Vec<u8> = (0..CAP * 40 + 77).map(|i| (i % 253) as u8).collect();
        let inbox = Arc::new(Inbox::default());
        let requests = Arc::new(AtomicU32::new(0));
        let server = FakeServer {
            inbox: Arc::clone(&inbox),
            files: vec![FileDescriptor::new("f.bin").with_file_size(data.len() as u64)],
            contents: vec![data.clone()],
            max_chunk: CAP,
            requests: Arc::clone(&requests),
        };
        let remote = Remote {
            inbox,
            proxy: SharedProxy::new(server),
        };
        let mut reader = Reader::default();
        let mut out = vec![0u8; data.len()];
        let mut done = 0;
        while done < out.len() {
            done += reader
                .read(&remote, 0, data.len() as u64, done as u64, &mut out[done..])
                .unwrap();
        }
        assert_eq!(out, data);
        // One reply per 64 KiB, plus the first large requests that were already out.
        let needed = data.len().div_ceil(CAP) as u32;
        assert!(
            requests.load(Ordering::Relaxed) <= needed + DEPTH as u32,
            "{requests:?} requests"
        );
    }

    #[test]
    fn a_closed_inbox_fails_waits_at_once() {
        let inbox = Arc::new(Inbox::default());
        inbox.close();
        let started = Instant::now();
        assert!(inbox.wait(Duration::from_secs(5), |_| None::<()>).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
