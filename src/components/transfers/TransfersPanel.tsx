import { memo, useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { BorderBeam } from "border-beam";
import { AlertCircle, Check, ChevronDown, ChevronUp, X } from "lucide-react";
import type { Transfer } from "../../lib/types";
import { activityDone, cancelTransfer, clearTransfers } from "../../state/actions";
import { displayName, useStore } from "../../state/store";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";
import { formatBytes, formatRate } from "../vitals/format";
import "./transfers.css";

const isActive = (t: Transfer) => t.state === "waiting" || t.state === "sending";

/** How long a fully successful batch stays up before it clears itself (ms). */
const LINGER = 6000;

/** Smoothed bytes per second while a transfer is sending. */
function useRate(sent: number, sending: boolean): number | null {
  const last = useRef<{ at: number; sent: number; rate: number | null }>({ at: performance.now(), sent, rate: null });
  const [rate, setRate] = useState<number | null>(null);
  useEffect(() => {
    const now = performance.now();
    const prev = last.current;
    if (!sending) {
      last.current = { at: now, sent, rate: null };
      setRate(null);
      return;
    }
    const secs = (now - prev.at) / 1000;
    if (secs < 0.4) return;
    const instant = Math.max(0, (sent - prev.sent) / secs);
    const next = prev.rate === null ? instant : prev.rate * 0.7 + instant * 0.3;
    last.current = { at: now, sent, rate: next };
    setRate(next);
  }, [sent, sending]);
  return rate;
}

function remaining(bytes: number, rate: number | null): string | null {
  if (!rate || rate < 1) return null;
  const s = Math.round(bytes / rate);
  if (s < 60) return `${Math.max(1, s)}s left`;
  if (s < 3600) return `${Math.round(s / 60)} min left`;
  return `${(s / 3600).toFixed(1)} h left`;
}

const TransferRow = memo(function TransferRow({ id }: { id: number }) {
  const t = useStore((s) => s.transfers[id]);
  const host = useStore((s) => {
    const c = t ? s.connById[t.session] : undefined;
    return c ? displayName(c) : (t?.session ?? "");
  });
  const rate = useRate(t?.sent ?? 0, t?.state === "sending");
  if (!t) return null;

  const pct = t.total ? Math.min(100, (t.sent / t.total) * 100) : t.state === "done" ? 100 : 0;
  const sending = t.state === "sending";
  const status =
    t.state === "waiting"
      ? "Connecting…"
      : sending
        ? `${Math.floor(pct)}%`
        : t.state === "done"
          ? "Sent"
          : t.state === "cancelled"
            ? "Cancelled"
            : "Failed";
  const detail =
    t.state === "failed" && t.error
      ? t.error
      : sending
        ? [`${formatBytes(t.sent)} of ${formatBytes(t.total)}`, rate !== null && formatRate(rate), remaining(t.total - t.sent, rate)]
            .filter(Boolean)
            .join(" · ")
        : `${formatBytes(t.total)} · ${t.files} file${t.files === 1 ? "" : "s"} → ${t.folder}`;

  return (
    <BorderBeam size="line" colorVariant="colorful" active={sending} strength={0.85} className="xfer-beam">
      <div className={`xfer is-${t.state}`}>
        <div className="xfer__top">
          <span className="xfer__icon">
            {t.state === "done" ? <Check size={13} /> : t.state === "failed" ? <AlertCircle size={13} /> : null}
          </span>
          <span className="xfer__label">{t.label}</span>
          <span className="xfer__to">→ {host}</span>
          <span className="xfer__status">{status}</span>
          {isActive(t) && (
            <button className="xfer__cancel" aria-label="Cancel" data-tip="Cancel" onClick={() => cancelTransfer(id)}>
              <X size={12} />
            </button>
          )}
        </div>
        {isActive(t) && (
          <div className="xfer__track">
            <span className="xfer__fill" style={{ width: `${pct}%` }} />
          </div>
        )}
        <div className="xfer__detail" title={detail}>
          {detail}
        </div>
      </div>
    </BorderBeam>
  );
});

/** "DC01", or "3 connections". */
function useTarget(ids: string[]): string {
  return useStore((s) => {
    if (ids.length !== 1) return `${ids.length} connections`;
    const c = s.connById[ids[0]];
    return c ? displayName(c) : ids[0];
  });
}

/** A command on one or more hosts (Launch Valor…): counts off as they answer, then says how
 *  it went, with each failure's reason. */
const ActivityRow = memo(function ActivityRow({ id }: { id: number }) {
  const a = useStore((s) => s.activities[id]);
  const target = useTarget(a?.ids ?? []);
  const failures = useStore(
    useShallow((s) =>
      (a?.failed ?? []).map((f) => {
        const c = s.connById[f.id];
        return `${c ? displayName(c) : f.id}: ${f.error}`;
      }),
    ),
  );
  const [expanded, setExpanded] = useState(false);
  if (!a) return null;

  const total = a.ids.length;
  const settled = a.ok.length + a.failed.length;
  const running = settled < total;
  const failed = a.failed.length;
  const state = running ? "sending" : failed ? "failed" : "done";
  const noop = !running && !failed && a.noop && a.idle.length === total;
  const label = running
    ? a.doing
    : failed
      ? a.ok.length
        ? `${a.done} on ${a.ok.length} of ${total}`
        : a.failedTitle
      : noop
        ? a.noop
        : a.done;
  const status = running ? (total > 1 ? `${settled}/${total}` : "…") : failed ? `${failed} failed` : "Done";
  const detail = running
    ? total > 1
      ? `${settled} of ${total} answered${failed ? ` · ${failed} failed` : ""}`
      : "Waiting for the host…"
    : failed
      ? failures[0] + (failed > 1 ? ` (and ${failed - 1} more)` : "")
      : total > 1
        ? `All ${total} connections`
        : "";

  return (
    <BorderBeam size="line" colorVariant="colorful" active={running} strength={0.85} className="xfer-beam">
      <div className={`xfer is-${state}`}>
        <div className="xfer__top">
          <span className="xfer__icon">
            {state === "done" ? (
              <Check size={13} />
            ) : state === "failed" ? (
              <AlertCircle size={13} />
            ) : a.icon ? (
              <img src={a.icon} alt="" className="xfer__logo" />
            ) : null}
          </span>
          <span className="xfer__label">{label}</span>
          {(running || !failed || !a.ok.length) && <span className="xfer__to">→ {target}</span>}
          <span className="xfer__status">{status}</span>
        </div>
        {running && total > 1 && (
          <div className="xfer__track">
            <span className="xfer__fill" style={{ width: `${(settled / total) * 100}%` }} />
          </div>
        )}
        {detail && (
          <div className="xfer__detail" title={failures.join("\n") || detail}>
            {detail}
            {failed > 1 && !running && (
              <button className="xfer__more" onClick={() => setExpanded((e) => !e)}>
                {expanded ? "Hide" : "Show all"}
              </button>
            )}
          </div>
        )}
        {expanded && failed > 1 && (
          <ul className="xfer__failures">
            {failures.map((f) => (
              <li key={f}>{f}</li>
            ))}
          </ul>
        )}
      </div>
    </BorderBeam>
  );
});

/** Files being sent to connections, and commands run on them: a glass shelf above the
 *  toasts, until cleared. */
export function TransfersPanel() {
  const ids = useStore(
    useShallow((s) =>
      Object.keys(s.transfers)
        .map(Number)
        .sort((a, b) => b - a),
    ),
  );
  const activityIds = useStore(
    useShallow((s) =>
      Object.keys(s.activities)
        .map(Number)
        .sort((a, b) => b - a),
    ),
  );
  const summary = useStore(
    useShallow((s) => {
      const all = Object.values(s.transfers);
      const acts = Object.values(s.activities);
      return {
        active: all.filter(isActive).length,
        failed: all.filter((t) => t.state === "failed").length,
        running: acts.filter((a) => !activityDone(a)).length,
        activityFailed: acts.filter((a) => a.failed.length > 0).length,
      };
    }),
  );
  const [collapsed, setCollapsed] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useGlassUnderlay(ref);

  const rows = ids.length + activityIds.length;
  const busy = summary.active + summary.running;
  const failures = summary.failed + summary.activityFailed;
  // A batch that all went through clears itself after a moment; failures stay to be read.
  useEffect(() => {
    if (!rows || busy || failures) return;
    const t = window.setTimeout(clearTransfers, LINGER);
    return () => window.clearTimeout(t);
  }, [rows, busy, failures]);

  if (!rows) return null;
  const finished = rows - busy;
  const title = summary.active
    ? `Sending to ${summary.active} host${summary.active === 1 ? "" : "s"}`
    : summary.running
      ? "Working…"
      : ids.length
        ? "Transfers"
        : "Activity";
  return (
    <div ref={ref} className="transfers glass" role="status" aria-live="polite">
      <div className="transfers__head">
        <span className="transfers__title">{title}</span>
        {finished > 0 && (
          <button className="transfers__clear" onClick={clearTransfers}>
            Clear
          </button>
        )}
        <button
          className="transfers__toggle"
          aria-label={collapsed ? "Show transfers" : "Hide transfers"}
          onClick={() => setCollapsed((c) => !c)}
        >
          {collapsed ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
        </button>
      </div>
      {!collapsed && (
        <div className="transfers__list">
          {activityIds.map((id) => (
            <ActivityRow key={`a${id}`} id={id} />
          ))}
          {ids.map((id) => (
            <TransferRow key={id} id={id} />
          ))}
        </div>
      )}
    </div>
  );
}
