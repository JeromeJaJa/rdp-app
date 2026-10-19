//! Shared-memory frame surfaces.
//!
//! A surface is one contiguous block of memory shared with the renderer
//! (WebView2 shared buffer in the app). The engine writes pixels and appends
//! the changed rectangles to a ring; the renderer polls the ring once per
//! display frame and uploads only what changed. No per-frame messages cross
//! the process boundary.
//!
//! Layout (all integers little-endian, offsets in bytes):
//!
//! ```text
//!   0  u32 magic        0x46504452 ("RDPF")
//!   4  u32 version      1
//!   8  u32 width
//!  12  u32 height
//!  16  u32 stride       bytes per pixel row (width * 4)
//!  20  u32 seq          rectangles published so far (wraps)
//!  24  u32 ring_cap     number of ring slots
//!  28  u32 pixel_offset
//!  64  ring             ring_cap slots of { u16 x, u16 y, u16 w, u16 h }
//! 4096 pixels           RGBA8, row-major, `stride` bytes per row
//! ```
//!
//! Protocol: the writer copies pixels, then writes the rect into slot
//! `seq % ring_cap`, then increments `seq` (release). The reader keeps its own
//! cursor; if it fell more than `ring_cap` behind it uploads everything.
//! A reader may observe a rect mid-write for one display frame; the rect is
//! always republished once the write completes, so the image converges.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ironrdp_pdu::geometry::InclusiveRectangle;

pub const MAGIC: u32 = 0x4650_4452;
pub const VERSION: u32 = 1;
pub const RING_CAP: u32 = 448;
pub const RING_OFFSET: usize = 64;
pub const PIXEL_OFFSET: usize = 4096;
const _: () = assert!(RING_OFFSET + RING_CAP as usize * 8 <= PIXEL_OFFSET);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceKind {
    /// Full-resolution framebuffer for the main session view.
    Full,
    /// Downscaled preview for grid tiles.
    Thumb,
}

impl SurfaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SurfaceKind::Full => "full",
            SurfaceKind::Thumb => "thumb",
        }
    }
}

/// Bytes needed for a surface of the given size.
pub fn surface_len(width: u32, height: u32) -> usize {
    PIXEL_OFFSET + width as usize * height as usize * 4
}

/// Host-owned memory backing a surface.
///
/// # Safety contract
/// `as_ptr()` must stay valid for `len()` bytes until the value is dropped, and
/// the start must be at least 8-byte aligned.
pub trait SurfaceMemory: Send + Sync {
    fn as_ptr(&self) -> *mut u8;
    fn len(&self) -> usize;
}

/// Allocates surfaces for sessions. Calls come from session threads and may block
/// (e.g. to marshal to a UI thread).
pub trait SurfaceProvider: Send + Sync {
    fn allocate(
        &self,
        session: &str,
        kind: SurfaceKind,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Arc<dyn SurfaceMemory>>;

    /// The session stopped using its surface of this kind and dropped every reference.
    fn release(&self, session: &str, kind: SurfaceKind);
}

/// Plain heap memory; used by tools and tests.
pub struct HeapSurface {
    buf: Box<[u64]>,
    len: usize,
}

impl HeapSurface {
    pub fn new(len: usize) -> Self {
        Self {
            buf: vec![0u64; len.div_ceil(8)].into_boxed_slice(),
            len,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the u64 buffer covers at least `len` bytes.
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.len) }
    }
}

