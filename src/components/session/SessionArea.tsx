import { memo, useCallback } from "react";
import { RemoteSurface } from "../../engine";
import { isLive, setState, useStatus, useStore } from "../../state/store";
import { EmptyState } from "./EmptyState";
import { IdleCard } from "./IdleCard";
import { SessionToolbar } from "./SessionToolbar";
import { Spinner } from "../ui/ui";
import "./session.css";

function LiveSession({ id }: { id: string }) {
  const status = useStatus(id);
  const onFocusChange = useCallback((focused: boolean) => setState({ remoteFocused: focused }), []);
  return (
    <div className="live-session">
      <RemoteSurface key={id} id={id} className="live-session__surface" onFocusChange={onFocusChange} />
      <SessionToolbar id={id} />
      {status === "reconnecting" && (
        <div className="live-session__overlay">
          <div className="live-session__pill">
            <Spinner />
            Reconnecting…
          </div>
        </div>
      )}
    </div>
  );
}

function SessionAreaInner() {
  const selectedId = useStore((s) => s.selectedId);
  const hasConnections = useStore((s) => s.order.length > 0);
  const status = useStore((s) => (s.selectedId ? s.sessions[s.selectedId]?.status ?? "idle" : "idle"));

  if (!hasConnections) return <EmptyState />;
  if (!selectedId)
    return (
      <div className="session-hint">
        <span>Select a connection</span>
      </div>
    );
  return isLive(status) ? <LiveSession id={selectedId} /> : <IdleCard key={selectedId} id={selectedId} />;
}

export const SessionArea = memo(SessionAreaInner);
