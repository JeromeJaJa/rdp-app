// Real engine: renders WebView2 shared-buffer surfaces written by the Rust
// engine and forwards keyboard/mouse input to it.

import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Engine, EngineModule, RemoteSurfaceProps, RemoteThumbnailProps } from "../types";
import { isTauri } from "../../lib/env";
import { initBridge, type Surface } from "./bridge";
import { Painter } from "./painter";
import { SCANCODES } from "./scancodes";
import "./engine.css";

if (isTauri()) initBridge();

// ── Remote cursors ───────────────────────────────────────────────────────────

const cursors = new Map<string, string>();
const cursorListeners = new Set<() => void>();
function applyCursor(e: { type: string; id: string; cursor?: string }) {
  if (e.type === "cursor" && e.cursor !== undefined) {
    cursors.set(e.id, e.cursor);
    cursorListeners.forEach((cb) => cb());
  }
}
if (isTauri()) {
  listen<{ type: string; id: string; cursor?: string }>("session-event", (e) => applyCursor(e.payload)).then(() =>
    invoke<{ type: string; id: string; cursor?: string }[]>("session_snapshot")
      .then((events) => events.forEach(applyCursor))
      .catch(() => {}),
  );
}
function useRemoteCursor(id: string) {
  return useSyncExternalStore(
    (cb) => {
      cursorListeners.add(cb);
      return () => cursorListeners.delete(cb);
    },
    () => cursors.get(id) ?? "default",
  );
}

// ── Engine facade ────────────────────────────────────────────────────────────

function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number) {
  let t: ReturnType<typeof setTimeout> | undefined;
  return (...a: A) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...a), ms);
  };
}

const sendViewport = debounce((width: number, height: number, scale: number) => {
  invoke("engine_set_viewport", { width, height, scale }).catch(() => {});
}, 120);

const sendThumbWidth = debounce((width: number) => {
  invoke("engine_set_thumbnail_width", { width }).catch(() => {});
}, 200);

let lastVisibility = "";

// Thumbnails are rendered at the widest tile actually on screen (device px), so
// they stay crisp when the grid stretches tiles beyond the nominal tile width.
const thumbWidths = new Map<Element, number>();
let thumbHint = 0;
const thumbObserver = new ResizeObserver((entries) => {
  for (const e of entries) {
    if (thumbWidths.has(e.target)) thumbWidths.set(e.target, e.contentRect.width * (window.devicePixelRatio || 1));
  }
  updateThumbWidth();
});
function updateThumbWidth() {
  let max = thumbHint;
  for (const w of thumbWidths.values()) max = Math.max(max, w);
  if (max > 0) sendThumbWidth(Math.round(max));
}

const engine: Engine = {
  kind: "tauri",
  setViewport(width, height, dpr) {
    if (width < 50 || height < 50) return;
    sendViewport(Math.round(width * dpr), Math.round(height * dpr), Math.round(dpr * 100));
  },
  setVisibility({ active, thumbnails }) {
    const key = JSON.stringify([active, [...thumbnails].sort()]);
    if (key === lastVisibility) return;
    lastVisibility = key;
    invoke("engine_set_visibility", { active, thumbnails }).catch(() => {});
  },
  setThumbnailWidth(px) {
    thumbHint = px;
    updateThumbWidth();
  },
  sendCtrlAltDel(id) {
    invoke("engine_ctrl_alt_del", { id }).catch(() => {});
  },
  sendKeys(id, scancodes) {
    invoke("engine_send_keys", { id, scancodes }).catch(() => {});
  },
};

// ── Input ────────────────────────────────────────────────────────────────────

type InputEvent =
  | { t: "key"; code: number; down: boolean }
  | { t: "unicode"; ch: string; down: boolean }
  | { t: "move"; x: number; y: number }
  | { t: "button"; button: number; down: boolean }
  | { t: "wheel"; delta: number; horizontal: boolean }
  | { t: "releaseAll" }
  | { t: "sync"; caps: boolean; num: boolean; scroll: boolean };

