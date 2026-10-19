// Mock engine: fake live desktops drawn on canvas so the UI can be developed and
// reviewed in a plain browser. One shared rAF loop drives every mounted canvas;
// off-screen canvases are skipped, thumbnails tick at ~4 fps.

import { useEffect, useRef, useSyncExternalStore } from "react";
import type { Engine, EngineModule, RemoteSurfaceProps, RemoteThumbnailProps } from "../types";
import { drawDesktop } from "./draw";
import "./mock.css";

interface Entry {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  id: string;
  interval: number;
  visible: boolean;
  last: number;
}

const entries = new Set<Entry>();
let raf = 0;

function loop(t: number) {
  raf = 0;
  for (const e of entries) {
    if (!e.visible || t - e.last < e.interval) continue;
    e.last = t;
    drawDesktop(e.ctx, e.id, e.canvas.width, e.canvas.height, t);
  }
  if (entries.size) raf = requestAnimationFrame(loop);
}

function register(e: Entry): () => void {
  entries.add(e);
  if (!raf) raf = requestAnimationFrame(loop);
  return () => {
    entries.delete(e);
  };
}

// Fake remote resolution: follows the viewport with a resize delay, like a real
// server would, so letterboxing during window resizes looks realistic.
let remote = { w: 1920, h: 1080 };
const remoteListeners = new Set<() => void>();
let resizeTimer: ReturnType<typeof setTimeout> | undefined;
function subscribeRemote(cb: () => void) {
  remoteListeners.add(cb);
  return () => remoteListeners.delete(cb);
}
const getRemote = () => remote;

const engine: Engine = {
  kind: "mock",
  setViewport(width, height, dpr) {
    const w = Math.max(640, Math.round((width * dpr) / 2) * 2);
    const h = Math.max(400, Math.round(height * dpr));
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      if (w === remote.w && h === remote.h) return;
      remote = { w, h };
      remoteListeners.forEach((cb) => cb());
    }, 350);
  },
  setVisibility(v) {
    if (import.meta.env.DEV) (window as unknown as { __visibility: unknown }).__visibility = v;
  },
  setThumbnailWidth() {},
  sendCtrlAltDel(id) {
    console.info("[mock engine] Ctrl+Alt+Del →", id);
  },
  sendKeys(id, scancodes) {
    console.info("[mock engine] keys →", id, scancodes.map((c) => c.toString(16)));
  },
};

/** Sizes `canvas` to the contain-fit rect of `root` for the given aspect ratio. */
function useContainFit(
  rootRef: React.RefObject<HTMLDivElement | null>,
  canvasRef: React.RefObject<HTMLCanvasElement | null>,
  aspect: number,
  maxDeviceWidth = Infinity,
) {
  useEffect(() => {
    const root = rootRef.current;
    const canvas = canvasRef.current;
    if (!root || !canvas) return;
    const fit = () => {
      const r = root.getBoundingClientRect();
      if (!r.width || !r.height) return;
      let w = r.width;
      let h = w / aspect;
      if (h > r.height) {
        h = r.height;
        w = h * aspect;
      }
      canvas.style.width = `${Math.round(w)}px`;
      canvas.style.height = `${Math.round(h)}px`;
      const dpr = window.devicePixelRatio || 1;
      const scale = Math.min(dpr, maxDeviceWidth / w);
      canvas.width = Math.max(2, Math.round(w * scale));
      canvas.height = Math.max(2, Math.round(h * scale));
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(root);
    return () => ro.disconnect();
  }, [rootRef, canvasRef, aspect, maxDeviceWidth]);
}

function useRegisteredCanvas(
  rootRef: React.RefObject<HTMLDivElement | null>,
  canvasRef: React.RefObject<HTMLCanvasElement | null>,
  id: string,
  interval: number,
) {
  useEffect(() => {
    const canvas = canvasRef.current;
    const root = rootRef.current;
    const ctx = canvas?.getContext("2d", { alpha: false });
    if (!canvas || !ctx || !root) return;
    const entry: Entry = { canvas, ctx, id, interval, visible: true, last: 0 };
    const io = new IntersectionObserver(([e]) => {
      entry.visible = e.isIntersecting;
      if (entry.visible) entry.last = 0;
    });
    io.observe(root);
    const unregister = register(entry);
    return () => {
      io.disconnect();
      unregister();
    };
  }, [rootRef, canvasRef, id, interval]);
}

function RemoteSurface({ id, className, onFocusChange }: RemoteSurfaceProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const size = useSyncExternalStore(subscribeRemote, getRemote);
  useContainFit(rootRef, canvasRef, size.w / size.h);
  useRegisteredCanvas(rootRef, canvasRef, id, 33);

  return (
    <div
      ref={rootRef}
      className={`mock-remote ${className ?? ""}`}
      tabIndex={0}
      onMouseDown={(e) => e.currentTarget.focus()}
      onFocus={() => onFocusChange?.(true)}
      onBlur={() => onFocusChange?.(false)}
      onKeyDown={(e) => {
        // A real surface forwards every key to the remote; swallow them here.
        e.preventDefault();
        e.stopPropagation();
      }}
      onContextMenu={(e) => e.preventDefault()}
    >
      <canvas ref={canvasRef} className="mock-remote__canvas" />
    </div>
  );
}

function RemoteThumbnail({ id, className }: RemoteThumbnailProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const size = useSyncExternalStore(subscribeRemote, getRemote);
  useContainFit(rootRef, canvasRef, size.w / size.h, 960);
  useRegisteredCanvas(rootRef, canvasRef, id, 250);
  return (
    <div ref={rootRef} className={`mock-thumb ${className ?? ""}`}>
      <canvas ref={canvasRef} className="mock-thumb__canvas" />
    </div>
  );
}

export function createMockEngine(): EngineModule {
  return { engine, RemoteSurface, RemoteThumbnail };
}
