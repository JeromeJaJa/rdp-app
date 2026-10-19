//! H.264 (AVC420, or AVC444 with `--avc444` / `--avc444v2`) output over the graphics
//! pipeline, like a modern Windows host.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ironrdp_egfx::encode::{EncodeFrame, H264Encoder as _, OpenH264Encoder};
use ironrdp_egfx::pdu::{Avc420Region, CapabilitiesAdvertisePdu, CapabilitySet, Encoding};
use ironrdp_egfx::server::{GraphicsPipelineHandler, GraphicsPipelineServer};
use ironrdp_server::{
    DesktopSize, EgfxServerMessage, GfxDvcBridge, GfxServerFactory, GfxServerHandle, ServerEvent, ServerEventSender,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::avc444::{Avc444Encoder, Layout};
use crate::{Animator, Shared};

enum Encoded {
    Avc420(Vec<u8>),
    /// Main and auxiliary views.
    Avc444(Vec<u8>, Vec<u8>),
}

#[derive(Clone, Default)]
pub struct GfxSlots {
    server: Arc<Mutex<Option<GfxServerHandle>>>,
    sender: Arc<Mutex<Option<UnboundedSender<ServerEvent>>>>,
}

pub struct Factory {
    pub slots: GfxSlots,
}

struct Handler;

impl GraphicsPipelineHandler for Handler {
    fn capabilities_advertise(&mut self, _pdu: &CapabilitiesAdvertisePdu) {}
    fn on_ready(&mut self, negotiated: &CapabilitySet) {
        tracing::info!(?negotiated, "graphics pipeline ready");
    }
}

impl ServerEventSender for Factory {
    fn set_sender(&mut self, sender: UnboundedSender<ServerEvent>) {
        *self.slots.sender.lock().unwrap() = Some(sender);
    }
}

impl GfxServerFactory for Factory {
    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        Box::new(Handler)
    }

    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        let server: GfxServerHandle = Arc::new(Mutex::new(GraphicsPipelineServer::new(Box::new(Handler))));
        *self.slots.server.lock().unwrap() = Some(Arc::clone(&server));
        Some((GfxDvcBridge::new(Arc::clone(&server)), server))
    }
}

/// Swatch colours for `--mix`: lossless codecs must reproduce these exactly.
pub const PALETTE: [(u8, u8, u8); 6] = [
    (230, 57, 70),
    (241, 250, 238),
    (168, 218, 220),
    (69, 123, 157),
    (29, 53, 87),
    (255, 183, 3),
];

