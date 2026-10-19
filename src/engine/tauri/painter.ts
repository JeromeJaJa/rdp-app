// Copies changed regions of a shared surface onto a canvas, once per display frame.
// All painters share one requestAnimationFrame loop; an idle painter costs one
// u32 read per frame.

import { MAGIC, getSurface, subscribeSurface, type Surface, type SurfaceKind } from "./bridge";

/** Above this many rects per frame, paint their bounding box instead. */
const MAX_RECTS = 24;

export class Painter {
  private surface: Surface | undefined;
  private ctx: CanvasRenderingContext2D | null = null;
  private lastSeq = -1;
  private unsubscribe: () => void;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    readonly session: string,
    readonly kind: SurfaceKind,
    private readonly onSurface?: (s: Surface | undefined) => void,
  ) {
    this.unsubscribe = subscribeSurface(session, kind, () => this.bind());
    this.bind();
    scheduler.add(this);
  }

  dispose() {
    scheduler.delete(this);
    this.unsubscribe();
  }

  get current() {
    return this.surface;
  }

  private bind() {
    const s = getSurface(this.session, this.kind);
    if (s === this.surface) return;
    this.surface = s;
    this.lastSeq = -1;
    if (s) {
      if (this.canvas.width !== s.width) this.canvas.width = s.width;
      if (this.canvas.height !== s.height) this.canvas.height = s.height;
      this.ctx = this.canvas.getContext("2d", { alpha: false, desynchronized: true });
    }
    this.onSurface?.(s);
  }

  /** Forces a full repaint on the next frame. */
  invalidate() {
    this.lastSeq = -1;
  }

  tick() {
    const s = this.surface;
    const ctx = this.ctx;
    if (!s || !ctx) return;
    const h = s.header;
    if (h[0] !== MAGIC) return;
    const seq = h[5];
    if (seq === this.lastSeq) return;
    const cap = h[6];
    const pending = this.lastSeq < 0 ? Infinity : (seq - this.lastSeq) >>> 0;

    if (pending > cap || pending > MAX_RECTS * 4) {
      ctx.putImageData(s.image, 0, 0);
    } else {
      const ring = s.ring;
      let x0 = Infinity, y0 = Infinity, x1 = 0, y1 = 0;
      const many = pending > MAX_RECTS;
      for (let i = this.lastSeq; i !== seq; i = (i + 1) >>> 0) {
        const o = (i % cap) * 4;
        const x = ring[o], y = ring[o + 1], w = ring[o + 2], hh = ring[o + 3];
        if (w === 0 || hh === 0) continue;
        if (many) {
          if (x < x0) x0 = x;
          if (y < y0) y0 = y;
          if (x + w > x1) x1 = x + w;
          if (y + hh > y1) y1 = y + hh;
        } else {
          ctx.putImageData(s.image, 0, 0, x, y, w, hh);
        }
      }
      if (many && x1 > x0) ctx.putImageData(s.image, 0, 0, x0, y0, x1 - x0, y1 - y0);
    }
    this.lastSeq = seq;
  }
}

class Scheduler {
  private painters = new Set<Painter>();
  private raf = 0;

  add(p: Painter) {
    this.painters.add(p);
    if (!this.raf) this.raf = requestAnimationFrame(this.frame);
  }

  delete(p: Painter) {
    this.painters.delete(p);
  }

  private frame = () => {
    // Reschedule first: one failing painter must never stop the others.
    this.raf = this.painters.size ? requestAnimationFrame(this.frame) : 0;
    for (const p of this.painters) {
      try {
        p.tick();
      } catch (e) {
        console.error(`painter ${p.kind}/${p.session} failed`, e);
        p.invalidate();
      }
    }
  };
}

const scheduler = new Scheduler();
