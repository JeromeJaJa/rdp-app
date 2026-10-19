//! Graphics Pipeline (MS-RDPEGFX) client built for many concurrent sessions.
//!
//! Compared with a straightforward compositor, the hot paths avoid per-frame
//! allocations and intermediate copies:
//!
//! - codecs write directly into persistent surface buffers;
//! - H.264 is converted from YUV only inside the server's changed regions;
//! - the primary surface (mapped unscaled at the origin, the normal desktop
//!   case) *is* the output framebuffer, so frames need no composition copy;
//! - the output framebuffer is shared with the session, which only copies the
//!   damaged rectangles onward to whatever is on screen.
//!
//! PDU parsing and the ZGFX / ClearCodec / Planar / RemoteFX Progressive codecs
//! come from IronRDP (MIT/Apache-2.0); surface management and composition
//! follow MS-RDPEGFX and are modelled on IronRDP's client.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ironrdp_core::{Decode as _, ReadCursor, impl_as_any};
use ironrdp_dvc::{DvcClientProcessor, DvcMessage, DvcProcessor};
use ironrdp_egfx::pdu::{
    Avc420BitmapStream, Avc444BitmapStream, CacheToSurfacePdu, CapabilitiesAdvertisePdu, CapabilitySet, Codec1Type,
    Encoding, FrameAcknowledgePdu, GfxPdu, Point, QueueDepth, SolidFillPdu, SurfaceToCachePdu, SurfaceToSurfacePdu,
    WireToSurface1Pdu, WireToSurface2Pdu,
};
use ironrdp_graphics::clearcodec::ClearCodecDecoder;
use ironrdp_graphics::progressive::{ProgressiveDecoder, TILE_DIM};
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_graphics::zgfx;
use ironrdp_pdu::geometry::{ExclusiveRectangle, InclusiveRectangle};
use ironrdp_pdu::{PduResult, decode_cursor, decode_err};
use parking_lot::Mutex;
use tracing::{debug, trace, warn};

use super::{GfxStats, avc444, h264};
use crate::DecoderPreference;

const CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Graphics";
const BPP: usize = 4;
const MAX_DIM: u32 = 8192;
/// Upper bound for the bitmap cache (the server is told SMALL_CACHE = 16 MiB).
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Above this many damage rectangles per frame, report their bounding box instead.
const MAX_DAMAGE_RECTS: usize = 192;

/// The composited desktop, shared with the session thread.
pub(crate) struct GfxOutput {
    /// Set once the server has sent ResetGraphics (the pipeline drives the display).
    pub active: bool,
    pub width: u16,
    pub height: u16,
    /// RGBA, `width * 4` bytes per row.
    pub data: Vec<u8>,
    damage: Vec<InclusiveRectangle>,
}

impl GfxOutput {
    pub(crate) fn new() -> Self {
        Self {
            active: false,
            width: 0,
            height: 0,
            data: Vec::new(),
            damage: Vec::new(),
        }
    }

    /// Damage accumulated since the last call (output coordinates).
    pub(crate) fn take_damage(&mut self) -> Vec<InclusiveRectangle> {
        std::mem::take(&mut self.damage)
    }

    fn reset(&mut self, width: u16, height: u16) {
        let len = usize::from(width) * usize::from(height) * BPP;
        if self.data.len() != len {
            self.data = vec![0; len];
        } else {
            self.data.fill(0);
        }
        // Opaque black.
        for px in self.data.chunks_exact_mut(BPP) {
            px[3] = 0xFF;
        }
        self.width = width;
        self.height = height;
        self.active = true;
        self.damage.clear();
        self.add_damage(Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        });
    }

    fn add_damage(&mut self, r: Rect) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        self.damage.push(InclusiveRectangle {
            left: r.x,
            top: r.y,
            right: r.x + r.w - 1,
            bottom: r.y + r.h - 1,
        });
    }
}

pub(crate) type SharedOutput = Arc<Mutex<GfxOutput>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

impl Rect {
    fn from_exclusive(r: &ExclusiveRectangle) -> Self {
        Self {
            x: r.left,
            y: r.top,
            w: r.right.saturating_sub(r.left),
            h: r.bottom.saturating_sub(r.top),
        }
    }

    /// Clips to `w × h` bounds.
    fn clip(self, bw: u16, bh: u16) -> Option<Self> {
        let x1 = (u32::from(self.x) + u32::from(self.w)).min(u32::from(bw));
        let y1 = (u32::from(self.y) + u32::from(self.h)).min(u32::from(bh));
        (u32::from(self.x) < x1 && u32::from(self.y) < y1).then(|| Self {
            x: self.x,
            y: self.y,
            w: (x1 - u32::from(self.x)) as u16,
            h: (y1 - u32::from(self.y)) as u16,
        })
    }

