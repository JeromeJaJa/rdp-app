import { memo, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { displayName, useConnection, useStore } from "../../state/store";
import type { ConnectionId } from "../../lib/types";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";
import { Sparkline } from "./Sparkline";
import { ago, formatBytes, formatPercent, formatRate, level } from "./format";
import "./vitals.css";

type Side = "bottom" | "right";

const memPercent = (used: number, total: number) => (total ? (used / total) * 100 : 0);

/**
 * A host's load at a glance: CPU, memory and disk as three tiny meters, coloured only when
 * they run high. Hover for the details. Nothing shows until the host's companion reports.
 */
export const VitalsGlyph = memo(function VitalsGlyph({ id, side = "bottom" }: { id: ConnectionId; side?: Side }) {
  const cpu = useStore((s) => s.stats[id]?.latest.cpu);
  const mem = useStore((s) => {
    const l = s.stats[id]?.latest;
    return l ? memPercent(l.memUsed, l.memTotal) : undefined;
  });
  const disk = useStore((s) => s.stats[id]?.latest.diskBusy);
  if (cpu === undefined || mem === undefined) return null;

  const bars: [string, number][] = [
    ["CPU", cpu],
    ["Memory", mem],
  ];
  if (disk !== undefined) bars.push(["Disk", disk]);
  return (
    <span
      className="vitals"
      data-vitals={id}
      data-vitals-side={side}
      // An empty title keeps the tab's own title tooltip from showing over the card.
      title=""
      role="img"
      aria-label={bars.map(([label, v]) => `${label} ${formatPercent(v)}`).join(", ")}
    >
      {bars.map(([label, v]) => (
        <span key={label} className={`vitals__bar is-${level(v)}`} style={{ "--v": Math.min(1, v / 100) } as CSSProperties} />
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
    const out = (e: PointerEvent) => {
      if (!e.relatedTarget) clear();
    };
    document.addEventListener("pointerover", over);
    document.addEventListener("pointerout", out);
    document.addEventListener("pointerdown", clear, true);
    document.addEventListener("keydown", clear, true);
    document.addEventListener("scroll", clear, true);
    window.addEventListener("blur", clear);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("pointerover", over);
      document.removeEventListener("pointerout", out);
      document.removeEventListener("pointerdown", clear, true);
      document.removeEventListener("keydown", clear, true);
      document.removeEventListener("scroll", clear, true);
      window.removeEventListener("blur", clear);
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

function VitalsCard({ id, anchor, side }: Open) {
  const vitals = useStore((s) => s.stats[id]);
  const conn = useConnection(id);
  const host = useStore((s) => s.sessions[id]?.companion?.host);
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useGlassUnderlay(ref);

  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, []);

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
