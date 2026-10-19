//! Files copied on the remote, offered to local apps (Explorer) as an OLE data object.
//!
//! Explorer reads `FileGroupDescriptorW` (the file list) and then one `FileContents`
//! stream per file. The list is fetched from the server on first use; each stream pulls
//! byte ranges (MS-RDPECLIP File Contents Request) as it is read. The object lives on its
//! own STA thread because reads block until the server answers, which must not stall the
//! UI thread.

use core::mem::ManuallyDrop;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, FileContentsFlags, FileContentsRequest,
    FileContentsResponse, FileDescriptor,
};
use tracing::{debug, warn};
use windows::Win32::Foundation::{
    DV_E_DVASPECT, DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_FAIL, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, FILETIME,
    GlobalFree, OLE_E_ADVISENOTSUPPORTED, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION, STG_E_INVALIDPOINTER,
    STG_E_READFAULT,
};
use windows::Win32::System::Com::{
    CoTaskMemAlloc, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG, STATFLAG_NONAME,
    STATSTG, STGC, STGM_READ, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END,
    STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::{GetClipboardOwner, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, OleInitialize, OleSetClipboard, OleUninitialize, ReleaseStgMedium};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_PREFERREDDROPEFFECT, SHCreateStdEnumFmtEtc,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowThreadProcessId, MSG, PM_NOREMOVE, PM_QS_SENDMESSAGE, PeekMessageW,
    PostThreadMessageW, TranslateMessage, WM_APP, WM_USER,
};
use windows::core::{BOOL, HRESULT, PWSTR, Ref, implement};

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
/// Requests kept in flight per stream, so the link stays busy while a chunk is consumed.
const DEPTH: usize = 4;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const RANGE_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a blocked wait lets other threads' sent messages (e.g. `WM_DESTROYCLIPBOARD`
/// from an `EmptyClipboard` on the UI thread) through, so they don't stall on this thread.
const PUMP_INTERVAL: Duration = Duration::from_millis(50);
/// Size of a FILEDESCRIPTORW.
const DESCRIPTOR_SIZE: usize = 592;

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
                .wait_timeout(state, (deadline - now).min(PUMP_INTERVAL))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            drop(state);
            pump_sent_messages();
            state = lock(&self.state);
        }
    }
}

