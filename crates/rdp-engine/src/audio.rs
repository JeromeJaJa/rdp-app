//! Remote audio (RDPSND playback, RDPEAI microphone).
//!
//! With dozens of sessions open, only the one on screen should normally make
//! noise: playback goes through a gate the session opens and closes as it is
//! shown and hidden. The OS output stream is created lazily on the first
//! audible block, so muted sessions cost nothing.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ironrdp_dvc::{DvcChannelListener, DvcClientProcessor};
use ironrdp_rdpsnd::client::{NoopRdpsndBackend, Rdpsnd, RdpsndClientHandler};
use ironrdp_rdpsnd::pdu::{AudioFormat, AudioFormatFlags, PitchPdu, VolumePdu};
use tokio::sync::mpsc;

use crate::Command;

/// Forwards audio to the real backend only while `audible` is set.
#[derive(Debug)]
struct Gated {
    inner: Box<dyn RdpsndClientHandler>,
    audible: Arc<AtomicBool>,
}

impl RdpsndClientHandler for Gated {
    fn get_flags(&self) -> AudioFormatFlags {
        self.inner.get_flags()
    }

    fn get_formats(&self) -> &[AudioFormat] {
        self.inner.get_formats()
    }

    fn wave(&mut self, format: &AudioFormat, ts: u32, data: Cow<'_, [u8]>) {
        if self.audible.load(Ordering::Relaxed) {
            self.inner.wave(format, ts, data);
        }
    }

    fn set_volume(&mut self, volume: VolumePdu) {
        self.inner.set_volume(volume);
    }

    fn set_pitch(&mut self, pitch: PitchPdu) {
        self.inner.set_pitch(pitch);
    }

    fn close(&mut self) {
        self.inner.close();
    }
}

/// The RDPSND channel: real playback behind a gate, or a no-op (Windows expects
/// RDPSND whenever device redirection is used).
pub(crate) fn rdpsnd(playback: bool, audible: Arc<AtomicBool>) -> Rdpsnd {
    let handler: Box<dyn RdpsndClientHandler> = if playback {
        #[cfg(windows)]
        {
            Box::new(Gated {
                inner: Box::new(ironrdp_rdpsnd_native::cpal::RdpsndBackend::new()),
                audible,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = audible;
            Box::new(NoopRdpsndBackend)
        }
    } else {
        let _ = audible;
        Box::new(NoopRdpsndBackend)
    };
    Rdpsnd::new(handler)
}

/// Creates a fresh microphone channel every time the server opens AUDIO_INPUT
/// (Windows opens and closes it as remote apps grab the microphone).
pub(crate) struct MicrophoneListener {
    pub tx: mpsc::UnboundedSender<Command>,
}

impl DvcChannelListener for MicrophoneListener {
    fn channel_name(&self) -> &str {
        ironrdp_rdpeai::CHANNEL_NAME
    }

    fn create(&mut self, _channel_id: ironrdp_dvc::DynamicChannelId) -> Option<Box<dyn DvcClientProcessor>> {
        #[cfg(windows)]
        {
            let tx = self.tx.clone();
            Some(Box::new(ironrdp_rdpeai::client::RdpeaiClient::new(
                Box::new(ironrdp_rdpsnd_native::capture::RdpeaiCaptureBackend::new()),
                Box::new(move |_channel_id, messages| {
                    tx.send(Command::SendDvc(messages))
                        .map_err(|_| ironrdp_pdu::pdu_other_err!("session closed"))?;
                    Ok(())
                }),
            )))
        }
        #[cfg(not(windows))]
        {
            let _ = &self.tx;
            None
        }
    }
}
