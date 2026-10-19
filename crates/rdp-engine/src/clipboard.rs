//! Clipboard redirection (CLIPRDR) plumbing.
//!
//! The OS side of the clipboard lives in the host (it needs a GUI thread); the
//! engine hands each connection a [`SessionClipboard`] proxy that feeds clipboard
//! events back into that session's command queue.

use ironrdp_cliprdr::CliprdrClient;
use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackendFactory};
use ironrdp_session::ActiveStage;
use tokio::sync::mpsc;
use tracing::warn;

use crate::Command;

/// Provides OS clipboard backends for sessions.
pub trait ClipboardHost: Send + Sync {
    /// Creates the clipboard backend for one connection of `session`.
    fn attach(&self, session: &str, proxy: SessionClipboard) -> Option<Box<dyn CliprdrBackendFactory + Send>>;

    /// The connection ended; release whatever `attach` created.
    fn detach(&self, session: &str);
}

/// Routes clipboard backend events to one session.
#[derive(Clone)]
pub struct SessionClipboard {
    tx: mpsc::UnboundedSender<Command>,
}

impl SessionClipboard {
    pub(crate) fn new(tx: mpsc::UnboundedSender<Command>) -> Self {
        Self { tx }
    }
}

impl std::fmt::Debug for SessionClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionClipboard")
    }
}

impl ClipboardMessageProxy for SessionClipboard {
    fn send_clipboard_message(&self, message: ClipboardMessage) {
        let _ = self.tx.send(Command::Clipboard(message));
    }
}

/// Encodes a clipboard backend event for the server.
pub(crate) fn encode(stage: &mut ActiveStage, message: ClipboardMessage) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
        return Ok(None);
    };
    let messages = match message {
        ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats),
        ClipboardMessage::SendInitiateFileCopy(files) => cliprdr.initiate_file_copy(files),
        ClipboardMessage::SendFormatData(response) => cliprdr.submit_format_data(response),
        ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format),
        ClipboardMessage::SendFileContentsRequest(request) => cliprdr.request_file_contents(request),
        ClipboardMessage::SendFileContentsResponse(response) => cliprdr.submit_file_contents(response),
        ClipboardMessage::Error(error) => {
            warn!(%error, "clipboard backend error");
            return Ok(None);
        }
    }
    .map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    let frame = stage
        .process_svc_processor_messages(messages)
        .map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    Ok(Some(frame))
}