    fn union(self, o: Self) -> Self {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

#[derive(Clone, Copy)]
struct Mapping {
    x: u32,
    y: u32,
    target_w: u32,
    target_h: u32,
}

enum Store {
    Own(Vec<u8>),
    /// The surface lives in the output framebuffer (primary surface).
    Output,
}

struct Surface {
    w: u16,
    h: u16,
    store: Store,
    mapping: Option<Mapping>,
}

struct CacheTile {
    w: u16,
    h: u16,
    data: Vec<u8>,
}

pub(crate) struct GfxClient {
    caps: Vec<CapabilitySet>,
    stats: Arc<GfxStats>,
    output: SharedOutput,

    zgfx: zgfx::Decompressor,
    zbuf: Vec<u8>,

    surfaces: BTreeMap<u16, Surface>,
    aliased: Option<u16>,
    cache: BTreeMap<u16, CacheTile>,
    cache_bytes: usize,

    /// AVC420 pictures and the AVC444 main view.
    h264: h264::Slot,
    /// The AVC444 auxiliary (chroma) view: a second, independent H.264 stream.
    h264_aux: h264::Slot,
    /// Last AVC444 main view per surface, for chroma-only updates.
    main_views: BTreeMap<u16, avc444::MainView>,
    avc444_scratch: avc444::Scratch,
    clear: Option<ClearCodecDecoder>,
    planar: BitmapStreamDecoder,
    progressive: ProgressiveDecoder,

    /// Surface-space rectangles touched in the current frame.
    dirty: Vec<(u16, Rect)>,
    total_frames: u32,
    scratch: Vec<u8>,
    warned_codecs: u32,
}

impl GfxClient {
    pub(crate) fn new(
        caps: Vec<CapabilitySet>,
        h264: Option<Box<dyn h264::Decoder>>,
        decoder: DecoderPreference,
        stats: Arc<GfxStats>,
        output: SharedOutput,
    ) -> Self {
        Self {
            caps,
            stats,
            output,
            zgfx: zgfx::Decompressor::new(),
            zbuf: Vec::new(),
            surfaces: BTreeMap::new(),
            aliased: None,
            cache: BTreeMap::new(),
            cache_bytes: 0,
            h264: h264::Slot::new(h264, decoder),
            h264_aux: h264::Slot::lazy(decoder),
            main_views: BTreeMap::new(),
            avc444_scratch: avc444::Scratch::default(),
            clear: None,
            planar: BitmapStreamDecoder::default(),
            progressive: ProgressiveDecoder::new(),
            dirty: Vec::new(),
            total_frames: 0,
            scratch: Vec::new(),
            warned_codecs: 0,
        }
    }

    fn warn_codec(&mut self, what: &str) {
        if self.warned_codecs < 8 {
            self.warned_codecs += 1;
            warn!("{what}");
        }
    }

    fn handle(&mut self, pdu: GfxPdu, out: &mut GfxOutput, replies: &mut Vec<DvcMessage>) {
        match pdu {
            GfxPdu::CapabilitiesConfirm(confirm) => match confirm.0.parsed() {
                Ok(Some(caps)) => {
                    let (avc420, avc444) = super::avc_support(&caps);
                    debug!(?caps, avc420, avc444, "graphics pipeline capabilities confirmed");
                    self.stats.avc420_enabled.store(avc420, Ordering::Relaxed);
                    self.stats.avc444_enabled.store(avc444, Ordering::Relaxed);
                    self.stats.confirmed.store(true, Ordering::Relaxed);
                }
                _ => {
                    warn!("server confirmed an unknown graphics capability set");
                    self.stats.confirmed.store(true, Ordering::Relaxed);
                }
            },
            GfxPdu::ResetGraphics(reset) => {
                let (w, h) = (
                    reset.width.clamp(1, MAX_DIM) as u16,
                    reset.height.clamp(1, MAX_DIM) as u16,
                );
                debug!(
                    w,
                    h,
                    surfaces = self.surfaces.len(),
                    cached = self.cache.len(),
                    "graphics reset"
                );
                // The reset only resizes the output buffer (MS-RDPEGFX 2.2.2.14): surfaces,
                // the bitmap cache and codec state live on until the server deletes them —
                // Windows keeps placing cached tiles after a resize.
                if let Some(id) = self.aliased.take() {
                    if let Some(s) = self.surfaces.get_mut(&id) {
                        // The primary surface's pixels are the output being replaced.
                        s.store = Store::Own(std::mem::take(&mut out.data));
                    }
                }
                out.reset(w, h);
                self.dirty.clear();
                let mapped: Vec<(u16, Rect)> = self
                    .surfaces
                    .iter()
                    .filter(|(_, s)| s.mapping.is_some())
                    .map(|(id, s)| {
                        (
                            *id,
                            Rect {
                                x: 0,
                                y: 0,
                                w: s.w,
                                h: s.h,
                            },
                        )
                    })
                    .collect();
                self.dirty.extend(mapped);
            }
            GfxPdu::CreateSurface(c) => {
                if c.width == 0 || c.height == 0 || u32::from(c.width) > MAX_DIM || u32::from(c.height) > MAX_DIM {
                    warn!(c.surface_id, c.width, c.height, "ignoring surface with invalid size");
                    return;
                }
                debug!(id = c.surface_id, w = c.width, h = c.height, "create surface");
                if self.aliased == Some(c.surface_id) {
                    self.aliased = None;
                }
                // A re-created id must not inherit the old surface's codec state.
                self.progressive.delete_surface(c.surface_id);
                self.main_views.remove(&c.surface_id);
                let len = usize::from(c.width) * usize::from(c.height) * BPP;
                self.surfaces.insert(
                    c.surface_id,
                    Surface {
                        w: c.width,
                        h: c.height,
                        store: Store::Own(vec![0; len]),
                        mapping: None,
                    },
                );
            }
            GfxPdu::DeleteSurface(d) => {
                debug!(id = d.surface_id, "delete surface");
                self.progressive.delete_surface(d.surface_id);
                self.surfaces.remove(&d.surface_id);
                self.main_views.remove(&d.surface_id);
                if self.aliased == Some(d.surface_id) {
                    self.aliased = None; // the output keeps its last pixels
                }
                self.dirty.retain(|(id, _)| *id != d.surface_id);
            }
            GfxPdu::MapSurfaceToOutput(m) => {
                self.map(m.surface_id, m.output_origin_x, m.output_origin_y, None, out);
            }
            GfxPdu::MapSurfaceToScaledOutput(m) => {
                self.map(
                    m.surface_id,
                    m.output_origin_x,
                    m.output_origin_y,
                    Some((m.target_width, m.target_height)),
                    out,
                );
            }
            GfxPdu::StartFrame(_) => self.progressive.begin_frame(),
            GfxPdu::EndFrame(end) => {
                self.progressive.end_frame();
                self.compose(out);
                self.total_frames = self.total_frames.wrapping_add(1);
                replies.push(Box::new(GfxPdu::FrameAcknowledge(FrameAcknowledgePdu {
                    queue_depth: QueueDepth::Unavailable,
                    frame_id: end.frame_id,
                    total_frames_decoded: self.total_frames,
                })));
            }
            GfxPdu::WireToSurface1(w) => self.wire_to_surface1(w, out),
            GfxPdu::WireToSurface2(w) => self.wire_to_surface2(w, out),
            GfxPdu::SolidFill(f) => self.solid_fill(&f, out),
            GfxPdu::SurfaceToSurface(s) => self.surface_to_surface(&s, out),
            GfxPdu::SurfaceToCache(s) => self.surface_to_cache(&s, out),
            GfxPdu::CacheToSurface(c) => self.cache_to_surface(&c, out),
            GfxPdu::EvictCacheEntry(e) => {
                if let Some(tile) = self.cache.remove(&e.cache_slot) {
                    self.cache_bytes -= tile.data.len();
                }
            }
            GfxPdu::DeleteEncodingContext(d) => self.progressive.delete_context(d.surface_id, d.codec_context_id),
            other => trace!(pdu = ?std::mem::discriminant(&other), "ignored graphics PDU"),
        }
    }

    fn map(&mut self, id: u16, x: u32, y: u32, scaled: Option<(u32, u32)>, out: &mut GfxOutput) {
        let Some(surface) = self.surfaces.get_mut(&id) else {
            warn!(id, "map of unknown surface");
            return;
        };
        let (target_w, target_h) = scaled.unwrap_or((u32::from(surface.w), u32::from(surface.h)));
        surface.mapping = Some(Mapping {
            x,
            y,
            target_w,
            target_h,
        });

        let primary = x == 0
            && y == 0
            && target_w == u32::from(surface.w)
            && target_h == u32::from(surface.h)
            && (surface.w, surface.h) == (out.width, out.height);

        if self.aliased == Some(id) && !primary {
            // Leaving the primary role: take the pixels back out of the output.
            surface.store = Store::Own(out.data.clone());
            self.aliased = None;
        } else if primary && self.aliased.is_none() {
            if let Store::Own(data) = &surface.store {
                out.data.copy_from_slice(data);
            }
            surface.store = Store::Output;
            self.aliased = Some(id);
            debug!(id, "primary surface aliased to output");
        }
        let full = Rect {
            x: 0,
            y: 0,
            w: surface.w,
            h: surface.h,
        };
        self.dirty.push((id, full));
    }

    /// Copies this frame's dirty surface regions to the output and records damage.
    fn compose(&mut self, out: &mut GfxOutput) {
        if self.dirty.is_empty() {
            return;
        }
        let mut dirty = std::mem::take(&mut self.dirty);
        if dirty.len() > MAX_DAMAGE_RECTS {
            // Collapse per surface to a bounding box.
            let mut by_surface: BTreeMap<u16, Rect> = BTreeMap::new();
            for (id, r) in dirty.drain(..) {
                by_surface.entry(id).and_modify(|u| *u = u.union(r)).or_insert(r);
            }
            dirty.extend(by_surface);
        }

        for (id, r) in dirty.iter().copied() {
            let Some(surface) = self.surfaces.get(&id) else {
                continue;
            };
            let Some(m) = surface.mapping else { continue };
            let Some(r) = r.clip(surface.w, surface.h) else {
                continue;
            };

            if self.aliased == Some(id) {
                out.add_damage(r);
                continue;
            }
            let Store::Own(src) = &surface.store else { continue };
            let (ow, oh) = (u32::from(out.width), u32::from(out.height));
            let ostride = usize::from(out.width) * BPP;
            let sstride = usize::from(surface.w) * BPP;

            if m.target_w == u32::from(surface.w) && m.target_h == u32::from(surface.h) {
                // Unscaled: straight row copies, clipped to the output.
                let dx0 = m.x + u32::from(r.x);
                let dy0 = m.y + u32::from(r.y);
                if dx0 >= ow || dy0 >= oh {
                    continue;
                }
                let w = (u32::from(r.w)).min(ow - dx0) as usize;
                let h = (u32::from(r.h)).min(oh - dy0) as usize;
                for row in 0..h {
                    let s = (usize::from(r.y) + row) * sstride + usize::from(r.x) * BPP;
                    let d = (dy0 as usize + row) * ostride + dx0 as usize * BPP;
                    out.data[d..d + w * BPP].copy_from_slice(&src[s..s + w * BPP]);
                }
                out.add_damage(Rect {
                    x: dx0 as u16,
                    y: dy0 as u16,
                    w: w as u16,
                    h: h as u16,
                });
            } else {
                // Scaled mapping: nearest-neighbour into the target rectangle.
                let sx = |v: u32| v * m.target_w / u32::from(surface.w);
                let sy = |v: u32| v * m.target_h / u32::from(surface.h);
                let tx0 = (m.x + sx(u32::from(r.x))).min(ow);
                let ty0 = (m.y + sy(u32::from(r.y))).min(oh);
                let tx1 = (m.x + sx(u32::from(r.x + r.w)).max(sx(u32::from(r.x)) + 1)).min(ow);
                let ty1 = (m.y + sy(u32::from(r.y + r.h)).max(sy(u32::from(r.y)) + 1)).min(oh);
                for ty in ty0..ty1 {
                    let srcy =
                        (((ty - m.y) * u32::from(surface.h)) / m.target_h).min(u32::from(surface.h) - 1) as usize;
                    for tx in tx0..tx1 {
                        let srcx =
                            (((tx - m.x) * u32::from(surface.w)) / m.target_w).min(u32::from(surface.w) - 1) as usize;
                        let s = srcy * sstride + srcx * BPP;
                        let d = ty as usize * ostride + tx as usize * BPP;
                        out.data[d..d + BPP].copy_from_slice(&src[s..s + BPP]);
                    }
                }
                if tx1 > tx0 && ty1 > ty0 {
                    out.add_damage(Rect {
                        x: tx0 as u16,
                        y: ty0 as u16,
                        w: (tx1 - tx0) as u16,
                        h: (ty1 - ty0) as u16,
                    });
                }
            }
        }
        dirty.clear();
        self.dirty = dirty; // keep the allocation
    }

    fn wire_to_surface1(&mut self, pdu: WireToSurface1Pdu, out: &mut GfxOutput) {
        let Some(surface) = self.surfaces.get_mut(&pdu.surface_id) else {
            warn!(pdu.surface_id, "bitmap for unknown surface");
            return;
        };
        let (sw, sh) = (surface.w, surface.h);
        let stride = usize::from(sw) * BPP;
        let buf: &mut [u8] = match &mut surface.store {
            Store::Own(v) => v,
            Store::Output => &mut out.data,
        };
        let dest = Rect::from_exclusive(&pdu.destination_rectangle);
        let id = pdu.surface_id;

        match pdu.codec_id {
            Codec1Type::Avc420 => {
                self.stats.avc.fetch_add(1, Ordering::Relaxed);
                let mut cursor = ReadCursor::new(&pdu.bitmap_data);
                let stream = match Avc420BitmapStream::decode(&mut cursor) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "malformed AVC420 stream");
                        return;
                    }
                };
                if !self.h264.available() {
                    self.warn_codec("H.264 frame received without a decoder");
                    return;
                }
                let started = std::time::Instant::now();
                self.stats.hardware.store(self.h264.hardware(), Ordering::Relaxed);
                let pic = match self.h264.decode(stream.data) {
                    Ok(Some(p)) => p,
                    Ok(None) => return,
                    Err(e) => {
                        decode_failed(&self.stats, &e);
                        return;
                    }
                };
                let (pw, ph) = (pic.width.min(usize::from(sw)), pic.height.min(usize::from(sh)));

                // Region rects are surface-absolute and sample the picture at the same
                // coordinates. Without any, the picture is placed at the destination.
                if stream.rectangles.is_empty() {
                    if let Some(r) = dest.clip(sw, sh) {
                        let w = usize::from(r.w).min(pic.width);
                        let h = usize::from(r.h).min(pic.height);
                        let mut tmp = std::mem::take(&mut self.scratch);
                        let row = w * BPP;
                        let mut rgba = vec![0u8; row * h];
                        if h264::convert_region(&pic, &mut rgba, row, (0, 0, w, h), &mut tmp).is_ok() {
                            for y in 0..h {
                                let d = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
                                buf[d..d + row].copy_from_slice(&rgba[y * row..(y + 1) * row]);
                            }
                            self.dirty.push((id, r));
                        }
                        self.scratch = tmp;
                    }
                } else {
                    for region in &stream.rectangles {
                        let Some(r) = Rect::from_exclusive(region).clip(pw as u16, ph as u16) else {
                            continue;
                        };
                        let rect = (usize::from(r.x), usize::from(r.y), usize::from(r.w), usize::from(r.h));
                        match h264::convert_region(&pic, buf, stride, rect, &mut self.scratch) {
                            Ok(()) => self.dirty.push((id, r)),
                            Err(e) => warn!(error = %e, "AVC420 region conversion failed"),
                        }
                    }
                }
                self.stats
                    .decode_us
                    .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                self.stats.decoded.fetch_add(1, Ordering::Relaxed);
            }
            Codec1Type::Avc444 | Codec1Type::Avc444v2 => {
                self.stats.avc444.fetch_add(1, Ordering::Relaxed);
                let layout = if pdu.codec_id == Codec1Type::Avc444 {
                    avc444::Layout::V1
                } else {
                    avc444::Layout::V2
                };
                let mut cursor = ReadCursor::new(&pdu.bitmap_data);
                let stream = match Avc444BitmapStream::decode(&mut cursor) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "malformed AVC444 stream");
                        return;
                    }
                };
                if !self.h264.available() {
                    self.warn_codec("H.264 frame received without a decoder");
                    return;
                }
                let started = std::time::Instant::now();
                // LC 0: stream1 = main (luma) view, stream2 = auxiliary (chroma) view;
                // LC 1: main view only; LC 2: stream1 is the auxiliary view.
                let (luma, chroma) = if stream.encoding == Encoding::CHROMA {
                    (None, Some(&stream.stream1))
                } else {
                    (Some(&stream.stream1), stream.stream2.as_ref())
                };
                let (w, h) = (usize::from(sw), usize::from(sh));
                let main = self.main_views.entry(id).or_insert_with(|| avc444::MainView::new(w, h));
                if !main.fits(w, h) {
                    *main = avc444::MainView::new(w, h);
                }

                if let Some(luma) = luma {
                    self.stats.hardware.store(self.h264.hardware(), Ordering::Relaxed);
                    let pic = match self.h264.decode(luma.data) {
                        Ok(Some(p)) => p,
                        Ok(None) => return,
                        Err(e) => {
                            decode_failed(&self.stats, &e);
                            return;
                        }
                    };
                    // Rects the chroma view of this same frame repaints are converted once, as 4:4:4.
                    let combined = chroma.is_some_and(|c| c.rectangles == luma.rectangles);
                    for r in avc_rects(&luma.rectangles, dest, (pic.width, pic.height), (sw, sh)) {
                        let rect = (usize::from(r.x), usize::from(r.y), usize::from(r.w), usize::from(r.h));
                        main.update(&pic, rect);
                        if combined {
                            continue;
                        }
                        match h264::convert_region(&pic, buf, stride, rect, &mut self.scratch) {
                            Ok(()) => self.dirty.push((id, r)),
                            Err(e) => warn!(error = %e, "AVC444 luma region conversion failed"),
                        }
                    }
                }
                if let Some(chroma) = chroma {
                    let aux = match self.h264_aux.decode(chroma.data) {
                        Ok(Some(p)) => p,
                        Ok(None) => return,
                        Err(e) => {
                            decode_failed(&self.stats, &e);
                            return;
                        }
                    };
                    for r in avc_rects(&chroma.rectangles, dest, (aux.width, aux.height), (sw, sh)) {
                        let rect = (usize::from(r.x), usize::from(r.y), usize::from(r.w), usize::from(r.h));
                        match avc444::convert_region(main, &aux, layout, buf, stride, rect, &mut self.avc444_scratch) {
                            Ok(()) => self.dirty.push((id, r)),
                            Err(e) => warn!(error = %e, "AVC444 region conversion failed"),
                        }
                    }
                }
                self.stats
                    .decode_us
                    .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                self.stats.decoded.fetch_add(1, Ordering::Relaxed);
            }
            Codec1Type::Uncompressed => {
                let Some(r) = dest.clip(sw, sh) else { return };
                let src_stride = usize::from(dest.w) * 4;
                for y in 0..usize::from(r.h) {
                    let s = y * src_stride;
                    let Some(src) = pdu.bitmap_data.get(s..s + usize::from(r.w) * 4) else {
                        break;
                    };
                    let d = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
                    bgrx_to_rgba(src, &mut buf[d..d + usize::from(r.w) * BPP]);
                }
                self.stats.other.fetch_add(1, Ordering::Relaxed);
                self.dirty.push((id, r));
            }
            Codec1Type::Planar => {
                let mut rgb = std::mem::take(&mut self.scratch);
                rgb.clear();
                let ok = self
                    .planar
                    .decode_bitmap_stream_to_rgb24(&pdu.bitmap_data, &mut rgb, usize::from(dest.w), usize::from(dest.h))
                    .inspect_err(|e| warn!(error = %e, "planar decode failed"))
                    .is_ok();
                if ok {
                    if let Some(r) = dest.clip(sw, sh) {
                        let src_stride = usize::from(dest.w) * 3;
                        for y in 0..usize::from(r.h) {
                            let s = y * src_stride;
                            let Some(src) = rgb.get(s..s + usize::from(r.w) * 3) else {
                                break;
                            };
                            let d = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
                            for (dp, sp) in buf[d..d + usize::from(r.w) * BPP]
                                .chunks_exact_mut(4)
                                .zip(src.chunks_exact(3))
                            {
                                dp.copy_from_slice(&[sp[0], sp[1], sp[2], 0xFF]);
                            }
                        }
                        self.dirty.push((id, r));
                    }
                }
                self.scratch = rgb;
                self.stats.planar.fetch_add(1, Ordering::Relaxed);
            }
            Codec1Type::ClearCodec => {
                let decoded =
                    self.clear
                        .get_or_insert_with(ClearCodecDecoder::new)
                        .decode(&pdu.bitmap_data, dest.w, dest.h);
                match decoded {
                    Ok(bgra) => {
                        if let Some(r) = dest.clip(sw, sh) {
                            let src_stride = usize::from(dest.w) * 4;
                            for y in 0..usize::from(r.h) {
                                let s = y * src_stride;
                                let Some(src) = bgra.get(s..s + usize::from(r.w) * 4) else {
                                    break;
                                };
                                let d = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
                                bgrx_to_rgba(src, &mut buf[d..d + usize::from(r.w) * BPP]);
                            }
                            self.dirty.push((id, r));
                        }
                    }
                    Err(e) => warn!(error = %e, "ClearCodec decode failed"),
                }
                self.stats.clear.fetch_add(1, Ordering::Relaxed);
            }
            Codec1Type::Alpha => {
                // Per-pixel opacity for RAIL/composited surfaces; a desktop session is opaque.
                self.stats.other.fetch_add(1, Ordering::Relaxed);
            }
            other => {
                self.stats.unhandled.fetch_add(1, Ordering::Relaxed);
                self.warn_codec(&format!("unsupported graphics codec {other:?}"));
            }
        }
    }

    fn wire_to_surface2(&mut self, pdu: WireToSurface2Pdu, out: &mut GfxOutput) {
        let Some(surface) = self.surfaces.get_mut(&pdu.surface_id) else {
            return;
        };
        let (sw, sh) = (surface.w, surface.h);
        let stride = usize::from(sw) * BPP;
        let buf: &mut [u8] = match &mut surface.store {
            Store::Own(v) => v,
            Store::Output => &mut out.data,
        };
        capture_progressive(pdu.surface_id, pdu.codec_context_id, sw, sh, &pdu.bitmap_data);
        let tiles = match self
            .progressive
            .decode_bitmap(pdu.surface_id, pdu.codec_context_id, sw, sh, &pdu.bitmap_data)
        {
            Ok(t) => t,
            Err(e) => {
                warn!(error = ?e, "RemoteFX progressive decode failed");
                self.stats.needs_refresh.store(true, Ordering::Relaxed);
                return;
            }
        };
        self.stats.progressive.fetch_add(1, Ordering::Relaxed);
        if tracing::enabled!(tracing::Level::DEBUG) {
            count_progressive_tiles(&self.stats, &pdu.bitmap_data);
        }
        let tile_stride = usize::from(TILE_DIM) * BPP;
        for tile in tiles {
            let (tx, ty) = (tile.x_idx * TILE_DIM, tile.y_idx * TILE_DIM);
            for rect in &tile.update_rectangles {
                let Some(r) = Rect::from_exclusive(rect).clip(sw, sh) else {
                    continue;
                };
                if r.x < tx || r.y < ty {
                    continue;
                }
                let (ox, oy) = (usize::from(r.x - tx), usize::from(r.y - ty));
                let w = usize::from(r.w).min(usize::from(TILE_DIM) - ox);
                let h = usize::from(r.h).min(usize::from(TILE_DIM) - oy);
                for row in 0..h {
                    let s = (oy + row) * tile_stride + ox * BPP;
                    let d = (usize::from(r.y) + row) * stride + usize::from(r.x) * BPP;
                    buf[d..d + w * BPP].copy_from_slice(&tile.pixels[s..s + w * BPP]);
                }
                self.dirty.push((pdu.surface_id, r));
            }
        }
    }

    fn solid_fill(&mut self, pdu: &SolidFillPdu, out: &mut GfxOutput) {
        let Some(surface) = self.surfaces.get_mut(&pdu.surface_id) else {
            return;
        };
        let (sw, sh) = (surface.w, surface.h);
        let stride = usize::from(sw) * BPP;
        let buf: &mut [u8] = match &mut surface.store {
            Store::Own(v) => v,
            Store::Output => &mut out.data,
        };
        let px = [pdu.fill_pixel.r, pdu.fill_pixel.g, pdu.fill_pixel.b, 0xFF];
        for rect in &pdu.rectangles {
            let Some(r) = Rect::from_exclusive(rect).clip(sw, sh) else {
                continue;
            };
            for y in 0..usize::from(r.h) {
                let d = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
                for dp in buf[d..d + usize::from(r.w) * BPP].chunks_exact_mut(4) {
                    dp.copy_from_slice(&px);
                }
            }
            self.dirty.push((pdu.surface_id, r));
        }
    }

    /// Reads a rectangle of a surface into `dst` (tightly packed).
    fn read_rect(&self, id: u16, r: Rect, out: &GfxOutput, dst: &mut Vec<u8>) -> Option<Rect> {
        let surface = self.surfaces.get(&id)?;
        let r = r.clip(surface.w, surface.h)?;
        let src: &[u8] = match &surface.store {
            Store::Own(v) => v,
            Store::Output => &out.data,
        };
        let stride = usize::from(surface.w) * BPP;
        let row = usize::from(r.w) * BPP;
        dst.clear();
        dst.reserve(row * usize::from(r.h));
        for y in 0..usize::from(r.h) {
            let s = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
            dst.extend_from_slice(&src[s..s + row]);
        }
        Some(r)
    }

    /// Writes a tightly packed `w × h` block to `points` of a surface.
    fn write_block(&mut self, id: u16, w: u16, h: u16, block: &[u8], points: &[Point], out: &mut GfxOutput) {
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return;
        };
        let (sw, sh) = (surface.w, surface.h);
        let stride = usize::from(sw) * BPP;
        let buf: &mut [u8] = match &mut surface.store {
            Store::Own(v) => v,
            Store::Output => &mut out.data,
        };
        let row = usize::from(w) * BPP;
        for p in points {
            let Some(r) = (Rect { x: p.x, y: p.y, w, h }).clip(sw, sh) else {
                continue;
            };
            let cw = usize::from(r.w) * BPP;
            for y in 0..usize::from(r.h) {
                let s = y * row;
                let d = (usize::from(r.y) + y) * stride + usize::from(r.x) * BPP;
                buf[d..d + cw].copy_from_slice(&block[s..s + cw]);
            }
            self.dirty.push((id, r));
        }
    }

    fn surface_to_surface(&mut self, pdu: &SurfaceToSurfacePdu, out: &mut GfxOutput) {
        let mut block = std::mem::take(&mut self.scratch);
        if let Some(r) = self.read_rect(
            pdu.source_surface_id,
            Rect::from_exclusive(&pdu.source_rectangle),
            out,
            &mut block,
        ) {
            self.write_block(
                pdu.destination_surface_id,
                r.w,
                r.h,
                &block,
                &pdu.destination_points,
                out,
            );
        }
        self.scratch = block;
    }

    fn surface_to_cache(&mut self, pdu: &SurfaceToCachePdu, out: &GfxOutput) {
        let mut data = Vec::new();
        let Some(r) = self.read_rect(
            pdu.surface_id,
            Rect::from_exclusive(&pdu.source_rectangle),
            out,
            &mut data,
        ) else {
            return;
        };
        if let Some(old) = self.cache.remove(&pdu.cache_slot) {
            self.cache_bytes -= old.data.len();
        }
        if self.cache_bytes + data.len() > MAX_CACHE_BYTES {
            warn!("bitmap cache over budget; dropping entry");
            return;
        }
        self.cache_bytes += data.len();
        self.cache.insert(pdu.cache_slot, CacheTile { w: r.w, h: r.h, data });
    }

    fn cache_to_surface(&mut self, pdu: &CacheToSurfacePdu, out: &mut GfxOutput) {
        let Some(tile) = self.cache.remove(&pdu.cache_slot) else {
            warn!(slot = pdu.cache_slot, "cache miss");
            return;
        };
        self.write_block(pdu.surface_id, tile.w, tile.h, &tile.data, &pdu.destination_points, out);
        self.cache.insert(pdu.cache_slot, tile);
    }
}

