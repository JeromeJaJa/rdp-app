//! Continuous network auto-detection (MS-RDPBCGR 2.2.14, 3.1.5.x).
//!
//! The client advertises auto-detect support, and Windows then measures the
//! bandwidth throughout the session (Bandwidth Measure Start … Stop). Without
//! results it assumes a poor link: RemoteFX Progressive stays at the coarse first
//! pass and never sends refinement passes, which looks blurry and blocky. IronRDP
//! answers RTT probes only, so the bandwidth measurements are answered here.

use std::time::Instant;

use ironrdp_core::{Decode as _, ReadCursor, WriteBuf, decode};
use ironrdp_pdu::Action;
use ironrdp_pdu::rdp::autodetect::{
    AutoDetectReqPdu, AutoDetectRequest, AutoDetectResponse, AutoDetectRspPdu, BW_RESULTS_CONTINUOUS,
    BW_START_CONNECT_TIME, BW_STOP_CONNECT_TIME,
};
use ironrdp_pdu::rdp::headers::{BasicSecurityHeader, BasicSecurityHeaderFlags};
use tracing::debug;

pub(crate) struct BandwidthProbe {
    user_channel_id: u16,
    message_channel_id: Option<u16>,
    /// Receipt time of the current measurement's Start, and bytes received since.
    measuring: Option<(Instant, u64)>,
}

impl BandwidthProbe {
    pub fn new(user_channel_id: u16, message_channel_id: Option<u16>) -> Self {
        Self {
            user_channel_id,
            message_channel_id,
            measuring: None,
        }
    }

    /// Looks at one received frame; returns a response to send, if any.
    pub fn on_frame(&mut self, action: Action, frame: &[u8]) -> Option<Vec<u8>> {
        let message_channel = self.message_channel_id?;
        if let Some((_, bytes)) = self.measuring.as_mut() {
            *bytes += frame.len() as u64;
        }
        if action != Action::X224 {
            return None;
        }
        let ctx = ironrdp_pdu::mcs::decode_send_data_indication(frame).ok()?;
        if ctx.channel_id != message_channel {
            return None;
        }
        let header = BasicSecurityHeader::decode(&mut ReadCursor::new(ctx.user_data)).ok()?;
        let flags = header
            .flags
            .difference(BasicSecurityHeaderFlags::RESET_SEQNO | BasicSecurityHeaderFlags::IGNORE_SEQNO);
        if flags != BasicSecurityHeaderFlags::AUTODETECT_REQ {
            return None;
        }
        // Connect-time measurements are handled by the connector.
        match decode::<AutoDetectReqPdu>(ctx.user_data).ok()?.request {
            AutoDetectRequest::BandwidthMeasureStart { request_type, .. } if request_type != BW_START_CONNECT_TIME => {
                self.measuring = Some((Instant::now(), 0));
                None
            }
            AutoDetectRequest::BandwidthMeasureStop {
                sequence_number,
                request_type,
                ..
            } if request_type != BW_STOP_CONNECT_TIME => {
                let (started, bytes) = self.measuring.take()?;
                let time_delta_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX).max(1);
                let byte_count = u32::try_from(bytes).unwrap_or(u32::MAX);
                let response = AutoDetectRspPdu::new(AutoDetectResponse::BandwidthMeasureResults {
                    sequence_number,
                    response_type: BW_RESULTS_CONTINUOUS,
                    time_delta_ms,
                    byte_count,
                });
                let mut out = WriteBuf::new();
                ironrdp_pdu::mcs::encode_send_data_request(self.user_channel_id, message_channel, &response, &mut out)
                    .ok()?;
                debug!(time_delta_ms, byte_count, "answered bandwidth measurement");
                Some(out.into_inner())
            }
            _ => None,
        }
    }
}
