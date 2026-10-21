import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { displayName, useConnection, useStore } from "../../state/store";
import type { ConnectionId, HostStats, TabStats } from "../../lib/types";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";
import { Sparkline } from "./Sparkline";
import { ago, formatBytes, formatPercent, formatRate, level } from "./format";
import "./vitals.css";

type Side = "bottom" | "right";

const memPercent = (used: number, total: number) => (total ? (used / total) * 100 : 0);

type Kind = Exclude<TabStats, "off">;

/** Tab labels, short enough to sit beside a name. */
const SHORT: Record<Kind, string> = { cpu: "CPU", memory: "RAM", network: "NET", disk: "DISK" };
const NAMES: Record<Kind, string> = { cpu: "CPU", memory: "Memory", network: "Network", disk: "Disk" };

/** A metric's latest value as text, and how loaded it is. */
function reading(kind: Kind, l: HostStats): { text: string; tone: "ok" | "warn" | "hot" } {
  switch (kind) {
    case "cpu":
      return { text: formatPercent(l.cpu), tone: level(l.cpu) };
    case "memory": {
      const m = memPercent(l.memUsed, l.memTotal);
      return { text: formatPercent(m), tone: level(m) };
    }
    case "disk":
      return l.diskBusy === undefined
        ? { text: "—", tone: "ok" }
        : { text: formatPercent(l.diskBusy), tone: level(l.diskBusy) };
    case "network":
      return { text: formatRate(l.netRx + l.netTx), tone: "ok" };
  }
}

/**
 * A host's load on its sidebar row or grid tile: one metric, labelled ("CPU 12%"),
 * coloured only when it runs high (setting "Host stat"). Hover it (or a top tab) for
 * everything. Nothing shows until the host's companion reports.
 */
export const TabStat = memo(function TabStat({ id, variant }: { id: ConnectionId; variant: "side" | "tile" }) {
  const latest = useStore((s) => s.stats[id]?.latest);
  const kind = useStore((s) => s.settings.tabStats);
  if (!latest || kind === "off") return null;
  const { text, tone } = reading(kind, latest);
  return (
    <span
      className={`vstat vstat--${variant}`}
      data-vitals={id}
      data-vitals-side={variant === "side" ? "right" : "bottom"}
      // An empty title keeps the tab's own title tooltip from showing over the card.
      title=""
      role="img"
      aria-label={`${NAMES[kind]} ${text}`}
    >
      <span className="vstat__label">{SHORT[kind]}</span>
      <span className={`vstat__value is-${tone}`}>{text}</span>
    </span>
  );
});

interface Open {
  id: ConnectionId;
  anchor: DOMRect;
  side: Side;
}

/** Hover this long (ms) before the card shows (a whole tab says longer: `data-vitals-delay`). */
const DELAY = 280;
const GAP = 8;
const EDGE = 8;

/** The details card for whichever host's glyph is hovered (one per window). */
export function VitalsCards() {
  const [open, setOpen] = useState<Open | null>(null);

  useEffect(() => {
    let target: Element | null = null;
    let timer = 0;
    const clear = () => {
      clearTimeout(timer);
      target = null;
      setOpen(null);
    };
    const over = (e: PointerEvent) => {
      const el = e.target instanceof Element ? e.target.closest("[data-vitals]") : null;
      if (el === target) return;
      clearTimeout(timer);
      target = el;
      setOpen(null);
      // Not while a button is held: that's a tab being dragged across others.
      if (!(el instanceof HTMLElement) || e.buttons !== 0) return;
      timer = window.setTimeout(() => {
        const id = el.dataset.vitals;
        if (target !== el || !el.isConnected || !id) return;
        setOpen({ id, anchor: el.getBoundingClientRect(), side: (el.dataset.vitalsSide as Side) ?? "bottom" });
      }, Number(el.dataset.vitalsDelay) || DELAY);
    };
    // The card stays while the pointer is away from the window (taking a screenshot, say)
    // and closes when it moves to something else here, on a click or key, or if what it
    // points at scrolls away.
    const scrolled = () => {
      if (!(target instanceof HTMLElement) || !target.isConnected) return clear();
      const r = target.getBoundingClientRect();
      setOpen((o) => (o && (Math.abs(o.anchor.left - r.left) > 1 || Math.abs(o.anchor.top - r.top) > 1) ? null : o));
    };
    document.addEventListener("pointerover", over);
    document.addEventListener("pointerdown", clear, true);
    document.addEventListener("keydown", clear, true);
    document.addEventListener("scroll", scrolled, true);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("pointerover", over);
      document.removeEventListener("pointerdown", clear, true);
      document.removeEventListener("keydown", clear, true);
      document.removeEventListener("scroll", scrolled, true);
    };
  }, []);

  return open ? <VitalsCard key={open.id} {...open} /> : null;
}

