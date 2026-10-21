//! File transfers over the companion channel, run the same way by both ends.
//!
//! The side holding the files is the *source*, the side receiving them the *sink*. The
//! source offers a list ([`Message::Offer`]); the sink asks for an entry from an offset
//! ([`Message::Fetch`]) and the source streams that entry and the ones after it, keeping
//! at most a [`Window`] of unacknowledged bytes in flight. The sink acknowledges what it
//! has taken off the wire ([`Message::Ack`]).
//!
//! Why not request each chunk, like the RDP clipboard does: one request in flight costs
//! a round trip per chunk, so a 70 ms link tops out near 1 MB/s whatever its bandwidth.
//! Streaming against a window keeps the link busy instead, all on the session's one
//! connection. Sizing the window to the link (bandwidth × round trip, plus a little)
//! keeps the queue short, so a big copy doesn't make the session itself laggy.
//!
//! Chunks are compressed (zstd) when that shrinks them: the session's link is usually
//! the limit, so text and documents move several times faster, while data that's already
//! compressed (archives, media) goes as is without spending much time trying.
//!
//! - [`Source`]: the sending state machine; a [`Pump`] thread drives a session's sources.
//! - [`Inflow`]: the receiving buffer; readers block on it (an OLE stream being pasted,
//!   or [`save`] writing files to disk).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{Entry, Message};

/// Bytes per [`Message::Data`].
pub const CHUNK: usize = 64 * 1024;
/// The sink acknowledges at least this often (and whenever it runs dry).
const ACK_EVERY: u64 = 64 * 1024;

/// zstd level for chunks: far faster than any link a session runs over (hundreds of MB/s
/// a core), and plain text typically shrinks to a quarter.
const ZSTD_LEVEL: i32 = 3;
/// A chunk that doesn't shrink below this share of its size goes as is...
const WORTH_IT: f64 = 0.9;
/// ...and this many after it aren't tried (the rest of an archive or video won't either).
const SKIP_AFTER_MISS: u32 = 16;

/// Compresses a source's chunks, backing off on data that doesn't compress.
struct Packer {
    cctx: zstd_safe::CCtx<'static>,
    skip: u32,
}

impl Packer {
    fn new() -> Self {
        Self {
            cctx: zstd_safe::CCtx::create(),
            skip: 0,
        }
    }

    /// `chunk` compressed, or `None` to send it as is.
    fn pack(&mut self, chunk: &[u8]) -> Option<Vec<u8>> {
        if self.skip > 0 {
            self.skip -= 1;
            return None;
        }
        let mut out = vec![0u8; zstd_safe::compress_bound(chunk.len())];
        match self.cctx.compress(&mut out[..], chunk, ZSTD_LEVEL) {
            Ok(n) if (n as f64) < chunk.len() as f64 * WORTH_IT => {
                out.truncate(n);
                Some(out)
            }
            _ => {
                self.skip = SKIP_AFTER_MISS;
                None
            }
        }
    }

    /// A new file: its data may compress differently.
    fn reset(&mut self) {
        self.skip = 0;
    }
}

/// The bytes a [`Message::Data`] carries, decompressed if they were compressed.
fn unpack(bytes: Vec<u8>, compressed: bool) -> Result<Vec<u8>, String> {
    if !compressed {
        return Ok(bytes);
    }
    let mut out = vec![0u8; CHUNK];
    let n = zstd_safe::decompress(&mut out[..], &bytes)
        .map_err(|code| format!("corrupt data ({})", zstd_safe::get_error_name(code)))?;
    out.truncate(n);
    Ok(out)
}

/// How much a transfer moved, for logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Throughput {
    /// Bytes of file data.
    pub bytes: u64,
    /// What they took on the wire (less when they compressed).
    pub wire: u64,
    /// From the first data to the last.
    pub elapsed: Duration,
}

impl Throughput {
    /// File data per second, in MiB.
    pub fn mib_per_s(&self) -> f64 {
        self.bytes as f64 / 1_048_576.0 / self.elapsed.as_secs_f64().max(0.001)
    }

    /// Wire bytes per byte of data (1 when nothing compressed).
    pub fn ratio(&self) -> f64 {
        if self.bytes == 0 {
            1.0
        } else {
            self.wire as f64 / self.bytes as f64
        }
    }

    fn add(&mut self, bytes: u64, wire: u64, first: &mut Option<Instant>, now: Instant) {
        self.bytes += bytes;
        self.wire += wire;
        let first = *first.get_or_insert(now);
        self.elapsed = now.saturating_duration_since(first);
    }
}

impl core::fmt::Display for Throughput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:.1} MiB in {:.1} s ({:.2} MiB/s, {:.0}% on the wire)",
            self.bytes as f64 / 1_048_576.0,
            self.elapsed.as_secs_f64(),
            self.mib_per_s(),
            self.ratio() * 100.0
        )
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many unacknowledged bytes a source may have in flight.
///
/// Tracks the delivery rate (bytes acknowledged per second, the best of the last few
/// seconds) and the shortest round trip seen, and allows a little over their product.
/// Starting small and growing by half each round trip finds a fast link's rate quickly;
/// staying near it means the bytes queued ahead of the session's own traffic (input
/// one way, graphics the other) add well under a round trip of delay.
#[derive(Debug)]
pub struct Window {
    limit: u64,
    /// (total bytes sent, when), for timing round trips as acks pass them.
    marks: VecDeque<(u64, Instant)>,
    min_rtt: Option<(Duration, Instant)>,
    last_ack: Option<(u64, Instant)>,
    rates: VecDeque<(Instant, f64)>,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            limit: Self::INITIAL,
            marks: VecDeque::new(),
            min_rtt: None,
            last_ack: None,
            rates: VecDeque::new(),
        }
    }
}

impl Window {
    pub const INITIAL: u64 = 256 * 1024;
    pub const MIN: u64 = 64 * 1024;
    pub const MAX: u64 = 16 * 1024 * 1024;
    /// Room over the measured bandwidth-delay product, so the rate can keep growing.
    const GAIN: f64 = 1.5;
    /// A round trip this old is re-measured (routes change).
    const RTT_EXPIRY: Duration = Duration::from_secs(10);
    /// Delivery rate is the best seen over this long.
    const RATE_WINDOW: Duration = Duration::from_secs(2);
    /// Shortest span a rate sample covers (acks can arrive in bursts).
    const RATE_INTERVAL: Duration = Duration::from_millis(25);
    const MAX_MARKS: usize = 4096;

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// What was in flight is gone: no round trips to time from it.
    pub fn forget_in_flight(&mut self) {
        self.marks.clear();
    }

    /// `total` bytes have now been sent.
    pub fn sent(&mut self, total: u64, now: Instant) {
        if self.marks.len() >= Self::MAX_MARKS {
            self.marks.pop_front();
        }
        self.marks.push_back((total, now));
    }

