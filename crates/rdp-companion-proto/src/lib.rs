//! Wire protocol shared by the RDP App client and the remote-side companion.
//!
//! The companion is a small program that runs inside the remote session and talks to
//! the client over a dynamic virtual channel ([`CHANNEL_NAME`]). This crate is the one
//! thing both sides agree on: the message set and how messages are framed.
//!
//! Framing is self-describing (a 4-byte little-endian length, then a MessagePack body)
//! so it survives whatever chunking the transport applies: a single DVC message on the
//! client side, `CHANNEL_CHUNK_LENGTH` pieces on the WTS side. A [`Decoder`] buffers
//! bytes and yields whole messages as they complete.
//!
//! The base protocol is a handshake and a heartbeat; file transfer and native drag
//! messages extend [`Message`] later. Both sides exchange [`PROTOCOL_VERSION`] up front
//! and refuse a mismatch, so an old companion meeting a new client fails cleanly rather
//! than misreading a message.

use serde::{Deserialize, Serialize};

/// Dynamic virtual channel the companion opens on the remote side. Kept short and
/// stable: changing it breaks every deployed companion.
pub const CHANNEL_NAME: &str = "RdpApp::Companion";

/// Bumped on any breaking change to [`Message`]. Both sides send it in the handshake.
pub const PROTOCOL_VERSION: u32 = 1;

/// A frame larger than this is treated as a desync and ends the channel, rather than
/// buffering without bound.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

const LEN_BYTES: usize = 4;

/// One message in each direction. Serialized as MessagePack; variants are only ever
/// added, and a version bump guards against an old peer misreading a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds received bytes. Follow with [`Decoder::next`] until it returns `None`.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Returns the next complete message, or `None` if more bytes are needed. An error
    /// means the stream is unusable and the channel should close.
    pub fn next_message(&mut self) -> Result<Option<Message>, DecodeError> {
        if self.buffer.len() < LEN_BYTES {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buffer[..LEN_BYTES].try_into().expect("4 bytes")) as usize;
        if len > MAX_FRAME {
            return Err(DecodeError::TooLarge(len));
        }
        if self.buffer.len() < LEN_BYTES + len {
            return Ok(None);
        }
        let body = &self.buffer[LEN_BYTES..LEN_BYTES + len];
        let message = rmp_serde::from_slice(body).map_err(DecodeError::Malformed)?;
        self.buffer.drain(..LEN_BYTES + len);
        Ok(Some(message))
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
