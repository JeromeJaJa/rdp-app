//! Client side of the companion channel.
//!
//! A companion running inside the remote session opens the dynamic virtual channel
//! [`rdp_companion_proto::CHANNEL_NAME`]; we register a listener for it, so the channel
//! is only ever created at the remote's request. This module does the handshake and
//! heartbeat; everything after that (host stats, file transfers) belongs to the host
//! app, which receives the messages through a [`CompanionHost`] and answers through a
//! [`CompanionLink`].
//!
//! The processor runs on the session thread. Replies it returns from `process` are sent
//! automatically; presence changes go to the session loop as [`Command::Companion`],
//! which turns them into engine events, keeping all event emission in one place.

use std::sync::Arc;

use ironrdp_core::{Encode, EncodeResult, WriteCursor, ensure_size, impl_as_any};
use ironrdp_dvc::{DvcChannelListener, DvcClientProcessor, DvcEncode, DvcMessage, DvcProcessor};
use ironrdp_pdu::{PduResult, pdu_other_err};
use ironrdp_svc::SvcMessage;
use rdp_companion_proto::{CHANNEL_NAME, Decoder, Message, PROTOCOL_VERSION, encode};
use tokio::sync::mpsc;

use crate::Command;

/// A presence change reported by the channel; the session loop emits the engine event.
#[derive(Debug, Clone)]
pub(crate) enum Update {
    Connected {
        version: String,
        host: String,
        build: String,
    },
    /// A companion speaking another protocol said hello and was turned away.
    Refused {
        protocol: u32,
    },
    Disconnected,
}

/// Receives companions as they attach to sessions (the host app's side of the channel).
pub trait CompanionHost: Send + Sync {
    /// A companion attached to `session` and completed the handshake. Its later messages
    /// go to the returned handler; `link` sends to it.
    /// `watches_clipboard`: it announces what the remote copies (`Message::RemoteCopy`).
    fn attached(
        &self,
        session: &str,
        link: CompanionLink,
        version: &str,
        host: &str,
        watches_clipboard: bool,
    ) -> Arc<dyn CompanionHandler>;
}

/// One attached companion's messages.
pub trait CompanionHandler: Send + Sync {
    /// A message from the companion. Runs on the session thread: hand work off, never
    /// block.
    fn message(&self, message: Message);

    /// The channel closed (the companion exited, or the connection ended).
    fn detached(&self);
}

/// Sends messages to one attached companion, from any thread. They queue behind the
/// session's own traffic (input first), so bulk transfers don't add input lag. Dead
/// once that connection ends.
#[derive(Clone)]
pub struct CompanionLink {
    channel_id: u32,
    bulk: mpsc::UnboundedSender<Vec<SvcMessage>>,
}

impl CompanionLink {
    /// Queues `message`. False when the connection is gone.
    pub fn send(&self, message: &Message) -> bool {
        let frame: DvcMessage = Box::new(Frame(encode(message)));
        match ironrdp_dvc::encode_dvc_messages(self.channel_id, vec![frame], ironrdp_svc::ChannelFlags::empty()) {
            Ok(messages) => self.bulk.send(messages).is_ok(),
            Err(error) => {
                tracing::warn!(%error, "companion message not encoded");
                false
            }
        }
    }

    /// Whether the connection it belongs to is still up.
    pub fn is_open(&self) -> bool {
        !self.bulk.is_closed()
    }
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
    session: String,
    commands: mpsc::UnboundedSender<Command>,
    bulk: mpsc::UnboundedSender<Vec<SvcMessage>>,
    host: Option<Arc<dyn CompanionHost>>,
    client_version: String,
}

impl CompanionListener {
    pub(crate) fn new(
        session: String,
        commands: mpsc::UnboundedSender<Command>,
        bulk: mpsc::UnboundedSender<Vec<SvcMessage>>,
        host: Option<Arc<dyn CompanionHost>>,
    ) -> Self {
        Self {
            session,
            commands,
            bulk,
            host,
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

impl DvcChannelListener for CompanionListener {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn create(&mut self, channel_id: ironrdp_dvc::DynamicChannelId) -> Option<Box<dyn DvcClientProcessor>> {
        Some(Box::new(CompanionChannel {
            decoder: Decoder::new(),
            session: self.session.clone(),
            commands: self.commands.clone(),
            link: CompanionLink {
                channel_id,
                bulk: self.bulk.clone(),
            },
            host: self.host.clone(),
            client_version: self.client_version.clone(),
            connected: false,
            handler: None,
        }))
    }
}

struct CompanionChannel {
    decoder: Decoder,
    session: String,
    commands: mpsc::UnboundedSender<Command>,
    link: CompanionLink,
    host: Option<Arc<dyn CompanionHost>>,
    client_version: String,
    connected: bool,
    handler: Option<Arc<dyn CompanionHandler>>,
}

impl CompanionChannel {
    fn report(&mut self, update: Update) {
        let _ = self.commands.send(Command::Companion(update));
    }

    /// Handles one decoded message, pushing any reply.
    fn handle(&mut self, message: Message, out: &mut Vec<DvcMessage>) {
        match message {
            Message::Hello {
                protocol,
                version,
                host,
                build,
                watches_clipboard,
                ..
            } => {
                if protocol != PROTOCOL_VERSION {
                    tracing::info!(
                        %version,
                        protocol,
                        ours = PROTOCOL_VERSION,
                        "companion speaks another protocol; refusing it"
                    );
                    out.push(frame(&Message::Refused {
                        reason: format!("client speaks protocol {PROTOCOL_VERSION}, companion {protocol}"),
                    }));
                    self.report(Update::Refused { protocol });
                    return;
                }
                self.connected = true;
                tracing::info!(%version, %host, "companion connected");
                self.report(Update::Connected {
                    version: version.clone(),
                    host: host.clone(),
                    build,
                });
                out.push(frame(&Message::Welcome {
                    protocol: PROTOCOL_VERSION,
                    version: self.client_version.clone(),
                }));
                if let Some(old) = self.handler.take() {
                    old.detached();
                }
                self.handler = self
                    .host
                    .as_ref()
                    .map(|h| h.attached(&self.session, self.link.clone(), &version, &host, watches_clipboard));
            }
            Message::Ping { nonce } => out.push(frame(&Message::Pong { nonce })),
            Message::Pong { .. } => {}
            Message::Bye => self.detach(),
            message => {
                if let Some(handler) = &self.handler {
                    handler.message(message);
                }
            }
        }
    }

    fn detach(&mut self) {
        if let Some(handler) = self.handler.take() {
            handler.detached();
        }
        if std::mem::take(&mut self.connected) {
            self.report(Update::Disconnected);
        }
    }
}

impl Drop for CompanionChannel {
    /// The connection ended without the channel closing first.
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take() {
            handler.detached();
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
        self.detach();
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