    /// The sink has taken `consumed` bytes in total.
    pub fn acked(&mut self, consumed: u64, now: Instant) {
        let mut rtt = None;
        while let Some(&(total, at)) = self.marks.front() {
            if total > consumed {
                break;
            }
            rtt = Some(now.saturating_duration_since(at));
            self.marks.pop_front();
        }
        if let Some(rtt) = rtt {
            let keep = self
                .min_rtt
                .is_some_and(|(min, at)| rtt >= min && now.saturating_duration_since(at) < Self::RTT_EXPIRY);
            if !keep {
                self.min_rtt = Some((rtt, now));
            }
        }

        match self.last_ack {
            None => self.last_ack = Some((consumed, now)),
            Some((before, at)) => {
                let span = now.saturating_duration_since(at);
                if span >= Self::RATE_INTERVAL && consumed > before {
                    self.rates
                        .push_back((now, (consumed - before) as f64 / span.as_secs_f64()));
                    self.last_ack = Some((consumed, now));
                }
            }
        }
        while self
            .rates
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) > Self::RATE_WINDOW)
        {
            self.rates.pop_front();
        }

        let rate = self.rates.iter().map(|&(_, r)| r).fold(0.0, f64::max);
        if let (Some((rtt, _)), true) = (self.min_rtt, rate > 0.0) {
            let bdp = rate * rtt.as_secs_f64();
            self.limit = ((bdp * Self::GAIN) as u64 + Self::MIN).clamp(Self::MIN, Self::MAX);
        }
    }
}

/// Opens offered files for reading.
pub trait Files: Send {
    /// Entry `index`, positioned at `offset`.
    fn open(&mut self, index: u32, offset: u64) -> io::Result<Box<dyn Read + Send>>;
}

/// Where a source is streaming from.
struct Cursor {
    index: u32,
    offset: u64,
    reader: Option<Box<dyn Read + Send>>,
    /// Stop at the end of this entry (the sink asked for just it).
    single: bool,
}

/// The sending side of one transfer.
pub struct Source {
    id: u64,
    entries: Arc<[Entry]>,
    files: Box<dyn Files>,
    seq: u32,
    cursor: Option<Cursor>,
    sent: u64,
    consumed: u64,
    window: Window,
    packer: Packer,
    /// What was sent (wire bytes) and when the sink took it, for logging.
    throughput: Throughput,
    first_sent: Option<Instant>,
    wire_sent: u64,
}

impl Source {
    pub fn new(id: u64, entries: Arc<[Entry]>, files: Box<dyn Files>) -> Self {
        Self {
            id,
            entries,
            files,
            seq: 0,
            cursor: None,
            sent: 0,
            consumed: 0,
            window: Window::default(),
            packer: Packer::new(),
            throughput: Throughput::default(),
            first_sent: None,
            wire_sent: 0,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Bytes the sink has taken so far (for progress).
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    /// What the sink has taken so far, and what it took on the wire.
    pub fn throughput(&self) -> Throughput {
        self.throughput
    }

    /// The sink asked for entry `index` from `offset`, and what follows unless `single`.
    pub fn fetch(&mut self, seq: u32, index: u32, offset: u64, single: bool) {
        self.seq = seq;
        self.cursor = Self::start(&self.entries, index).map(|first| Cursor {
            index: first,
            // Folders are skipped; an offset only applies to the entry asked for.
            offset: if first == index { offset } else { 0 },
            reader: None,
            single,
        });
    }

    /// The first file at or after `index`, if any.
    fn start(entries: &[Entry], index: u32) -> Option<u32> {
        let first = usize::try_from(index).ok()?;
        entries
            .iter()
            .enumerate()
            .skip(first)
            .find(|(_, e)| !e.dir)
            .and_then(|(i, _)| u32::try_from(i).ok())
    }

    pub fn ack(&mut self, consumed: u64, now: Instant) {
        if consumed > self.consumed {
            let consumed = consumed.min(self.sent);
            // Wire bytes are only known per chunk sent: count them in the same share.
            let wire = if self.sent == 0 {
                0
            } else {
                (self.wire_sent as f64 * consumed as f64 / self.sent as f64) as u64
            };
            let (bytes, wire) = (consumed - self.consumed, wire.saturating_sub(self.throughput.wire));
            self.throughput.add(bytes, wire, &mut self.first_sent, now);
            self.consumed = consumed;
            self.window.acked(self.consumed, now);
        }
    }

    fn window_open(&self) -> bool {
        self.sent.saturating_sub(self.consumed) < self.window.limit()
    }

    /// What was in flight went on a path that's gone (the direct link closed): count it
    /// as never sent, so the window opens for the sink's fetch to resend it.
    pub fn rewind(&mut self) {
        self.sent = self.consumed;
        self.window.forget_in_flight();
    }

    /// Whether [`Source::poll`] would produce a message now.
    pub fn ready(&self) -> bool {
        self.cursor.is_some() && self.window_open()
    }

    /// The next message to send, or `None` while the window is full or the sink hasn't
    /// asked for anything (more).
    pub fn poll(&mut self, now: Instant) -> Option<Message> {
        if !self.window_open() {
            return None;
        }
        let (id, seq) = (self.id, self.seq);
        let cursor = self.cursor.as_mut()?;
        let index = cursor.index;
        if cursor.reader.is_none() {
            self.packer.reset();
            match self.files.open(index, cursor.offset) {
                Ok(reader) => cursor.reader = Some(reader),
                Err(error) => {
                    self.advance();
                    return Some(Message::Unreadable {
                        id,
                        seq,
                        index,
                        reason: error.to_string(),
                    });
                }
            }
        }
        let reader = cursor.reader.as_mut().expect("opened above");
        let mut bytes = vec![0u8; CHUNK];
        match read_full(reader, &mut bytes) {
            Ok(0) => {
                let size = cursor.offset;
                self.advance();
                Some(Message::End { id, seq, index, size })
            }
            Ok(n) => {
                bytes.truncate(n);
                let offset = cursor.offset;
                cursor.offset += n as u64;
                self.sent += n as u64;
                self.first_sent.get_or_insert(now);
                self.window.sent(self.sent, now);
                let (bytes, compressed) = match self.packer.pack(&bytes) {
                    Some(packed) => (packed, true),
                    None => (bytes, false),
                };
                self.wire_sent += bytes.len() as u64;
                Some(Message::Data {
                    id,
                    seq,
                    index,
                    offset,
                    bytes,
                    compressed,
                })
            }
            Err(error) => {
                self.advance();
                Some(Message::Unreadable {
                    id,
                    seq,
                    index,
                    reason: error.to_string(),
                })
            }
        }
    }

    /// On to the next file (folders carry no data), unless the sink asked for just one.
    fn advance(&mut self) {
        let Some(current) = self.cursor.take().filter(|c| !c.single) else {
            return;
        };
        self.cursor = Self::start(&self.entries, current.index.saturating_add(1)).map(|index| Cursor {
            index,
            offset: 0,
            reader: None,
            single: false,
        });
    }
}

/// Reads until `buf` is full or the reader ends.
fn read_full(reader: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Sends the sink's control messages ([`Message::Fetch`], [`Message::Ack`],
/// [`Message::Close`]) to the source. Must not block for long.
pub type Outbox = Arc<dyn Fn(Message) + Send + Sync>;

/// A received item, in stream order.
enum Item {
    Data {
        index: u32,
        offset: u64,
        bytes: Vec<u8>,
        used: usize,
    },
    End {
        index: u32,
        size: u64,
    },
    Unreadable {
        index: u32,
        reason: String,
    },
}

#[derive(Default)]
struct InflowState {
    seq: u32,
    /// Where the current fetch's stream continues: the next file and offset the source
    /// will send (`None` before the first fetch).
    next: Option<(u32, u64)>,
    queue: VecDeque<Item>,
    consumed: u64,
    acked: u64,
    /// Set once the transfer is over: `Some(error)`.
    closed: Option<Option<String>>,
    /// What arrived, for logging.
    throughput: Throughput,
    first_data: Option<Instant>,
}

/// Why a read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The source couldn't read this file.
    Unreadable(String),
    /// The transfer was closed (by either side, or the session ended).
    Closed(Option<String>),
    /// No data arrived in time.
    TimedOut,
}

impl core::fmt::Display for ReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable(reason) => write!(f, "the file couldn't be read: {reason}"),
            Self::Closed(Some(reason)) => write!(f, "the transfer ended: {reason}"),
            Self::Closed(None) => write!(f, "the transfer ended"),
            Self::TimedOut => write!(f, "the transfer stalled"),
        }
    }
}

