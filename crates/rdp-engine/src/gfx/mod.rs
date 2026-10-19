//! Graphics Pipeline (MS-RDPEGFX): capability selection, decoder choice and
//! per-codec statistics used to report what the server actually sends.

mod avc444;
mod client;
mod h264;
#[cfg(windows)]
mod mf;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ironrdp_egfx::pdu::{
    CapabilitiesV8Flags, CapabilitiesV10Flags, CapabilitiesV81Flags, CapabilitiesV103Flags, CapabilitiesV104Flags,
    CapabilitiesV107Flags, CapabilitySet,
};
use tracing::{info, warn};

pub(crate) use client::{GfxClient, GfxOutput, SharedOutput};

use crate::{CodecPreference, ConnectParams};

#[derive(Default)]
pub(crate) struct GfxStats {
    pub confirmed: AtomicBool,
    pub avc420_enabled: AtomicBool,
    pub avc444_enabled: AtomicBool,
    pub avc: AtomicU64,
    pub avc444: AtomicU64,
    /// The last H.264 picture was decoded on the GPU.
    pub hardware: AtomicBool,
    pub progressive: AtomicU64,
    pub clear: AtomicU64,
    pub planar: AtomicU64,
    pub other: AtomicU64,
    pub unhandled: AtomicU64,
    /// H.264 frames that failed to decode (the session survives them).
    pub decode_errors: AtomicU64,
    /// A decode error happened: the session should ask the server for a full repaint.
    pub needs_refresh: AtomicBool,
    /// RemoteFX Progressive tiles by kind (counted only with debug logging on):
    /// simple, first pass, first pass at full quality, upgrade, upgrade reaching full quality.
    pub prog_tiles: [AtomicU64; 5],
    /// Cumulative time decoding + converting H.264, microseconds.
    pub decode_us: AtomicU64,
    pub decoded: AtomicU64,
}

impl GfxStats {
    /// Human-readable description of what is being decoded, once known.
    pub(crate) fn label(&self) -> Option<String> {
        if !self.confirmed.load(Ordering::Relaxed) {
            return None;
        }
        let avc = self.avc.load(Ordering::Relaxed);
        let avc444 = self.avc444.load(Ordering::Relaxed);
        let progressive = self.progressive.load(Ordering::Relaxed);
        let clear = self.clear.load(Ordering::Relaxed);
        let planar = self.planar.load(Ordering::Relaxed);
        let gpu = if self.hardware.load(Ordering::Relaxed) {
            " · GPU"
        } else {
            ""
        };
        if avc444 > 0 {
            Some(format!("H.264 · AVC444{gpu}"))
        } else if avc > 0 {
            Some(format!("H.264 · AVC420{gpu}"))
        } else if progressive > 0 {
            Some("RemoteFX Progressive".into())
        } else if clear + planar > 0 {
            Some("Graphics Pipeline".into())
        } else {
            None
        }
    }
}

/// Builds the graphics pipeline client, or `None` when the classic (non-GFX) path is preferred.
pub(crate) fn build(params: &ConnectParams, stats: Arc<GfxStats>, output: SharedOutput) -> Option<GfxClient> {
    if params.codec == CodecPreference::RemoteFx {
        return None;
    }

    let decoder = h264::new_decoder(params.decoder);
    let mut caps = Vec::new();
    match &decoder {
        Some(d) => {
            info!(decoder = d.name(), avc444 = params.h264_444, "H.264 available");
            if params.h264_444 {
                caps.extend(avc444_capabilities());
            }
            caps.push(CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
            });
        }
        None => warn!("no H.264 decoder available; using non-AVC graphics pipeline codecs"),
    }
    caps.push(CapabilitySet::V8 {
        flags: CapabilitiesV8Flags::SMALL_CACHE,
    });

    Some(GfxClient::new(caps, decoder, params.decoder, stats, output))
}

/// Version 10.x capability sets. They imply AVC444 support (the server then picks
/// AVC420 or AVC444 per frame); the server chooses the highest version it knows.
fn avc444_capabilities() -> Vec<CapabilitySet> {
    vec![
        CapabilitySet::V10_7 {
            flags: CapabilitiesV107Flags::SMALL_CACHE,
        },
        CapabilitySet::V10_6 {
            flags: CapabilitiesV104Flags::SMALL_CACHE,
        },
        CapabilitySet::V10_5 {
            flags: CapabilitiesV104Flags::SMALL_CACHE,
        },
        CapabilitySet::V10_4 {
            flags: CapabilitiesV104Flags::SMALL_CACHE,
        },
        CapabilitySet::V10_3 {
            flags: CapabilitiesV103Flags::empty(),
        },
        CapabilitySet::V10_2 {
            flags: CapabilitiesV10Flags::SMALL_CACHE,
        },
        CapabilitySet::V10_1,
        CapabilitySet::V10 {
            flags: CapabilitiesV10Flags::SMALL_CACHE,
        },
    ]
}

/// (AVC420, AVC444) availability under a confirmed capability set.
pub(crate) fn avc_support(caps: &CapabilitySet) -> (bool, bool) {
    let avc = match caps {
        CapabilitySet::V8 { .. } => return (false, false),
        CapabilitySet::V8_1 { flags } => return (flags.contains(CapabilitiesV81Flags::AVC420_ENABLED), false),
        CapabilitySet::V10 { flags } | CapabilitySet::V10_2 { flags } => {
            !flags.contains(CapabilitiesV10Flags::AVC_DISABLED)
        }
        CapabilitySet::V10_1 => true,
        CapabilitySet::V10_3 { flags } => !flags.contains(CapabilitiesV103Flags::AVC_DISABLED),
        CapabilitySet::V10_4 { flags }
        | CapabilitySet::V10_5 { flags }
        | CapabilitySet::V10_6 { flags }
        | CapabilitySet::V10_6Err { flags } => !flags.contains(CapabilitiesV104Flags::AVC_DISABLED),
        CapabilitySet::V10_7 { flags } => !flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
    };
    (avc, avc)
}
