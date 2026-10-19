//! H.264 decoding to YUV, plus region-limited colour conversion.
//!
//! Decoders hand out the YUV planes rather than RGBA so the client converts
//! only the rectangles the server marked as changed, straight into the target
//! surface (a full 1080p RGBA conversion per frame would dominate CPU time on
//! typical desktops where little changes).

use anyhow::{Context as _, bail};
#[cfg(feature = "openh264")]
use ironrdp_egfx::pdu::{avc_to_annex_b_into, is_avc_format};

use crate::DecoderPreference;

/// One plane of a picture: `data[y * stride + x * step]` is the sample at (x, y).
#[derive(Clone, Copy)]
pub(crate) struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    /// 1 for planar data, 2 for one half of an interleaved (NV12) chroma plane.
    pub step: usize,
}

impl Plane<'_> {
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> Option<u8> {
        self.data.get(y * self.stride + x * self.step).copied()
    }
}

/// Chroma layout of a decoded picture (both are 4:2:0).
pub(crate) enum Chroma<'a> {
    /// I420: separate U and V planes.
    Planar {
        u: &'a [u8],
        v: &'a [u8],
        u_stride: usize,
        v_stride: usize,
    },
    /// NV12: one plane of interleaved U, V pairs.
    Interleaved { uv: &'a [u8], stride: usize },
}

/// A decoded 4:2:0 picture, borrowed from the decoder until the next call.
///
/// `width` × `height` is the visible size; planes may extend further (codec padding).
pub(crate) struct Yuv<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    pub chroma: Chroma<'a>,
    pub width: usize,
    pub height: usize,
}

impl<'a> Yuv<'a> {
    pub fn y_plane(&self) -> Plane<'a> {
        Plane {
            data: self.y,
            stride: self.y_stride,
            step: 1,
        }
    }

    pub fn u_plane(&self) -> Plane<'a> {
        match self.chroma {
            Chroma::Planar { u, u_stride, .. } => Plane {
                data: u,
                stride: u_stride,
                step: 1,
            },
            Chroma::Interleaved { uv, stride } => Plane {
                data: uv,
                stride,
                step: 2,
            },
        }
    }

    pub fn v_plane(&self) -> Plane<'a> {
        match self.chroma {
            Chroma::Planar { v, v_stride, .. } => Plane {
                data: v,
                stride: v_stride,
                step: 1,
            },
            Chroma::Interleaved { uv, stride } => Plane {
                data: uv.get(1..).unwrap_or_default(),
                stride,
                step: 2,
            },
        }
    }
}

pub(crate) trait Decoder: Send {
    /// Decodes one access unit. `Ok(None)` means the decoder produced no picture yet.
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<Yuv<'_>>>;
    fn name(&self) -> &'static str;
    /// Decodes on the GPU.
    fn hardware(&self) -> bool {
        false
    }
}

/// A decoder for `pref`: the GPU when asked for (or, for `Auto`, while GPU decoders
/// are available), otherwise software.
pub(crate) fn new_decoder(pref: DecoderPreference) -> Option<Box<dyn Decoder>> {
    #[cfg(windows)]
    {
        let gpu = match pref {
            DecoderPreference::Hardware => true,
            DecoderPreference::Auto => super::mf::active_decoders() < super::mf::AUTO_LIMIT,
            DecoderPreference::Software => false,
        };
        if gpu {
            use std::sync::atomic::{AtomicBool, Ordering};
            match super::mf::MfDecoder::new() {
                Ok(d) => return Some(Box::new(d)),
                Err(e) => {
                    static WARNED: AtomicBool = AtomicBool::new(false);
                    if !WARNED.swap(true, Ordering::Relaxed) {
                        tracing::warn!(error = %format!("{e:#}"), "GPU H.264 decoding unavailable; using software");
                    }
                }
            }
        }
    }
    let _ = pref;
    new_software()
}

/// Owns one H.264 stream's decoder: created on first use (unless given up front), and
/// a GPU decoder that fails is replaced by software decoding for the rest of the session.
pub(crate) struct Slot {
    decoder: Option<Box<dyn Decoder>>,
    pref: DecoderPreference,
    created: bool,
    gpu_failed: bool,
}

impl Slot {
    pub fn new(decoder: Option<Box<dyn Decoder>>, pref: DecoderPreference) -> Self {
        Self {
            created: decoder.is_some(),
            decoder,
            pref,
            gpu_failed: false,
        }
    }

    pub fn lazy(pref: DecoderPreference) -> Self {
        Self::new(None, pref)
    }

    pub fn available(&self) -> bool {
        self.decoder.is_some() || !self.created
    }

    pub fn hardware(&self) -> bool {
        self.decoder.as_ref().is_some_and(|d| d.hardware())
    }

    pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<Yuv<'_>>> {
        if self.gpu_failed {
            self.gpu_failed = false;
            tracing::warn!("GPU H.264 decoding failed; switching this stream to software");
            self.decoder = new_software();
        }
        if !self.created {
            self.created = true;
            self.decoder = new_decoder(self.pref);
        }
        let Some(decoder) = self.decoder.as_mut() else {
            bail!("no H.264 decoder");
        };
        let gpu = decoder.hardware();
        let result = decoder.decode(data);
        if gpu && result.is_err() {
            self.gpu_failed = true;
        }
        result
    }
}

