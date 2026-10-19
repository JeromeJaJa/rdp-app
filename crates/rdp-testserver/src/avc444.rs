//! Server side of H.264 4:4:4: splits a full-chroma frame into the two 4:2:0 views of
//! MS-RDPEGFX 3.3.8.3.2 (AVC444) / 3.3.8.3.3 (AVC444v2) and encodes each as its own
//! H.264 stream.

use openh264::OpenH264API;
use openh264::encoder::{Encoder, EncoderConfig, QpRange, UsageType};
use openh264::formats::YUVSlices;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    V1,
    V2,
}

/// A 4:2:0 picture (I420) with tight strides.
struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

pub struct Avc444Encoder {
    layout: Layout,
    main: Encoder,
    aux: Encoder,
}

impl Avc444Encoder {
    pub fn new(layout: Layout) -> anyhow::Result<Self> {
        let encoder = || {
            let config = EncoderConfig::new()
                .qp(QpRange::new(18, 28))
                .usage_type(UsageType::ScreenContentRealTime)
                .skip_frames(false);
            Encoder::with_api_config(OpenH264API::from_source(), config)
        };
        Ok(Self {
            layout,
            main: encoder()?,
            aux: encoder()?,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn request_key_frame(&mut self) {
        self.main.force_intra_frame();
        self.aux.force_intra_frame();
    }

    /// Encodes an RGBA frame; returns the (main, auxiliary) H.264 streams.
    pub fn encode(&mut self, rgba: &[u8], width: usize, height: usize) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        // The packing works on whole 16×16 macroblocks: pad by repeating edges.
        let (w, h) = (width.next_multiple_of(16), height.next_multiple_of(16));
        let (y444, u444, v444) = to_yuv444(rgba, width, height, w, h);
        let (main, aux) = pack(&y444, &u444, &v444, w, h, self.layout);
        let encode = |enc: &mut Encoder, p: &Planes| -> anyhow::Result<Vec<u8>> {
            let src = YUVSlices::new((&p.y, &p.u, &p.v), (w, h), (w, w / 2, w / 2));
            Ok(enc.encode(&src)?.to_vec())
        };
        Ok((encode(&mut self.main, &main)?, encode(&mut self.aux, &aux)?))
    }
}

/// Full-range BT.709 (MS-RDPEGFX 3.3.8.3.1), padded to `w × h` by edge repetition.
fn to_yuv444(rgba: &[u8], sw: usize, sh: usize, w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut y = vec![0; w * h];
    let mut u = vec![0; w * h];
    let mut v = vec![0; w * h];
    for row in 0..h {
        let sr = row.min(sh - 1);
        for col in 0..w {
            let sc = col.min(sw - 1);
            let i = (sr * sw + sc) * 4;
            let (r, g, b) = (f32::from(rgba[i]), f32::from(rgba[i + 1]), f32::from(rgba[i + 2]));
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let o = row * w + col;
            y[o] = luma.round().clamp(0.0, 255.0) as u8;
            u[o] = ((b - luma) / 1.8556 + 128.0).round().clamp(0.0, 255.0) as u8;
            v[o] = ((r - luma) / 1.5748 + 128.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    (y, u, v)
}

/// The forward equations (areas B1…B9) of the two layouts.
fn pack(y444: &[u8], u444: &[u8], v444: &[u8], w: usize, h: usize, layout: Layout) -> (Planes, Planes) {
    let at = |p: &[u8], x: usize, y: usize| p[y * w + x];
    let avg = |p: &[u8], x: usize, y: usize| {
        let s = u32::from(at(p, x, y)) + u32::from(at(p, x + 1, y)) + u32::from(at(p, x, y + 1));
        ((s + u32::from(at(p, x + 1, y + 1)) + 2) / 4) as u8
    };
    let (cw, ch) = (w / 2, h / 2);
    let mut main = Planes {
        y: y444.to_vec(),
        u: vec![0; cw * ch],
        v: vec![0; cw * ch],
    };
    let mut aux = Planes {
        y: vec![0; w * h],
        u: vec![0; cw * ch],
        v: vec![0; cw * ch],
    };
    for y in 0..ch {
        for x in 0..cw {
            main.u[y * cw + x] = avg(u444, 2 * x, 2 * y);
            main.v[y * cw + x] = avg(v444, 2 * x, 2 * y);
        }
    }
    match layout {
        Layout::V1 => {
            for mb in (0..h).step_by(16) {
                for k in 0..8 {
                    for x in 0..w {
                        aux.y[(mb + k) * w + x] = at(u444, x, mb + 2 * k + 1);
                        aux.y[(mb + 8 + k) * w + x] = at(v444, x, mb + 2 * k + 1);
                    }
                }
            }
            for y in 0..ch {
                for x in 0..cw {
                    aux.u[y * cw + x] = at(u444, 2 * x + 1, 2 * y);
                    aux.v[y * cw + x] = at(v444, 2 * x + 1, 2 * y);
                }
            }
        }
        Layout::V2 => {
            for y in 0..h {
                for x in 0..cw {
                    aux.y[y * w + x] = at(u444, 2 * x + 1, y);
                    aux.y[y * w + cw + x] = at(v444, 2 * x + 1, y);
                }
            }
            for y in 0..ch {
                for x in 0..w / 4 {
                    aux.u[y * cw + x] = at(u444, 4 * x, 2 * y + 1);
                    aux.u[y * cw + w / 4 + x] = at(v444, 4 * x, 2 * y + 1);
                    aux.v[y * cw + x] = at(u444, 4 * x + 2, 2 * y + 1);
                    aux.v[y * cw + w / 4 + x] = at(v444, 4 * x + 2, 2 * y + 1);
                }
            }
        }
    }
    (main, aux)
}
