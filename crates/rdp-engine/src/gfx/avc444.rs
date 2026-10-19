//! H.264 4:4:4 (AVC444 / AVC444v2): rebuilds full-resolution chroma from two 4:2:0
//! streams, per MS-RDPEGFX 3.3.8.3.2 (YUV444) and 3.3.8.3.3 (YUV444v2).
//!
//! The *main view* is an ordinary 4:2:0 picture (luma plus 2×2-filtered chroma). The
//! *auxiliary view* carries the chroma samples the main view dropped, packed into the
//! Y, U and V planes of a second 4:2:0 picture:
//!
//! - v1: odd rows of U/V in aux Y (8 lines of U, then 8 of V, per 16-line macroblock
//!   row); odd columns of even rows in aux U / aux V.
//! - v2: odd columns in the left (U) and right (V) halves of aux Y; even columns of
//!   odd rows in aux U (x % 4 == 0) and aux V (x % 4 == 2), U left and V right.
//! - Both: the even/even sample comes from the main view through the reverse filter.

use anyhow::Context as _;

use super::h264::{Plane, Yuv};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    V1,
    V2,
}

/// A surface's last main view (luma and filtered half-resolution chroma), kept so a
/// later chroma-only update can be combined with it.
pub(crate) struct MainView {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl MainView {
    /// A blank view (black, neutral chroma) covering `width × height`.
    pub fn new(width: usize, height: usize) -> Self {
        let (w, h) = (align16(width), align16(height));
        Self {
            width: w,
            height: h,
            y: vec![0; w * h],
            u: vec![128; (w / 2) * (h / 2)],
            v: vec![128; (w / 2) * (h / 2)],
        }
    }

    pub fn fits(&self, width: usize, height: usize) -> bool {
        self.width == align16(width) && self.height == align16(height)
    }

