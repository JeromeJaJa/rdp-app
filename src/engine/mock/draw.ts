// Procedural fake Windows desktop, used only by the mock engine so the UI can be
// developed in a plain browser. Static layers are cached per (id, size); each
// frame only blits the cache and paints the animated bits.

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function rng(seed: number) {
  let x = seed || 1;
  return () => {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    return ((x >>> 0) % 10000) / 10000;
  };
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, r);
}

interface Layout {
  hue: number;
  darkWindows: boolean;
  windows: { x: number; y: number; w: number; h: number; lines: number[] }[];
}

const layouts = new Map<string, Layout>();
function layoutFor(id: string): Layout {
  let l = layouts.get(id);
  if (l) return l;
  const h = hash(id);
  const r = rng(h);
  const windows = [0, 1].map((i) => ({
    x: 0.08 + r() * 0.3 + i * 0.22,
    y: 0.08 + r() * 0.2 + i * 0.12,
    w: 0.34 + r() * 0.16,
    h: 0.36 + r() * 0.16,
    lines: Array.from({ length: 7 }, () => 0.35 + r() * 0.6),
  }));
  l = { hue: h % 360, darkWindows: (h >> 9) % 3 !== 0, windows };
  layouts.set(id, l);
  return l;
}

type Cache = { key: string; canvas: OffscreenCanvas | HTMLCanvasElement };
const caches = new Map<string, Cache>();

function makeCanvas(w: number, h: number): OffscreenCanvas | HTMLCanvasElement {
  if (typeof OffscreenCanvas !== "undefined") return new OffscreenCanvas(w, h);
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  return c;
}

function staticLayer(id: string, w: number, h: number) {
  const key = `${w}x${h}`;
  const cacheKey = `${id}:${key}`;
  const hit = caches.get(cacheKey);
  if (hit) return hit.canvas;
  const canvas = makeCanvas(w, h);
  const ctx = canvas.getContext("2d") as CanvasRenderingContext2D;
  const L = layoutFor(id);

  // Wallpaper
  const g = ctx.createLinearGradient(0, 0, w, h);
  g.addColorStop(0, `hsl(${L.hue} 42% 26%)`);
  g.addColorStop(0.55, `hsl(${(L.hue + 30) % 360} 48% 14%)`);
  g.addColorStop(1, `hsl(${(L.hue + 60) % 360} 55% 8%)`);
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  const glow = ctx.createRadialGradient(w * 0.72, h * 0.3, 0, w * 0.72, h * 0.3, Math.max(w, h) * 0.55);
  glow.addColorStop(0, `hsla(${(L.hue + 20) % 360} 80% 65% / 0.28)`);
  glow.addColorStop(1, "transparent");
  ctx.fillStyle = glow;
  ctx.fillRect(0, 0, w, h);

  const u = Math.min(w, h) / 100;

  // Desktop icons
  for (let i = 0; i < 4; i++) {
    ctx.fillStyle = "rgba(255,255,255,0.16)";
    roundRect(ctx, u * 2, u * 2.5 + i * u * 9, u * 5, u * 5, u * 1.1);
    ctx.fill();
    ctx.fillStyle = "rgba(255,255,255,0.35)";
    ctx.fillRect(u * 1.6, u * 8.2 + i * u * 9, u * 5.8, u * 0.7);
  }

  // Windows
  for (const win of L.windows) {
    const x = win.x * w;
    const y = win.y * h;
    const ww = win.w * w;
    const wh = win.h * h;
    ctx.save();
    ctx.shadowColor = "rgba(0,0,0,0.45)";
    ctx.shadowBlur = u * 4;
    ctx.shadowOffsetY = u * 1;
    ctx.fillStyle = L.darkWindows ? "#202024" : "#f4f4f6";
    roundRect(ctx, x, y, ww, wh, u * 0.9);
    ctx.fill();
    ctx.restore();
    // Title bar
    ctx.fillStyle = L.darkWindows ? "#2a2a30" : "#e6e6ea";
    roundRect(ctx, x, y, ww, u * 4.2, u * 0.9);
    ctx.fill();
    ctx.fillRect(x, y + u * 2.5, ww, u * 1.7);
    ctx.fillStyle = L.darkWindows ? "rgba(255,255,255,0.55)" : "rgba(0,0,0,0.55)";
    ctx.fillRect(x + u * 1.6, y + u * 1.75, ww * 0.22, u * 0.8);
    for (let k = 0; k < 3; k++) {
      ctx.fillRect(x + ww - u * (2.4 + k * 3.4), y + u * 1.85, u * 1.3, u * 0.55);
    }
    // Content lines
    ctx.fillStyle = L.darkWindows ? "rgba(255,255,255,0.12)" : "rgba(0,0,0,0.1)";
    win.lines.forEach((len, i) => {
      const ly = y + u * 7 + i * u * 2.6;
      if (ly < y + wh - u * 8) ctx.fillRect(x + u * 2, ly, (ww - u * 4) * len, u * 1);
    });
  }

  // Taskbar
  const tb = Math.max(u * 4.6, 18);
  ctx.fillStyle = "rgba(18,18,22,0.86)";
  ctx.fillRect(0, h - tb, w, tb);
  ctx.fillStyle = "rgba(255,255,255,0.06)";
  ctx.fillRect(0, h - tb, w, 1);
  const icons = 6;
  const iconSize = tb * 0.52;
  const start = w / 2 - (icons * iconSize * 1.6) / 2;
  for (let i = 0; i < icons; i++) {
    ctx.fillStyle = i === 0 ? `hsl(${L.hue} 80% 65%)` : "rgba(255,255,255,0.28)";
    roundRect(ctx, start + i * iconSize * 1.6, h - tb + (tb - iconSize) / 2, iconSize, iconSize, iconSize * 0.25);
    ctx.fill();
  }

  // Keep the cache bounded.
  if (caches.size > 64) caches.delete(caches.keys().next().value!);
  caches.set(cacheKey, { key, canvas });
  return canvas;
}