impl std::error::Error for ReadError {}

/// The receiving side of one transfer: buffers what the source streams and hands it to
/// readers, fetching (again) whenever a reader wants something the stream isn't about
/// to deliver.
pub struct Inflow {
    id: u64,
    entries: Arc<[Entry]>,
    outbox: Outbox,
    /// Entries are fetched one at a time (`single`).
    per_entry: bool,
    state: Mutex<InflowState>,
    changed: Condvar,
}

impl Inflow {
    pub fn new(id: u64, entries: Arc<[Entry]>, outbox: Outbox) -> Self {
        Self {
            id,
            entries,
            outbox,
            per_entry: false,
            state: Mutex::new(InflowState::default()),
            changed: Condvar::new(),
        }
    }

    /// Entries that are each read whole, on their own (clipboard formats): the source
    /// stops at the end of the one asked for instead of running on into the next.
    pub fn per_entry(id: u64, entries: Arc<[Entry]>, outbox: Outbox) -> Self {
        Self {
            per_entry: true,
            ..Self::new(id, entries, outbox)
        }
    }

    /// The source is already streaming the first entry, without waiting for a fetch
    /// ([`Message::ClipboardRead`]): a read of it waits for that data.
    pub fn streaming(self) -> Self {
        let first = self.file_at_or_after(0);
        lock(&self.state).next = Some((first, 0));
        self
    }

    /// What has arrived so far, and what it took on the wire.
    pub fn throughput(&self) -> Throughput {
        lock(&self.state).throughput
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn entries(&self) -> &Arc<[Entry]> {
        &self.entries
    }

    /// The first file at or after `index` (folders carry no data, so the source skips
    /// them).
    fn file_at_or_after(&self, index: u32) -> u32 {
        Source::start(&self.entries, index).unwrap_or(u32::try_from(self.entries.len()).unwrap_or(u32::MAX))
    }

    /// Network side: a message for this transfer arrived. Never blocks for long.
    pub fn receive(&self, message: Message) {
        let mut ack = None;
        let message = match message {
            Message::Data {
                id,
                seq,
                index,
                offset,
                bytes,
                compressed,
            } => {
                let wire = bytes.len() as u64;
                match unpack(bytes, compressed) {
                    Ok(bytes) => {
                        let mut state = lock(&self.state);
                        let st = &mut *state;
                        st.throughput
                            .add(bytes.len() as u64, wire, &mut st.first_data, Instant::now());
                        drop(state);
                        Message::Data {
                            id,
                            seq,
                            index,
                            offset,
                            bytes,
                            compressed: false,
                        }
                    }
                    Err(error) => {
                        self.finish(Some(error));
                        return;
                    }
                }
            }
            other => other,
        };
        {
            let mut state = lock(&self.state);
            match message {
                Message::Data {
                    seq,
                    index,
                    offset,
                    bytes,
                    ..
                } => {
                    let len = bytes.len() as u64;
                    if seq != state.seq || state.closed.is_some() {
                        // From before the latest fetch: dropped, but it still counts
                        // toward what the source may send.
                        state.consumed += len;
                        ack = Self::due_ack(&mut state, true);
                    } else {
                        state.next = Some((index, offset + len));
                        state.queue.push_back(Item::Data {
                            index,
                            offset,
                            bytes,
                            used: 0,
                        });
                    }
                }
                Message::End { seq, index, size, .. } if seq == state.seq => {
                    state.next = self.after(index);
                    state.queue.push_back(Item::End { index, size });
                }
                Message::Unreadable { seq, index, reason, .. } if seq == state.seq => {
                    state.next = self.after(index);
                    state.queue.push_back(Item::Unreadable { index, reason });
                }
                Message::Close { error, .. } => {
                    state.closed.get_or_insert(error);
                }
                _ => {}
            }
            self.changed.notify_all();
        }
        self.send(ack);
    }

    /// Where the stream goes on after entry `index` ends: the next file, or nowhere when
    /// entries are fetched one at a time.
    fn after(&self, index: u32) -> Option<(u32, u64)> {
        (!self.per_entry).then(|| (self.file_at_or_after(index.saturating_add(1)), 0))
    }

    /// What the source had in flight may be lost (the direct link it came over closed):
    /// ask again from where the data received so far ends. What's already here stays.
    pub fn resume(&self) {
        let fetch = {
            let mut state = lock(&self.state);
            let st: &mut InflowState = &mut state;
            match (st.closed.is_some(), st.next) {
                (false, Some((index, offset))) if usize::try_from(index).is_ok_and(|i| i < self.entries.len()) => {
                    st.seq = st.seq.wrapping_add(1);
                    Some(Message::Fetch {
                        id: self.id,
                        seq: st.seq,
                        index,
                        offset,
                        single: self.per_entry,
                    })
                }
                _ => None,
            }
        };
        self.send(fetch);
    }

    /// Whether the transfer is over (closed by either side, or aborted).
    pub fn is_closed(&self) -> bool {
        lock(&self.state).closed.is_some()
    }

    /// The session (or the far side) went away: fail every read from now on.
    pub fn abort(&self, reason: &str) {
        lock(&self.state).closed.get_or_insert(Some(reason.to_owned()));
        self.changed.notify_all();
    }

    /// This side is done with the transfer: tells the source, and fails later reads.
    pub fn finish(&self, error: Option<String>) {
        let first = {
            let mut state = lock(&self.state);
            let first = state.closed.is_none();
            state.closed.get_or_insert(error.clone());
            first
        };
        self.changed.notify_all();
        if first {
            (self.outbox)(Message::Close { id: self.id, error });
        }
    }

    /// An ack to send now, if enough was consumed since the last one (or `drained`).
    fn due_ack(state: &mut InflowState, drained: bool) -> Option<Message> {
        let pending = state.consumed - state.acked;
        (pending >= ACK_EVERY || (drained && pending > 0)).then(|| {
            state.acked = state.consumed;
            Message::Ack {
                id: 0, // filled in by the caller
                consumed: state.consumed,
            }
        })
    }

    fn send(&self, message: Option<Message>) {
        if let Some(mut message) = message {
            if let Message::Ack { id, .. } | Message::Fetch { id, .. } = &mut message {
                *id = self.id;
            }
            (self.outbox)(message);
        }
    }

    /// Reads entry `index` at `position` into `out`, waiting up to `timeout` for data.
    /// Returns 0 at the end of the file.
    pub fn read(&self, index: u32, position: u64, out: &mut [u8], timeout: Duration) -> Result<usize, ReadError> {
        if out.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            let st: &mut InflowState = &mut state;
            if let Some(reason) = &st.closed {
                return Err(ReadError::Closed(reason.clone()));
            }
            // Whatever comes before the position asked for won't be read.
            while let Some(item) = st.queue.front() {
                let behind = match item {
                    Item::Data {
                        index: i,
                        offset,
                        bytes,
                        ..
                    } => *i < index || (*i == index && offset + bytes.len() as u64 <= position),
                    Item::End { index: i, .. } | Item::Unreadable { index: i, .. } => *i < index,
                };
                if !behind {
                    break;
                }
                if let Some(Item::Data { bytes, used, .. }) = st.queue.pop_front() {
                    st.consumed += (bytes.len() - used) as u64;
                }
            }

            enum Next {
                Copied { taken: usize, done: bool },
                Eof,
                Failed(String),
                Wait,
                Refetch,
            }
            let next = match st.queue.front_mut() {
                Some(Item::Data {
                    index: i,
                    offset,
                    bytes,
                    used,
                }) if *i == index && *offset + *used as u64 <= position => {
                    // Skip to `position`, then copy what fits.
                    let available = bytes.len() - *used;
                    let skip = usize::try_from(position - (*offset + *used as u64))
                        .unwrap_or(usize::MAX)
                        .min(available);
                    let taken = (available - skip).min(out.len());
                    let from = *used + skip;
                    out[..taken].copy_from_slice(&bytes[from..from + taken]);
                    *used = from + taken;
                    st.consumed += (skip + taken) as u64;
                    Next::Copied {
                        taken,
                        done: *used == bytes.len(),
                    }
                }
                Some(Item::End { index: i, size }) if *i == index && position >= *size => Next::Eof,
                Some(Item::Unreadable { index: i, reason }) if *i == index => Next::Failed(reason.clone()),
                Some(_) => Next::Refetch,
                None if st.next == Some((index, position)) => Next::Wait,
                None => Next::Refetch,
            };
            match next {
                Next::Copied { taken, done } => {
                    if done {
                        st.queue.pop_front();
                    }
                    if taken == 0 {
                        continue; // the chunk ended exactly at `position`; look again
                    }
                    let drained = st.queue.is_empty();
                    let ack = Self::due_ack(st, drained);
                    drop(state);
                    self.send(ack);
                    return Ok(taken);
                }
                Next::Eof => return Ok(0),
                Next::Failed(reason) => return Err(ReadError::Unreadable(reason)),
                Next::Refetch => {
                    let dropped: u64 = st
                        .queue
                        .drain(..)
                        .map(|item| match item {
                            Item::Data { bytes, used, .. } => (bytes.len() - used) as u64,
                            _ => 0,
                        })
                        .sum();
                    st.consumed += dropped;
                    st.seq = st.seq.wrapping_add(1);
                    st.next = Some((index, position));
                    let fetch = Message::Fetch {
                        id: self.id,
                        seq: st.seq,
                        index,
                        offset: position,
                        single: self.per_entry,
                    };
                    let ack = Self::due_ack(st, true);
                    drop(state);
                    self.send(ack);
                    self.send(Some(fetch));
                    state = lock(&self.state);
                }
                Next::Wait => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(ReadError::TimedOut);
                    }
                    state = self
                        .changed
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }
}

/// Something a [`Pump`] reports about its sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The sink has taken `consumed` bytes (reported a few times a second at most).
    Progress { id: u64, consumed: u64 },
    /// The transfer ended; `error` is what the sink reported, if anything.
    Closed {
        id: u64,
        error: Option<String>,
        throughput: Throughput,
    },
}

