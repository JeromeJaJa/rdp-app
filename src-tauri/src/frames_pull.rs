//! Frames for webviews without shared buffers (WKWebView on macOS, WebKitGTK on Linux).
//!
//! The engine writes into ordinary memory laid out as `rdp_engine::surface` describes, and the
//! page pulls what changed over the `rdpframe:` URI scheme: one request per surface at a time,
//! answered with the rectangles changed since the page's last sequence number (or the whole
//! frame when it's behind). A request made while nothing has changed waits here, so an idle
//! session costs a sleeping request, not one per display frame.
//!
//! This trades the Windows path's zero-copy (WebView2 shared buffers, `frames.rs`) for one
//! request and one copy per changed region. Drawing sessions natively behind the page would
//! remove that, and is the next step for macOS.
//!
//! Plain Rust, built on every platform: Windows runs it too with `RDP_FRAMES=pull`, to test
//! the page's side against the real app.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rdp_engine::surface::{HeapSurface, MAGIC, PIXEL_OFFSET, RING_OFFSET, SurfaceKind, surface_len};
use rdp_engine::{SurfaceMemory, SurfaceProvider};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::frames_host::{FrameHost, MAIN_WINDOW};

/// The URI scheme pages pull frames from.
pub const SCHEME: &str = "rdpframe";
/// How often a waiting request looks for a change.
const POLL: Duration = Duration::from_millis(4);
/// How long a request waits for a change before answering "nothing new".
const HOLD: Duration = Duration::from_millis(500);
/// Above this many changed rects, send their bounding box instead.
const MAX_RECTS: u32 = 24;
/// Above this many, send the whole frame.
const MAX_BOX: u32 = 96;
/// What a page sends as `since` before it has anything.
const NOTHING_YET: u32 = u32::MAX;

