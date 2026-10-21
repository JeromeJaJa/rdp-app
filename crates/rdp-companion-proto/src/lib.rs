//! Wire protocol shared by the RDP App client and the remote-side companion.
//!
//! The companion is a small program that runs inside the remote session and talks to
//! the client over a dynamic virtual channel ([`CHANNEL_NAME`]). This crate is the one
//! thing both sides agree on: the message set, how messages are framed, and the file
//! transfer machinery both ends run ([`transfer`]).
//!
//! Framing is self-describing (a 4-byte little-endian length, then a MessagePack body)
//! so it survives whatever chunking the transport applies: a single DVC message on the
//! client side, `CHANNEL_CHUNK_LENGTH` pieces on the WTS side. A [`Decoder`] buffers
//! bytes and yields whole messages as they complete.
//!
//! Both sides exchange [`PROTOCOL_VERSION`] up front and refuse a mismatch, so an old
//! companion meeting a new client fails cleanly rather than misreading a message (the
//! client then deploys its own build).

pub mod transfer;

use serde::{Deserialize, Serialize};

/// Dynamic virtual channel the companion opens on the remote side. Kept short and
/// stable: changing it breaks every deployed companion.
pub const CHANNEL_NAME: &str = "RdpApp::Companion";

/// Bumped on any breaking change to [`Message`]. Both sides send it in the handshake.
pub const PROTOCOL_VERSION: u32 = 8;

/// A frame larger than this is treated as a desync and ends the channel, rather than
/// buffering without bound.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Name of a clipboard format the companion adds to everything it puts on the remote
/// clipboard. The remote's clipboard redirection then announces it to the client with
/// the rest of the formats, which tells the client the change was its own (a copy made
/// on this PC) and must not be mirrored back.
pub const CLIPBOARD_MARKER: &str = "RdpApp.Companion";

const LEN_BYTES: usize = 4;

/// One file or folder in a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path below the transfer's root, `\`-separated (`docs\a.txt`). Folders come before
    /// their contents.
    pub path: String,
    pub dir: bool,
    /// Size when offered (a file can still change; [`Message::End`] has the final size).
    pub size: u64,
    /// Last write time as a Windows FILETIME (100 ns ticks since 1601), 0 if unknown.
    pub modified: u64,
}

/// A clipboard format: standard ones by id, registered ones by name (their ids differ
/// between machines).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipFormat {
    pub id: u32,
    pub name: Option<String>,
}

impl ClipFormat {
    /// First id of registered (named) formats.
    pub const REGISTERED: u32 = 0xC000;

    /// Whether the format's data is plain memory (an `HGLOBAL`) that can be copied as bytes:
    /// registered formats, and the standard ones that aren't GDI handles or file lists.
    pub fn is_memory(&self) -> bool {
        // CF_TEXT, SYLK, DIF, TIFF, OEMTEXT, DIB, PENDATA, RIFF, WAVE, UNICODETEXT, LOCALE, DIBV5.
        const STANDARD: [u32; 12] = [1, 4, 5, 6, 7, 8, 10, 11, 12, 13, 16, 17];
        if self.id >= Self::REGISTERED {
            self.name
                .as_deref()
                .is_some_and(|n| !n.is_empty() && n != CLIPBOARD_MARKER)
        } else {
            STANDARD.contains(&self.id)
        }
    }
}

/// What to do with a program on the remote ([`Message::AppControl`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppAction {
    /// Start it (a single-instance app that's running comes forward instead).
    Launch,
    /// Close it (if running), then start it.
    Restart,
    /// Ask its windows to close; end it if it's still running a few seconds later.
    Close,
}

/// What the receiving side does with offered files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    /// Save them into `folder` on the receiving machine (environment variables such as
    /// `%USERPROFILE%` are expanded there). The receiver fetches everything, then closes
    /// the transfer.
    Save { folder: String },
    /// They were copied: offer them on the receiving side's clipboard. Contents are
    /// fetched only when (and if) they are pasted.
    Clipboard,
}