enum PumpEvent {
    Start(Box<Source>),
    Fetch {
        id: u64,
        seq: u32,
        index: u32,
        offset: u64,
        single: bool,
    },
    Ack {
        id: u64,
        consumed: u64,
    },
    /// The sink closed it.
    Closed {
        id: u64,
        error: Option<String>,
    },
    /// This side withdraws it (tell the sink).
    Withdraw {
        id: u64,
    },
    /// What was in flight is lost: see [`Source::rewind`].
    Rewind,
}

/// Drives one link's sources on a thread of its own: reads files and sends their data
/// as each transfer's window allows. Dropping the pump stops it.
pub struct Pump {
    events: mpsc::Sender<PumpEvent>,
}

impl Pump {
    const PROGRESS_EVERY: Duration = Duration::from_millis(250);
    /// Messages per source per round, so one transfer can't starve the others.
    const BURST: usize = 8;

    /// `send` delivers a message to the far side (false: the link is gone, so the pump
    /// stops); `notes` hears about progress and endings.
    pub fn spawn(
        name: &str,
        mut send: impl FnMut(Message) -> bool + Send + 'static,
        mut notes: impl FnMut(Note) + Send + 'static,
    ) -> io::Result<Self> {
        let (events, rx) = mpsc::channel();
        std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            let mut sources: Vec<(Box<Source>, Instant, u64)> = Vec::new();
            loop {
                let busy = sources.iter().any(|(s, ..)| s.ready());
                let first = if busy {
                    match rx.try_recv() {
                        Ok(event) => Some(event),
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                } else {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(event) => Some(event),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                };
                let now = Instant::now();
                for event in first.into_iter().chain(rx.try_iter()) {
                    match event {
                        PumpEvent::Start(source) => sources.push((source, now, 0)),
                        PumpEvent::Fetch {
                            id,
                            seq,
                            index,
                            offset,
                            single,
                        } => {
                            if let Some((s, ..)) = sources.iter_mut().find(|(s, ..)| s.id() == id) {
                                s.fetch(seq, index, offset, single);
                            }
                        }
                        PumpEvent::Ack { id, consumed } => {
                            if let Some((s, ..)) = sources.iter_mut().find(|(s, ..)| s.id() == id) {
                                s.ack(consumed, now);
                            }
                        }
                        PumpEvent::Closed { id, error } => {
                            if let Some(at) = sources.iter().position(|(s, ..)| s.id() == id) {
                                let (s, ..) = sources.swap_remove(at);
                                notes(Note::Progress {
                                    id,
                                    consumed: s.consumed(),
                                });
                                notes(Note::Closed {
                                    id,
                                    error,
                                    throughput: s.throughput(),
                                });
                            }
                        }
                        PumpEvent::Rewind => sources.iter_mut().for_each(|(s, ..)| s.rewind()),
                        PumpEvent::Withdraw { id } => {
                            if let Some(at) = sources.iter().position(|(s, ..)| s.id() == id) {
                                sources.swap_remove(at);
                                if !send(Message::Close { id, error: None }) {
                                    return;
                                }
                            }
                        }
                    }
                }
                let now = Instant::now();
                for (source, reported_at, reported) in &mut sources {
                    for _ in 0..Self::BURST {
                        match source.poll(now) {
                            Some(message) => {
                                if !send(message) {
                                    return;
                                }
                            }
                            None => break,
                        }
                    }
                    let consumed = source.consumed();
                    if consumed != *reported && now.saturating_duration_since(*reported_at) >= Self::PROGRESS_EVERY {
                        *reported = consumed;
                        *reported_at = now;
                        notes(Note::Progress {
                            id: source.id(),
                            consumed,
                        });
                    }
                }
            }
        })?;
        Ok(Self { events })
    }

    /// Starts serving a source (offer it to the sink first).
    pub fn start(&self, source: Source) {
        let _ = self.events.send(PumpEvent::Start(Box::new(source)));
    }

    /// Routes a sink's message for one of this pump's sources. Returns false for
    /// messages that aren't for a source.
    pub fn receive(&self, message: &Message) -> bool {
        let event = match *message {
            Message::Fetch {
                id,
                seq,
                index,
                offset,
                single,
            } => PumpEvent::Fetch {
                id,
                seq,
                index,
                offset,
                single,
            },
            Message::Ack { id, consumed } => PumpEvent::Ack { id, consumed },
            _ => return false,
        };
        let _ = self.events.send(event);
        true
    }

    /// The sink closed transfer `id`.
    pub fn closed(&self, id: u64, error: Option<String>) {
        let _ = self.events.send(PumpEvent::Closed { id, error });
    }

    /// What every source had in flight is lost (the direct link closed).
    pub fn rewind(&self) {
        let _ = self.events.send(PumpEvent::Rewind);
    }

    /// Stops serving `id` and tells the sink (the files are no longer on offer).
    pub fn withdraw(&self, id: u64) {
        let _ = self.events.send(PumpEvent::Withdraw { id });
    }
}