function Meter({ value }: { value: number }) {
  return (
    <span className={`vmeter is-${level(value)}`}>
      <span className="vmeter__fill" style={{ width: `${Math.max(0, Math.min(100, value))}%` }} />
    </span>
  );
}

function Metric({
  label,
  value,
  detail,
  percent,
  history,
  max,
}: {
  label: string;
  value: string;
  detail?: string;
  /** Shows a meter (and colours the trend) for a 0–100 value. */
  percent?: number;
  history?: number[];
  max?: number;
}) {
  const tone = percent === undefined ? "ok" : level(percent);
  return (
    <div className="vmetric">
      <span className="vmetric__label">{label}</span>
      <span className="vmetric__value">{value}</span>
      {history && (
        <span className="vmetric__spark">
          <Sparkline values={history} max={max} tone={tone} />
        </span>
      )}
      {percent !== undefined && <Meter value={percent} />}
      {detail && <span className="vmetric__detail">{detail}</span>}
    </div>
  );
}

/** "updated 12s ago", ticking. */
function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, []);
  return now;
}

function VitalsCard({ id, anchor, side }: Open) {
  const vitals = useStore((s) => s.stats[id]);
  const conn = useConnection(id);
  const host = useStore((s) => s.sessions[id]?.companion?.host);
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const now = useNow();
  useGlassUnderlay(ref);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let left: number;
    let top: number;
    if (side === "right") {
      left = anchor.right + GAP;
      top = anchor.top + anchor.height / 2 - h / 2;
      if (left + w > window.innerWidth - EDGE) left = anchor.left - GAP - w;
    } else {
      left = anchor.left + anchor.width / 2 - w / 2;
      top = anchor.bottom + GAP;
      if (top + h > window.innerHeight - EDGE) top = anchor.top - GAP - h;
    }
    left = Math.min(Math.max(left, EDGE), window.innerWidth - EDGE - w);
    top = Math.min(Math.max(top, EDGE), window.innerHeight - EDGE - h);
    setPos({ left, top });
  }, [anchor, side, !!vitals]);

  if (!vitals || !conn) return null;
  const l = vitals.latest;
  const mem = memPercent(l.memUsed, l.memTotal);
  return (
    <div ref={ref} className="vcard glass" data-side={side} aria-hidden style={pos ?? { visibility: "hidden" }}>
      <div className="vcard__head">
        <span className="vcard__title">{displayName(conn)}</span>
        {host && host.toLowerCase() !== displayName(conn).toLowerCase() && (
          <span className="vcard__host mono">{host}</span>
        )}
        <span className="vcard__age">{ago(l.at, now)}</span>
      </div>
      <Metric label="CPU" value={formatPercent(l.cpu)} percent={l.cpu} history={vitals.cpu} max={100} />
      <Metric
        label="Memory"
        value={formatPercent(mem)}
        detail={`${formatBytes(l.memUsed)} of ${formatBytes(l.memTotal)}`}
        percent={mem}
        history={vitals.mem}
        max={100}
      />
      {l.diskBusy !== undefined ? (
        <Metric
          label="Disk"
          value={`${formatPercent(l.diskBusy)} active`}
          detail={`${formatRate(l.diskRead ?? 0)} read · ${formatRate(l.diskWrite ?? 0)} written`}
          percent={l.diskBusy}
          history={vitals.disk}
          max={100}
        />
      ) : (
        <Metric label="Disk" value="—" detail="This host doesn't share disk activity" />
      )}
      <Metric
        label="Network"
        value={formatRate(l.netRx + l.netTx)}
        detail={`↓ ${formatRate(l.netRx)} in · ↑ ${formatRate(l.netTx)} out`}
        history={vitals.net}
      />
      {l.volumes.length > 0 && (
        <div className="vcard__volumes">
          {l.volumes.map((v) => (
            <div key={v.name} className="vvol">
              <span className="vvol__name mono">{v.name}</span>
              <Meter value={v.total ? (1 - v.free / v.total) * 100 : 0} />
              <span className="vvol__free">
                {formatBytes(v.free)} free of {formatBytes(v.total)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