const LOCK_KEYS = new Set(["CapsLock", "NumLock", "ScrollLock"]);

/** Batches input per session: pointer motion once per frame, everything else ASAP. */
class InputQueue {
  private events: InputEvent[] = [];
  private raf = 0;
  private micro = false;

  constructor(private readonly id: string) {}

  push(ev: InputEvent) {
    const last = this.events[this.events.length - 1];
    if (ev.t === "move" && last?.t === "move") {
      this.events[this.events.length - 1] = ev;
    } else {
      this.events.push(ev);
    }
    if (ev.t === "move") {
      if (!this.raf && !this.micro) this.raf = requestAnimationFrame(this.flush);
    } else if (!this.micro) {
      this.micro = true;
      queueMicrotask(this.flush);
    }
  }

  flush = () => {
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.micro = false;
    if (!this.events.length) return;
    const events = this.events;
    this.events = [];
    invoke("engine_input", { id: this.id, events }).catch(() => {});
  };
}

// ── Layout ───────────────────────────────────────────────────────────────────

interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Fits the remote image in the container, snapping to whole device pixels. */
function fit(cw: number, ch: number, rw: number, rh: number, dpr: number): Box {
  const rwCss = rw / dpr;
  const rhCss = rh / dpr;
  let scale = Math.min(cw / rwCss, ch / rhCss);
  if (Math.abs(scale - 1) < 0.02) scale = 1;
  const width = Math.round(rwCss * scale * dpr) / dpr;
  const height = Math.round(rhCss * scale * dpr) / dpr;
  return {
    width,
    height,
    left: Math.round(((cw - width) / 2) * dpr) / dpr,
    top: Math.round(((ch - height) / 2) * dpr) / dpr,
  };
}