    /// Stores the part of a decoded main view under `rect` (expanded to even bounds).
    pub fn update(&mut self, pic: &Yuv<'_>, (x, y, w, h): (usize, usize, usize, usize)) {
        let (x0, y0) = (x & !1, y & !1);
        let x1 = ((x + w + 1) & !1).min(self.width).min(pic.width.next_multiple_of(2));
        let y1 = ((y + h + 1) & !1).min(self.height).min(pic.height.next_multiple_of(2));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let src = pic.y_plane();
        for row in y0..y1 {
            for col in x0..x1 {
                self.y[row * self.width + col] = src.at(col, row).unwrap_or(0);
            }
        }
        let (su, sv) = (pic.u_plane(), pic.v_plane());
        let cw = self.width / 2;
        for row in y0 / 2..y1 / 2 {
            for col in x0 / 2..x1 / 2 {
                self.u[row * cw + col] = su.at(col, row).unwrap_or(128);
                self.v[row * cw + col] = sv.at(col, row).unwrap_or(128);
            }
        }
    }
}

pub(crate) fn align16(v: usize) -> usize {
    v.next_multiple_of(16)
}

/// Combines the stored main view with a decoded auxiliary view and converts `rect`
/// (clipped to the surface) to RGBA in `dst`.
pub(crate) fn convert_region(
    main: &MainView,
    aux: &Yuv<'_>,
    layout: Layout,
    dst: &mut [u8],
    dst_stride: usize,
    (x, y, w, h): (usize, usize, usize, usize),
    scratch: &mut Scratch,
) -> anyhow::Result<()> {
    if w == 0 || h == 0 {
        return Ok(());
    }
    // Work on whole 2×2 blocks (the reverse filter needs all four samples).
    let (bx, by) = (x & !1, y & !1);
    let bx1 = ((x + w + 1) & !1).min(main.width);
    let by1 = ((y + h + 1) & !1).min(main.height);
    if bx >= bx1 || by >= by1 {
        return Ok(());
    }
    let (bw, bh) = (bx1 - bx, by1 - by);
    scratch.u.resize(bw * bh, 0);
    scratch.v.resize(bw * bh, 0);

    let (ay, au, av) = (aux.y_plane(), aux.u_plane(), aux.v_plane());
    // The packing is defined on the codec's macroblock-aligned frame.
    let fw = main.width;
    let cw = fw / 2;
    for j in by / 2..by1 / 2 {
        let (row0, row1) = (2 * j, 2 * j + 1);
        for i in bx / 2..bx1 / 2 {
            let (col0, col1) = (2 * i, 2 * i + 1);
            // [odd column, even row], [even column, odd row], [odd column, odd row]
            let (u, v) = match layout {
                Layout::V1 => {
                    // B4/B5: odd rows, 8 lines of U then 8 of V per 16-line macroblock row.
                    let mb = row1 / 16 * 16;
                    let k = (row1 % 16) / 2;
                    let (ur, vr) = (mb + k, mb + 8 + k);
                    (
                        [au.at(i, j), ay.at(col0, ur), ay.at(col1, ur)],
                        [av.at(i, j), ay.at(col0, vr), ay.at(col1, vr)],
                    )
                }
                Layout::V2 => {
                    // B4/B5: odd columns in the left (U) and right (V) halves of aux Y.
                    // B6..B9: even columns of odd rows, split by x % 4 between aux U and V.
                    let q = col0 / 4;
                    let plane: &Plane<'_> = if col0 % 4 == 0 { &au } else { &av };
                    (
                        [ay.at(i, row0), plane.at(q, j), ay.at(i, row1)],
                        [ay.at(cw + i, row0), plane.at(fw / 4 + q, j), ay.at(cw + i, row1)],
                    )
                }
            };
            let main_u = main.u[j * cw + i];
            let main_v = main.v[j * cw + i];
            let (lx, ly) = (col0 - bx, row0 - by);
            put(&mut scratch.u, bw, lx, ly, main_u, u);
            put(&mut scratch.v, bw, lx, ly, main_v, v);
        }
    }

    let img = yuv::YuvPlanarImage {
        // Exactly `bh` rows: the converter finds the last row from the slice length.
        y_plane: &main.y[by * main.width + bx..((by + bh) * main.width).min(main.y.len())],
        y_stride: main.width as u32,
        u_plane: &scratch.u,
        u_stride: bw as u32,
        v_plane: &scratch.v,
        v_stride: bw as u32,
        width: bw as u32,
        height: bh as u32,
    };
    let start = y * dst_stride + x * 4;
    if (bx, by, bw, bh) == (x, y, w, h) && dst.len() >= start + h * dst_stride {
        return yuv::yuv444_to_rgba(
            &img,
            &mut dst[start..],
            dst_stride as u32,
            yuv::YuvRange::Full,
            yuv::YuvStandardMatrix::Bt709,
        )
        .context("YUV444→RGBA");
    }
    let sstride = bw * 4;
    scratch.rgba.resize(sstride * bh, 0);
    yuv::yuv444_to_rgba(
        &img,
        &mut scratch.rgba,
        sstride as u32,
        yuv::YuvRange::Full,
        yuv::YuvStandardMatrix::Bt709,
    )
    .context("YUV444→RGBA")?;
    let w = w.min(bx1 - x);
    let h = h.min(by1 - y);
    super::h264::copy_rows(&scratch.rgba, sstride, (x - bx, y - by), dst, dst_stride, (x, y, w, h))
}

/// Writes one 2×2 chroma block. `others` are the [odd column, even row], [even column,
/// odd row] and [odd column, odd row] samples; the even/even sample is recovered from
/// the main view's filtered value (`filtered` = average of all four).
#[inline]
fn put(plane: &mut [u8], stride: usize, x: usize, y: usize, filtered: u8, others: [Option<u8>; 3]) {
    let [a, b, c] = others.map(|s| s.unwrap_or(filtered));
    let reversed = (4 * i32::from(filtered) - i32::from(a) - i32::from(b) - i32::from(c)).clamp(0, 255);
    // Quantization noise makes small corrections unreliable: keep the filtered value
    // unless the reverse filter changes it by more than 30 (MS-RDPEGFX threshold).
    let even = if (reversed - i32::from(filtered)).abs() > 30 {
        reversed as u8
    } else {
        filtered
    };
    plane[y * stride + x] = even;
    plane[y * stride + x + 1] = a;
    plane[(y + 1) * stride + x] = b;
    plane[(y + 1) * stride + x + 1] = c;
}

#[derive(Default)]
pub(crate) struct Scratch {
    u: Vec<u8>,
    v: Vec<u8>,
    rgba: Vec<u8>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::gfx::h264::Chroma;