/// The software decoder (also the fallback when a GPU decoder fails mid-session).
pub(crate) fn new_software() -> Option<Box<dyn Decoder>> {
    #[cfg(feature = "openh264")]
    {
        match OpenH264::new() {
            Ok(d) => return Some(Box::new(d)),
            Err(e) => tracing::warn!(error = %e, "OpenH264 decoder unavailable"),
        }
    }
    None
}

#[cfg(feature = "openh264")]
struct OpenH264 {
    decoder: openh264::decoder::Decoder,
    annex_b: Vec<u8>,
}

#[cfg(feature = "openh264")]
impl OpenH264 {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            decoder: openh264::decoder::Decoder::new().context("create OpenH264 decoder")?,
            annex_b: Vec::new(),
        })
    }
}

#[cfg(feature = "openh264")]
impl Decoder for OpenH264 {
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<Yuv<'_>>> {
        use openh264::formats::YUVSource as _;

        let input = if is_avc_format(data) {
            avc_to_annex_b_into(data, &mut self.annex_b);
            &self.annex_b[..]
        } else {
            data
        };
        let Some(pic) = self.decoder.decode(input).context("OpenH264 decode")? else {
            return Ok(None);
        };
        let (width, height) = pic.dimensions();
        let (y_stride, u_stride, v_stride) = pic.strides();
        // SAFETY: the planes live in OpenH264's internal buffers, which stay valid until
        // the next decode call; the returned value borrows `self` mutably, so that
        // cannot happen while it is alive.
        let extend = |s: &[u8]| unsafe { std::slice::from_raw_parts(s.as_ptr(), s.len()) };
        Ok(Some(Yuv {
            y: extend(pic.y()),
            y_stride,
            chroma: Chroma::Planar {
                u: extend(pic.u()),
                v: extend(pic.v()),
                u_stride,
                v_stride,
            },
            width,
            height,
        }))
    }

    fn name(&self) -> &'static str {
        "OpenH264"
    }
}

/// Converts `rect` (x, y, w, h — already clipped to the picture) of a picture into
/// RGBA at the same position of `dst` (`dst_stride` bytes per row).
///
/// Colour space per MS-RDPEGFX 3.3.8.3.1: full-range BT.709.
pub(crate) fn convert_region(
    yuv: &Yuv<'_>,
    dst: &mut [u8],
    dst_stride: usize,
    rect: (usize, usize, usize, usize),
    scratch: &mut Vec<u8>,
) -> anyhow::Result<()> {
    let (x, y, w, h) = rect;
    if w == 0 || h == 0 {
        return Ok(());
    }

    // 4:2:0 chroma needs an even origin; convert the even-aligned superset into a
    // scratch buffer when the region is odd, then copy exactly the region.
    let ax = x & !1;
    let ay = y & !1;
    let aw = (w + (x - ax)).min(yuv.width - ax);
    let ah = (h + (y - ay)).min(yuv.height - ay);

    let convert = |cw: usize, ch: usize, out: &mut [u8], out_stride: usize| -> anyhow::Result<()> {
        let y_plane = rows(yuv.y, ay * yuv.y_stride + ax, yuv.y_stride, ch)?;
        let crows = ch.div_ceil(2);
        match yuv.chroma {
            Chroma::Planar {
                u,
                v,
                u_stride,
                v_stride,
            } => {
                let img = yuv::YuvPlanarImage {
                    y_plane,
                    y_stride: yuv.y_stride as u32,
                    u_plane: rows(u, (ay / 2) * u_stride + ax / 2, u_stride, crows)?,
                    u_stride: u_stride as u32,
                    v_plane: rows(v, (ay / 2) * v_stride + ax / 2, v_stride, crows)?,
                    v_stride: v_stride as u32,
                    width: cw as u32,
                    height: ch as u32,
                };
                yuv::yuv420_to_rgba(
                    &img,
                    out,
                    out_stride as u32,
                    yuv::YuvRange::Full,
                    yuv::YuvStandardMatrix::Bt709,
                )
            }
            Chroma::Interleaved { uv, stride } => {
                let uv_plane = rows(uv, (ay / 2) * stride + ax, stride, crows)?;
                // The NV12 converter needs a full stride after every row; near the
                // plane's end with an x offset there is none, so use tight copies.
                let (y_tight, uv_tight);
                let (y_plane, y_stride, uv_plane, uv_stride) =
                    if y_plane.len() < ch * yuv.y_stride || uv_plane.len() < crows * stride {
                        let cuv = cw.div_ceil(2) * 2;
                        y_tight = tight(y_plane, yuv.y_stride, cw, ch)?;
                        uv_tight = tight(uv_plane, stride, cuv, crows)?;
                        (&y_tight[..], cw, &uv_tight[..], cuv)
                    } else {
                        (y_plane, yuv.y_stride, uv_plane, stride)
                    };
                let img = yuv::YuvBiPlanarImage {
                    y_plane,
                    y_stride: y_stride as u32,
                    uv_plane,
                    uv_stride: uv_stride as u32,
                    width: cw as u32,
                    height: ch as u32,
                };
                yuv::yuv_nv12_to_rgba(
                    &img,
                    out,
                    out_stride as u32,
                    yuv::YuvRange::Full,
                    yuv::YuvStandardMatrix::Bt709,
                    yuv::YuvConversionMode::Balanced,
                )
            }
        }
        .context("YUV→RGBA")
    };

    // Convert in place when the origin is even and `h` whole strides follow it (the
    // converters work in full rows); otherwise go through the scratch buffer.
    let start = y * dst_stride + x * 4;
    if ax == x && ay == y && dst.len() >= start + h * dst_stride {
        // `aw`/`ah` may be clipped to the picture; convert exactly w×h.
        return convert(w, h, &mut dst[start..], dst_stride);
    }

    let sstride = aw * 4;
    scratch.resize(sstride * ah, 0);
    convert(aw, ah, scratch, sstride)?;
    copy_rows(scratch, sstride, (x - ax, y - ay), dst, dst_stride, (x, y, w, h))
}

