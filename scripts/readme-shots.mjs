// Renders the README's hero image: the UI (mock backend, fake sessions) in grid view with a
// few groups connected, host stats on the rows and a send in progress, on a designed
// background. Needs `bun run dev` running and Microsoft Edge installed:
//
//   bun run dev            # in one terminal
//   bun scripts/readme-shots.mjs
//
// Writes docs/images/hero.jpg. SEED picks the mock's random outcomes (which hosts run hot,
// the stats, the send's size); EDGE overrides the browser's path.
import { mkdirSync, writeFileSync } from "node:fs";
import puppeteer from "puppeteer-core";

const SEED = Number(process.env.SEED ?? 3);
const EDGE = process.env.EDGE ?? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const URL = process.env.URL ?? "http://localhost:1420/";
const OUT = new globalThis.URL("../docs/images/", import.meta.url);
const WIDTH = 1440;
const HEIGHT = 900;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({ executablePath: EDGE, headless: true });

// ── The UI ──────────────────────────────────────────────────────────────────
const page = await browser.newPage();
await page.setViewport({ width: WIDTH, height: HEIGHT, deviceScaleFactor: 2 });
// Repeatable mock outcomes (connections, stats, transfer speed), and ids, which pick the hosts
// that run hot.
await page.evaluateOnNewDocument((seed) => {
  let s = seed % 2147483647 || 1;
  Math.random = () => (s = (s * 48271) % 2147483647) / 2147483647;
  crypto.randomUUID = () =>
    "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
      const r = Math.floor(Math.random() * 16);
      return (c === "x" ? r : (r & 3) | 8).toString(16);
    });
}, SEED);
await (await page.createCDPSession()).send("Emulation.setFocusEmulationEnabled", { enabled: true });
await page.goto(URL, { waitUntil: "networkidle2" });
// The mock seeds a demo library on first load. Turn the companion on (host stats, sends), and
// move the two ungrouped hosts into Production, so its 8 tiles fill the grid's first rows (and
// fold the then-empty "All" group).
await page.evaluate(() => {
  const key = "rdp-app:mock-library:v1";
  const lib = JSON.parse(localStorage.getItem(key));
  lib.settings.companion = true;
  const builtin = lib.groups.find((g) => g.builtin);
  builtin.collapsed = true;
  const all = builtin.id;
  const prod = lib.groups.find((g) => g.name === "Production").id;
  const loose = lib.connections.filter((c) => c.groupId === all);
  const names = [
    ["File server", "10.20.3.10"],
    ["Backup", "10.20.3.20"],
  ];
  loose.forEach((c, i) => Object.assign(c, { name: names[i][0], host: names[i][1], groupId: prod }));
  lib.connections = [...lib.connections.filter((c) => !loose.includes(c)), ...loose];
  localStorage.setItem(key, JSON.stringify(lib));
});
await page.reload({ waitUntil: "networkidle2" });
await sleep(800);

async function connectGroup(name) {
  await page.evaluate((name) => {
    const header = [...document.querySelectorAll(".group-header")].find((h) => h.textContent.startsWith(name));
    const r = header.getBoundingClientRect();
    header.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: r.x + 30, clientY: r.y + 6 }));
  }, name);
  await sleep(200);
  await page.evaluate(() =>
    [...document.querySelectorAll(".menu__label")].find((l) => l.textContent.startsWith("Connect all"))?.parentElement.click(),
  );
  await sleep(300);
}

/** Trusts any certificate the mock asks about, so no prompt sits over the shot. */
async function trustPrompts() {
  await page.evaluate(() => {
    const trust = [...document.querySelectorAll(".dialog button")].find((b) => /^Trust|^Connect/.test(b.textContent));
    trust?.click();
  });
}

for (const group of ["Production", "Staging", "Lab"]) await connectGroup(group);
// Trust certificate prompts as they come, and retry the few connections the mock fails.
for (let i = 0; i < 60; i++) {
  await trustPrompts();
  await sleep(250);
  if (i % 12 === 11) {
    const failed = await page.evaluate(() => document.querySelectorAll(".tab--side .status-dot--error").length);
    if (failed) for (const group of ["Production", "Staging", "Lab"]) await connectGroup(group);
  }
}

// Grid view, a little smaller tiles so more of them show.
await page.keyboard.down("Control");
await page.keyboard.press("g");
await page.keyboard.up("Control");
await sleep(600);
await page.evaluate(() => {
  const range = document.querySelector(".grid-size input[type=range]");
  if (!range) return;
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
  set.call(range, "256");
  range.dispatchEvent(new Event("input", { bubbles: true }));
});
await sleep(400);