/// Header of a response: magic, width, height, seq, rect count, flags (u32 each, little-endian),
/// then `count` rects of { u16 x, y, w, h }, then each rect's pixels (RGBA, rows packed).
pub const RESPONSE_HEADER: usize = 24;
/// Response flag: the rects cover the whole frame (the page was behind or new).
pub const FLAG_FULL: u32 = 1;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SurfaceReady {
    session: String,
    kind: &'static str,
    width: u32,
    height: u32,
    generation: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SurfaceReleased {
    session: String,
    kind: &'static str,
    generation: u64,
}

struct Entry {
    memory: Arc<HeapSurface>,
    generation: u64,
    width: u32,
    height: u32,
    /// Window showing it.
    window: String,
}

pub struct PulledSurfaces {
    app: AppHandle,
    next_generation: AtomicU64,
    surfaces: Mutex<HashMap<(String, SurfaceKind), Entry>>,
    /// Window showing each session's full surface (absent = the main window).
    hosts: Mutex<HashMap<String, String>>,
}

static ACTIVE: std::sync::OnceLock<Arc<PulledSurfaces>> = std::sync::OnceLock::new();

impl PulledSurfaces {
    /// The provider, made the one the `rdpframe:` scheme serves.
    pub fn install(app: AppHandle) -> Arc<Self> {
        let me = Arc::new(Self {
            app,
            next_generation: AtomicU64::new(1),
            surfaces: Mutex::new(HashMap::new()),
            hosts: Mutex::new(HashMap::new()),
        });
        let _ = ACTIVE.set(Arc::clone(&me));
        me
    }

    fn window_for(&self, session: &str, kind: SurfaceKind) -> String {
        match kind {
            SurfaceKind::Full => self
                .hosts
                .lock()
                .get(session)
                .cloned()
                .unwrap_or_else(|| MAIN_WINDOW.to_owned()),
            SurfaceKind::Thumb => MAIN_WINDOW.to_owned(),
        }
    }

    fn ready(&self, session: &str, kind: SurfaceKind, entry: &Entry) {
        let _ = self.app.emit_to(
            entry.window.as_str(),
            "surface-ready",
            SurfaceReady {
                session: session.to_owned(),
                kind: kind.as_str(),
                width: entry.width,
                height: entry.height,
                generation: entry.generation,
            },
        );
    }

    fn released(&self, window: &str, session: &str, kind: SurfaceKind, generation: u64) {
        let _ = self.app.emit_to(
            window,
            "surface-released",
            SurfaceReleased {
                session: session.to_owned(),
                kind: kind.as_str(),
                generation,
            },
        );
    }
}

impl FrameHost for PulledSurfaces {
    fn repost_all(&self, label: &str) -> anyhow::Result<()> {
        let surfaces = self.surfaces.lock();
        for ((session, kind), entry) in surfaces.iter() {
            if entry.window == label {
                self.ready(session, *kind, entry);
            }
        }
        Ok(())
    }

    fn set_host(&self, session: &str, label: &str) {
        {
            let mut hosts = self.hosts.lock();
            if label == MAIN_WINDOW {
                hosts.remove(session);
            } else {
                hosts.insert(session.to_owned(), label.to_owned());
            }
        }
        let mut surfaces = self.surfaces.lock();
        if let Some(entry) = surfaces.get_mut(&(session.to_owned(), SurfaceKind::Full))
            && entry.window != label
        {
            let old = std::mem::replace(&mut entry.window, label.to_owned());
            self.released(&old, session, SurfaceKind::Full, entry.generation);
            self.ready(session, SurfaceKind::Full, entry);
        }
    }
}

impl SurfaceProvider for PulledSurfaces {
    fn allocate(
        &self,
        session: &str,
        kind: SurfaceKind,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Arc<dyn SurfaceMemory>> {
        let memory = Arc::new(HeapSurface::new(surface_len(width, height)));
        let entry = Entry {
            memory: Arc::clone(&memory),
            generation: self.next_generation.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            window: self.window_for(session, kind),
        };
        self.ready(session, kind, &entry);
        self.surfaces.lock().insert((session.to_owned(), kind), entry);
        Ok(memory)
    }

    fn release(&self, session: &str, kind: SurfaceKind) {
        if let Some(entry) = self.surfaces.lock().remove(&(session.to_owned(), kind)) {
            self.released(&entry.window, session, kind, entry.generation);
        }
    }
}

/// Answers a page's request for what changed in one surface:
/// `rdpframe://localhost/<kind>/<session>/<generation>?since=<seq>`.
pub async fn serve(request: tauri::http::Request<Vec<u8>>) -> tauri::http::Response<Vec<u8>> {
    let response = |status: u16, body: Vec<u8>| {
        tauri::http::Response::builder()
            .status(status)
            .header("Access-Control-Allow-Origin", "*")
            .header("Content-Type", "application/octet-stream")
            .header("Cache-Control", "no-store")
            .body(body)
            .unwrap_or_default()
    };
    let Some(active) = ACTIVE.get() else {
        return response(404, Vec::new());
    };
    let Some((kind, session, generation, since)) = parse(request.uri()) else {
        return response(400, Vec::new());
    };
    let memory = {
        let surfaces = active.surfaces.lock();
        match surfaces.get(&(session, kind)) {
            Some(entry) if entry.generation == generation => Arc::clone(&entry.memory),
            // Released, or replaced by a newer one: the page lets go of it.
            _ => return response(410, Vec::new()),
        }
    };
    // Nothing new: wait a little for something before answering.
    let started = Instant::now();
    while since != NOTHING_YET && seq_of(&memory) == since {
        if started.elapsed() >= HOLD {
            return response(204, Vec::new());
        }
        tokio::time::sleep(POLL).await;
    }
    response(200, encode(&memory, since))
}

fn parse(uri: &tauri::http::Uri) -> Option<(SurfaceKind, String, u64, u32)> {
    let mut parts = uri.path().trim_start_matches('/').split('/');
    let kind = match parts.next()? {
        "full" => SurfaceKind::Full,
        "thumb" => SurfaceKind::Thumb,
        _ => return None,
    };
    let session = parts.next()?.to_owned();
    let generation = parts.next()?.parse().ok()?;
    let since = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("since=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or(NOTHING_YET);
    Some((kind, session, generation, since))
}

fn header(memory: &HeapSurface, index: usize) -> u32 {
    let b = memory.bytes();
    u32::from_le_bytes(b[index * 4..index * 4 + 4].try_into().unwrap_or_default())
}

fn seq_of(memory: &HeapSurface) -> u32 {
    // SAFETY: the header is 8-byte aligned memory owned by `memory`; `seq` (offset 20) is
    // published with release ordering by the writer.
    unsafe { (*(memory.as_ptr().add(20) as *const AtomicU32)).load(Ordering::Acquire) }
}

/// The rects changed since `since` (coalesced like the page's painter does), with their
/// pixels. Rects are clipped to the frame.
pub(crate) fn encode(memory: &HeapSurface, since: u32) -> Vec<u8> {
    let (width, height, stride, cap) = (
        header(memory, 2),
        header(memory, 3),
        header(memory, 4),
        header(memory, 6),
    );
    let seq = seq_of(memory);
    let pending = if since == NOTHING_YET {
        u32::MAX
    } else {
        seq.wrapping_sub(since)
    };
    let full = (0, 0, width, height);
    let rects: Vec<(u32, u32, u32, u32)> = if magic_ok(memory) && cap > 0 && pending <= cap && pending <= MAX_BOX {
        let bytes = memory.bytes();
        let mut list = Vec::new();
        let mut i = since;
        while i != seq {
            let o = RING_OFFSET + (i % cap) as usize * 8;
            let at = |k: usize| u32::from(u16::from_le_bytes([bytes[o + k * 2], bytes[o + k * 2 + 1]]));
            let (x, y, w, h) = (at(0), at(1), at(2), at(3));
            let (w, h) = (w.min(width.saturating_sub(x)), h.min(height.saturating_sub(y)));
            if w > 0 && h > 0 {
                list.push((x, y, w, h));
            }
            i = i.wrapping_add(1);
        }
        if pending > MAX_RECTS && !list.is_empty() {
            let x0 = list.iter().map(|r| r.0).min().unwrap_or(0);
            let y0 = list.iter().map(|r| r.1).min().unwrap_or(0);
            let x1 = list.iter().map(|r| r.0 + r.2).max().unwrap_or(0);
            let y1 = list.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
            vec![(x0, y0, x1 - x0, y1 - y0)]
        } else {
            list
        }
    } else {
        vec![full]
    };
    let is_full = rects.len() == 1 && rects[0] == full;
    let pixels: usize = rects.iter().map(|r| (r.2 * r.3 * 4) as usize).sum();
    let mut out = Vec::with_capacity(RESPONSE_HEADER + rects.len() * 8 + pixels);
    for v in [
        MAGIC,
        width,
        height,
        seq,
        rects.len() as u32,
        if is_full { FLAG_FULL } else { 0 },
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for &(x, y, w, h) in &rects {
        for v in [x, y, w, h] {
            out.extend_from_slice(&(v as u16).to_le_bytes());
        }
    }
    let bytes = memory.bytes();
    for &(x, y, w, h) in &rects {
        for row in y..y + h {
            let start = PIXEL_OFFSET + row as usize * stride as usize + x as usize * 4;
            out.extend_from_slice(&bytes[start..start + w as usize * 4]);
        }
    }
    out
}

fn magic_ok(memory: &HeapSurface) -> bool {
    header(memory, 0) == MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface as the engine leaves it: header, a ring of rects, pixels.
    fn surface(width: u32, height: u32, rects: &[(u16, u16, u16, u16)]) -> HeapSurface {
        let mem = HeapSurface::new(surface_len(width, height));
        let p = mem.as_ptr();
        let put =
            |off: usize, v: u32| unsafe { std::ptr::copy_nonoverlapping(v.to_le_bytes().as_ptr(), p.add(off), 4) };
        put(0, MAGIC);
        put(8, width);
        put(12, height);
        put(16, width * 4);
        put(24, rdp_engine::surface::RING_CAP);
        for (i, &(x, y, w, h)) in rects.iter().enumerate() {
            let o = RING_OFFSET + i * 8;
            for (k, v) in [x, y, w, h].into_iter().enumerate() {
                unsafe { std::ptr::copy_nonoverlapping(v.to_le_bytes().as_ptr(), p.add(o + k * 2), 2) };
            }
        }
        put(20, rects.len() as u32);
        // Pixel (x, y) = [x, y, 7, 255] so a copy can be checked.
        for y in 0..height {
            for x in 0..width {
                let o = PIXEL_OFFSET + (y * width * 4 + x * 4) as usize;
                unsafe { std::ptr::copy_nonoverlapping([x as u8, y as u8, 7, 255].as_ptr(), p.add(o), 4) };
            }
        }
        mem
    }

    fn word(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap())
    }

    #[test]
    fn a_new_page_gets_the_whole_frame() {
        let mem = surface(8, 6, &[(1, 1, 2, 2)]);
        let out = encode(&mem, NOTHING_YET);
        assert_eq!(word(&out, 0), MAGIC);
        assert_eq!((word(&out, 1), word(&out, 2), word(&out, 3)), (8, 6, 1));
        assert_eq!((word(&out, 4), word(&out, 5)), (1, FLAG_FULL));
        assert_eq!(out.len(), RESPONSE_HEADER + 8 + 8 * 6 * 4);
    }

    #[test]
    fn a_page_that_is_current_gets_only_what_changed() {
        let mem = surface(8, 6, &[(0, 0, 8, 6), (2, 3, 2, 1), (5, 1, 1, 2)]);
        // Seen the first rect: the other two, with their pixels.
        let out = encode(&mem, 1);
        assert_eq!((word(&out, 3), word(&out, 4), word(&out, 5)), (3, 2, 0));
        let rect = |i: usize| -> Vec<u16> {
            (0..4)
                .map(|k| u16::from_le_bytes([out[24 + i * 8 + k * 2], out[24 + i * 8 + k * 2 + 1]]))
                .collect()
        };
        assert_eq!(rect(0), [2, 3, 2, 1]);
        assert_eq!(rect(1), [5, 1, 1, 2]);
        let px = RESPONSE_HEADER + 16;
        // First rect's pixels: (2,3) then (3,3); then the second's: (5,1), (5,2).
        assert_eq!(&out[px..px + 8], &[2, 3, 7, 255, 3, 3, 7, 255]);
        assert_eq!(&out[px + 8..px + 16], &[5, 1, 7, 255, 5, 2, 7, 255]);
        assert_eq!(out.len(), px + 16);
    }

    #[test]
    fn many_rects_become_their_bounding_box_and_rects_are_clipped() {
        let mut rects = vec![(0u16, 0u16, 1u16, 1u16); 30];
        rects.push((6, 4, 50, 50)); // runs off the frame
        let mem = surface(8, 6, &rects);
        let out = encode(&mem, 0);
        assert_eq!(word(&out, 4), 1);
        let r: Vec<u16> = (0..4)
            .map(|k| u16::from_le_bytes([out[24 + k * 2], out[25 + k * 2]]))
            .collect();
        assert_eq!(r, [0, 0, 8, 6]);
    }
}