/// Lets messages other threads sent to this thread's windows through, without
/// dispatching posted ones (which would re-enter COM).
fn pump_sent_messages() {
    let mut msg = MSG::default();
    // SAFETY: plain message queue query; `msg` outlives the call.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
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
    pub(crate) fn spawn(remote: Remote) -> Option<Self> {
        let (commands, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("clipboard-files".into())
            .spawn(move || run(remote, rx, ready_tx))
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

fn run(remote: Remote, commands: mpsc::Receiver<Command>, ready: mpsc::SyncSender<Option<u32>>) {
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

    let formats = Formats::register();
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
                        let object: IDataObject = RemoteDataObject {
                            remote: remote.clone(),
                            formats,
                            list_format,
                            files: Mutex::new(None),
                        }
                        .into();
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

/// Registered ids of the formats this object serves.
#[derive(Clone, Copy, Debug)]
struct Formats {
    descriptor: u16,
    contents: u16,
    drop_effect: u16,
}

impl Formats {
    fn register() -> Self {
        // SAFETY: registering a format by a static name.
        let id = |name| u16::try_from(unsafe { RegisterClipboardFormatW(name) }).unwrap_or(0);
        Self {
            descriptor: id(CFSTR_FILEDESCRIPTORW),
            contents: id(CFSTR_FILECONTENTS),
            drop_effect: id(CFSTR_PREFERREDDROPEFFECT),
        }
    }

    fn all(&self) -> [FORMATETC; 3] {
        let entry = |format, tymed: i32| FORMATETC {
            cfFormat: format,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: tymed as u32,
        };
        [
            entry(self.descriptor, TYMED_HGLOBAL.0),
            entry(self.contents, TYMED_ISTREAM.0),
            entry(self.drop_effect, TYMED_HGLOBAL.0),
        ]
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Descriptor,
    Contents,
    DropEffect,
}

#[implement(IDataObject)]
struct RemoteDataObject {
    remote: Remote,
    formats: Formats,
    /// The remote's id for `FileGroupDescriptorW`.
    list_format: ClipboardFormatId,
    files: Mutex<Option<Arc<[FileDescriptor]>>>,
}

impl RemoteDataObject {
    fn kind(&self, format: &FORMATETC) -> windows::core::Result<Kind> {
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return Err(DV_E_DVASPECT.into());
        }
        let (kind, tymed) = if format.cfFormat == self.formats.descriptor {
            (Kind::Descriptor, TYMED_HGLOBAL)
        } else if format.cfFormat == self.formats.contents {
            (Kind::Contents, TYMED_ISTREAM)
        } else if format.cfFormat == self.formats.drop_effect {
            (Kind::DropEffect, TYMED_HGLOBAL)
        } else {
            return Err(DV_E_FORMATETC.into());
        };
        if format.tymed & tymed.0 as u32 == 0 {
            return Err(DV_E_TYMED.into());
        }
        Ok(kind)
    }

    /// The remote file list, fetched on first use.
    fn files(&self) -> windows::core::Result<Arc<[FileDescriptor]>> {
        if let Some(files) = lock(&self.files).as_ref() {
            return Ok(Arc::clone(files));
        }
        // Not holding the lock while waiting: a nested GetData (a sent WM_RENDERFORMAT
        // let through by the wait) may fetch the list too.
        match self.remote.fetch_list(self.list_format) {
            Some(fetched) => {
                *lock(&self.files) = Some(Arc::clone(&fetched));
                Ok(fetched)
            }
            None => lock(&self.files).as_ref().map(Arc::clone).ok_or_else(|| E_FAIL.into()),
        }
    }
}

impl IDataObject_Impl for RemoteDataObject_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: COM passes a valid FORMATETC or null.
        let format = unsafe { format.as_ref() }.ok_or(E_POINTER)?;
        match self.kind(format)? {
            Kind::Descriptor => hglobal(&descriptor_bytes(&self.files()?)),
            Kind::DropEffect => hglobal(&DROPEFFECT_COPY.0.to_le_bytes()),
            Kind::Contents => {
                let files = self.files()?;
                let file = usize::try_from(format.lindex)
                    .ok()
                    .and_then(|i| files.get(i))
                    .filter(|f| !is_directory(f))
                    .ok_or(DV_E_LINDEX)?;
                let size = match file.file_size {
                    Some(size) => size,
                    None => self.remote.fetch_size(format.lindex).ok_or(E_FAIL)?,
                };
                let stream: IStream = RemoteStream {
                    remote: self.remote.clone(),
                    index: format.lindex,
                    name: file.name.clone(),
                    size,
                    modified: file.last_write_time,
                    reader: Mutex::new(Reader::default()),
                }
                .into();
                Ok(STGMEDIUM {
                    tymed: TYMED_ISTREAM.0 as u32,
                    u: STGMEDIUM_0 {
                        pstm: ManuallyDrop::new(Some(stream)),
                    },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            }
        }
    }

    fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        // SAFETY: COM passes a valid FORMATETC or null.
        match unsafe { format.as_ref() } {
            None => E_POINTER,
            Some(format) => self.kind(format).map_or_else(|e| e.code(), |_| S_OK),
        }
    }

    fn GetCanonicalFormatEtc(&self, _format: *const FORMATETC, _out: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    /// Explorer reports the paste outcome here ("Performed DropEffect" etc.); nothing to keep.
    fn SetData(&self, _format: *const FORMATETC, medium: *const STGMEDIUM, release: BOOL) -> windows::core::Result<()> {
        if release.as_bool() && !medium.is_null() {
            // SAFETY: with `release` set, ownership of the medium passed to this object.
            unsafe { ReleaseStgMedium(medium.cast_mut()) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        // SAFETY: the enumerator copies the array.
        unsafe { SHCreateStdEnumFmtEtc(&self.formats.all()) }
    }

    fn DAdvise(&self, _format: *const FORMATETC, _advf: u32, _sink: Ref<IAdviseSink>) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

fn is_directory(file: &FileDescriptor) -> bool {
    file.attributes
        .is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY))
}

/// FILEGROUPDESCRIPTORW: the item count, then one FILEDESCRIPTORW per entry with its
/// path relative to the paste target.
fn descriptor_bytes(files: &[FileDescriptor]) -> Vec<u8> {
    const FD_ATTRIBUTES: u32 = 0x4;
    const FD_WRITESTIME: u32 = 0x20;
    const FD_FILESIZE: u32 = 0x40;
    const FD_PROGRESSUI: u32 = 0x4000;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

    let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(4 + files.len() * DESCRIPTOR_SIZE);
    out.extend_from_slice(&count.to_le_bytes());
    for file in files {
        let mut flags = FD_ATTRIBUTES | FD_PROGRESSUI;
        let attributes = file.attributes.map_or(FILE_ATTRIBUTE_NORMAL, |a| a.bits());
        let mut write_time = 0u64;
        if let Some(time) = file.last_write_time {
            flags |= FD_WRITESTIME;
            write_time = time;
        }
        let mut size = 0u64;
        if let Some(file_size) = file.file_size.filter(|_| !is_directory(file)) {
            flags |= FD_FILESIZE;
            size = file_size;
        }
        let start = out.len();
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&[0; 16 + 8 + 8]); // clsid, sizel, pointl
        out.extend_from_slice(&attributes.to_le_bytes());
        out.extend_from_slice(&[0; 16]); // creation and last access time
        out.extend_from_slice(&write_time.to_le_bytes());
        out.extend_from_slice(&((size >> 32) as u32).to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
        let name = match &file.relative_path {
            Some(dir) => format!("{dir}\\{}", file.name),
            None => file.name.clone(),
        };
        let mut chars = 0;
        for unit in name.encode_utf16().take(259) {
            out.extend_from_slice(&unit.to_le_bytes());
            chars += 1;
        }
        out.resize(start + DESCRIPTOR_SIZE, 0);
        debug_assert!(chars < 260);
    }
    out
}

fn hglobal(bytes: &[u8]) -> windows::core::Result<STGMEDIUM> {
    // SAFETY: a fresh movable block, locked only while it is filled.
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let target = GlobalLock(handle);
        if target.is_null() {
            let _ = GlobalFree(Some(handle));
            return Err(E_OUTOFMEMORY.into());
        }
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(handle);
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: handle },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }
}

/// One remote file, read sequentially (with read-ahead) from the server.
#[implement(IStream)]
struct RemoteStream {
    remote: Remote,
    index: i32,
    name: String,
    size: u64,
    modified: Option<u64>,
    reader: Mutex<Reader>,
}

#[derive(Default)]
struct Reader {
    position: u64,
    /// Bytes at `[buf_start, buf_start + buf.len())`.
    buf_start: u64,
    buf: Vec<u8>,
    /// Requests sent and not consumed yet, in file order: (stream id, position, length).
    in_flight: VecDeque<(u32, u64, u32)>,
    /// Where the next request starts.
    next: u64,
}

impl Reader {
    fn read(&mut self, remote: &Remote, index: i32, size: u64, out: &mut [u8]) -> Result<usize, ()> {
        let mut done = 0;
        while done < out.len() && self.position < size {
            let buf_end = self.buf_start + self.buf.len() as u64;
            if (self.buf_start..buf_end).contains(&self.position) {
                let offset = usize::try_from(self.position - self.buf_start).map_err(|_| ())?;
                let take = (self.buf.len() - offset).min(out.len() - done);
                out[done..done + take].copy_from_slice(&self.buf[offset..offset + take]);
                done += take;
                self.position += take as u64;
                continue;
            }
            if self.in_flight.front().map(|r| r.1) != Some(self.position) {
                self.restart(remote, self.position); // first read, or after a seek
            }
            self.fill(remote, index, size);
            let (id, start, len) = self.in_flight.pop_front().ok_or(())?;
            let data = remote.take_response(id).ok_or(())?;
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
                // Later requests assumed a full chunk; ask again from where this one ended.
                self.restart(remote, start + data.len() as u64);
            }
            self.buf_start = start;
            self.buf = data;
        }
        Ok(done)
    }

    fn fill(&mut self, remote: &Remote, index: i32, size: u64) {
        while self.in_flight.len() < DEPTH && self.next < size {
            let len = u32::try_from((size - self.next).min(u64::from(CHUNK))).unwrap_or(CHUNK);
            let id = remote.request_range(index, self.next, len);
            self.in_flight.push_back((id, self.next, len));
            self.next += u64::from(len);
        }
    }

    fn restart(&mut self, remote: &Remote, at: u64) {
        remote.abandon(self.in_flight.drain(..).map(|r| r.0));
        self.next = at;
    }
}

impl Drop for RemoteStream {
    fn drop(&mut self) {
        let reader = self.reader.get_mut().unwrap_or_else(PoisonError::into_inner);
        self.remote.abandon(reader.in_flight.drain(..).map(|r| r.0));
    }
}

impl RemoteStream_Impl {
    fn read_into(&self, out: &mut [u8]) -> Result<usize, ()> {
        lock(&self.reader).read(&self.remote, self.index, self.size, out)
    }
}

impl ISequentialStream_Impl for RemoteStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return STG_E_INVALIDPOINTER;
        }
        // SAFETY: COM guarantees `pv` points to `cb` writable bytes.
        let out = unsafe { core::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let (read, result) = match self.read_into(out) {
            Ok(read) => (read, S_OK),
            Err(()) => (0, STG_E_READFAULT),
        };
        if !pcbread.is_null() {
            // SAFETY: optional out pointer supplied by the caller.
            unsafe { *pcbread = u32::try_from(read).unwrap_or(cb) };
        }
        result
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for RemoteStream_Impl {
    fn Seek(&self, offset: i64, origin: STREAM_SEEK, new_position: *mut u64) -> windows::core::Result<()> {
        let mut reader = lock(&self.reader);
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => reader.position,
            STREAM_SEEK_END => self.size,
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let target = base.checked_add_signed(offset).ok_or(STG_E_INVALIDFUNCTION)?;
        reader.position = target;
        if !new_position.is_null() {
            // SAFETY: optional out pointer supplied by the caller.
            unsafe { *new_position = target };
        }
        Ok(())
    }

    fn SetSize(&self, _size: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(
        &self,
        target: Ref<IStream>,
        cb: u64,
        pcbread: *mut u64,
        pcbwritten: *mut u64,
    ) -> windows::core::Result<()> {
        let target = target.ok()?;
        let mut buf = vec![0u8; 64 * 1024];
        let (mut read_total, mut written_total) = (0u64, 0u64);
        let mut result = Ok(());
        while read_total < cb {
            let want = usize::try_from((cb - read_total).min(buf.len() as u64)).unwrap_or(buf.len());
            let read = match self.read_into(&mut buf[..want]) {
                Ok(0) => break,
                Ok(read) => read,
                Err(()) => {
                    result = Err(STG_E_READFAULT.into());
                    break;
                }
            };
            read_total += read as u64;
            let mut written = 0u32;
            // SAFETY: `buf` holds `read` initialized bytes.
            let hr = unsafe { target.Write(buf.as_ptr().cast(), read as u32, Some(&mut written)) };
            written_total += u64::from(written);
            if hr.is_err() {
                result = Err(hr.into());
                break;
            }
        }
        // SAFETY: optional out pointers supplied by the caller.
        unsafe {
            if !pcbread.is_null() {
                *pcbread = read_total;
            }
            if !pcbwritten.is_null() {
                *pcbwritten = written_total;
            }
        }
        result
    }

    fn Commit(&self, _flags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _offset: u64, _cb: u64, _lock_type: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _offset: u64, _cb: u64, _lock_type: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, out: *mut STATSTG, flag: &STATFLAG) -> windows::core::Result<()> {
        // SAFETY: COM passes a valid STATSTG or null.
        let out = unsafe { out.as_mut() }.ok_or(STG_E_INVALIDPOINTER)?;
        let filetime = |t: u64| FILETIME {
            dwLowDateTime: t as u32,
            dwHighDateTime: (t >> 32) as u32,
        };
        *out = STATSTG {
            pwcsName: if flag.0 & STATFLAG_NONAME.0 == 0 {
                co_task_string(&self.name)
            } else {
                PWSTR::null()
            },
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.size,
            mtime: self.modified.map(filetime).unwrap_or_default(),
            grfMode: STGM_READ,
            ..Default::default()
        };
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

/// A NUL-terminated copy the caller frees with `CoTaskMemFree` (null if out of memory).
fn co_task_string(value: &str) -> PWSTR {
    let wide: Vec<u16> = value.encode_utf16().chain(core::iter::once(0)).collect();
    // SAFETY: the allocation is sized for `wide` and filled before it is handed out.
    unsafe {
        let ptr = CoTaskMemAlloc(wide.len() * 2).cast::<u16>();
        if ptr.is_null() {
            return PWSTR::null();
        }
        core::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        PWSTR(ptr)
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Com::{CoTaskMemFree, STATFLAG_DEFAULT, TYMED};

    use super::*;

    /// Answers requests the way the server's CLIPRDR channel would, synchronously.
    #[derive(Debug)]
    struct FakeServer {
        inbox: Arc<Inbox>,
        files: Vec<FileDescriptor>,
        contents: Vec<Vec<u8>>,
        /// Ranges come back at most this long (servers may answer short).
        max_chunk: usize,
    }

    impl ClipboardMessageProxy for FakeServer {
        fn send_clipboard_message(&self, message: ClipboardMessage) {
            match message {
                ClipboardMessage::SendInitiatePaste(_) => {
                    assert!(self.inbox.file_list(&self.files));
                }
                ClipboardMessage::SendFileContentsRequest(request) => {
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
        };
        let formats = Formats::register();
        let object: IDataObject = RemoteDataObject {
            remote: Remote {
                inbox: Arc::clone(&inbox),
                proxy: SharedProxy::new(server),
            },
            formats,
            list_format: ClipboardFormatId::new(0xC0FE),
            files: Mutex::new(None),
        }
        .into();
        let etc = |format, lindex, tymed: TYMED| FORMATETC {
            cfFormat: format,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex,
            tymed: tymed.0 as u32,
        };

        // SAFETY: COM calls on locally owned objects with valid arguments.
        unsafe {
            let mut medium = object.GetData(&etc(formats.descriptor, -1, TYMED_HGLOBAL)).unwrap();
            let block = GlobalLock(medium.u.hGlobal).cast::<u8>();
            let count = u32::from_le_bytes(core::slice::from_raw_parts(block, 4).try_into().unwrap());
            let _ = GlobalUnlock(medium.u.hGlobal);
            ReleaseStgMedium(&mut medium);
            assert_eq!(count, 3);

            let mut medium = object.GetData(&etc(formats.contents, 1, TYMED_ISTREAM)).unwrap();
            let stream = ManuallyDrop::take(&mut medium.u.pstm).unwrap();
            assert_eq!(
                read_all(&stream, 100_000),
                big,
                "short answers and read-ahead keep the order"
            );

            let mut stat = STATSTG::default();
            stream.Stat(&mut stat, STATFLAG_DEFAULT).unwrap();
            assert_eq!(stat.cbSize, big.len() as u64);
            assert_eq!(stat.pwcsName.to_string().unwrap(), "big.bin");
            CoTaskMemFree(Some(stat.pwcsName.0 as *const _));

            stream.Seek(10, STREAM_SEEK_SET, None).unwrap();
            let mut five = [0u8; 5];
            stream.Read(five.as_mut_ptr().cast(), 5, None).ok().unwrap();
            assert_eq!(five, big[10..15]);
            drop(stream);

            let mut medium = object.GetData(&etc(formats.contents, 2, TYMED_ISTREAM)).unwrap();
            let stream = ManuallyDrop::take(&mut medium.u.pstm).unwrap();
            assert_eq!(read_all(&stream, 3), b"hello", "size asked for when the list has none");

            let folder = object.GetData(&etc(formats.contents, 0, TYMED_ISTREAM));
            assert_eq!(folder.err().map(|e| e.code()), Some(DV_E_LINDEX));
            assert_eq!(object.QueryGetData(&etc(1, -1, TYMED_HGLOBAL)), DV_E_FORMATETC);
            assert_eq!(
                object.QueryGetData(&etc(formats.contents, 1, TYMED_HGLOBAL)),
                DV_E_TYMED
            );
        }
        assert!(lock(&inbox.state).waiting.is_empty(), "nothing left in flight");
    }

    #[test]
    fn a_closed_inbox_fails_waits_at_once() {
        let inbox = Arc::new(Inbox::default());
        inbox.close();
        let started = Instant::now();
        assert!(inbox.wait(Duration::from_secs(5), |_| None::<()>).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn descriptors_carry_relative_names_sizes_and_folders() {
        let files = [
            FileDescriptor::new("docs").with_attributes(ClipboardFileAttributes::DIRECTORY),
            FileDescriptor::new("a.txt")
                .with_relative_path("docs")
                .with_attributes(ClipboardFileAttributes::ARCHIVE)
                .with_file_size(0x1_0000_0005)
                .with_last_write_time(0x0102_0304_0506_0708),
        ];
        let bytes = descriptor_bytes(&files);
        assert_eq!(bytes.len(), 4 + 2 * DESCRIPTOR_SIZE);
        assert_eq!(&bytes[..4], &2u32.to_le_bytes());

        let field = |entry: usize, offset: usize| {
            let at = 4 + entry * DESCRIPTOR_SIZE + offset;
            u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
        };
        let name = |entry: usize| {
            let at = 4 + entry * DESCRIPTOR_SIZE + 72;
            let units: Vec<u16> = bytes[at..at + 520]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            String::from_utf16(&units).unwrap()
        };
        // Folder: attributes, no size.
        assert_eq!(field(0, 0) & 0x40, 0);
        assert_eq!(field(0, 36), 0x10);
        assert_eq!(name(0), "docs");
        // File: size split high/low, write time, relative name.
        assert_eq!(field(1, 0), 0x4 | 0x20 | 0x40 | 0x4000);
        assert_eq!(field(1, 64), 1);
        assert_eq!(field(1, 68), 5);
        assert_eq!(field(1, 56), 0x0506_0708);
        assert_eq!(field(1, 60), 0x0102_0304);
        assert_eq!(name(1), "docs\\a.txt");
    }
}