    /// A 4:2:0 picture with owned planes.
    pub struct Planes {
        pub w: usize,
        pub h: usize,
        pub y: Vec<u8>,
        pub u: Vec<u8>,
        pub v: Vec<u8>,
    }

    impl Planes {
        pub fn yuv(&self) -> Yuv<'_> {
            Yuv {
                y: &self.y,
                y_stride: self.w,
                chroma: Chroma::Planar {
                    u: &self.u,
                    v: &self.v,
                    u_stride: self.w / 2,
                    v_stride: self.w / 2,
                },
                width: self.w,
                height: self.h,
            }
        }
    }

    /// Packs full-resolution planes into main and auxiliary views exactly as the
    /// forward equations of MS-RDPEGFX 3.3.8.3.2 / 3.3.8.3.3 describe (areas B1…B9).
    /// `w` and `h` must be multiples of 16.
    pub fn pack(y444: &[u8], u444: &[u8], v444: &[u8], w: usize, h: usize, layout: Layout) -> (Planes, Planes) {
        let at = |p: &[u8], x: usize, y: usize| p[y * w + x];
        let avg = |p: &[u8], x: usize, y: usize| {
            let s = u32::from(at(p, x, y)) + u32::from(at(p, x + 1, y)) + u32::from(at(p, x, y + 1));
            ((s + u32::from(at(p, x + 1, y + 1)) + 2) / 4) as u8
        };
        let (cw, ch) = (w / 2, h / 2);
        let mut main = Planes {
            w,
            h,
            y: y444.to_vec(),
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
        };
        let mut aux = Planes {
            w,
            h,
            y: vec![0; w * h],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
        };
        for y in 0..ch {
            for x in 0..cw {
                // B2, B3
                main.u[y * cw + x] = avg(u444, 2 * x, 2 * y);
                main.v[y * cw + x] = avg(v444, 2 * x, 2 * y);
            }
        }
        match layout {
            Layout::V1 => {
                for mb in (0..h).step_by(16) {
                    for yy in 0..8 {
                        for x in 0..w {
                            // B4: Y420(x, y) = U444(x, 2y + 1); B5: Y420(x, 8 + y) = V444(x, 2y + 1)
                            aux.y[(mb + yy) * w + x] = at(u444, x, mb + 2 * yy + 1);
                            aux.y[(mb + 8 + yy) * w + x] = at(v444, x, mb + 2 * yy + 1);
                        }
                    }
                }
                for y in 0..ch {
                    for x in 0..cw {
                        // B6: U420(x, y) = U444(2x + 1, 2y); B7: V420(x, y) = V444(2x + 1, 2y)
                        aux.u[y * cw + x] = at(u444, 2 * x + 1, 2 * y);
                        aux.v[y * cw + x] = at(v444, 2 * x + 1, 2 * y);
                    }
                }
            }
            Layout::V2 => {
                for y in 0..h {
                    for x in 0..cw {
                        // B4: Y420(x, y) = U444(2x + 1, y); B5: Y420(W/2 + x, y) = V444(2x + 1, y)
                        aux.y[y * w + x] = at(u444, 2 * x + 1, y);
                        aux.y[y * w + cw + x] = at(v444, 2 * x + 1, y);
                    }
                }
                for y in 0..ch {
                    for x in 0..w / 4 {
                        // B6/B7: U420(x, y) = U444(4x, 2y + 1); U420(W/4 + x, y) = V444(4x, 2y + 1)
                        aux.u[y * cw + x] = at(u444, 4 * x, 2 * y + 1);
                        aux.u[y * cw + w / 4 + x] = at(v444, 4 * x, 2 * y + 1);
                        // B8/B9: V420(x, y) = U444(4x + 2, 2y + 1); V420(W/4 + x, y) = V444(4x + 2, 2y + 1)
                        aux.v[y * cw + x] = at(u444, 4 * x + 2, 2 * y + 1);
                        aux.v[y * cw + w / 4 + x] = at(v444, 4 * x + 2, 2 * y + 1);
                    }
                }
            }
        }
        (main, aux)
    }