// A send in progress: files dropped on a tile, then Send.
const tile = await page.evaluate(() => {
  const t = [...document.querySelectorAll(".tile[data-id]")].find((el) => el.textContent.includes("SQL primary"));
  const r = t.getBoundingClientRect();
  return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
});
const files = ["C:\\Users\\me\\Downloads\\quarterly-report.xlsx", "C:\\Users\\me\\Downloads\\deploy-2026.10.zip"];
for (const phase of ["enter", "over", "drop"]) {
  await page.evaluate((d) => window.__mockFileDrag?.(d), { phase, x: tile.x, y: tile.y, paths: files });
  await sleep(120);
}
await sleep(400);
await page.evaluate(() => [...document.querySelectorAll(".dialog .btn")].find((b) => b.textContent.trim() === "Send")?.click());
await sleep(300);
await page.mouse.move(WIDTH - 2, 2);
// Shoot while the transfer is part-way through.
for (let i = 0; i < 400; i++) {
  const pct = await page.evaluate(() => parseFloat(document.querySelector(".xfer__fill")?.style.width ?? "NaN"));
  if (pct >= 30 || !(i < 300)) break;
  await sleep(50);
}
const ui = await page.screenshot({ type: "png", captureBeyondViewport: false });

// ── The composition ─────────────────────────────────────────────────────────
const CANVAS_W = 1920;
const CANVAS_H = 1240;
const SHOT_W = 1640;
const shotH = Math.round((SHOT_W * HEIGHT) / WIDTH);
const html = `<!doctype html><html><head><style>
  html, body { margin: 0; width: ${CANVAS_W}px; height: ${CANVAS_H}px; overflow: hidden; }
  body {
    position: relative;
    background:
      radial-gradient(1100px 700px at 12% 8%, rgba(110, 125, 255, 0.55), transparent 60%),
      radial-gradient(900px 650px at 92% 18%, rgba(168, 85, 247, 0.45), transparent 62%),
      radial-gradient(1000px 700px at 78% 105%, rgba(34, 211, 238, 0.32), transparent 60%),
      radial-gradient(900px 600px at 5% 100%, rgba(236, 72, 153, 0.28), transparent 60%),
      linear-gradient(160deg, #0d1024 0%, #090a14 55%, #07080f 100%);
  }
  /* Film grain, so the gradients don't band. */
  body::after {
    content: ""; position: absolute; inset: 0; opacity: 0.09; mix-blend-mode: overlay;
    background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='220' height='220'><filter id='n'><feTurbulence type='fractalNoise' baseFrequency='0.9' numOctaves='2' stitchTiles='stitch'/></filter><rect width='100%' height='100%' filter='url(%23n)'/></svg>");
  }
  .window {
    position: absolute; left: 50%; top: 50%;
    width: ${SHOT_W}px; height: ${shotH}px;
    transform: translate(-50%, -50%);
    border-radius: 16px; overflow: hidden;
    box-shadow:
      0 0 0 1px rgba(255, 255, 255, 0.10),
      0 2px 6px rgba(0, 0, 0, 0.35),
      0 40px 120px rgba(5, 6, 20, 0.75),
      0 0 160px rgba(110, 125, 255, 0.22);
  }
  .window img { display: block; width: 100%; height: 100%; }
  /* A sheen along the top edge, like light on glass. */
  .window::after {
    content: ""; position: absolute; inset: 0; border-radius: inherit; pointer-events: none;
    background: linear-gradient(180deg, rgba(255, 255, 255, 0.06), transparent 18%);
  }
</style></head><body>
  <div class="window"><img src="data:image/png;base64,${Buffer.from(ui).toString("base64")}"></div>
</body></html>`;

const canvas = await browser.newPage();
await canvas.setViewport({ width: CANVAS_W, height: CANVAS_H, deviceScaleFactor: 1.25 });
await canvas.setContent(html, { waitUntil: "load" });
await sleep(300);
mkdirSync(OUT, { recursive: true });
// JPEG: the grain and gradients make a PNG several times the size, for no visible gain.
const hero = await canvas.screenshot({ type: "jpeg", quality: 90 });
writeFileSync(new globalThis.URL("hero.jpg", OUT), hero);
console.log(`docs/images/hero.jpg: ${(hero.length / 1024).toFixed(0)} KiB`);
await browser.close();
