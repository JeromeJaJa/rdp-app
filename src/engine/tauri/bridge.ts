// Receives frame surfaces (WebView2 shared buffers) from Rust and keeps them
// indexed by session + kind. Memory layout: see `crates/rdp-engine/src/surface.rs`.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

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

function release(buffer: ArrayBuffer) {
  try {
    webview?.releaseBuffer(buffer);
  } catch {
    // already released
  }
}

function adopt(meta: SharedBufferMeta, buffer: ArrayBuffer) {
  const key = keyOf(meta.session, meta.kind);
  const old = surfaces.get(key);
  if (old && old.generation > meta.generation) {
    release(buffer); // late duplicate of an older surface
    return;
  }
  const pixels = meta.width * meta.height * 4;
  if (buffer.byteLength < PIXEL_OFFSET + pixels) {
    release(buffer);
    return;
  }
  if (old && old.buffer !== buffer) release(old.buffer);
  surfaces.set(key, {
    ...meta,
    buffer,
    header: new Uint32Array(buffer, 0, 8),
    ring: new Uint16Array(buffer, RING_OFFSET, (PIXEL_OFFSET - RING_OFFSET) / 2),
    image: new ImageData(new Uint8ClampedArray(buffer, PIXEL_OFFSET, pixels), meta.width, meta.height),
  });
  notify(key);
}

let initialized = false;

export function initBridge() {
  if (initialized || !webview) return;
  initialized = true;

  webview.addEventListener("sharedbufferreceived", (e) => adopt(e.additionalData, e.getBuffer()));

  listen<{ session: string; kind: SurfaceKind; generation: number }>("surface-released", (e) => {
    const key = keyOf(e.payload.session, e.payload.kind);
    const s = surfaces.get(key);
    if (s && s.generation === e.payload.generation) {
      surfaces.delete(key);
      release(s.buffer);
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