/// Streams the animated desktop as H.264 frames whenever a client's pipeline is ready.
pub async fn produce(
    port: u16,
    slots: GfxSlots,
    size: Arc<Mutex<DesktopSize>>,
    shared: Arc<Mutex<Shared>>,
    fps: u32,
    drop_every: u32,
    mix: bool,
    avc444: Option<Layout>,
) {
    let mut clear = ironrdp_graphics::clearcodec::ClearCodecEncoder::new();
    let interval = Duration::from_millis(u64::from(1000 / fps.max(1)));
    let started = Instant::now();
    #[allow(clippy::type_complexity)]
    let mut current: Option<(GfxServerHandle, u16, Animator, OpenH264Encoder, Option<Avc444Encoder>)> = None;
    let mut frame_no: u64 = 0;
    let mut sent_full = false;

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let Some(handle) = slots.server.lock().unwrap().clone() else {
            continue;
        };
        // The DVC bridge holds a reference while a client is connected; once only our
        // own handles remain, the client is gone and there is nothing to encode for.
        let ours = 2 + usize::from(current.as_ref().is_some_and(|(h, ..)| Arc::ptr_eq(h, &handle)));
        if Arc::strong_count(&handle) <= ours {
            current = None;
            continue;
        }

        // New connection → new pipeline server: start over with a fresh surface + encoder.
        if current.as_ref().is_none_or(|(h, ..)| !Arc::ptr_eq(h, &handle)) {
            let mut server = handle.lock().unwrap();
            if !server.is_ready() {
                continue;
            }
            let desktop = *size.lock().unwrap();
            let (w, h) = (desktop.width & !1, desktop.height & !1);
            let Some(surface) = server.create_surface(w, h) else {
                continue;
            };
            server.map_surface_to_output(surface, 0, 0);
            let Ok(encoder) = OpenH264Encoder::new() else {
                tracing::error!("OpenH264 encoder unavailable");
                return;
            };
            // AVC444 only when asked for and the client negotiated a version-10 capability set.
            let enc444 = match avc444 {
                Some(layout) if server.supports_avc444() => match Avc444Encoder::new(layout) {
                    Ok(e) => Some(e),
                    Err(e) => {
                        tracing::error!(error = %e, "AVC444 encoder unavailable");
                        None
                    }
                },
                Some(_) => {
                    tracing::info!("client did not negotiate AVC444; sending AVC420");
                    None
                }
                None => None,
            };
            let mut animator = Animator::new(port, DesktopSize { width: w, height: h }, Arc::clone(&shared));
            if avc444.is_some() {
                animator.enable_chroma_detail();
            }
            drop(server);
            current = Some((Arc::clone(&handle), surface, animator, encoder, enc444));
            sent_full = false;
        }

        let Some((handle, surface, animator, encoder, enc444)) = current.as_mut() else {
            continue;
        };
        frame_no += 1;
        // Periodic key frames, as a real host produces after loss/refresh.
        if frame_no % 60 == 0 {
            encoder.request_key_frame();
            if let Some(e) = enc444.as_mut() {
                e.request_key_frame();
            }
        }
        let (bar, marker) = animator.step(started.elapsed().as_secs_f32());
        // Swatches change colour every half second; lossless codecs must reproduce
        // the palette exactly (see PALETTE).
        let period = frame_no / 15;
        let origin_swatch = mix.then(|| animator.swatch(0, 0, 96, PALETTE[(period % 6) as usize]));
        let clear_swatch = mix.then(|| animator.swatch(120, 0, 96, PALETTE[((period + 3) % 6) as usize]));
        let rgba = animator.rgba();
        let (w, h) = (u32::from(animator.width()), u32::from(animator.height()));
        let encoded = match enc444.as_mut() {
            Some(e) => e
                .encode(&rgba, w as usize, h as usize)
                .map(|(main, aux)| Encoded::Avc444(main, aux)),
            None => encoder
                .encode(EncodeFrame {
                    data: &rgba,
                    width: w,
                    height: h,
                })
                .map(Encoded::Avc420)
                .map_err(anyhow::Error::from),
        };
        let encoded = match encoded {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "encode failed");
                continue;
            }
        };

        // Fault injection: silently lose a P-frame to break the decoder's reference chain.
        if drop_every > 0 && frame_no % u64::from(drop_every) == 0 {
            continue;
        }

        let (channel_id, messages) = {
            let mut server = handle.lock().unwrap();
            let first = !sent_full;
            let region = if mix && !first && enc444.is_none() {
                // Only the bar comes from H.264; everything else from the other codecs.
                // Pad by a macroblock so lossy edge ringing gets refined (Windows sends
                // refinement regions for this; a tight rect leaves faint trails).
                let pad = |v: u16| v.saturating_sub(16);
                Avc420Region::new(
                    pad(bar.0),
                    pad(bar.1),
                    (bar.0 + bar.2 + 16).min(w as u16),
                    (bar.1 + bar.3 + 16).min(h as u16),
                    22,
                    100,
                )
            } else {
                Avc420Region::new(0, 0, w as u16, h as u16, 22, 100)
            };
            let ts = started.elapsed().as_millis() as u32;
            let sent = match &encoded {
                Encoded::Avc420(bits) => server.send_avc420_frame(*surface, bits, std::slice::from_ref(&region), ts),
                Encoded::Avc444(main, aux) => match enc444.as_ref().map(Avc444Encoder::layout) {
                    Some(Layout::V2) => server.send_avc444v2_frame(
                        *surface,
                        Encoding::LUMA_AND_CHROMA,
                        main,
                        std::slice::from_ref(&region),
                        Some(aux),
                        Some(std::slice::from_ref(&region)),
                        ts,
                    ),
                    _ => server.send_avc444_frame(
                        *surface,
                        main,
                        std::slice::from_ref(&region),
                        Some(aux),
                        Some(std::slice::from_ref(&region)),
                        ts,
                    ),
                },
            };
            if sent.is_some() {
                sent_full = true;
            } else {
                // Client is behind on acks. The dropped P-frame breaks the reference
                // chain, so restart from a key frame (still send what was queued).
                encoder.request_key_frame();
                if let Some(e) = enc444.as_mut() {
                    e.request_key_frame();
                }
            }
            if mix && sent_full && enc444.is_none() {
                let rect = |(x, y, w, h): (u16, u16, u16, u16)| ironrdp_pdu::geometry::ExclusiveRectangle {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                };
                if marker.2 > 0 && marker.3 > 0 {
                    let data = clear.encode(&animator.bgra_region(marker), marker.2, marker.3);
                    server.send_clearcodec_frame(*surface, rect(marker), data, ts);
                }
                if let Some(sw) = clear_swatch {
                    let data = clear.encode(&animator.bgra_region(sw), sw.2, sw.3);
                    server.send_clearcodec_frame(*surface, rect(sw), data, ts);
                }
                if let Some(sw) = origin_swatch {
                    let bgra = animator.bgra_region(sw);
                    if period % 2 == 0 {
                        let mut planar = vec![0u8; bgra.len() + 64];
                        let mut enc =
                            ironrdp_graphics::rdp6::BitmapStreamEncoder::new(usize::from(sw.2), usize::from(sw.3));
                        if let Ok(n) =
                            enc.encode_bitmap::<ironrdp_graphics::rdp6::BgrAChannels>(&bgra, &mut planar, true)
                        {
                            server.send_planar_frame(*surface, &planar[..n], sw.2, sw.3, ts);
                        }
                    } else {
                        server.send_uncompressed_frame(*surface, &bgra, sw.2, sw.3, ts);
                    }
                }
            }
            (server.channel_id(), server.drain_output())
        };
        let Some(channel_id) = channel_id else { continue };
        if messages.is_empty() {
            continue;
        }
        let Ok(messages) = ironrdp_dvc::encode_dvc_messages(channel_id, messages, ironrdp_svc::ChannelFlags::empty())
        else {
            continue;
        };
        let sender = slots.sender.lock().unwrap().clone();
        if let Some(sender) = sender {
            let _ = sender.send(ServerEvent::Egfx(EgfxServerMessage::SendMessages { messages }));
        }
    }
}