    /// Chroma with sharp single-pixel detail (what 4:2:0 cannot keep) plus a smooth part.
    pub fn sample_planes(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut y = vec![0; w * h];
        let mut u = vec![0; w * h];
        let mut v = vec![0; w * h];
        for row in 0..h {
            for col in 0..w {
                let i = row * w + col;
                y[i] = ((col * 7 + row * 3) % 256) as u8;
                if col < w / 2 {
                    // 1-pixel checkerboard of strongly different chroma.
                    let on = (col + row) % 2 == 0;
                    u[i] = if on { 40 } else { 220 };
                    v[i] = if on { 200 } else { 60 };
                } else {
                    // Gentle gradient: the reverse filter correction stays under the threshold.
                    u[i] = (100 + col % 16) as u8;
                    v[i] = (150 - row % 16) as u8;
                }
            }
        }
        (y, u, v)
    }

    fn rgba_of(y: &[u8], u: &[u8], v: &[u8], w: usize, h: usize) -> Vec<u8> {
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

    fn roundtrip(layout: Layout) {
        let (w, h) = (64, 32);
        let (y, u, v) = sample_planes(w, h);
        let (main_p, aux_p) = pack(&y, &u, &v, w, h, layout);
        let mut main = MainView::new(w, h);
        main.update(&main_p.yuv(), (0, 0, w, h));

        let expected = rgba_of(&y, &u, &v, w, h);
        let mut out = vec![0u8; w * h * 4];
        let mut scratch = Scratch::default();
        // Whole frame, then an odd-aligned rect on top: both must match the source.
        convert_region(&main, &aux_p.yuv(), layout, &mut out, w * 4, (0, 0, w, h), &mut scratch).unwrap();
        out.fill(0);
        convert_region(
            &main,
            &aux_p.yuv(),
            layout,
            &mut out,
            w * 4,
            (3, 5, 40, 17),
            &mut scratch,
        )
        .unwrap();
        for row in 0..h {
            for col in 0..w {
                let i = (row * w + col) * 4;
                let inside = (3..43).contains(&col) && (5..22).contains(&row);
                if !inside {
                    assert_eq!(
                        out[i..i + 4],
                        [0, 0, 0, 0],
                        "pixel ({col},{row}) outside the rect was written"
                    );
                    continue;
                }
                for c in 0..3 {
                    let d = (i32::from(out[i + c]) - i32::from(expected[i + c])).abs();
                    // The checkerboard is recovered exactly through the reverse filter;
                    // the gradient keeps the (rounded) filtered value for one sample in four.
                    let limit = if col < w / 2 { 2 } else { 12 };
                    assert!(
                        d <= limit,
                        "{layout:?} ({col},{row}) channel {c}: got {} want {}",
                        out[i + c],
                        expected[i + c]
                    );
                }
            }
        }
    }

    #[test]
    fn v1_reconstructs_full_chroma() {
        roundtrip(Layout::V1);
    }

    #[test]
    fn v2_reconstructs_full_chroma() {
        roundtrip(Layout::V2);
    }

    #[test]
    fn main_view_alone_loses_the_checkerboard() {
        // Sanity check of the test pattern: plain 4:2:0 cannot reproduce it.
        let (w, h) = (64, 32);
        let (y, u, v) = sample_planes(w, h);
        let (main_p, _) = pack(&y, &u, &v, w, h, Layout::V1);
        let mut out = vec![0u8; w * h * 4];
        crate::gfx::h264::convert_region(&main_p.yuv(), &mut out, w * 4, (0, 0, w, h), &mut Vec::new()).unwrap();
        let expected = rgba_of(&y, &u, &v, w, h);
        let worst = (0..h * (w / 2))
            .map(|k| (k / (w / 2) * w + k % (w / 2)) * 4)
            .map(|i| (i32::from(out[i]) - i32::from(expected[i])).abs())
            .max()
            .unwrap();
        assert!(
            worst > 60,
            "4:2:0 unexpectedly kept single-pixel chroma (max error {worst})"
        );
    }
}
