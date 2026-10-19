//! Remote pointer bitmaps → CSS `cursor` values.
//!
//! Using the browser's native cursor (instead of drawing it into the frame)
//! keeps pointer motion at zero latency regardless of network round-trips.

use std::collections::VecDeque;

use base64::Engine as _;
use ironrdp_graphics::pointer::DecodedPointer;

/// Chromium ignores cursor images larger than this.
const MAX_CSS_CURSOR: u16 = 128;
const CACHE_SIZE: usize = 32;

#[derive(Default)]
pub(crate) struct CursorCache {
    entries: VecDeque<(u64, String)>,
}

impl CursorCache {
    pub(crate) fn css_for(&mut self, pointer: &DecodedPointer) -> String {
        let key = fingerprint(pointer);
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            let entry = self.entries.remove(pos).expect("position is valid");
            let css = entry.1.clone();
            self.entries.push_front(entry);
            return css;
        }
        let css = encode(pointer).unwrap_or_else(|| "default".to_owned());
        self.entries.push_front((key, css.clone()));
        self.entries.truncate(CACHE_SIZE);
        css
    }
}

fn fingerprint(p: &DecodedPointer) -> u64 {
    // FNV-1a over geometry + pixels; pointers are tiny so this is cheap.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    for v in [p.width, p.height, p.hotspot_x, p.hotspot_y] {
        v.to_le_bytes().into_iter().for_each(&mut feed);
    }
    p.bitmap_data.iter().copied().for_each(feed);
    h
}

fn encode(p: &DecodedPointer) -> Option<String> {
    if p.width == 0 || p.height == 0 {
        return Some("none".to_owned());
    }
    let (w, h) = (p.width.min(MAX_CSS_CURSOR), p.height.min(MAX_CSS_CURSOR));
    let src_stride = usize::from(p.width) * 4;
    if p.bitmap_data.len() < src_stride * usize::from(p.height) {
        return None;
    }
    let mut rgba = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
    for y in 0..usize::from(h) {
        rgba.extend_from_slice(&p.bitmap_data[y * src_stride..y * src_stride + usize::from(w) * 4]);
    }
    if rgba.chunks_exact(4).all(|px| px[3] == 0) {
        return Some("none".to_owned());
    }

    let mut png_bytes = Vec::with_capacity(rgba.len() / 2);
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, u32::from(w), u32::from(h));
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&rgba).ok()?;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    let hx = p.hotspot_x.min(w.saturating_sub(1));
    let hy = p.hotspot_y.min(h.saturating_sub(1));
    Some(format!("url(\"data:image/png;base64,{b64}\") {hx} {hy}, default"))
}
