//! Files from the other end of an RDP session, offered to Windows apps as an OLE data
//! object, for pasting and dropping.
//!
//! Explorer reads `FileGroupDescriptorW` (the list, with paths relative to where the
//! files land) and then one `FileContents` stream per file: the way it pastes or drops
//! Outlook attachments. Contents come from a [`VirtualFiles`] source as they're read:
//! the clipboard channel (CLIPRDR) or a companion transfer ([`InflowFiles`]).
//!
//! Objects should live on an STA thread that pumps messages. Reads block until data
//! arrives, letting other threads' sent messages through meanwhile
//! ([`pump_sent_messages`]), so a blocked read never stalls a thread that's waiting on
//! this one (`EmptyClipboard` sends `WM_DESTROYCLIPBOARD` to the owner, for example).
#![cfg(windows)]

use core::mem::ManuallyDrop;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rdp_companion_proto::Entry;
use rdp_companion_proto::transfer::{Inflow, ReadError};
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
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, ReleaseStgMedium};
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_PREFERREDDROPEFFECT, SHCreateStdEnumFmtEtc,
};
use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_NOREMOVE, PM_QS_SENDMESSAGE, PeekMessageW};
use windows::core::{BOOL, HRESULT, HSTRING, PWSTR, Ref, implement};

/// An entry size meaning "not known up front": the stream asks [`VirtualFiles::size`].
pub const UNKNOWN_SIZE: u64 = u64::MAX;

/// How often a blocked read lets other threads' sent messages through.
pub const PUMP_INTERVAL: Duration = Duration::from_millis(50);

/// Size of a FILEDESCRIPTORW.
const DESCRIPTOR_SIZE: usize = 592;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where a data object's files come from.
pub trait VirtualFiles: Send + Sync + 'static {
    /// The files and folders, folders before their contents (may block to fetch them).
    fn entries(&self) -> Result<Arc<[Entry]>, String>;

    /// Size of file `index`; only asked when its entry says [`UNKNOWN_SIZE`].
    fn size(&self, index: u32) -> Result<u64, String> {
        let entries = self.entries()?;
        usize::try_from(index)
            .ok()
            .and_then(|i| entries.get(i))
            .map(|e| e.size)
            .ok_or_else(|| "no such file".to_owned())
    }

    /// Reads file `index` at `position` into `out`, blocking until some data arrives.
    /// Returns 0 at the end of the file.
    fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String>;

    /// The app let go of file `index`'s stream (drop any read-ahead).
    fn release(&self, _index: u32) {}
}

/// Extras a data object offers besides the files.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Also offer this registered format (empty): lets a clipboard watcher recognize the
    /// object as ours.
    pub marker: Option<&'static str>,
}

/// An OLE data object serving `files`.
pub fn data_object(files: Arc<dyn VirtualFiles>, options: Options) -> IDataObject {
    FilesObject {
        files,
        formats: Formats::register(options.marker),
    }
    .into()
}

/// Lets messages other threads sent to this thread's windows through, without
/// dispatching posted ones (which would re-enter COM).
pub fn pump_sent_messages() {
    let mut msg = MSG::default();
    // SAFETY: plain message queue query; `msg` outlives the call.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
    }
}

/// A companion transfer's files, read as they stream in.
pub struct InflowFiles {
    inflow: Arc<Inflow>,
}

impl InflowFiles {
    /// Longest a read waits for data before failing the stream.
    const STALL: Duration = Duration::from_secs(60);

    pub fn new(inflow: Arc<Inflow>) -> Self {
        Self { inflow }
    }
}

/// The app let go of the files (the clipboard moved on, the drop finished): the transfer
/// is over, so the source can stop serving it.
impl Drop for InflowFiles {
    fn drop(&mut self) {
        self.inflow.finish(None);
    }
}

impl VirtualFiles for InflowFiles {
    fn entries(&self) -> Result<Arc<[Entry]>, String> {
        Ok(Arc::clone(self.inflow.entries()))
    }

    fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
        let started = Instant::now();
        loop {
            match self.inflow.read(index, position, out, PUMP_INTERVAL) {
                Err(ReadError::TimedOut) if started.elapsed() < Self::STALL => pump_sent_messages(),
                Ok(n) => return Ok(n),
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

/// Registered ids of the formats a data object serves.
#[derive(Clone, Copy, Debug)]
struct Formats {
    descriptor: u16,
    contents: u16,
    drop_effect: u16,
    marker: Option<u16>,
}

fn register(name: &HSTRING) -> u16 {
    // SAFETY: registering a format by name.
    u16::try_from(unsafe { RegisterClipboardFormatW(name) }).unwrap_or(0)
}

impl Formats {
    fn register(marker: Option<&str>) -> Self {
        // SAFETY: the CFSTR_ constants are static, NUL-terminated names.
        let named = |name: windows::core::PCWSTR| register(&HSTRING::from_wide(unsafe { name.as_wide() }));
        Self {
            descriptor: named(CFSTR_FILEDESCRIPTORW),
            contents: named(CFSTR_FILECONTENTS),
            drop_effect: named(CFSTR_PREFERREDDROPEFFECT),
            marker: marker.map(|name| register(&HSTRING::from(name))),
        }
    }

    fn all(&self) -> Vec<FORMATETC> {
        let entry = |format, tymed: i32| FORMATETC {
            cfFormat: format,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: tymed as u32,
        };
        let mut all = vec![
            entry(self.descriptor, TYMED_HGLOBAL.0),
            entry(self.contents, TYMED_ISTREAM.0),
            entry(self.drop_effect, TYMED_HGLOBAL.0),
        ];
        if let Some(marker) = self.marker {
            all.push(entry(marker, TYMED_HGLOBAL.0));
        }
        all
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Descriptor,
    Contents,
    DropEffect,
    Marker,
}

#[implement(IDataObject)]
struct FilesObject {
    files: Arc<dyn VirtualFiles>,
    formats: Formats,
}

impl FilesObject {
    fn kind(&self, format: &FORMATETC) -> windows::core::Result<Kind> {
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return Err(DV_E_DVASPECT.into());
        }
        let f = &self.formats;
        let (kind, tymed) = if format.cfFormat == f.descriptor {
            (Kind::Descriptor, TYMED_HGLOBAL)
        } else if format.cfFormat == f.contents {
            (Kind::Contents, TYMED_ISTREAM)
        } else if format.cfFormat == f.drop_effect {
            (Kind::DropEffect, TYMED_HGLOBAL)
        } else if f.marker == Some(format.cfFormat) {
            (Kind::Marker, TYMED_HGLOBAL)
        } else {
            return Err(DV_E_FORMATETC.into());
        };
        if format.tymed & tymed.0 as u32 == 0 {
            return Err(DV_E_TYMED.into());
        }
        Ok(kind)
    }

    fn entries(&self) -> windows::core::Result<Arc<[Entry]>> {
        self.files.entries().map_err(|error| {
            tracing::warn!(%error, "file list unavailable");
            E_FAIL.into()
        })
    }
}

impl IDataObject_Impl for FilesObject_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: COM passes a valid FORMATETC or null.
        let format = unsafe { format.as_ref() }.ok_or(E_POINTER)?;
        match self.kind(format)? {
            Kind::Descriptor => hglobal(&descriptor_bytes(&self.entries()?)),
            Kind::DropEffect => hglobal(&DROPEFFECT_COPY.0.to_le_bytes()),
            Kind::Marker => hglobal(&[0]),
            Kind::Contents => {
                let entries = self.entries()?;
                let index = u32::try_from(format.lindex).map_err(|_| DV_E_LINDEX)?;
                let entry = usize::try_from(index)
                    .ok()
                    .and_then(|i| entries.get(i))
                    .filter(|e| !e.dir)
                    .ok_or(DV_E_LINDEX)?;
                let size = if entry.size == UNKNOWN_SIZE {
                    self.files.size(index).map_err(|error| {
                        tracing::warn!(%error, index, "file size unavailable");
                        windows::core::Error::from(E_FAIL)
                    })?
                } else {
                    entry.size
                };
                let name = entry.path.rsplit('\\').next().unwrap_or(&entry.path).to_owned();
                let stream: IStream = FileStream {
                    files: Arc::clone(&self.files),
                    index,
                    name,
                    size,
                    modified: entry.modified,
                    position: Mutex::new(0),
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

    /// Explorer reports the outcome here ("Performed DropEffect" etc.); nothing to keep.
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

/// FILEGROUPDESCRIPTORW: the item count, then one FILEDESCRIPTORW per entry with its
/// path relative to the paste target.
pub fn descriptor_bytes(entries: &[Entry]) -> Vec<u8> {
    const FD_ATTRIBUTES: u32 = 0x4;
    const FD_WRITESTIME: u32 = 0x20;
    const FD_FILESIZE: u32 = 0x40;
    const FD_PROGRESSUI: u32 = 0x4000;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

    let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(4 + entries.len() * DESCRIPTOR_SIZE);
    out.extend_from_slice(&count.to_le_bytes());
    for entry in entries {
        let mut flags = FD_ATTRIBUTES | FD_PROGRESSUI;
        let attributes = if entry.dir {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_ARCHIVE
        };
        if entry.modified != 0 {
            flags |= FD_WRITESTIME;
        }
        let mut size = 0u64;
        if !entry.dir && entry.size != UNKNOWN_SIZE {
            flags |= FD_FILESIZE;
            size = entry.size;
        }
        let start = out.len();
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&[0; 16 + 8 + 8]); // clsid, sizel, pointl
        out.extend_from_slice(&attributes.to_le_bytes());
        out.extend_from_slice(&[0; 16]); // creation and last access time
        out.extend_from_slice(&entry.modified.to_le_bytes());
        out.extend_from_slice(&((size >> 32) as u32).to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
        for unit in entry.path.encode_utf16().take(259) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.resize(start + DESCRIPTOR_SIZE, 0);
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

/// One file, read sequentially from its source.
#[implement(IStream)]
struct FileStream {
    files: Arc<dyn VirtualFiles>,
    index: u32,
    name: String,
    size: u64,
    modified: u64,
    position: Mutex<u64>,
}

impl Drop for FileStream {
    fn drop(&mut self) {
        self.files.release(self.index);
    }
}

impl FileStream_Impl {
    /// Fills `out` unless the file ends first (a short read means "end" to some apps).
    fn read_into(&self, out: &mut [u8]) -> Result<usize, String> {
        let mut position = lock(&self.position);
        let mut done = 0;
        while done < out.len() && *position < self.size {
            let want = usize::try_from(self.size - *position)
                .unwrap_or(usize::MAX)
                .min(out.len() - done);
            match self.files.read(self.index, *position, &mut out[done..done + want])? {
                0 => break,
                n => {
                    done += n;
                    *position += n as u64;
                }
            }
        }
        Ok(done)
    }
}

impl ISequentialStream_Impl for FileStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return STG_E_INVALIDPOINTER;
        }
        // SAFETY: COM guarantees `pv` points to `cb` writable bytes.
        let out = unsafe { core::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let (read, result) = match self.read_into(out) {
            Ok(read) => (read, S_OK),
            Err(error) => {
                tracing::warn!(file = %self.name, %error, "file read failed");
                (0, STG_E_READFAULT)
            }
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

impl IStream_Impl for FileStream_Impl {
    fn Seek(&self, offset: i64, origin: STREAM_SEEK, new_position: *mut u64) -> windows::core::Result<()> {
        let mut position = lock(&self.position);
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => *position,
            STREAM_SEEK_END => self.size,
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let target = base.checked_add_signed(offset).ok_or(STG_E_INVALIDFUNCTION)?;
        *position = target;
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
        let mut buf = vec![0u8; 256 * 1024];
        let (mut read_total, mut written_total) = (0u64, 0u64);
        let mut result = Ok(());
        while read_total < cb {
            let want = usize::try_from((cb - read_total).min(buf.len() as u64)).unwrap_or(buf.len());
            let read = match self.read_into(&mut buf[..want]) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => {
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
        *out = STATSTG {
            pwcsName: if flag.0 & STATFLAG_NONAME.0 == 0 {
                co_task_string(&self.name)
            } else {
                PWSTR::null()
            },
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.size,
            mtime: FILETIME {
                dwLowDateTime: self.modified as u32,
                dwHighDateTime: (self.modified >> 32) as u32,
            },
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

/// Starts dragging `files` from this process (the primary button must be down, in the
/// window in front), on a thread of its own: the user drops them wherever they like
/// (Explorer copies them, reading the streams). `ended` hears what the drop did
/// (`DROPEFFECT` bits; 0 if cancelled). The thread keeps serving the files until the drop
/// target lets go of them.
///
/// The button was pressed in another thread's window (a webview's), so this thread's input
/// state doesn't know it's down, and the release goes there. A drag that never saw the
/// release would run forever, holding the mouse: no other app could drag until this one
/// quit. So the drag thread joins the input of the thread holding the button for the drag,
/// follows the physical button ([`ButtonSource`]), and a watchdog wakes the drag's loop once
/// the button comes up, whatever reached it.
pub fn drag(
    files: Arc<dyn VirtualFiles>,
    options: Options,
    ended: impl FnOnce(u32) + Send + 'static,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Ole::{DROPEFFECT_NONE, DoDragDrop, IDropSource, OleInitialize, OleUninitialize};
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId,
        PostMessageW, PostThreadMessageW, TranslateMessage, WM_KEYDOWN, WM_MOUSEMOVE,
    };

    // The thread whose window has the button (the one in front).
    // SAFETY: plain queries.
    let holder = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), None) };
    std::thread::Builder::new().name("drag-files".into()).spawn(move || {
        // SAFETY: OLE for this thread, paired with OleUninitialize; the objects live through
        // the modal drag and the loop that serves them afterwards. The input join lasts
        // only the drag.
        unsafe {
            if OleInitialize(None).is_err() {
                ended(0);
                return;
            }
            let thread = GetCurrentThreadId();
            let joined = holder != 0 && holder != thread && AttachThreadInput(thread, holder, true).as_bool();
            let data = data_object(Arc::new(QuitWhenReleased { files, thread }), options);
            let source: IDropSource = ButtonSource.into();
            let mut effect = DROPEFFECT_NONE;
            // Wakes the drag's loop when the button is up (or Esc down), in case the release
            // never reached it: the loop then asks the source, which ends the drag. If even
            // that doesn't end it soon, the drag's own window hears Esc (it cancels).
            let dragging = Arc::new(AtomicBool::new(true));
            let watchdog = {
                let dragging = Arc::clone(&dragging);
                std::thread::spawn(move || {
                    let mut released_at: Option<std::time::Instant> = None;
                    while dragging.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        if primary_button_down() && !escape_down() {
                            released_at = None;
                            continue;
                        }
                        let since = *released_at.get_or_insert_with(std::time::Instant::now);
                        let (message, key) = if since.elapsed() >= std::time::Duration::from_millis(500) {
                            (WM_KEYDOWN, WPARAM(usize::from(VK_ESCAPE.0)))
                        } else {
                            (WM_MOUSEMOVE, WPARAM(0))
                        };
                        let mut info = GUITHREADINFO {
                            cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
                            ..Default::default()
                        };
                        let capture = GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndCapture.is_invalid();
                        if capture {
                            let _ = PostMessageW(Some(info.hwndCapture), message, key, LPARAM(0));
                        } else {
                            let _ = PostThreadMessageW(thread, message, key, LPARAM(0));
                        }
                    }
                })
            };
            let result = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
            dragging.store(false, Ordering::SeqCst);
            let _ = watchdog.join();
            if joined {
                let _ = AttachThreadInput(thread, holder, false);
            }
            ended(if result == DRAGDROP_S_DROP { effect.0 } else { 0 });
            drop(data);
            // Explorer may read the files after the drop returns: serve until it lets go.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            OleUninitialize();
        }
    })?;
    Ok(())
}

const DRAGDROP_S_DROP: HRESULT = HRESULT(0x0004_0100);
const DRAGDROP_S_CANCEL: HRESULT = HRESULT(0x0004_0101);
const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = HRESULT(0x0004_0102);

/// Ends the drag thread's loop once nothing holds the files any more.
struct QuitWhenReleased {
    files: Arc<dyn VirtualFiles>,
    thread: u32,
}

impl VirtualFiles for QuitWhenReleased {
    fn entries(&self) -> Result<Arc<[Entry]>, String> {
        self.files.entries()
    }

    fn size(&self, index: u32) -> Result<u64, String> {
        self.files.size(index)
    }

    fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
        self.files.read(index, position, out)
    }

    fn release(&self, index: u32) {
        self.files.release(index);
    }
}

impl Drop for QuitWhenReleased {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
        // SAFETY: posting to the drag thread's own queue.
        let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

/// Whether the user's primary mouse button is physically down (the right one when the
/// buttons are swapped).
fn primary_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    // SAFETY: plain state queries (physical buttons).
    unsafe {
        let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        GetAsyncKeyState(i32::from(key.0)) < 0
    }
}

/// Whether Esc is physically down.
fn escape_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
    // SAFETY: plain state query.
    unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE.0)) < 0 }
}

/// A drag that follows the physical primary button, not the drag thread's idea of it
/// (which may never have seen it pressed or released): released drops, Esc cancels.
#[implement(windows::Win32::System::Ole::IDropSource)]
struct ButtonSource;

impl windows::Win32::System::Ole::IDropSource_Impl for ButtonSource_Impl {
    fn QueryContinueDrag(
        &self,
        escape: BOOL,
        _keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
    ) -> HRESULT {
        if escape.as_bool() || escape_down() {
            DRAGDROP_S_CANCEL
        } else if !primary_button_down() {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: windows::Win32::System::Ole::DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use windows::Win32::System::Com::{CoTaskMemFree, STATFLAG_DEFAULT, TYMED};

    use super::*;

    /// Files in memory, answering at most `max_read` bytes per read.
    struct Memory {
        entries: Arc<[Entry]>,
        contents: Vec<Vec<u8>>,
        max_read: usize,
        released: AtomicU32,
    }

    impl VirtualFiles for Memory {
        fn entries(&self) -> Result<Arc<[Entry]>, String> {
            Ok(Arc::clone(&self.entries))
        }

        fn size(&self, index: u32) -> Result<u64, String> {
            Ok(self.contents[index as usize].len() as u64)
        }

        fn read(&self, index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
            let data = &self.contents[index as usize];
            let start = (position as usize).min(data.len());
            let n = (data.len() - start).min(out.len()).min(self.max_read);
            out[..n].copy_from_slice(&data[start..start + n]);
            Ok(n)
        }

        fn release(&self, _index: u32) {
            self.released.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn entry(path: &str, dir: bool, size: u64) -> Entry {
        Entry {
            path: path.into(),
            dir,
            size,
            modified: 0,
        }
    }

    fn etc(format: u16, lindex: i32, tymed: TYMED) -> FORMATETC {
        FORMATETC {
            cfFormat: format,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex,
            tymed: tymed.0 as u32,
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
            assert!(
                read == step || out.len() + read as usize >= 1,
                "short reads only at the end"
            );
            if read == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..read as usize]);
        }
    }

    #[test]
    fn serves_the_list_and_streams_contents() {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let files = Arc::new(Memory {
            entries: vec![
                entry("dir", true, 0),
                entry("dir\\big.bin", false, big.len() as u64),
                entry("nosize.txt", false, UNKNOWN_SIZE),
            ]
            .into(),
            contents: vec![Vec::new(), big.clone(), b"hello".to_vec()],
            max_read: 10_000,
            released: AtomicU32::new(0),
        });
        let object = data_object(
            Arc::clone(&files) as Arc<dyn VirtualFiles>,
            Options {
                marker: Some("RdpApp.Test.Marker"),
            },
        );
        let formats = Formats::register(Some("RdpApp.Test.Marker"));

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
            assert_eq!(read_all(&stream, 65_536), big, "reads fill the buffer until the end");

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
            assert_eq!(files.released.load(Ordering::Relaxed), 1);

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
            assert_eq!(
                object.QueryGetData(&etc(formats.marker.unwrap(), -1, TYMED_HGLOBAL)),
                S_OK,
                "the marker is on offer"
            );
        }
    }

    #[test]
    fn descriptors_carry_relative_names_sizes_and_folders() {
        let entries = [
            entry("docs", true, 0),
            Entry {
                path: "docs\\a.txt".into(),
                dir: false,
                size: 0x1_0000_0005,
                modified: 0x0102_0304_0506_0708,
            },
        ];
        let bytes = descriptor_bytes(&entries);
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
