//! Client side of the companion channel.
//!
//! A companion running inside the remote session opens the dynamic virtual channel
//! [`rdp_companion_proto::CHANNEL_NAME`]; we register a listener for it, so the channel
//! is only ever created at the remote's request. The exchange is the handshake and
//! heartbeat from [`rdp_companion_proto`]; richer features (fast file transfer, native
//! drag) extend the message set later.
//!
//! The processor runs on the session thread. Replies it returns from `process` are sent
//! automatically; presence changes go to the session loop as [`Command::Companion`],
//! which turns them into engine events, keeping all event emission in one place.

use ironrdp_core::{Encode, EncodeResult, WriteCursor, ensure_size, impl_as_any};
use ironrdp_dvc::{DvcChannelListener, DvcClientProcessor, DvcEncode, DvcMessage, DvcProcessor};
use ironrdp_pdu::{PduResult, pdu_other_err};
use rdp_companion_proto::{CHANNEL_NAME, Decoder, Message, PROTOCOL_VERSION, encode};
use tokio::sync::mpsc;

use crate::Command;

/// A presence change reported by the channel; the session loop emits the engine event.
#[derive(Debug, Clone)]
pub(crate) enum Update {
    Connected { version: String, host: String },
    Disconnected,
}

/// A pre-framed companion message on the wire (the frame is built by the proto crate).
struct Frame(Vec<u8>);

impl Encode for Frame {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.0.len());
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "RdpAppCompanion"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

impl DvcEncode for Frame {}

fn frame(message: &Message) -> DvcMessage {
    Box::new(Frame(encode(message)))
}

/// Accepts the companion's channel when the remote opens it.
pub(crate) struct CompanionListener {
    commands: mpsc::UnboundedSender<Command>,
    client_version: String,
}

impl CompanionListener {
    pub(crate) fn new(commands: mpsc::UnboundedSender<Command>) -> Self {
        Self {
            commands,
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

impl DvcChannelListener for CompanionListener {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn create(&mut self, _channel_id: ironrdp_dvc::DynamicChannelId) -> Option<Box<dyn DvcClientProcessor>> {
        Some(Box::new(CompanionChannel {
            decoder: Decoder::new(),
            commands: self.commands.clone(),
            client_version: self.client_version.clone(),
            connected: false,
        }))
    }
}

struct CompanionChannel {
    decoder: Decoder,
    commands: mpsc::UnboundedSender<Command>,
    client_version: String,
    connected: bool,
}

impl CompanionChannel {
    fn report(&mut self, update: Update) {
        let _ = self.commands.send(Command::Companion(update));
    }

    /// Handles one decoded message, pushing any reply.
    fn handle(&mut self, message: Message, out: &mut Vec<DvcMessage>) {
        match message {
            Message::Hello { protocol, version, host, .. } => {
                if protocol != PROTOCOL_VERSION {
                    out.push(frame(&Message::Refused {
                        reason: format!("client speaks protocol {PROTOCOL_VERSION}, companion {protocol}"),
                    }));
                    return;
                }
                self.connected = true;
                tracing::info!(%version, %host, "companion connected");
                self.report(Update::Connected { version, host });
                out.push(frame(&Message::Welcome {
                    protocol: PROTOCOL_VERSION,
                    version: self.client_version.clone(),
                }));
            }
            Message::Ping { nonce } => out.push(frame(&Message::Pong { nonce })),
            Message::Bye => {
                if std::mem::take(&mut self.connected) {
                    self.report(Update::Disconnected);
                }
            }
            // The client doesn't initiate these yet.
            Message::Welcome { .. } | Message::Refused { .. } | Message::Pong { .. } => {}
        }
    }
}

impl_as_any!(CompanionChannel);

impl DvcProcessor for CompanionChannel {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        // The companion speaks first (Hello).
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        self.decoder.feed(payload);
        let mut out = Vec::new();
        loop {
            match self.decoder.next_message() {
                Ok(Some(message)) => self.handle(message, &mut out),
                Ok(None) => break,
                Err(error) => return Err(pdu_other_err!("companion channel", source: CompanionError(error))),
            }
        }
        Ok(out)
    }

    fn close(&mut self, _channel_id: u32) {
        if std::mem::take(&mut self.connected) {
            self.report(Update::Disconnected);
        }
    }
}

impl DvcClientProcessor for CompanionChannel {}

/// Wraps a decode error so it can be a `pdu_other_err` source.
#[derive(Debug)]
struct CompanionError(rdp_companion_proto::DecodeError);

impl core::fmt::Display for CompanionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl core::error::Error for CompanionError {}