fn bgrx_to_rgba(src: &[u8], dst: &mut [u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = 0xFF;
    }
}

impl_as_any!(GfxClient);

impl DvcProcessor for GfxClient {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let pdu = GfxPdu::CapabilitiesAdvertise(CapabilitiesAdvertisePdu::from_typed(&self.caps));
        Ok(vec![Box::new(pdu)])
    }

    fn close(&mut self, _channel_id: u32) {
        debug!("graphics pipeline closed");
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        self.zbuf.clear();
        self.zgfx
            .decompress(payload, &mut self.zbuf)
            .map_err(|e| decode_err!(e))?;

        let zbuf = std::mem::take(&mut self.zbuf);
        let mut pdus = Vec::new();
        let mut cursor = ReadCursor::new(&zbuf);
        let parsed = (|| {
            while !cursor.is_empty() {
                pdus.push(decode_cursor::<GfxPdu>(&mut cursor)?);
            }
            Ok::<_, ironrdp_core::DecodeError>(())
        })();
        self.zbuf = zbuf;
        parsed.map_err(|e| decode_err!(e))?;

        let output = Arc::clone(&self.output);
        let mut out = output.lock();
        let mut replies = Vec::new();
        for pdu in pdus {
            self.handle(pdu, &mut out, &mut replies);
        }
        Ok(replies)
    }
}

