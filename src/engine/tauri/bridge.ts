// Receives frame surfaces from Rust and keeps them indexed by session + kind. Memory layout:
// see `crates/rdp-engine/src/surface.rs`.
//
// On Windows they're WebView2 shared buffers: Rust writes, the page reads, nothing is sent.
// Elsewhere (WKWebView, WebKitGTK) there are none, so each surface is a local buffer with the
// same layout, kept current by pulling what changed from Rust (`frames_pull.rs`). Either way
// the painter just reads a buffer.

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

export const MAGIC = 0x46504452;
export const RING_OFFSET = 64;
export const PIXEL_OFFSET = 4096;

export type SurfaceKind = "full" | "thumb";

export interface Surface {
  session: string;
  kind: SurfaceKind;
  width: number;
  height: number;
  generation: number;
  buffer: ArrayBuffer;
  /** u32 header words: [magic, version, width, height, stride, seq, ringCap, pixelOffset] */
  header: Uint32Array;
  ring: Uint16Array;
  image: ImageData;
  /** Kept current by pulling from Rust (not a shared buffer). */
  pulled?: boolean;
}

interface SharedBufferMeta {
  session: string;
  kind: SurfaceKind;
  width: number;
  height: number;
  generation: number;
}

interface WebView2 {
  addEventListener(
    type: "sharedbufferreceived",
    cb: (e: { getBuffer(): ArrayBuffer; additionalData: SharedBufferMeta }) => void,
  ): void;
  releaseBuffer(buffer: ArrayBuffer): void;
}

const webview = (window as unknown as { chrome?: { webview?: WebView2 } }).chrome?.webview;

const surfaces = new Map<string, Surface>();
// Debug handle for devtools / automated checks.
if (import.meta.env.DEV) (window as unknown as { __rdpSurfaces?: Map<string, Surface> }).__rdpSurfaces = surfaces;
const listeners = new Map<string, Set<() => void>>();

const keyOf = (session: string, kind: SurfaceKind) => `${kind}|${session}`;

function notify(key: string) {
  listeners.get(key)?.forEach((cb) => cb());
}

function release(s: Pick<Surface, "buffer" | "pulled">) {
  if (s.pulled) return; // ours: garbage-collected
  try {
    webview?.releaseBuffer(s.buffer);
  } catch {
    // already released
  }
}

function adopt(meta: SharedBufferMeta, buffer: ArrayBuffer) {
  const key = keyOf(meta.session, meta.kind);
  const old = surfaces.get(key);
  if (old && old.generation > meta.generation) {
    release({ buffer }); // late duplicate of an older surface
    return;
  }
  const pixels = meta.width * meta.height * 4;
  if (buffer.byteLength < PIXEL_OFFSET + pixels) {
    release({ buffer });
    return;
  }
  if (old && old.buffer !== buffer) release(old);
  surfaces.set(key, {
    ...meta,
    buffer,
    header: new Uint32Array(buffer, 0, 8),
    ring: new Uint16Array(buffer, RING_OFFSET, (PIXEL_OFFSET - RING_OFFSET) / 2),
    image: new ImageData(new Uint8ClampedArray(buffer, PIXEL_OFFSET, pixels), meta.width, meta.height),
  });
  notify(key);
}

// ── Pulled surfaces (no shared buffers) ─────────────────────────────────────

/** Where Rust serves frames: custom schemes are http://<scheme>.localhost on Windows. */
const PULL_BASE = navigator.userAgent.includes("Windows") ? "http://rdpframe.localhost" : "rdpframe://localhost";
/** Response header: magic, width, height, seq, rect count, flags (u32 each). */
const PULL_HEADER = 24;
const RING_CAP = 448;

const nextFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));
const pause = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** A surface Rust announced: a local buffer with the shared layout, then kept current. */
function adoptPulled(meta: SharedBufferMeta) {
  const key = keyOf(meta.session, meta.kind);
  const old = surfaces.get(key);
  if (old && old.generation >= meta.generation) return; // this one, or newer, is here
  const pixels = meta.width * meta.height * 4;
  const buffer = new ArrayBuffer(PIXEL_OFFSET + pixels);
  const header = new Uint32Array(buffer, 0, 8);
  header.set([MAGIC, 1, meta.width, meta.height, meta.width * 4, 0, RING_CAP, PIXEL_OFFSET]);
  const s: Surface = {
    ...meta,
    buffer,
    header,
    ring: new Uint16Array(buffer, RING_OFFSET, (PIXEL_OFFSET - RING_OFFSET) / 2),
    image: new ImageData(new Uint8ClampedArray(buffer, PIXEL_OFFSET, pixels), meta.width, meta.height),
    pulled: true,
  };
  surfaces.set(key, s);
  notify(key);
  void pull(key, s);
}

