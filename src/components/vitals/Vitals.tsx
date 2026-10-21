import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { displayName, useConnection, useStore, type Vitals } from "../../state/store";
import type { ConnectionId, TabStats } from "../../lib/types";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";
import { Sparkline } from "./Sparkline";
import { ago, formatBytes, formatPercent, formatRate, formatRateShort, level } from "./format";
import "./vitals.css";

type Side = "bottom" | "right";

const memPercent = (used: number, total: number) => (total ? (used / total) * 100 : 0);

/** Samples a tab's trend line covers (two minutes at the companion's 5 s sampling). */
const TAB_POINTS = 24;

type Kind = Exclude<TabStats, "all" | "off">;

const LABELS: Record<Kind, string> = { cpu: "CPU", memory: "Memory", network: "Network", disk: "Disk" };

/** A metric's recent values, its latest value as text, and how loaded it is. */
function metric(kind: Kind, v: Vitals): { values: number[]; text: string; max?: number; tone: "ok" | "warn" | "hot" } {
  const l = v.latest;
  switch (kind) {
    case "cpu":
      return { values: v.cpu, text: formatPercent(l.cpu), max: 100, tone: level(l.cpu) };
    case "memory": {
      const m = memPercent(l.memUsed, l.memTotal);
      return { values: v.mem, text: formatPercent(m), max: 100, tone: level(m) };
    }
    case "disk":
      return {
        values: v.disk,
        text: l.diskBusy === undefined ? "—" : formatPercent(l.diskBusy),
        max: 100,
        tone: level(l.diskBusy ?? 0),
      };
    case "network":
      return { values: v.net, text: formatRateShort(l.netRx + l.netTx), tone: "ok" };
  }
}

function Mini({ kind, vitals, value }: { kind: Kind; vitals: Vitals; value: boolean }) {
  const m = metric(kind, vitals);
  return (
    <span className={`vmini vmini--${kind}`}>
      {!value && <span className="vmini__label">{kind === "memory" ? "RAM" : kind === "network" ? "NET" : kind.toUpperCase()}</span>}
      <Sparkline values={m.values.slice(-TAB_POINTS)} max={m.max} width={value ? 26 : 22} height={12} tone={m.tone} mini />
      {value && <span className={`vmini__value is-${m.tone}`}>{m.text}</span>}
    </span>
  );
}

/**
 * A host's load on its tab, sidebar row or grid tile, as tiny trend lines: CPU, memory and
 * network together, or one metric with its value (setting "Stats on tabs"). Top tabs show
 * one metric; tiles show all with values. Hover for the details. Nothing shows until the
 * host's companion reports.
 */
export const TabStatsView = memo(function TabStatsView({
  id,
  variant,
}: {
  id: ConnectionId;
  variant: "side" | "top" | "tile";
}) {
  const vitals = useStore((s) => s.stats[id]);
  const setting = useStore((s) => s.settings.tabStats);
  if (!vitals || setting === "off") return null;
  const kinds: Kind[] =
    variant === "tile"
      ? ["cpu", "memory", "network"]
      : setting === "all"
        ? variant === "top"
          ? ["cpu"]
          : ["cpu", "memory", "network"]
        : [setting];
  const values = variant === "tile" || kinds.length === 1;
  return (
    <span
      className={`vitals vitals--${variant}`}
      data-vitals={id}
      data-vitals-side={variant === "side" ? "right" : "bottom"}
      // An empty title keeps the tab's own title tooltip from showing over the card.
      title=""
      role="img"
      aria-label={kinds.map((k) => `${LABELS[k]} ${metric(k, vitals).text}`).join(", ")}
    >
      {kinds.map((k) => (
        <Mini key={k} kind={k} vitals={vitals} value={values} />
      ))}
    </span>
  );
});

interface Open {
  id: ConnectionId;
  anchor: DOMRect;
  side: Side;
}

/** Hover this long (ms) before the card shows. */
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
      if (!(el instanceof HTMLElement)) return;
      timer = window.setTimeout(() => {
        const id = el.dataset.vitals;
        if (target !== el || !el.isConnected || !id) return;
        setOpen({ id, anchor: el.getBoundingClientRect(), side: (el.dataset.vitalsSide as Side) ?? "bottom" });
      }, DELAY);
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
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, []);
  return now;
}

/** A stat tile: label, value, a trend line under them (the sidebar panel). */
function Tile({ label, value, detail, history, max, tone }: {
  label: string;
  value: string;
  detail?: string;
  history: number[];
  max?: number;
  tone: "ok" | "warn" | "hot";
}) {
  return (
    <div className="vtile">
      <div className="vtile__top">
        <span className="vtile__label">{label}</span>
        <span className={`vtile__value is-${tone}`}>{value}</span>
      </div>
      <Sparkline values={history.slice(-60)} max={max} width={110} height={22} tone={tone} stretch />
      {detail && <span className="vtile__detail">{detail}</span>}
    </div>
  );
}

/** The focused host's load as four tiles and its free space (the sidebar's panel). */
export function HostTiles({ id }: { id: ConnectionId }) {
  const vitals = useStore((s) => s.stats[id]);
  if (!vitals) return null;
  const l = vitals.latest;
  const mem = memPercent(l.memUsed, l.memTotal);
  return (
    <>
      <div className="vtiles">
        <Tile label="CPU" value={formatPercent(l.cpu)} history={vitals.cpu} max={100} tone={level(l.cpu)} />
        <Tile
          label="Memory"
          value={formatPercent(mem)}
          detail={`${formatBytes(l.memUsed)} / ${formatBytes(l.memTotal)}`}
          history={vitals.mem}
          max={100}
          tone={level(mem)}
        />
        <Tile
          label="Disk"
          value={l.diskBusy === undefined ? "—" : formatPercent(l.diskBusy)}
          detail={l.diskBusy === undefined ? "Not shared" : `${formatRateShort((l.diskRead ?? 0) + (l.diskWrite ?? 0))}/s`}
          history={vitals.disk}
          max={100}
          tone={level(l.diskBusy ?? 0)}
        />
        <Tile
          label="Network"
          value={formatRateShort(l.netRx + l.netTx) + "/s"}
          detail={`↓ ${formatRateShort(l.netRx)} ↑ ${formatRateShort(l.netTx)}`}
          history={vitals.net}
          tone="ok"
        />
      </div>
      {l.volumes.length > 0 && (
        <div className="vtiles__volumes">
          {l.volumes.map((v) => (
            <div key={v.name} className="vvol">
              <span className="vvol__name mono">{v.name}</span>
              <Meter value={v.total ? (1 - v.free / v.total) * 100 : 0} />
              <span className="vvol__free">{formatBytes(v.free)} free</span>
            </div>
          ))}
        </div>
      )}
    </>
  );
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