/// `n` rows of a plane from `start`. The converter locates the last (odd) row from
/// the slice length, so it must get exactly `n` strides (or up to the plane's end).
fn rows(data: &[u8], start: usize, stride: usize, n: usize) -> anyhow::Result<&[u8]> {
    let end = (start + stride * n).min(data.len());
    data.get(start..end).context("region outside picture")
}

/// The first `w` bytes of `n` rows, packed without padding.
fn tight(data: &[u8], stride: usize, w: usize, n: usize) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(w * n);
    for row in 0..n {
        out.extend_from_slice(
            data.get(row * stride..row * stride + w)
                .context("region outside picture")?,
        );
    }
    Ok(out)
}

/// Copies a `w × h` block from `src` at `from` to `dst` at (x, y).
pub(crate) fn copy_rows(
    src: &[u8],
    src_stride: usize,
    from: (usize, usize),
    dst: &mut [u8],
    dst_stride: usize,
    (x, y, w, h): (usize, usize, usize, usize),
) -> anyhow::Result<()> {
    for row in 0..h {
        let s = (from.1 + row) * src_stride + from.0 * 4;
        let d = (y + row) * dst_stride + x * 4;
        dst.get_mut(d..d + w * 4)
            .context("region outside surface")?
            .copy_from_slice(src.get(s..s + w * 4).context("region outside picture")?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        // Luma ramps horizontally; neutral chroma → greys.
        let y = (0..h).flat_map(|_| (0..w).map(|x| (x * 10) as u8)).collect();
        let c = vec![128u8; w.div_ceil(2) * h.div_ceil(2)];
        (y, c.clone(), c)
    }

    fn check_regions(yuv: &Yuv<'_>, w: usize, h: usize) {
        let stride = w * 4;
        let mut dst = vec![7u8; stride * h];
        let mut scratch = Vec::new();

        // Odd region (1,1) 3×2: only those pixels change, others stay 7.
        convert_region(yuv, &mut dst, stride, (1, 1, 3, 2), &mut scratch).unwrap();
        let px = |dst: &[u8], x: usize, y: usize| dst[y * stride + x * 4];
        assert_eq!(px(&dst, 0, 1), 7);
        assert_eq!(px(&dst, 4, 1), 7);
        assert_eq!(px(&dst, 1, 0), 7);
        assert_eq!(px(&dst, 1, 3), 7);
        assert_eq!(px(&dst, 1, 1), 10, "full-range grey equals luma");
        assert_eq!(px(&dst, 3, 2), 30);

        // Aligned region converts in place.
        convert_region(yuv, &mut dst, stride, (4, 2, 4, 2), &mut scratch).unwrap();
        assert_eq!(px(&dst, 7, 3), 70);
        assert_eq!(dst[3 * stride + 7 * 4 + 3], 255, "opaque alpha");
    }

    #[test]
    fn converts_aligned_and_odd_regions_exactly() {
        let (w, h) = (8, 4);
        let (y, u, v) = picture(w, h);
        let yuv = Yuv {
            y: &y,
            y_stride: w,
            chroma: Chroma::Planar {
                u: &u,
                v: &v,
                u_stride: w / 2,
                v_stride: w / 2,
            },
            width: w,
            height: h,
        };
        check_regions(&yuv, w, h);
    }

    #[test]
    fn converts_nv12_like_i420() {
        let (w, h) = (8, 4);
        let (y, _, _) = picture(w, h);
        let uv = vec![128u8; w * h / 2];
        let yuv = Yuv {
            y: &y,
            y_stride: w,
            chroma: Chroma::Interleaved { uv: &uv, stride: w },
            width: w,
            height: h,
        };
        check_regions(&yuv, w, h);
        assert_eq!(yuv.v_plane().at(1, 0), Some(128));
    }
}
