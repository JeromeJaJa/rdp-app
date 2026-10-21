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
pub const PROTOCOL_VERSION: u32 = 2;

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
    /// Companion → client: one sample.
    Stats(HostStats),

    /// Source → sink: files on offer as transfer `id`. Ids are picked by whoever offers:
    /// even on the client, odd on the companion, so they never collide.
    Offer {
        id: u64,
        entries: Vec<Entry>,
        intent: Intent,
    },
    /// Sink → source: stream entry `index` from `offset`, then carry on through the
    /// following entries until the window fills. `seq` numbers fetches so the sink can
    /// drop data still in flight from an earlier one.
    Fetch {
        id: u64,
        seq: u32,
        index: u32,
        offset: u64,
    },
    /// Source → sink: bytes of entry `index` at `offset`.
    Data {
        id: u64,
        seq: u32,
        index: u32,
        offset: u64,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
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
            Message::Stats(HostStats {
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
            }),
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
    fn rejects_an_oversized_frame() {
        let mut decoder = Decoder::new();
        decoder.feed(&((MAX_FRAME as u32 + 1).to_le_bytes()));
        assert!(matches!(decoder.next_message(), Err(DecodeError::TooLarge(_))));
    }
}