impl DvcClientProcessor for GfxClient {}

/// Debugging aid: with `RDP_CAPTURE_PROGRESSIVE=<file>` set, appends every RemoteFX
/// Progressive message as `surface u16, context u32, width u16, height u16, len u32, data`
/// (little endian) so decoding can be replayed offline.
fn capture_progressive(surface: u16, context: u32, w: u16, h: u16, data: &[u8]) {
    use std::io::Write as _;
    static FILE: std::sync::OnceLock<Option<parking_lot::Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let file = FILE.get_or_init(|| {
        let path = std::env::var_os("RDP_CAPTURE_PROGRESSIVE")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(parking_lot::Mutex::new)
    });
    if let Some(file) = file {
        let mut f = file.lock();
        let _ = f.write_all(&surface.to_le_bytes());
        let _ = f.write_all(&context.to_le_bytes());
        let _ = f.write_all(&w.to_le_bytes());
        let _ = f.write_all(&h.to_le_bytes());
        let _ = f.write_all(&(data.len() as u32).to_le_bytes());
        let _ = f.write_all(data);
    }
}

/// Tallies progressive tile kinds (diagnostics: are quality upgrades arriving?).
fn count_progressive_tiles(stats: &GfxStats, data: &[u8]) {
    use ironrdp_pdu::codecs::rfx::progressive::{ProgressiveBlock, ProgressiveTile, decode_progressive_stream};
    let Ok(blocks) = decode_progressive_stream(data) else {
        return;
    };
    for block in blocks {
        let ProgressiveBlock::Region(region) = block else {
            continue;
        };
        let mut qualities: Vec<u8> = region
            .tiles
            .iter()
            .map(|t| match t {
                ProgressiveTile::Simple(_) => 0xFE,
                ProgressiveTile::First(t) => t.quality,
                ProgressiveTile::Upgrade(t) => t.quality,
            })
            .collect();
        qualities.sort_unstable();
        qualities.dedup();
        debug!(
            tiles = region.tiles.len(),
            ?qualities,
            levels = ?region.quant_prog_vals.iter().map(|q| q.quality).collect::<Vec<_>>(),
            "progressive region"
        );
        for tile in &region.tiles {
            let kind = match tile {
                ProgressiveTile::Simple(_) => 0,
                ProgressiveTile::First(t) if t.quality == 0xFF => 2,
                ProgressiveTile::First(_) => 1,
                ProgressiveTile::Upgrade(t) if t.quality == 0xFF => 4,
                ProgressiveTile::Upgrade(_) => 3,
            };
            stats.prog_tiles[kind].fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Counts a failed H.264 frame and asks for a repaint (the session survives it).
fn decode_failed(stats: &GfxStats, e: &anyhow::Error) {
    let n = stats.decode_errors.fetch_add(1, Ordering::Relaxed);
    if n < 3 || n.is_power_of_two() {
        warn!(error = %format!("{e:#}"), total = n + 1, "H.264 frame failed to decode; requesting repaint");
    }
    stats.needs_refresh.store(true, Ordering::Relaxed);
}

/// The surface rectangles an H.264 (sub)frame updates: its region rects, clipped to the
/// picture and the surface, or the destination rectangle when it lists none.
fn avc_rects(regions: &[ExclusiveRectangle], dest: Rect, (pw, ph): (usize, usize), (sw, sh): (u16, u16)) -> Vec<Rect> {
    let (cw, ch) = (pw.min(usize::from(sw)) as u16, ph.min(usize::from(sh)) as u16);
    if regions.is_empty() {
        return dest.clip(cw, ch).into_iter().collect();
    }
    regions
        .iter()
        .filter_map(|r| Rect::from_exclusive(r).clip(cw, ch))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_egfx::pdu::{
        CreateSurfacePdu, EndFramePdu, MapSurfaceToOutputPdu, PixelFormat, ResetGraphicsPdu, StartFramePdu,
    };

    fn client() -> (GfxClient, SharedOutput) {
        let output: SharedOutput = Arc::new(Mutex::new(GfxOutput::new()));
        let c = GfxClient::new(
            Vec::new(),
            None,
            DecoderPreference::Software,
            Arc::new(GfxStats::default()),
            Arc::clone(&output),
        );
        (c, output)
    }

    fn run(c: &mut GfxClient, out: &SharedOutput, pdus: Vec<GfxPdu>) -> usize {
        let mut replies = Vec::new();
        let mut o = out.lock();
        for p in pdus {
            c.handle(p, &mut o, &mut replies);
        }
        replies.len()
    }

    fn rect(l: u16, t: u16, r: u16, b: u16) -> ExclusiveRectangle {
        ExclusiveRectangle {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn primary_surface_aliases_output_and_reports_damage() {
        let (mut c, out) = client();
        let acks = run(
            &mut c,
            &out,
            vec![
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: 8,
                    height: 4,
                    monitors: Vec::new(),
                }),
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 1,
                    width: 8,
                    height: 4,
                    pixel_format: PixelFormat::XRgb,
                }),
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 1,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
                GfxPdu::StartFrame(StartFramePdu {
                    timestamp: ironrdp_egfx::pdu::Timestamp {
                        milliseconds: 0,
                        seconds: 0,
                        minutes: 0,
                        hours: 0,
                    },
                    frame_id: 7,
                }),
                GfxPdu::SolidFill(SolidFillPdu {
                    surface_id: 1,
                    fill_pixel: ironrdp_egfx::pdu::Color {
                        b: 3,
                        g: 2,
                        r: 1,
                        xa: 0,
                    },
                    rectangles: vec![rect(2, 1, 4, 3)],
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 7 }),
            ],
        );
        assert_eq!(acks, 1, "one frame acknowledgement");
        assert_eq!(c.aliased, Some(1));
        let mut o = out.lock();
        assert!(o.active);
        let px = |o: &GfxOutput, x: usize, y: usize| o.data[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].to_vec();
        assert_eq!(px(&o, 2, 1), vec![1, 2, 3, 255]);
        assert_eq!(px(&o, 1, 1)[..3], [0, 0, 0]);
        let damage = o.take_damage();
        assert!(
            damage
                .iter()
                .any(|d| d.left == 2 && d.top == 1 && d.right == 3 && d.bottom == 2)
        );
    }

    fn setup_primary(c: &mut GfxClient, out: &SharedOutput, w: u16, h: u16) {
        run(
            c,
            out,
            vec![
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: u32::from(w),
                    height: u32::from(h),
                    monitors: Vec::new(),
                }),
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 1,
                    width: w,
                    height: h,
                    pixel_format: PixelFormat::XRgb,
                }),
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 1,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
            ],
        );
    }

    fn fill(id: u16, r: u8, rect_: ExclusiveRectangle) -> GfxPdu {
        GfxPdu::SolidFill(SolidFillPdu {
            surface_id: id,
            fill_pixel: ironrdp_egfx::pdu::Color { b: 0, g: 0, r, xa: 0 },
            rectangles: vec![rect_],
        })
    }

    fn red_at(out: &SharedOutput, w: usize, x: usize, y: usize) -> u8 {
        out.lock().data[(y * w + x) * 4]
    }

    #[test]
    fn surface_to_surface_scrolls_within_the_primary_surface() {
        let (mut c, out) = client();
        setup_primary(&mut c, &out, 8, 8);
        run(
            &mut c,
            &out,
            vec![
                fill(1, 50, rect(0, 4, 8, 6)),
                // Scroll rows 4..6 up to row 1 (overlapping source/destination surface).
                GfxPdu::SurfaceToSurface(SurfaceToSurfacePdu {
                    source_surface_id: 1,
                    destination_surface_id: 1,
                    source_rectangle: rect(0, 2, 8, 6),
                    destination_points: vec![Point { x: 0, y: 0 }],
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 2 }),
            ],
        );
        assert_eq!(red_at(&out, 8, 3, 2), 50, "row 4 moved to row 2");
        assert_eq!(red_at(&out, 8, 3, 3), 50, "row 5 moved to row 3");
        assert_eq!(red_at(&out, 8, 3, 1), 0, "row 3 (black) moved to row 1");
    }

    #[test]
    fn cache_round_trip_repaints_and_survives_overwrite() {
        let (mut c, out) = client();
        setup_primary(&mut c, &out, 8, 8);
        run(
            &mut c,
            &out,
            vec![
                fill(1, 90, rect(0, 0, 2, 2)),
                GfxPdu::SurfaceToCache(SurfaceToCachePdu {
                    surface_id: 1,
                    cache_key: 7,
                    cache_slot: 3,
                    source_rectangle: rect(0, 0, 2, 2),
                }),
                fill(1, 10, rect(0, 0, 8, 8)),
                GfxPdu::CacheToSurface(CacheToSurfacePdu {
                    cache_slot: 3,
                    surface_id: 1,
                    destination_points: vec![Point { x: 5, y: 5 }, Point { x: 7, y: 7 }],
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 3 }),
            ],
        );
        assert_eq!(red_at(&out, 8, 0, 0), 10, "origin overwritten after caching");
        assert_eq!(red_at(&out, 8, 6, 6), 90, "cached tile restored at (5,5)");
        assert_eq!(red_at(&out, 8, 7, 7), 90, "clipped copy at the edge");
        assert_eq!(c.cache_bytes, 2 * 2 * 4);

        run(
            &mut c,
            &out,
            vec![GfxPdu::EvictCacheEntry(ironrdp_egfx::pdu::EvictCacheEntryPdu {
                cache_slot: 3,
            })],
        );
        assert_eq!(c.cache_bytes, 0);
    }

    /// A resize (ResetGraphics) must not drop the bitmap cache: Windows keeps placing
    /// cached tiles afterwards, which came out as black blocks.
    #[test]
    fn graphics_reset_keeps_the_cache_and_surfaces() {
        let (mut c, out) = client();
        setup_primary(&mut c, &out, 8, 8);
        run(
            &mut c,
            &out,
            vec![
                fill(1, 90, rect(0, 0, 2, 2)),
                GfxPdu::SurfaceToCache(SurfaceToCachePdu {
                    surface_id: 1,
                    cache_key: 7,
                    cache_slot: 3,
                    source_rectangle: rect(0, 0, 2, 2),
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }),
            ],
        );
        // What Windows sends on a resize: delete, reset, create, map, then cached tiles.
        run(
            &mut c,
            &out,
            vec![
                GfxPdu::DeleteSurface(ironrdp_egfx::pdu::DeleteSurfacePdu { surface_id: 1 }),
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: 12,
                    height: 6,
                    monitors: Vec::new(),
                }),
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 1,
                    width: 12,
                    height: 6,
                    pixel_format: PixelFormat::XRgb,
                }),
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 1,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
                GfxPdu::CacheToSurface(CacheToSurfacePdu {
                    cache_slot: 3,
                    surface_id: 1,
                    destination_points: vec![Point { x: 9, y: 3 }],
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 2 }),
            ],
        );
        let size = {
            let o = out.lock();
            (o.width, o.height)
        };
        assert_eq!(size, (12, 6));
        assert_eq!(red_at(&out, 12, 10, 4), 90, "cached tile placed after the reset");
        assert_eq!(c.aliased, Some(1), "the re-created surface is the primary again");

        // A surface the server did not delete survives a reset with its pixels.
        run(
            &mut c,
            &out,
            vec![GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 12,
                height: 6,
                monitors: Vec::new(),
            })],
        );
        assert!(c.surfaces.contains_key(&1));
        assert_eq!(c.aliased, None);
        run(
            &mut c,
            &out,
            vec![
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 1,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 3 }),
            ],
        );
        assert_eq!(red_at(&out, 12, 10, 4), 90, "pixels kept across the reset");
    }

    #[test]
    fn remapping_the_primary_surface_takes_its_pixels_back() {
        let (mut c, out) = client();
        setup_primary(&mut c, &out, 4, 4);
        run(&mut c, &out, vec![fill(1, 77, rect(0, 0, 4, 4))]);
        assert_eq!(c.aliased, Some(1));
        run(
            &mut c,
            &out,
            vec![GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 1,
                output_origin_y: 1,
            })],
        );
        assert_eq!(c.aliased, None);
        match &c.surfaces[&1].store {
            Store::Own(data) => assert_eq!(data[0], 77),
            Store::Output => panic!("surface should own its pixels again"),
        }
    }

    #[test]
    fn offset_surface_composes_into_output() {
        let (mut c, out) = client();
        run(
            &mut c,
            &out,
            vec![
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: 8,
                    height: 8,
                    monitors: Vec::new(),
                }),
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 2,
                    width: 2,
                    height: 2,
                    pixel_format: PixelFormat::XRgb,
                }),
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 2,
                    output_origin_x: 5,
                    output_origin_y: 6,
                }),
                GfxPdu::SolidFill(SolidFillPdu {
                    surface_id: 2,
                    fill_pixel: ironrdp_egfx::pdu::Color {
                        b: 30,
                        g: 20,
                        r: 10,
                        xa: 0,
                    },
                    rectangles: vec![rect(0, 0, 2, 2)],
                }),
                GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }),
            ],
        );
        assert_eq!(c.aliased, None);
        let o = out.lock();
        let i = (7 * 8 + 6) * 4;
        assert_eq!(&o.data[i..i + 4], &[10, 20, 30, 255]);
    }

    /// Real H.264 through the client: both views encoded with OpenH264, sent as
    /// AVC444 / AVC444v2 wire PDUs, decoded and combined.
    #[cfg(feature = "openh264")]
    mod avc444_end_to_end {
        use super::*;
        use crate::gfx::avc444::Layout;
        use crate::gfx::avc444::tests::{Planes, pack, sample_planes};
        use ironrdp_egfx::pdu::{PixelFormat, QuantQuality};
        use openh264::OpenH264API;
        use openh264::encoder::{Encoder, EncoderConfig, QpRange, UsageType};
        use openh264::formats::YUVSlices;

        const W: u16 = 128;
        const H: u16 = 64;

        fn encoder() -> Encoder {
            let config = EncoderConfig::new()
                .qp(QpRange::new(6, 12))
                .usage_type(UsageType::ScreenContentRealTime)
                .skip_frames(false);
            Encoder::with_api_config(OpenH264API::from_source(), config).unwrap()
        }

        fn encode(enc: &mut Encoder, p: &Planes) -> Vec<u8> {
            let src = YUVSlices::new((&p.y, &p.u, &p.v), (p.w, p.h), (p.w, p.w / 2, p.w / 2));
            enc.encode(&src).unwrap().to_vec()
        }

        fn sub(data: &[u8]) -> Avc420BitmapStream<'_> {
            Avc420BitmapStream {
                rectangles: vec![rect(0, 0, W, H)],
                quant_qual_vals: vec![QuantQuality {
                    quantization_parameter: 10,
                    progressive: false,
                    quality: 100,
                }],
                data,
            }
        }

        fn wire(codec_id: Codec1Type, bitmap_data: Vec<u8>) -> GfxPdu {
            GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id,
                pixel_format: PixelFormat::XRgb,
                destination_rectangle: rect(0, 0, W, H),
                bitmap_data,
            })
        }

        fn avc444(codec: Codec1Type, encoding: Encoding, s1: &[u8], s2: Option<&[u8]>) -> GfxPdu {
            let stream = Avc444BitmapStream {
                encoding,
                stream1: sub(s1),
                stream2: s2.map(sub),
            };
            wire(codec, ironrdp_core::encode_vec(&stream).unwrap())
        }

        fn client_with_decoder() -> (GfxClient, SharedOutput) {
            let output: SharedOutput = Arc::new(Mutex::new(GfxOutput::new()));
            let mut c = GfxClient::new(
                Vec::new(),
                h264::new_software(),
                DecoderPreference::Software,
                Arc::new(GfxStats::default()),
                Arc::clone(&output),
            );
            setup_primary(&mut c, &output, W, H);
            (c, output)
        }

        /// Mean absolute RGB error over the left half (single-pixel chroma detail).
        fn detail_error(out: &SharedOutput, expected: &[u8]) -> f64 {
            let o = out.lock();
            let (w, h) = (usize::from(W), usize::from(H));
            let mut sum = 0u64;
            let mut n = 0u64;
            for y in 0..h {
                for x in 0..w / 2 {
                    let i = (y * w + x) * 4;
                    for c in 0..3 {
                        sum += u64::from(o.data[i + c].abs_diff(expected[i + c]));
                        n += 1;
                    }
                }
            }
            sum as f64 / n as f64
        }

        fn expected_rgba(y: &[u8], u: &[u8], v: &[u8]) -> Vec<u8> {
            let (w, h) = (usize::from(W), usize::from(H));
            let img = yuv::YuvPlanarImage {
                y_plane: y,
                y_stride: w as u32,
                u_plane: u,
                u_stride: w as u32,
                v_plane: v,
                v_stride: w as u32,
                width: w as u32,
                height: h as u32,
            };
            let mut out = vec![0; w * h * 4];
            yuv::yuv444_to_rgba(
                &img,
                &mut out,
                (w * 4) as u32,
                yuv::YuvRange::Full,
                yuv::YuvStandardMatrix::Bt709,
            )
            .unwrap();
            out
        }

        fn check(layout: Layout, codec: Codec1Type) {
            let (w, h) = (usize::from(W), usize::from(H));
            let (y, u, v) = sample_planes(w, h);
            let expected = expected_rgba(&y, &u, &v);
            let (main, aux) = pack(&y, &u, &v, w, h, layout);
            let main_bits = encode(&mut encoder(), &main);
            let aux_bits = encode(&mut encoder(), &aux);

            // Both views in one PDU (LC 0).
            let (mut c, out) = client_with_decoder();
            run(
                &mut c,
                &out,
                vec![avc444(codec, Encoding::LUMA_AND_CHROMA, &main_bits, Some(&aux_bits))],
            );
            let full = detail_error(&out, &expected);

            // Luma first (LC 1: looks like 4:2:0), then chroma alone (LC 2) completes it.
            let (mut c, out) = client_with_decoder();
            run(&mut c, &out, vec![avc444(codec, Encoding::LUMA, &main_bits, None)]);
            let luma_only = detail_error(&out, &expected);
            run(&mut c, &out, vec![avc444(codec, Encoding::CHROMA, &aux_bits, None)]);
            let split = detail_error(&out, &expected);

            // Plain AVC420 of the main view, for comparison.
            let (mut c, out) = client_with_decoder();
            let avc420 = encode_avc420(&main_bits);
            run(&mut c, &out, vec![wire(Codec1Type::Avc420, avc420)]);
            let plain = detail_error(&out, &expected);

            eprintln!("{layout:?}: LC0 {full:.2}, LC1 {luma_only:.2}, LC1+LC2 {split:.2}, AVC420 {plain:.2}");
            assert!(plain > 40.0, "4:2:0 should lose the detail ({plain:.2})");
            assert!(luma_only > 40.0, "luma-only update converts as 4:2:0 ({luma_only:.2})");
            assert!(full < 6.0, "AVC444 should keep single-pixel chroma ({full:.2})");
            assert!(
                (split - full).abs() < 0.01,
                "LC1 + LC2 must equal LC0 ({split:.2} vs {full:.2})"
            );
            assert_eq!(c.stats.avc444.load(Ordering::Relaxed), 0);
        }

        fn encode_avc420(bits: &[u8]) -> Vec<u8> {
            ironrdp_core::encode_vec(&sub(bits)).unwrap()
        }

        #[test]
        fn avc444_v1_decodes_full_chroma() {
            check(Layout::V1, Codec1Type::Avc444);
        }

        #[test]
        fn avc444_v2_decodes_full_chroma() {
            check(Layout::V2, Codec1Type::Avc444v2);
        }
    }
}