/// A listing of local files and folders, in transfer order.
#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// `paths[i]` is the file or folder `entries[i]` describes.
    pub paths: Vec<PathBuf>,
}

impl Listing {
    /// Total bytes of the files.
    pub fn total(&self) -> u64 {
        self.entries.iter().filter(|e| !e.dir).map(|e| e.size).sum()
    }
}

/// Upper bound on entries listed at once (copying a whole drive should not hang).
const MAX_ENTRIES: usize = 200_000;

/// Lists `roots` and everything inside the folders among them (folders before their
/// contents; links into folders aren't followed).
pub fn list(roots: &[PathBuf]) -> Listing {
    let mut out = Listing::default();
    for root in roots {
        add(&mut out, root, None);
    }
    out
}

fn add(out: &mut Listing, path: &Path, parent: Option<&str>) {
    if out.entries.len() >= MAX_ENTRIES {
        return;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return; // a drive root has no name to copy under
    };
    let Ok(link) = std::fs::symlink_metadata(path) else {
        return;
    };
    let Ok(meta) = std::fs::metadata(path) else { return };
    let relative = match parent {
        Some(parent) => format!("{parent}\\{name}"),
        None => name.to_owned(),
    };
    out.entries.push(Entry {
        path: relative.clone(),
        dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified: filetime(&meta),
    });
    out.paths.push(path.to_owned());
    if meta.is_dir() && !link.file_type().is_symlink() {
        let Ok(children) = std::fs::read_dir(path) else { return };
        let mut children: Vec<PathBuf> = children.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            add(out, &child, Some(&relative));
        }
    }
}

const FILETIME_UNIX_OFFSET_SECS: u64 = 11_644_473_600;

/// Last write time as a Windows FILETIME, 0 if unknown.
fn filetime(meta: &std::fs::Metadata) -> u64 {
    let Ok(since) = meta.modified().map(|t| t.duration_since(UNIX_EPOCH)) else {
        return 0;
    };
    let Ok(since) = since else { return 0 };
    (since.as_secs() + FILETIME_UNIX_OFFSET_SECS)
        .saturating_mul(10_000_000)
        .saturating_add(u64::from(since.subsec_nanos() / 100))
}

fn system_time(filetime: u64) -> Option<SystemTime> {
    let ticks_since_unix = filetime.checked_sub(FILETIME_UNIX_OFFSET_SECS * 10_000_000)?;
    UNIX_EPOCH.checked_add(Duration::from_nanos(ticks_since_unix.checked_mul(100)?))
}

/// Serves a [`Listing`]'s files from disk.
pub struct DiskFiles {
    paths: Vec<PathBuf>,
}

impl DiskFiles {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Files for DiskFiles {
    fn open(&mut self, index: u32, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        use std::io::{Seek, SeekFrom};
        let path = usize::try_from(index)
            .ok()
            .and_then(|i| self.paths.get(i))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such entry"))?;
        let mut file = std::fs::File::open(path)?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))?;
        }
        Ok(Box::new(file))
    }
}

/// Entries made whole when opened (clipboard formats, read off the clipboard when they
/// are pasted); the last one is kept while it's being read.
pub struct Produced<F> {
    produce: F,
    last: Option<(u32, Arc<Vec<u8>>)>,
}

impl<F> Produced<F> {
    pub fn new(produce: F) -> Self {
        Self { produce, last: None }
    }
}

/// Shared bytes, for a cursor.
struct SharedBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl<F: FnMut(u32) -> io::Result<Vec<u8>> + Send> Files for Produced<F> {
    fn open(&mut self, index: u32, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let data = match &self.last {
            Some((i, data)) if *i == index => Arc::clone(data),
            _ => {
                let data = Arc::new((self.produce)(index)?);
                self.last = Some((index, Arc::clone(&data)));
                data
            }
        };
        let mut reader = io::Cursor::new(SharedBytes(data));
        reader.set_position(offset);
        Ok(Box::new(reader))
    }
}

/// A relative path from the far side, made safe to join under a local folder: plain
/// names only (no roots, drives, `..`, or stream suffixes). `None` if it can't be.
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split(['\\', '/']) {
        let invalid = part.is_empty()
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
            || is_device_name(part);
        if invalid {
            return None;
        }
        out.push(part);
    }
    let normal = out.components().all(|c| matches!(c, Component::Normal(_)));
    (normal && !out.as_os_str().is_empty()).then_some(out)
}

/// Windows device names (`CON`, `NUL`, `COM1`...), which open a device whatever the
/// folder or extension.
fn is_device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

/// `name` if nothing is at `folder\name`, else the first free `stem (2).ext`,
/// `stem (3).ext`… the way Explorer names a copy.
pub fn free_name(folder: &Path, name: &str) -> String {
    if !folder.join(name).exists() {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !folder.join(candidate).exists())
        .expect("some name is free")
}

/// How a [`save`] went.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SaveReport {
    /// Top-level names as saved (renamed if the folder already had them).
    pub saved: Vec<String>,
    pub files: u32,
    /// Entries that failed, with why.
    pub failed: Vec<(String, String)>,
}