function useContainerSize(ref: React.RefObject<HTMLElement | null>) {
  const [size, setSize] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const r = entry.contentRect;
      setSize((s) => (s.w === r.width && s.h === r.height ? s : { w: r.width, h: r.height }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return size;
}

function useDpr() {
  const [dpr, setDpr] = useState(window.devicePixelRatio || 1);
  useEffect(() => {
    let mq: MediaQueryList | null = null;
    const update = () => {
      setDpr(window.devicePixelRatio || 1);
      mq?.removeEventListener("change", update);
      mq = matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
      mq.addEventListener("change", update);
    };
    update();
    return () => mq?.removeEventListener("change", update);
  }, []);
  return dpr;
}

// ── Components ───────────────────────────────────────────────────────────────

function RemoteSurface({ id, className, onFocusChange }: RemoteSurfaceProps) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [surface, setSurface] = useState<Surface | undefined>();
  const size = useContainerSize(wrapRef);
  const dpr = useDpr();
  const cursor = useRemoteCursor(id);
  const queue = useRef<InputQueue>(null);
  if (!queue.current || (queue.current as unknown as { id: string }).id !== id) queue.current = new InputQueue(id);
  const wheelAcc = useRef({ x: 0, y: 0 });

  useEffect(() => {
    const canvas = canvasRef.current!;
    const painter = new Painter(canvas, id, "full", setSurface);
    return () => painter.dispose();
  }, [id]);

  const box = surface && size.w > 0 ? fit(size.w, size.h, surface.width, surface.height, dpr) : null;

  // Read through refs: the input handlers must not be torn down and re-attached
  // when the surface is replaced (resolution change), which released held buttons.
  const surfaceRef = useRef(surface);
  surfaceRef.current = surface;
  const onFocusChangeRef = useRef(onFocusChange);
  onFocusChangeRef.current = onFocusChange;

  useEffect(() => {
    const el = wrapRef.current!;
    const q = queue.current!;
    /** Mouse buttons currently held down in the remote session. */
    const held = new Set<number>();
    /** Focus left mid-drag: release everything once the drag ends. */
    let releasePending = false;
    /** Lock key state last sent to the server ("" = never). */
    let lastLocks = "";

    /** Mirrors the local Caps/Num/Scroll Lock state to the server when it differs. */
    const syncLocks = (e: MouseEvent | KeyboardEvent) => {
      const locks = {
        caps: e.getModifierState("CapsLock"),
        num: e.getModifierState("NumLock"),
        scroll: e.getModifierState("ScrollLock"),
      };
      const key = `${+locks.caps}${+locks.num}${+locks.scroll}`;
      if (key === lastLocks) return;
      lastLocks = key;
      q.push({ t: "sync", ...locks });
    };

    const toRemote = (clientX: number, clientY: number) => {
      const canvas = canvasRef.current;
      const surface = surfaceRef.current;
      if (!canvas || !surface || !Number.isFinite(clientX) || !Number.isFinite(clientY)) return null;
      const r = canvas.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return null;
      const x = Math.min(surface.width - 1, Math.max(0, Math.floor(((clientX - r.left) / r.width) * surface.width)));
      const y = Math.min(surface.height - 1, Math.max(0, Math.floor(((clientY - r.top) / r.height) * surface.height)));
      return { x, y };
    };
    const releaseAll = () => {
      held.clear();
      releasePending = false;
      q.push({ t: "releaseAll" });
    };
    /** Releases everything now, or when the current drag ends if a button is down. */
    const releaseSoon = () => {
      if (held.size) releasePending = true;
      else releaseAll();
    };

    // Leaving the session with the left button held may be a drag of files out of it: ask
    // (once per press) so the companion can carry the drag on here.
    let probedOut = false;
    const outside = (clientX: number, clientY: number) => {
      const r = canvasRef.current?.getBoundingClientRect();
      return !!r && (clientX < r.left || clientX >= r.right || clientY < r.top || clientY >= r.bottom);
    };

    // The event's own position is the latest one. (Coalesced samples add nothing here
    // since only the last position is sent, and fast drags could yield bogus ones.)
    const onPointerMove = (e: PointerEvent) => {
      const p = toRemote(e.clientX, e.clientY);
      if (p) q.push({ t: "move", ...p });
      if (p && held.has(0) && !probedOut && outside(e.clientX, e.clientY)) {
        probedOut = true;
        invoke("drag_out_probe", { id, x: p.x, y: p.y }).catch(() => {});
      }
    };
    const onPointerDown = (e: PointerEvent) => {
      probedOut = false;
      el.focus({ preventScroll: true });
      const p = toRemote(e.clientX, e.clientY);
      if (!p) return;
      e.preventDefault();
      el.setPointerCapture(e.pointerId);
      syncLocks(e);
      held.add(e.button);
      q.push({ t: "move", ...p });
      q.push({ t: "button", button: e.button, down: true });
    };
    const onPointerUp = (e: PointerEvent) => {
      const p = toRemote(e.clientX, e.clientY);
      if (p) q.push({ t: "move", ...p });
      held.delete(e.button);
      q.push({ t: "button", button: e.button, down: false });
      if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
      if (releasePending && !held.size) releaseAll();
    };
    // Capture lost without a pointerup (e.g. the window lost activation): let go of
    // what is still held so nothing stays stuck.
    const onLostCapture = () => {
      if (!held.size) return;
      for (const button of held) q.push({ t: "button", button, down: false });
      held.clear();
      if (releasePending) releaseAll();
    };
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      // Pixel deltas (touchpads) → RDP units, 120 per notch (~100px).
      const factor = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 120 * 3 : 1.2;
      const acc = wheelAcc.current;
      acc.y += -e.deltaY * factor;
      acc.x += e.deltaX * factor;
      for (const [axis, horizontal] of [["y", false], ["x", true]] as const) {
        let units = Math.trunc(acc[axis]);
        if (Math.abs(units) < 1) continue;
        acc[axis] -= units;
        while (units !== 0) {
          const step = Math.max(-255, Math.min(255, units));
          q.push({ t: "wheel", delta: step, horizontal });
          units -= step;
        }
      }
    };
    const onKey = (down: boolean) => (e: KeyboardEvent) => {
      // Ctrl+Alt+Home releases keyboard capture (same chord as mstsc).
      if (down && e.ctrlKey && e.altKey && e.code === "Home") {
        e.preventDefault();
        releaseAll();
        el.blur();
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      // Lock keys are mirrored as state (sync), never forwarded as presses: forwarding
      // them toggles both sides and keeps any existing mismatch forever. The local state
      // has flipped by keyup.
      if (LOCK_KEYS.has(e.code)) {
        if (!down) syncLocks(e);
        return;
      }
      syncLocks(e);
      const code = SCANCODES[e.code];
      if (code !== undefined) {
        q.push({ t: "key", code, down });
      } else if (e.key.length === 1) {
        q.push({ t: "unicode", ch: e.key, down });
      }
    };
    const onKeyDown = onKey(true);
    const onKeyUp = onKey(false);
    // Leaving the session releases keys (no stuck Alt/Ctrl), but a drag in progress
    // keeps its button until the mouse is actually let go.
    const onBlur = () => {
      releaseSoon();
      onFocusChangeRef.current?.(false);
    };
    const onFocus = () => {
      // The server may have changed (or reconnected) meanwhile: re-sync locks on the next input.
      lastLocks = "";
      onFocusChangeRef.current?.(true);
    };
    const onContextMenu = (e: Event) => e.preventDefault();
    const onWindowBlur = () => {
      if (document.activeElement === el) releaseSoon();
    };

    el.addEventListener("pointermove", onPointerMove);
    el.addEventListener("pointerdown", onPointerDown);
    el.addEventListener("pointerup", onPointerUp);
    el.addEventListener("lostpointercapture", onLostCapture);
    el.addEventListener("wheel", onWheel, { passive: false });
    el.addEventListener("keydown", onKeyDown);
    el.addEventListener("keyup", onKeyUp);
    el.addEventListener("blur", onBlur);
    el.addEventListener("focus", onFocus);
    el.addEventListener("contextmenu", onContextMenu);
    window.addEventListener("blur", onWindowBlur);
    return () => {
      el.removeEventListener("pointermove", onPointerMove);
      el.removeEventListener("pointerdown", onPointerDown);
      el.removeEventListener("pointerup", onPointerUp);
      el.removeEventListener("lostpointercapture", onLostCapture);
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("keydown", onKeyDown);
      el.removeEventListener("keyup", onKeyUp);
      el.removeEventListener("blur", onBlur);
      el.removeEventListener("focus", onFocus);
      el.removeEventListener("contextmenu", onContextMenu);
      window.removeEventListener("blur", onWindowBlur);
      releaseAll();
      q.flush();
    };
  }, [id]);

  return (
    <div ref={wrapRef} className={`rs-surface ${className ?? ""}`} tabIndex={0} data-remote-surface="">
      <canvas
        ref={canvasRef}
        className="rs-surface__canvas"
        style={
          box
            ? { left: box.left, top: box.top, width: box.width, height: box.height, cursor }
            : { visibility: "hidden" }
        }
      />
      {!surface && <div className="rs-surface__waiting" />}
    </div>
  );
}

function RemoteThumbnail({ id, className }: RemoteThumbnailProps) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    const painter = new Painter(canvasRef.current!, id, "thumb", (s) => setReady(!!s));
    return () => painter.dispose();
  }, [id]);

  useEffect(() => {
    const el = wrapRef.current!;
    thumbWidths.set(el, 0);
    thumbObserver.observe(el);
    return () => {
      thumbObserver.unobserve(el);
      thumbWidths.delete(el);
    };
  }, []);

  return (
    <div ref={wrapRef} className={`rs-thumb ${className ?? ""}`}>
      <canvas ref={canvasRef} className="rs-thumb__canvas" style={ready ? undefined : { visibility: "hidden" }} />
    </div>
  );
}

export function createTauriEngine(): EngineModule {
  return { engine, RemoteSurface, RemoteThumbnail };
}