/// Host load, sampled by the companion. Rates and percentages are averages over the
/// time since the previous sample.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostStats {
    /// Seconds the sample covers.
    pub span: f32,
    /// CPU use, 0–100.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    /// Network bytes per second across the machine's interfaces.
    pub net_rx: u64,
    pub net_tx: u64,
    /// Physical disk activity, when the host lets the companion read it.
    pub disk: Option<DiskLoad>,
    /// Fixed volumes.
    pub volumes: Vec<Volume>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskLoad {
    /// Bytes per second across physical disks.
    pub read: u64,
    pub write: u64,
    /// The busiest disk's active time, 0–100 (Task Manager's "Active time").
    pub busy: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Drive name, e.g. `C:`.
    pub name: String,
    pub free: u64,
    pub total: u64,
}

/// One message in each direction. Serialized as MessagePack; a version bump guards
/// against an old peer misreading a new one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Companion → client, first message: who and where it is running.
    Hello {
        protocol: u32,
        /// The companion binary's version (`CARGO_PKG_VERSION`).
        version: String,
        /// The remote machine's name, for display.
        host: String,
        /// The remote session the companion is running in (WTS session id).
        session: u32,
        /// Fingerprint of the companion's executable ([`build_id`]): the client replaces a
        /// companion that isn't the build it carries. Empty from companions before it.
        #[serde(default)]
        build: String,
        /// It watches the remote clipboard and announces copies ([`Message::RemoteCopy`]),
        /// so the client can ignore the clipboard channel's announcements.
        #[serde(default)]
        watches_clipboard: bool,
    },
    /// Client → companion, reply to `Hello`: the handshake is accepted.
    Welcome {
        protocol: u32,
        version: String,
    },
    /// Client → companion, reply to a rejected `Hello`: why, then the channel closes.
    Refused {
        reason: String,
    },
    /// Either side: liveness probe; the peer answers with `Pong` carrying the same nonce.
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// Either side: a clean shutdown is starting.
    Bye,

    /// Client → companion: report [`HostStats`] every `secs` seconds (0 stops).
    StatsInterval {
        secs: u32,
    },
    /// Companion → client: the samples taken since the last report, oldest first (the
    /// host is sampled every few seconds, and reported at the interval).
    Stats(Vec<HostStats>),

    /// Source → sink: files on offer as transfer `id`. Ids are picked by whoever offers:
    /// even on the client, odd on the companion, so they never collide.
    Offer {
        id: u64,
        entries: Vec<Entry>,
        intent: Intent,
    },
    /// Sink → source: stream entry `index` from `offset`, then carry on through the
    /// following entries until the window fills (or stop at the end of this one, if
    /// `single`). `seq` numbers fetches so the sink can drop data still in flight from an
    /// earlier one.
    Fetch {
        id: u64,
        seq: u32,
        index: u32,
        offset: u64,
        #[serde(default)]
        single: bool,
    },
    /// Source → sink: bytes of entry `index` at `offset`; `bytes` is a zstd frame if
    /// `compressed` (offsets and acks count the bytes it decompresses to).
    Data {
        id: u64,
        seq: u32,
        index: u32,
        offset: u64,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        #[serde(default)]
        compressed: bool,
    },
    /// Source → sink: entry `index` ended; `size` is its true size.
    End {
        id: u64,
        seq: u32,
        index: u32,
        size: u64,
    },
    /// Source → sink: entry `index` could not be read.
    Unreadable {
        id: u64,
        seq: u32,
        index: u32,
        reason: String,
    },
    /// Sink → source: total bytes it has taken off the wire for this transfer (used or
    /// dropped). The source keeps at most a window's worth beyond this in flight.
    Ack {
        id: u64,
        consumed: u64,
    },
    /// Either side: transfer `id` is over (done, cancelled, or the offer withdrawn).
    Close {
        id: u64,
        error: Option<String>,
    },

    /// Client → companion: offer the files on your clipboard as transfer `id` (an
    /// [`Intent::Clipboard`] offer), or answer [`Message::NoFiles`].
    ClipboardQuery {
        id: u64,
    },
    /// Companion → client: the clipboard holds no files it can serve.
    NoFiles {
        id: u64,
    },
    /// Client → companion: something other than files was copied on the client. Put these
    /// formats on the remote clipboard (with [`CLIPBOARD_MARKER`]); they are transfer `id`,
    /// one entry per format in this order, each fetched (`single`) when something pastes it.
    ClipboardOffer {
        id: u64,
        formats: Vec<ClipFormat>,
    },
    /// Companion → client: something was copied on the remote (not the client's own copy):
    /// its plain-memory formats, and whether it holds files. The client offers them on its
    /// clipboard, read with [`Message::ClipboardRead`] (files with
    /// [`Message::ClipboardQuery`]) when pasted.
    RemoteCopy {
        formats: Vec<ClipFormat>,
        files: bool,
    },
    /// Client → companion: something on the client is pasting what the remote copied.
    /// Stream that format of the remote clipboard as transfer `id`: one entry, sent
    /// straight away (as if fetched with seq 0, `single`), or close the transfer with an
    /// error if the clipboard can't give it.
    ClipboardRead {
        id: u64,
        format: ClipFormat,
    },

    /// Client → companion: let's open a direct link for transfers (see the `rdp-direct`
    /// crate): the fingerprint of this side's certificate for it, and the addresses
    /// (`ip:port`) it might be reached at.
    DirectOffer {
        #[serde(with = "serde_bytes")]
        fingerprint: Vec<u8>,
        candidates: Vec<String>,
    },
    /// Companion → client: the same for the companion, which now waits for the client.
    DirectAnswer {
        #[serde(with = "serde_bytes")]
        fingerprint: Vec<u8>,
        candidates: Vec<String>,
    },
    /// Companion → client: no direct link from here (why); transfers stay on this channel.
    DirectUnavailable {
        reason: String,
    },

    /// Client → companion: do `action` to the program at `path` (environment variables such
    /// as `%LOCALAPPDATA%` expanded on the remote) in the session. Answered with
    /// [`Message::AppControlDone`].
    AppControl {
        id: u64,
        action: AppAction,
        path: String,
    },
    /// Companion → client: how [`Message::AppControl`] `id` went (`error` if it failed), and
    /// whether the program was running before.
    AppControlDone {
        id: u64,
        error: Option<String>,
        was_running: bool,
    },

    /// Client → companion: files from this PC are being dragged onto the session at
    /// (`x`, `y`) (remote desktop pixels). Get a drag ready there and answer
    /// [`Message::DragReady`]; the client then presses the button there through the
    /// session, which starts a real drag on the remote that follows the pointer. The files
    /// are transfer `id`; their contents are fetched when dropped.
    DragIn {
        id: u64,
        entries: Vec<Entry>,
        x: i32,
        y: i32,
    },
    /// Companion → client: ready; press the button.
    DragReady {
        id: u64,
    },
    /// Companion → client: what dropping where the pointer is would do on the remote
    /// (`DROPEFFECT` bits; 0 = nothing).
    DragFeedback {
        id: u64,
        effect: u32,
    },
    /// Client → companion: the drag left the session or was cancelled.
    DragCancel {
        id: u64,
    },
    /// Companion → client: the remote drag ended (`effect`: what the drop did; 0 = no drop).
    DragEnded {
        id: u64,
        effect: u32,
    },

    /// Client → companion: the pointer left the session with the button held at (`x`, `y`)
    /// (remote pixels, at the edge). If files are being dragged there, offer them as
    /// transfer `id` ([`Message::DragOut`]) so the drag can carry on on the client; else
    /// [`Message::DragOutNone`]. The companion answers [`Message::DragOutReady`] first.
    DragOutProbe {
        id: u64,
        x: i32,
        y: i32,
    },
    /// Companion → client: a drop target is under the pointer; nudge the pointer so the
    /// remote drag (if any) finds it.
    DragOutReady {
        id: u64,
    },
    /// Companion → client: these files are being dragged (served as transfer `id`).
    DragOut {
        id: u64,
        entries: Vec<Entry>,
    },
    /// Companion → client: nothing it can carry over is being dragged.
    DragOutNone {
        id: u64,
    },
}