impl SurfaceMemory for HeapSurface {
    fn as_ptr(&self) -> *mut u8 {
        self.buf.as_ptr().cast::<u8>().cast_mut()
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// A provider that hands out heap surfaces (no renderer attached).
#[derive(Default)]
pub struct HeapProvider;

impl SurfaceProvider for HeapProvider {
    fn allocate(&self, _: &str, _: SurfaceKind, width: u32, height: u32) -> anyhow::Result<Arc<dyn SurfaceMemory>> {
        Ok(Arc::new(HeapSurface::new(surface_len(width, height))))
    }

    fn release(&self, _: &str, _: SurfaceKind) {}
}

/// Writes pixels and damage into a surface.
pub struct SurfaceWriter {
    mem: Arc<dyn SurfaceMemory>,
    width: u32,
    height: u32,
    seq: u32,
}

impl SurfaceWriter {
    pub fn new(mem: Arc<dyn SurfaceMemory>, width: u32, height: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(mem.len() >= surface_len(width, height), "surface memory too small");
        anyhow::ensure!(mem.as_ptr() as usize % 8 == 0, "surface memory misaligned");
        let writer = Self {
            mem,
            width,
            height,
            seq: 0,
        };
        writer.header(8).store(width, Ordering::Relaxed);
        writer.header(12).store(height, Ordering::Relaxed);
        writer.header(16).store(width * 4, Ordering::Relaxed);
        writer.header(20).store(0, Ordering::Relaxed);
        writer.header(24).store(RING_CAP, Ordering::Relaxed);
        writer.header(28).store(PIXEL_OFFSET as u32, Ordering::Relaxed);
        writer.header(4).store(VERSION, Ordering::Relaxed);
        writer.header(0).store(MAGIC, Ordering::Release);
        Ok(writer)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    fn header(&self, offset: usize) -> &AtomicU32 {
        // SAFETY: offset < RING_OFFSET, memory is 8-byte aligned and lives as long as `self.mem`.
        unsafe { &*self.mem.as_ptr().add(offset).cast::<AtomicU32>() }
    }

    fn pixels_ptr(&self) -> *mut u8 {
        // SAFETY: the allocation is at least PIXEL_OFFSET + w*h*4 bytes (checked in `new`).
        unsafe { self.mem.as_ptr().add(PIXEL_OFFSET) }
    }

    /// Mutable view of one pixel row.
    #[inline]
    pub fn row_mut(&mut self, y: u32) -> &mut [u8] {
        debug_assert!(y < self.height);
        let stride = self.width as usize * 4;
        // SAFETY: row `y` is within the pixel area; `&mut self` prevents aliasing from this side.
        unsafe { std::slice::from_raw_parts_mut(self.pixels_ptr().add(y as usize * stride), stride) }
    }

    /// Copies a rectangle from an RGBA source image of identical dimensions.
    pub fn copy_rect(&mut self, src: &[u8], src_stride: usize, rect: &InclusiveRectangle) {
        let Some((x, y, w, h)) = clip(rect, self.width, self.height) else {
            return;
        };
        let stride = self.width as usize * 4;
        let row_bytes = w as usize * 4;
        let dst = self.pixels_ptr();
        for row in y..y + h {
            let s = row as usize * src_stride + x as usize * 4;
            let d = row as usize * stride + x as usize * 4;
            if s + row_bytes > src.len() {
                break;
            }
            // SAFETY: both ranges were bounds-checked against their buffers.
            unsafe { std::ptr::copy_nonoverlapping(src.as_ptr().add(s), dst.add(d), row_bytes) };
        }
    }

    /// Announces a changed rectangle to the reader.
    pub fn publish(&mut self, rect: &InclusiveRectangle) {
        let Some((x, y, w, h)) = clip(rect, self.width, self.height) else {
            return;
        };
        let slot = (self.seq % RING_CAP) as usize;
        // SAFETY: slot is within the ring area.
        unsafe {
            let p = self.mem.as_ptr().add(RING_OFFSET + slot * 8).cast::<u16>();
            p.write_volatile(x as u16);
            p.add(1).write_volatile(y as u16);
            p.add(2).write_volatile(w as u16);
            p.add(3).write_volatile(h as u16);
        }
        self.seq = self.seq.wrapping_add(1);
        self.header(20).store(self.seq, Ordering::Release);
    }

    pub fn publish_full(&mut self) {
        let full = InclusiveRectangle {
            left: 0,
            top: 0,
            right: (self.width - 1) as u16,
            bottom: (self.height - 1) as u16,
        };
        self.publish(&full);
    }
}

fn clip(rect: &InclusiveRectangle, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let x0 = u32::from(rect.left);
    let y0 = u32::from(rect.top);
    let x1 = (u32::from(rect.right) + 1).min(width);
    let y1 = (u32::from(rect.bottom) + 1).min(height);
    (x0 < x1 && y0 < y1).then(|| (x0, y0, x1 - x0, y1 - y0))
}

/// Maintains a downscaled copy of a framebuffer, updated incrementally.
pub struct ThumbWriter {
    pub writer: SurfaceWriter,
    src_w: u32,
    src_h: u32,
    /// Union of source-space damage not yet reflected in the thumbnail.
    pending: Option<InclusiveRectangle>,
}

impl ThumbWriter {
    /// Thumbnail dimensions for a source of `src_w × src_h` at `target_w` wide.
    pub fn dims(src_w: u32, src_h: u32, target_w: u32) -> (u32, u32) {
        let w = target_w.clamp(64, src_w.max(64)).min(src_w.max(1));
        let h = ((u64::from(w) * u64::from(src_h)) / u64::from(src_w.max(1))).max(1) as u32;
        (w, h)
    }

    pub fn new(writer: SurfaceWriter, src_w: u32, src_h: u32) -> Self {
        Self {
            writer,
            src_w,
            src_h,
            pending: None,
        }
    }

    pub fn matches(&self, src_w: u32, src_h: u32, target_w: u32) -> bool {
        self.src_w == src_w
            && self.src_h == src_h
            && Self::dims(src_w, src_h, target_w) == (self.writer.width(), self.writer.height())
    }

    pub fn mark(&mut self, rect: &InclusiveRectangle) {
        self.pending = Some(match self.pending.take() {
            None => rect.clone(),
            Some(p) => InclusiveRectangle {
                left: p.left.min(rect.left),
                top: p.top.min(rect.top),
                right: p.right.max(rect.right),
                bottom: p.bottom.max(rect.bottom),
            },
        });
    }

    pub fn mark_all(&mut self) {
        self.pending = Some(InclusiveRectangle {
            left: 0,
            top: 0,
            right: (self.src_w - 1) as u16,
            bottom: (self.src_h - 1) as u16,
        });
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Resamples pending damage from `src` (RGBA, `src_w × src_h`) and publishes it.
    pub fn flush(&mut self, src: &[u8]) {
        let Some(rect) = self.pending.take() else {
            return;
        };
        let (tw, th) = (self.writer.width(), self.writer.height());
        let (sw, sh) = (self.src_w, self.src_h);
        let src_stride = sw as usize * 4;
        if src.len() < src_stride * sh as usize {
            return;
        }

        // Thumbnail pixels whose source footprint intersects the damage.
        let tx0 = (u64::from(rect.left) * u64::from(tw) / u64::from(sw)) as u32;
        let ty0 = (u64::from(rect.top) * u64::from(th) / u64::from(sh)) as u32;
        let tx1 = ((u64::from(rect.right) + 1) * u64::from(tw))
            .div_ceil(u64::from(sw))
            .min(u64::from(tw)) as u32;
        let ty1 = ((u64::from(rect.bottom) + 1) * u64::from(th))
            .div_ceil(u64::from(sh))
            .min(u64::from(th)) as u32;
        if tx0 >= tx1 || ty0 >= ty1 {
            return;
        }

        // 2×2 samples per output pixel at the quarter points of its source box:
        // close to a box filter for typical 3–6× reductions at a fraction of the cost.
        let fx = |tx: u32, q: u32| -> usize {
            ((u64::from(tx) * 4 + u64::from(q)) * u64::from(sw) / (u64::from(tw) * 4)).min(u64::from(sw - 1)) as usize
        };
        let fy = |ty: u32, q: u32| -> usize {
            ((u64::from(ty) * 4 + u64::from(q)) * u64::from(sh) / (u64::from(th) * 4)).min(u64::from(sh - 1)) as usize
        };

        let xs: Vec<(usize, usize)> = (tx0..tx1).map(|tx| (fx(tx, 1) * 4, fx(tx, 3) * 4)).collect();
        for ty in ty0..ty1 {
            let r0 = &src[fy(ty, 1) * src_stride..][..src_stride];
            let r1 = &src[fy(ty, 3) * src_stride..][..src_stride];
            let row = self.writer.row_mut(ty);
            for (i, &(a, b)) in xs.iter().enumerate() {
                let o = (tx0 as usize + i) * 4;
                for c in 0..3 {
                    let sum = u32::from(r0[a + c]) + u32::from(r0[b + c]) + u32::from(r1[a + c]) + u32::from(r1[b + c]);
                    row[o + c] = ((sum + 2) >> 2) as u8;
                }
                row[o + 3] = 255;
            }
        }

        self.writer.publish(&InclusiveRectangle {
            left: tx0 as u16,
            top: ty0 as u16,
            right: (tx1 - 1) as u16,
            bottom: (ty1 - 1) as u16,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_publishes_rects_into_ring() {
        let mem: Arc<dyn SurfaceMemory> = Arc::new(HeapSurface::new(surface_len(4, 2)));
        let mut w = SurfaceWriter::new(Arc::clone(&mem), 4, 2).unwrap();
        let src: Vec<u8> = (0..32).collect();
        let rect = InclusiveRectangle {
            left: 1,
            top: 0,
            right: 2,
            bottom: 1,
        };
        w.copy_rect(&src, 16, &rect);
        w.publish(&rect);

        let bytes = unsafe { std::slice::from_raw_parts(mem.as_ptr(), mem.len()) };
        let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        assert_eq!(u32_at(0), MAGIC);
        assert_eq!(u32_at(20), 1);
        let slot = &bytes[RING_OFFSET..RING_OFFSET + 8];
        assert_eq!(slot, &[1, 0, 0, 0, 2, 0, 2, 0]);
        // Row 0, pixels 1..=2 copied; pixel 0 untouched.
        assert_eq!(&bytes[PIXEL_OFFSET..PIXEL_OFFSET + 4], &[0, 0, 0, 0]);
        assert_eq!(&bytes[PIXEL_OFFSET + 4..PIXEL_OFFSET + 12], &src[4..12]);
        assert_eq!(&bytes[PIXEL_OFFSET + 16 + 4..PIXEL_OFFSET + 16 + 12], &src[20..28]);
    }

    #[test]
    fn thumbnail_downscales_uniform_color() {
        let (sw, sh) = (64u32, 32u32);
        let src: Vec<u8> = std::iter::repeat([10u8, 20, 30, 255])
            .take((sw * sh) as usize)
            .flatten()
            .collect();
        let (tw, th) = ThumbWriter::dims(sw, sh, 16);
        assert_eq!((tw, th), (64, 32), "clamped to at least 64 wide");
        let (tw, th) = (16, 8);
        let mem: Arc<dyn SurfaceMemory> = Arc::new(HeapSurface::new(surface_len(tw, th)));
        let writer = SurfaceWriter::new(Arc::clone(&mem), tw, th).unwrap();
        let mut t = ThumbWriter::new(writer, sw, sh);
        t.mark_all();
        t.flush(&src);
        let bytes = unsafe { std::slice::from_raw_parts(mem.as_ptr(), mem.len()) };
        let px = &bytes[PIXEL_OFFSET + (3 * tw as usize + 5) * 4..][..4];
        assert_eq!(px, &[10, 20, 30, 255]);
    }
}
