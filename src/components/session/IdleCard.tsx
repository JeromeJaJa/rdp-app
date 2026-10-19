import { useEffect } from "react";
import { AlertCircle, Pencil } from "lucide-react";
import { connect, disconnect, openDialog } from "../../state/actions";
import { displayName, getState, userLabel, useConnection, useSession } from "../../state/store";
import { Button, Kbd, Spinner } from "../ui/ui";
import { ConnectIcon } from "../common/SessionIcons";

export function IdleCard({ id }: { id: string }) {
  const conn = useConnection(id);
  const session = useSession(id);
  const { status, message } = session;
  const busy = status === "queued" || status === "connecting";

  // Enter connects (only when nothing else has focus/overlays).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || e.ctrlKey || e.altKey || e.metaKey) return;
      const s = getState();
      if (s.dialog || s.menu || s.remoteFocused) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      e.preventDefault();
      connect([id]);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [id]);

  if (!conn) return null;

  return (
    <div className="idle">
      <div className="idle__card">
        <div className={`idle__glyph idle__glyph--${status}`}>
          {busy ? (
            <Spinner size={20} />
          ) : (
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" aria-hidden>
              <rect x="2.5" y="4" width="19" height="13" rx="2.5" stroke="currentColor" strokeWidth="1.5" />
              <path d="M8.5 20.5h7M12 17v3.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
            </svg>
          )}
        </div>
        <h1 className="idle__name">{displayName(conn)}</h1>
        <div className="idle__meta">
          <span className="mono">
            {conn.host}
            {conn.port !== 3389 ? `:${conn.port}` : ""}
          </span>
          {conn.username && (
            <>
              <span className="idle__dot">·</span>
              <span>{userLabel(conn)}</span>
            </>
          )}
          {!conn.hasPassword && (
            <>
              <span className="idle__dot">·</span>
              <span className="idle__nopass">no saved password</span>
            </>
          )}
        </div>

        {status === "error" && message && (
          <div className="idle__error">
            <AlertCircle size={14} />
            <span>{message}</span>
          </div>
        )}

        <div className="idle__actions">
          {busy ? (
            <>
              <div className="idle__progress">{status === "queued" ? "Waiting for a free slot…" : "Connecting…"}</div>
              <Button variant="ghost" onClick={() => disconnect([id])}>
                Cancel
              </Button>
            </>
          ) : (
            <>
              <Button
                variant="primary"
                size="lg"
                className="idle__connect"
                icon={<ConnectIcon size={14} plain />}
                onClick={() => connect([id])}
              >
                {status === "error" ? "Try again" : "Connect"}
                <Kbd>↵</Kbd>
              </Button>
              <Button variant="ghost" icon={<Pencil size={13} />} onClick={() => openDialog({ kind: "edit", id })}>
                Edit
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