/// Fingerprint of a companion executable's bytes: the same build gives the same id on both
/// ends, whatever its version number says.
pub fn build_id(executable: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(executable)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Serializes a message into a self-framed `[len][body]` buffer.
pub fn encode(message: &Message) -> Vec<u8> {
    let body = rmp_serde::to_vec_named(message).expect("companion messages always serialize");
    let mut out = Vec::with_capacity(LEN_BYTES + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// A problem reading the channel: the far side sent something malformed or oversized.
#[derive(Debug)]
pub enum DecodeError {
    /// A frame claimed a length over [`MAX_FRAME`].
    TooLarge(usize),
    /// A frame body was not a valid message.
    Malformed(rmp_serde::decode::Error),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLarge(n) => write!(f, "companion frame too large ({n} bytes)"),
            Self::Malformed(e) => write!(f, "malformed companion frame: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Buffers transport bytes and yields whole [`Message`]s as they complete. One decoder
/// per channel; feed it every chunk the transport delivers.
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    /// Start of the unread part of `buffer` (consumed bytes are dropped lazily, so a
    /// burst of small frames doesn't shift the buffer once per frame).
    start: usize,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds received bytes. Follow with [`Decoder::next_message`] until it returns `None`.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Returns the next complete message, or `None` if more bytes are needed. An error
    /// means the stream is unusable and the channel should close.
    pub fn next_message(&mut self) -> Result<Option<Message>, DecodeError> {
        let pending = &self.buffer[self.start..];
        if pending.len() < LEN_BYTES {
            self.compact();
            return Ok(None);
        }
        let len = u32::from_le_bytes(pending[..LEN_BYTES].try_into().expect("4 bytes")) as usize;
        if len > MAX_FRAME {
            return Err(DecodeError::TooLarge(len));
        }
        if pending.len() < LEN_BYTES + len {
            self.compact();
            return Ok(None);
        }
        let message = rmp_serde::from_slice(&pending[LEN_BYTES..LEN_BYTES + len]).map_err(DecodeError::Malformed)?;
        self.start += LEN_BYTES + len;
        Ok(Some(message))
    }

    /// Drops consumed bytes once nothing complete is left to read.
    fn compact(&mut self) {
        if self.start > 0 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Message> {
        vec![
            Message::Hello {
                protocol: PROTOCOL_VERSION,
                version: "0.1.0".into(),
                host: "REMOTE-PC".into(),
                session: 2,
                build: build_id(b"companion"),
                watches_clipboard: true,
            },
            Message::Welcome {
                protocol: PROTOCOL_VERSION,
                version: "0.1.0".into(),
            },
            Message::Ping { nonce: 42 },
            Message::Pong { nonce: 42 },
            Message::Refused {
                reason: "old protocol".into(),
            },
            Message::StatsInterval { secs: 30 },
            Message::Stats(vec![HostStats {
                span: 30.0,
                cpu: 12.5,
                mem_used: 3 << 30,
                mem_total: 8 << 30,
                volumes: vec![Volume {
                    name: "C:".into(),
                    free: 1 << 30,
                    total: 2 << 30,
                }],
                ..Default::default()
            }]),
            Message::Offer {
                id: 2,
                entries: vec![Entry {
                    path: "docs\\a.txt".into(),
                    dir: false,
                    size: 5,
                    modified: 7,
                }],
                intent: Intent::Save {
                    folder: "%USERPROFILE%\\Desktop".into(),
                },
            },
            Message::Data {
                id: 2,
                seq: 1,
                index: 0,
                offset: 0,
                bytes: b"hello".to_vec(),
                compressed: false,
            },
            Message::ClipboardOffer {
                id: 4,
                formats: vec![
                    ClipFormat { id: 13, name: None },
                    ClipFormat {
                        id: 0xC0F1,
                        name: Some("HTML Format".into()),
                    },
                ],
            },
            Message::ClipboardRead {
                id: 6,
                format: ClipFormat { id: 13, name: None },
            },
            Message::DirectOffer {
                fingerprint: vec![7; 32],
                candidates: vec!["192.0.2.1:50000".into()],
            },
            Message::RemoteCopy {
                formats: vec![ClipFormat { id: 13, name: None }],
                files: false,
            },
            Message::AppControl {
                id: 8,
                action: AppAction::Restart,
                path: "%LOCALAPPDATA%\\Programs\\app\\app.exe".into(),
            },
            Message::Close { id: 2, error: None },
            Message::Bye,
        ]
    }

    #[test]
    fn round_trips_each_message() {
        for message in sample() {
            let mut decoder = Decoder::new();
            decoder.feed(&encode(&message));
            assert_eq!(decoder.next_message().unwrap(), Some(message));
            assert_eq!(decoder.next_message().unwrap(), None);
        }
    }

    #[test]
    fn data_is_sent_as_raw_bytes() {
        let bytes = vec![0xAB; 64 * 1024];
        let frame = encode(&Message::Data {
            id: 1,
            seq: 1,
            index: 0,
            offset: 0,
            bytes,
            compressed: false,
        });
        // MessagePack would spend up to two bytes per element on a plain integer array.
        assert!(frame.len() < 64 * 1024 + 100, "{} bytes", frame.len());
    }

    #[test]
    fn reassembles_across_chunks() {
        let encoded = encode(&Message::Ping { nonce: 7 });
        let mut decoder = Decoder::new();
        // One byte at a time: a message only appears once its last byte arrives.
        for (i, byte) in encoded.iter().enumerate() {
            decoder.feed(&[*byte]);
            let expected = if i + 1 == encoded.len() {
                Some(Message::Ping { nonce: 7 })
            } else {
                None
            };
            assert_eq!(decoder.next_message().unwrap(), expected);
        }
    }

    #[test]
    fn splits_several_messages_in_one_chunk() {
        let mut buffer = Vec::new();
        for message in sample() {
            buffer.extend_from_slice(&encode(&message));
        }
        let mut decoder = Decoder::new();
        decoder.feed(&buffer);
        for message in sample() {
            assert_eq!(decoder.next_message().unwrap(), Some(message));
        }
        assert_eq!(decoder.next_message().unwrap(), None);
    }

    #[test]
    fn knows_which_formats_are_plain_memory() {
        let standard = |id| ClipFormat { id, name: None };
        let named = |name: &str| ClipFormat {
            id: 0xC123,
            name: Some(name.into()),
        };
        assert!(standard(13).is_memory() && standard(8).is_memory() && standard(17).is_memory());
        // Bitmaps, metafiles, palettes and file lists are handles, not bytes.
        for id in [2, 3, 9, 14, 15, 0x82, 0x200, 0x300] {
            assert!(!standard(id).is_memory(), "{id:#x}");
        }
        assert!(named("HTML Format").is_memory());
        assert!(!named(CLIPBOARD_MARKER).is_memory());
        assert!(!ClipFormat { id: 0xC123, name: None }.is_memory());
    }

    #[test]
    fn rejects_an_oversized_frame() {
        let mut decoder = Decoder::new();
        decoder.feed(&((MAX_FRAME as u32 + 1).to_le_bytes()));
        assert!(matches!(decoder.next_message(), Err(DecodeError::TooLarge(_))));
    }
}
