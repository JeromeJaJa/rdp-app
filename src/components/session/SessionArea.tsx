import { memo, useCallback } from "react";
import { AppWindow, Undo2 } from "lucide-react";
import { RemoteSurface } from "../../engine";
import { isMainWindow } from "../../lib/env";
import { moveToMainWindow, showTabWindow } from "../../state/actions";
import { displayName, inDetachedWindow, isLive, setState, useConnection, useStatus, useStore } from "../../state/store";
import { Button, Spinner } from "../ui/ui";
import { EmptyState } from "./EmptyState";
import { IdleCard } from "./IdleCard";
import { SessionToolbar } from "./SessionToolbar";
import "./session.css";

function LiveSession({ id }: { id: string }) {
  const status = useStatus(id);
  const fullscreen = useStore((s) => s.fullscreen);
  const onFocusChange = useCallback((focused: boolean) => setState({ remoteFocused: focused }), []);
  return (
    <div className="live-session">
      <RemoteSurface
        key={id}
        id={id}
        className="live-session__surface"
        onFocusChange={onFocusChange}
        sendWinKey={fullscreen}
      />
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

/** The main window's stand-in for a tab that was dragged into another window. */
function ElsewhereCard({ id }: { id: string }) {
  const conn = useConnection(id);
  if (!conn) return null;
  return (
    <div className="idle">
      <div className="idle__card">
        <div className="idle__glyph">
          <AppWindow size={22} strokeWidth={1.5} />
        </div>
        <h1 className="idle__name">{displayName(conn)}</h1>
        <div className="idle__meta">
          <span>Open in another window</span>
        </div>
        <div className="idle__actions">
          <Button variant="primary" size="lg" icon={<AppWindow size={14} />} onClick={() => showTabWindow(id)}>
            Show window
          </Button>
          <Button variant="ghost" icon={<Undo2 size={13} />} onClick={() => moveToMainWindow(id)}>
            Bring back here
          </Button>
        </div>
      </div>
    </div>
  );
}

function SessionAreaInner() {
  const selectedId = useStore((s) => s.selectedId);
  const hasConnections = useStore((s) => s.order.length > 0);
  const status = useStore((s) => (s.selectedId ? s.sessions[s.selectedId]?.status ?? "idle" : "idle"));
  const elsewhere = useStore((s) => !!s.selectedId && isMainWindow() && inDetachedWindow(s, s.selectedId));

  if (!hasConnections) return <EmptyState />;
  if (!selectedId)
    return (
      <div className="session-hint">
        <span>Select a connection</span>
      </div>
    );
  if (elsewhere) return <ElsewhereCard key={selectedId} id={selectedId} />;
  return isLive(status) ? <LiveSession id={selectedId} /> : <IdleCard key={selectedId} id={selectedId} />;
}

export const SessionArea = memo(SessionAreaInner);