/** Keeps `s` current while it's the surface for `key`: one request at a time, at most one
 *  update per display frame. Rust holds a request while nothing changes. */
async function pull(key: string, s: Surface) {
  const url = `${PULL_BASE}/${s.kind}/${encodeURIComponent(s.session)}/${s.generation}`;
  let since: number | null = null;
  while (surfaces.get(key) === s) {
    let res: Response;
    try {
      res = await fetch(since === null ? url : `${url}?since=${since}`, { cache: "no-store" });
    } catch {
      await pause(500);
      continue;
    }
    if (res.status === 410) {
      // Released or replaced on the Rust side.
      if (surfaces.get(key) === s) {
        surfaces.delete(key);
        notify(key);
      }
      return;
    }
    if (res.status === 204) continue; // nothing changed for a while
    if (!res.ok) {
      await pause(250);
      continue;
    }
    const data = await res.arrayBuffer();
    if (surfaces.get(key) !== s) return;
    const seq = applyPulled(s, data);
    if (seq !== null) since = seq;
    await nextFrame();
  }
}

/** Copies a response's rects into `s` and records them in its ring for the painter. Returns
 *  the sequence number it brings `s` to (null if it didn't fit this surface). */
function applyPulled(s: Surface, data: ArrayBuffer): number | null {
  const v = new DataView(data);
  if (data.byteLength < PULL_HEADER || v.getUint32(0, true) !== MAGIC) return null;
  if (v.getUint32(4, true) !== s.width || v.getUint32(8, true) !== s.height) return null;
  const seq = v.getUint32(12, true);
  const count = v.getUint32(16, true);
  const px = s.image.data;
  let at = PULL_HEADER + count * 8;
  for (let i = 0; i < count; i++) {
    const r = PULL_HEADER + i * 8;
    const x = v.getUint16(r, true), y = v.getUint16(r + 2, true), w = v.getUint16(r + 4, true), h = v.getUint16(r + 6, true);
    const row = w * 4;
    for (let line = 0; line < h; line++) {
      px.set(new Uint8Array(data, at, row), ((y + line) * s.width + x) * 4);
      at += row;
    }
    const local = s.header[5];
    s.ring.set([x, y, w, h], (local % RING_CAP) * 4);
    s.header[5] = (local + 1) >>> 0;
  }
  return seq;
}

let initialized = false;

export function initBridge() {
  if (initialized || !("__TAURI_INTERNALS__" in window)) return;
  initialized = true;

  // WebView2 hands over shared buffers; elsewhere (or with RDP_FRAMES=pull) Rust says a
  // surface is ready and the page pulls it.
  webview?.addEventListener("sharedbufferreceived", (e) => adopt(e.additionalData, e.getBuffer()));
  getCurrentWebviewWindow().listen<SharedBufferMeta>("surface-ready", (e) => adoptPulled(e.payload));

  // Listened on this window only: a release can be meant for another window (the
  // surface moved there along with its tab).
  getCurrentWebviewWindow().listen<{ session: string; kind: SurfaceKind; generation: number }>("surface-released", (e) => {
    const key = keyOf(e.payload.session, e.payload.kind);
    const s = surfaces.get(key);
    if (s && s.generation === e.payload.generation) {
      surfaces.delete(key);
      release(s);
      notify(key);
    }
  });

  // After a page reload the buffers still exist on the Rust side; ask for them again.
  invoke("engine_repost_surfaces").catch(() => {});
}

export function getSurface(session: string, kind: SurfaceKind): Surface | undefined {
  return surfaces.get(keyOf(session, kind));
}

export function subscribeSurface(session: string, kind: SurfaceKind, cb: () => void): () => void {
  const key = keyOf(session, kind);
  let set = listeners.get(key);
  if (!set) listeners.set(key, (set = new Set()));
  set.add(cb);
  return () => {
    set!.delete(cb);
    if (set!.size === 0) listeners.delete(key);
  };
}