/// How long a save waits for data before giving up.
const SAVE_STALL: Duration = Duration::from_secs(120);

/// Receives every entry of `inflow` into `folder` (created if missing). A top-level name
/// that already exists there is kept, and the new one saved as a numbered copy.
/// `progress` hears the bytes written so far.
pub fn save(inflow: &Inflow, folder: &Path, mut progress: impl FnMut(u64)) -> Result<SaveReport, String> {
    use std::io::Write;

    std::fs::create_dir_all(folder).map_err(|e| format!("couldn't create {}: {e}", folder.display()))?;
    let mut report = SaveReport::default();
    // Top-level name as offered → as saved.
    let mut renamed: Vec<(String, String)> = Vec::new();
    let mut written = 0u64;
    let mut buf = vec![0u8; CHUNK];
    for (index, entry) in inflow.entries().iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| "too many entries".to_owned())?;
        let Some(relative) = safe_relative(&entry.path) else {
            report.failed.push((entry.path.clone(), "unsafe name".into()));
            continue;
        };
        let mut parts = relative.iter();
        let top = parts.next().and_then(|t| t.to_str()).unwrap_or_default().to_owned();
        let rest: PathBuf = parts.collect();
        let top_saved = match renamed.iter().find(|(offered, _)| *offered == top) {
            Some((_, saved)) => saved.clone(),
            None => {
                let saved = free_name(folder, &top);
                renamed.push((top.clone(), saved.clone()));
                report.saved.push(saved.clone());
                saved
            }
        };
        let target = if rest.as_os_str().is_empty() {
            folder.join(&top_saved)
        } else {
            folder.join(&top_saved).join(rest)
        };
        if entry.dir {
            if let Err(error) = std::fs::create_dir_all(&target) {
                report.failed.push((entry.path.clone(), error.to_string()));
            }
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let result = (|| -> Result<(), String> {
            let mut file = std::fs::File::create(&target).map_err(|e| e.to_string())?;
            let mut position = 0u64;
            loop {
                let n = inflow
                    .read(index, position, &mut buf, SAVE_STALL)
                    .map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                position += n as u64;
                written += n as u64;
                progress(written);
            }
            if let Some(time) = system_time(entry.modified) {
                let _ = file.set_modified(time);
            }
            Ok(())
        })();
        match result {
            Ok(()) => report.files += 1,
            Err(error) => {
                let _ = std::fs::remove_file(&target);
                report.failed.push((entry.path.clone(), error));
                // A closed transfer fails everything after it too; stop here.
                if inflow.is_closed() {
                    return Ok(report);
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::{Decoder, encode};

    /// In-memory files.
    struct Memory(Vec<Vec<u8>>);

    impl Files for Memory {
        fn open(&mut self, index: u32, offset: u64) -> io::Result<Box<dyn Read + Send>> {
            let data = self
                .0
                .get(index as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))?;
            if data == b"UNREADABLE" {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
            }
            Ok(Box::new(io::Cursor::new(data[offset as usize..].to_vec())))
        }
    }

    fn file(path: &str, data: &[u8]) -> Entry {
        Entry {
            path: path.into(),
            dir: false,
            size: data.len() as u64,
            modified: 0,
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            path: path.into(),
            dir: true,
            size: 0,
            modified: 0,
        }
    }

    /// A source and a sink joined through a pump, with every message framed and decoded
    /// the way the channel would.
    struct Link {
        inflow: Arc<Inflow>,
        _pump: Pump,
        sent_frames: Arc<Mutex<u64>>,
    }

    /// Delivers what's sent to it `delay` later, in order, on a thread of its own.
    fn wire(delay: Duration, deliver: impl Fn(Message) + Send + 'static) -> mpsc::Sender<Message> {
        let (tx, rx) = mpsc::channel::<Message>();
        let (stamped_tx, stamped_rx) = mpsc::channel::<(Instant, Message)>();
        std::thread::spawn(move || {
            for message in rx {
                if stamped_tx.send((Instant::now() + delay, message)).is_err() {
                    return;
                }
            }
        });
        std::thread::spawn(move || {
            for (at, message) in stamped_rx {
                if let Some(wait) = at.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                deliver(message);
            }
        });
        tx
    }

    fn link(entries: Vec<Entry>, contents: Vec<Vec<u8>>) -> Link {
        link_over(entries, contents, Duration::ZERO)
    }

    /// `delay` each way, so a round trip takes twice that.
    fn link_over(entries: Vec<Entry>, contents: Vec<Vec<u8>>, delay: Duration) -> Link {
        let entries: Arc<[Entry]> = entries.into();
        let (to_pump, from_sink) = mpsc::channel::<Message>();
        let upstream = Mutex::new(wire(delay, move |m| {
            let _ = to_pump.send(m);
        }));
        let outbox: Outbox = Arc::new(move |m: Message| {
            let _ = upstream.lock().unwrap().send(m);
        });
        let inflow = Arc::new(Inflow::new(2, Arc::clone(&entries), outbox));
        let sent_frames = Arc::new(Mutex::new(0u64));
        let (sink, frames) = (Arc::clone(&inflow), Arc::clone(&sent_frames));
        let downstream = wire(delay, move |message| {
            let mut decoder = Decoder::new();
            decoder.feed(&encode(&message));
            sink.receive(decoder.next_message().unwrap().unwrap());
        });
        let pump = Pump::spawn(
            "test-pump",
            move |message| {
                *frames.lock().unwrap() += 1;
                downstream.send(message).is_ok()
            },
            |_| {},
        )
        .unwrap();
        pump.start(Source::new(2, entries, Box::new(Memory(contents))));
        // Forward the sink's fetches and acks to the pump.
        let events = pump.events.clone();
        std::thread::spawn(move || {
            for message in from_sink {
                let event = match message {
                    Message::Fetch {
                        id,
                        seq,
                        index,
                        offset,
                        single,
                    } => PumpEvent::Fetch {
                        id,
                        seq,
                        index,
                        offset,
                        single,
                    },
                    Message::Ack { id, consumed } => PumpEvent::Ack { id, consumed },
                    Message::Close { id, error } => PumpEvent::Closed { id, error },
                    _ => continue,
                };
                if events.send(event).is_err() {
                    return;
                }
            }
        });
        Link {
            inflow,
            _pump: pump,
            sent_frames,
        }
    }

    fn read_all(inflow: &Inflow, index: u32, step: usize) -> Result<Vec<u8>, ReadError> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; step];
        loop {
            let n = inflow.read(index, out.len() as u64, &mut buf, Duration::from_secs(5))?;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    /// Bytes that don't compress.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn streams_files_in_order_across_folders() {
        let big = pattern(CHUNK * 5 + 123, 1);
        let small = pattern(10, 2);
        let link = link(
            vec![
                dir("docs"),
                file("docs\\big.bin", &big),
                dir("docs\\sub"),
                file("docs\\sub\\empty", &[]),
                file("docs\\sub\\small", &small),
            ],
            vec![vec![], big.clone(), vec![], vec![], small.clone()],
        );
        assert_eq!(read_all(&link.inflow, 1, 10_000).unwrap(), big);
        assert_eq!(read_all(&link.inflow, 3, 100).unwrap(), b"");
        assert_eq!(read_all(&link.inflow, 4, 3).unwrap(), small);
    }

    #[test]
    fn streaming_beats_a_round_trip_per_chunk() {
        // A 20 ms round trip: asking for one 64 KiB chunk at a time would top out near
        // 3 MB/s. Streaming against the window should do several times better (on data
        // that doesn't compress, so it's the window being measured).
        let data = noise(16 * 1024 * 1024, 8);
        let link = link_over(vec![file("f", &data)], vec![data.clone()], Duration::from_millis(10));
        let started = Instant::now();
        assert_eq!(read_all(&link.inflow, 0, 256 * 1024).unwrap(), data);
        let mb_per_s = data.len() as f64 / 1e6 / started.elapsed().as_secs_f64();
        assert!(mb_per_s > 15.0, "{mb_per_s:.1} MB/s");
    }

    #[test]
    fn a_seek_refetches_and_skipped_files_are_dropped() {
        let a = pattern(CHUNK * 3, 3);
        let b = pattern(CHUNK * 2 + 7, 4);
        let link = link(vec![file("a", &a), file("b", &b)], vec![a.clone(), b.clone()]);
        // Read the middle of `b` first (skipping `a` entirely), then go back to `a`.
        let mut buf = vec![0u8; 100];
        let n = link
            .inflow
            .read(1, CHUNK as u64 + 5, &mut buf, Duration::from_secs(5))
            .unwrap();
        assert_eq!(buf[..n], b[CHUNK + 5..CHUNK + 5 + n]);
        assert_eq!(read_all(&link.inflow, 0, 4096).unwrap(), a);
        assert_eq!(read_all(&link.inflow, 1, 4096).unwrap(), b);
    }

    #[test]
    fn an_unreadable_file_fails_only_itself() {
        let ok = pattern(1000, 5);
        let link = link(
            vec![file("bad", b"UNREADABLE"), file("ok", &ok)],
            vec![b"UNREADABLE".to_vec(), ok.clone()],
        );
        assert!(matches!(read_all(&link.inflow, 0, 100), Err(ReadError::Unreadable(_))));
        assert_eq!(read_all(&link.inflow, 1, 100).unwrap(), ok);
    }

    #[test]
    fn closing_wakes_a_blocked_reader() {
        let entries: Arc<[Entry]> = vec![file("x", &[0; 10])].into();
        let inflow = Arc::new(Inflow::new(2, entries, Arc::new(|_| {})));
        let reader = Arc::clone(&inflow);
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let handle = std::thread::spawn(move || {
            let result = reader.read(0, 0, &mut [0u8; 4], Duration::from_secs(30));
            flag.store(true, Ordering::SeqCst);
            result
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(!done.load(Ordering::SeqCst));
        inflow.abort("session ended");
        assert_eq!(
            handle.join().unwrap(),
            Err(ReadError::Closed(Some("session ended".into())))
        );
    }

    #[test]
    fn the_window_bounds_what_is_in_flight() {
        let data = pattern(Window::INITIAL as usize * 4, 6);
        let entries: Arc<[Entry]> = vec![file("f", &data)].into();
        let mut source = Source::new(4, entries, Box::new(Memory(vec![data])));
        source.fetch(1, 0, 0, false);
        let now = Instant::now();
        let mut sent = 0u64;
        while let Some(message) = source.poll(now) {
            if let Message::Data { bytes, compressed, .. } = message {
                sent += unpack(bytes, compressed).unwrap().len() as u64;
            }
        }
        assert_eq!(sent, Window::INITIAL, "stops at the initial window");
        source.ack(CHUNK as u64, now);
        assert!(source.ready(), "an ack opens the window again");
    }

    /// Every message `source` has to send now, in order.
    fn drain(source: &mut Source) -> Vec<Message> {
        let now = Instant::now();
        std::iter::from_fn(|| source.poll(now)).collect()
    }

    #[test]
    fn text_is_compressed_and_noise_is_not() {
        let text: Vec<u8> = "the quick brown fox jumps over the lazy dog\r\n"
            .repeat(CHUNK / 16)
            .into_bytes();
        let random = noise(CHUNK * 3, 9);
        let entries: Arc<[Entry]> = vec![file("a.txt", &text), file("b.bin", &random)].into();
        let mut source = Source::new(6, entries, Box::new(Memory(vec![text.clone(), random.clone()])));
        source.fetch(1, 0, 0, false);
        let mut got: Vec<(u32, bool, usize, Vec<u8>)> = Vec::new();
        let now = Instant::now();
        while let Some(message) = source.poll(now) {
            source.ack(u64::MAX, now); // taken at once: the window never closes
            if let Message::Data {
                index,
                bytes,
                compressed,
                ..
            } = message
            {
                let wire = bytes.len();
                got.push((index, compressed, wire, unpack(bytes, compressed).unwrap()));
            }
        }
        let text_chunks: Vec<_> = got.iter().filter(|g| g.0 == 0).collect();
        assert!(
            text_chunks.iter().all(|g| g.1 && g.2 < CHUNK / 10),
            "text compresses well"
        );
        assert_eq!(text_chunks.iter().flat_map(|g| g.3.clone()).collect::<Vec<_>>(), text);
        let noise_chunks: Vec<_> = got.iter().filter(|g| g.0 == 1).collect();
        assert!(noise_chunks.iter().all(|g| !g.1), "noise goes as is");
        assert_eq!(
            noise_chunks.iter().flat_map(|g| g.3.clone()).collect::<Vec<_>>(),
            random
        );
    }

    #[test]
    fn corrupt_compressed_data_fails_the_transfer() {
        let entries: Arc<[Entry]> = vec![file("x", &[0; 10])].into();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let outbox_sent = Arc::clone(&sent);
        let inflow = Inflow::new(2, entries, Arc::new(move |m| outbox_sent.lock().unwrap().push(m)));
        inflow.receive(Message::Data {
            id: 2,
            seq: 0,
            index: 0,
            offset: 0,
            bytes: vec![1, 2, 3, 4],
            compressed: true,
        });
        assert!(inflow.is_closed());
        assert!(matches!(
            sent.lock().unwrap()[..],
            [Message::Close { error: Some(_), .. }]
        ));
    }

    #[test]
    fn a_single_fetch_stops_at_the_end_of_its_entry() {
        let a = pattern(CHUNK + 10, 1);
        let b = pattern(100, 2);
        let entries: Arc<[Entry]> = vec![file("a", &a), file("b", &b)].into();
        let mut source = Source::new(8, entries, Box::new(Memory(vec![a, b])));
        source.fetch(1, 0, 0, true);
        let messages = drain(&mut source);
        assert!(matches!(messages.last(), Some(Message::End { index: 0, .. })));
        assert!(messages.iter().all(|m| !matches!(m, Message::Data { index: 1, .. })));
        assert!(!source.ready());
    }

    #[test]
    fn a_streamed_entry_needs_no_fetch_and_the_next_is_fetched_alone() {
        let a = pattern(CHUNK * 2 + 3, 4);
        let b = pattern(500, 5);
        let entries: Arc<[Entry]> = vec![file("a", &a), file("b", &b)].into();
        let sent = Arc::new(Mutex::new(Vec::<Message>::new()));
        let outbox_sent = Arc::clone(&sent);
        let inflow = Inflow::per_entry(
            10,
            Arc::clone(&entries),
            Arc::new(move |m| outbox_sent.lock().unwrap().push(m)),
        )
        .streaming();
        // The source starts on its own (seq 0, just the first entry).
        let mut source = Source::new(10, entries, Box::new(Memory(vec![a.clone(), b.clone()])));
        source.fetch(0, 0, 0, true);
        for message in drain(&mut source) {
            inflow.receive(message);
        }
        assert_eq!(read_all(&inflow, 0, 4096).unwrap(), a);
        let fetches = |sent: &[Message]| {
            sent.iter()
                .filter_map(|m| match m {
                    Message::Fetch { seq, index, single, .. } => Some((*seq, *index, *single)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(
            fetches(&sent.lock().unwrap()).is_empty(),
            "no fetch for a streamed entry"
        );
        assert!(inflow.throughput().bytes >= a.len() as u64);

        // Reading the second entry asks for it, alone.
        let reader = {
            let inflow = Arc::new(inflow);
            let reading = Arc::clone(&inflow);
            (inflow, std::thread::spawn(move || read_all(&reading, 1, 64)))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while fetches(&sent.lock().unwrap()).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fetches(&sent.lock().unwrap()), vec![(1, 1, true)]);
        source.fetch(1, 1, 0, true);
        for message in drain(&mut source) {
            reader.0.receive(message);
        }
        assert_eq!(reader.1.join().unwrap().unwrap(), b);
    }

    #[test]
    fn a_lost_stream_resumes_where_it_stopped() {
        let a = pattern(CHUNK * 6, 7);
        let entries: Arc<[Entry]> = vec![file("a", &a)].into();
        let sent = Arc::new(Mutex::new(Vec::<Message>::new()));
        let outbox_sent = Arc::clone(&sent);
        let inflow = Arc::new(Inflow::new(
            12,
            Arc::clone(&entries),
            Arc::new(move |m| outbox_sent.lock().unwrap().push(m)),
        ));
        let mut source = Source::new(12, entries, Box::new(Memory(vec![a.clone()])));
        // The reader asks; two chunks arrive, the rest are lost with the link.
        let reader = {
            let inflow = Arc::clone(&inflow);
            std::thread::spawn(move || read_all(&inflow, 0, 4096))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let fetch = loop {
            let found = sent.lock().unwrap().iter().find_map(|m| match m {
                Message::Fetch { seq, index, offset, .. } => Some((*seq, *index, *offset)),
                _ => None,
            });
            if let Some(f) = found {
                break f;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        source.fetch(fetch.0, fetch.1, fetch.2, false);
        let messages = drain(&mut source);
        assert!(messages.len() > 2);
        for message in messages.into_iter().take(2) {
            inflow.receive(message);
        }
        assert!(!source.ready(), "the window is full of what was lost");
        // The link closes: the source forgets what was in flight, the sink asks again.
        source.rewind();
        sent.lock().unwrap().clear();
        inflow.resume();
        let again = sent
            .lock()
            .unwrap()
            .iter()
            .find_map(|m| match m {
                Message::Fetch { seq, index, offset, .. } => Some((*seq, *index, *offset)),
                _ => None,
            })
            .expect("a fetch from where the data stopped");
        assert_eq!(again.2, 2 * CHUNK as u64);
        source.fetch(again.0, again.1, again.2, false);
        let now = Instant::now();
        while let Some(message) = source.poll(now) {
            source.ack(u64::MAX, now);
            inflow.receive(message);
        }
        assert_eq!(reader.join().unwrap().unwrap(), a);
    }

    #[test]
    fn produced_entries_are_made_once_while_read() {
        let made = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&made);
        let mut files = Produced::new(move |index: u32| {
            *counter.lock().unwrap() += 1;
            Ok(vec![index as u8; 10])
        });
        let mut out = Vec::new();
        files.open(3, 4).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, vec![3; 6]);
        files.open(3, 0).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(*made.lock().unwrap(), 1);
        files.open(1, 0).unwrap();
        assert_eq!(*made.lock().unwrap(), 2);
    }

    #[test]
    fn the_window_settles_near_bandwidth_times_round_trip() {
        // 10 MB/s with a 50 ms round trip: a 500 KB bandwidth-delay product.
        let mut window = Window::default();
        let start = Instant::now();
        let rate = 10_000_000.0;
        let rtt = Duration::from_millis(50);
        let mut sent = 0u64;
        for step in 0..400u64 {
            let at = start + Duration::from_millis(step * 5);
            sent += (rate * 0.005) as u64;
            window.sent(sent, at);
            // Acks trail sends by one round trip.
            if let Some(acked) = sent.checked_sub((rate * rtt.as_secs_f64()) as u64) {
                window.acked(acked, at);
            }
        }
        let bdp = rate * rtt.as_secs_f64();
        let limit = window.limit() as f64;
        assert!(limit > bdp && limit < bdp * 2.0, "limit {limit} for a {bdp} BDP");
    }

    #[test]
    fn save_writes_the_tree_and_numbers_existing_names() {
        let root = std::env::temp_dir().join(format!("rdp-transfer-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let a = pattern(CHUNK + 50, 7);
        let link = link(
            vec![dir("docs"), file("docs\\a.bin", &a), file("top.txt", b"hi")],
            vec![vec![], a.clone(), b"hi".to_vec()],
        );
        let report = save(&link.inflow, &root, |_| {}).unwrap();
        assert_eq!(report.saved, vec!["docs (2)".to_owned(), "top.txt".to_owned()]);
        assert_eq!(report.files, 2);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(std::fs::read(root.join("docs (2)").join("a.bin")).unwrap(), a);
        assert_eq!(std::fs::read(root.join("top.txt")).unwrap(), b"hi");
        assert!(*link.sent_frames.lock().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_names_are_refused() {
        assert_eq!(safe_relative("docs\\a.txt"), Some(PathBuf::from("docs").join("a.txt")));
        for bad in [
            "",
            "..\\x",
            "a\\..\\..\\x",
            "C:\\x",
            "\\\\server\\share",
            "a:stream",
            "x.",
            "a\\\\b",
            "nul.txt",
            "docs\\COM1",
        ] {
            assert_eq!(safe_relative(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn listing_puts_folders_before_contents() {
        let root = std::env::temp_dir().join(format!("rdp-transfer-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs/sub")).unwrap();
        std::fs::write(root.join("docs/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("docs/sub/b.bin"), [7u8; 10]).unwrap();
        let listing = list(&[root.join("docs")]);
        let paths: Vec<(&str, bool)> = listing.entries.iter().map(|e| (e.path.as_str(), e.dir)).collect();
        assert_eq!(
            paths,
            vec![
                ("docs", true),
                ("docs\\a.txt", false),
                ("docs\\sub", true),
                ("docs\\sub\\b.bin", false)
            ]
        );
        assert_eq!(listing.total(), 15);
        assert!(listing.entries[1].modified > 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