export function drawDesktop(ctx: CanvasRenderingContext2D, id: string, w: number, h: number, t: number) {
  if (w < 2 || h < 2) return;
  ctx.drawImage(staticLayer(id, w, h), 0, 0);
  const L = layoutFor(id);
  const u = Math.min(w, h) / 100;
  const tb = Math.max(u * 4.6, 18);

  // Animated progress bar in the first window.
  const win = L.windows[0];
  const x = win.x * w + u * 2;
  const y = win.y * h + win.h * h - u * 5;
  const bw = win.w * w - u * 4;
  ctx.fillStyle = L.darkWindows ? "rgba(255,255,255,0.08)" : "rgba(0,0,0,0.08)";
  roundRect(ctx, x, y, bw, u * 1.2, u * 0.6);
  ctx.fill();
  const p = (t / 6000 + (hash(id) % 100) / 100) % 1;
  ctx.fillStyle = `hsl(${L.hue} 85% 62%)`;
  roundRect(ctx, x, y, Math.max(u, bw * p), u * 1.2, u * 0.6);
  ctx.fill();

  // Sparkline in the second window.
  const w2 = L.windows[1];
  const sx = w2.x * w + u * 2;
  const sy = w2.y * h + w2.h * h - u * 4;
  const sw = w2.w * w - u * 4;
  ctx.strokeStyle = `hsla(${(L.hue + 150) % 360} 75% 60% / 0.9)`;
  ctx.lineWidth = Math.max(1, u * 0.35);
  ctx.beginPath();
  for (let i = 0; i <= 40; i++) {
    const px = sx + (sw * i) / 40;
    const py = sy - u * 3 * (0.5 + 0.5 * Math.sin(i * 0.45 + t / 700 + win.x * 10));
    if (i === 0) ctx.moveTo(px, py);
    else ctx.lineTo(px, py);
  }
  ctx.stroke();

  // Clock
  const now = new Date();
  const time = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  ctx.fillStyle = "rgba(255,255,255,0.85)";
  ctx.font = `${Math.max(9, tb * 0.32)}px "Segoe UI", system-ui, sans-serif`;
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  ctx.fillText(time, w - tb * 0.5, h - tb / 2);
}
