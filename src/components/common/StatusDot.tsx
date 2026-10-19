import type { SessionStatus } from "../../lib/types";
import "./status.css";

const LABEL: Record<SessionStatus, string> = {
  idle: "Disconnected",
  queued: "Queued",
  connecting: "Connecting",
  connected: "Connected",
  reconnecting: "Reconnecting",
  error: "Error",
};

export function statusLabel(s: SessionStatus): string {
  return LABEL[s];
}

export function StatusDot({ status, className }: { status: SessionStatus; className?: string }) {
  return <span className={`status-dot status-dot--${status} ${className ?? ""}`} aria-label={LABEL[status]} />;
}
