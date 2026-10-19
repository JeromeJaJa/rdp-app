import { memo, useContext, useEffect, useRef } from "react";
import { AlertCircle } from "lucide-react";
import { ConnectIcon } from "../common/SessionIcons";
import { RemoteThumbnail } from "../../engine";
import { connect, openMenu, select } from "../../state/actions";
import { displayName, isLive, useConnection, useStatus, useStore } from "../../state/store";
import { StatusDot, statusLabel } from "../common/StatusDot";
import { Spinner } from "../ui/ui";
import { TileObserverContext } from "./tileObserver";

function GridTileInner({ id }: { id: string }) {
  const conn = useConnection(id);
  const status = useStatus(id);
  const message = useStore((s) => (s.sessions[id]?.status === "error" ? s.sessions[id]?.message : undefined));
  const visible = useStore((s) => s.visibleTiles.includes(id));
  const selected = useStore((s) => s.selectedId === id);
  const observer = useContext(TileObserverContext);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!observer || !ref.current) return;
    return observer.observe(ref.current, id);
  }, [observer, id]);

  if (!conn) return null;
  const live = isLive(status);
  const busy = status === "queued" || status === "connecting";

  return (
    <div
      ref={ref}
      className={`tile ${live ? "is-live" : ""} ${selected ? "is-selected" : ""}`}
      onClick={() => select(id, { connect: false })}
      onContextMenu={(e) => {
        e.preventDefault();
        openMenu({ x: e.clientX, y: e.clientY, target: { kind: "connection", id } });
      }}
    >
      <div className="tile__screen">
        {live && visible ? (
          <RemoteThumbnail id={id} className="tile__thumb" />
        ) : live ? null : (
          <div className={`tile__placeholder tile__placeholder--${status}`}>
            {busy ? (
              <span className="tile__state">
                <Spinner size={13} />
                {statusLabel(status)}
              </span>
            ) : status === "error" ? (
              <span className="tile__state tile__state--error" title={message}>
                <AlertCircle size={13} />
                Failed
              </span>
            ) : (
              <span className="tile__state">Disconnected</span>
            )}
            {!busy && (
              <button
                className="tile__connect"
                onClick={(e) => {
                  e.stopPropagation();
                  connect([id]);
                }}
              >
                <ConnectIcon size={12} plain />
                {status === "error" ? "Retry" : "Connect"}
              </button>
            )}
          </div>
        )}
        {status === "reconnecting" && (
          <div className="tile__overlay">
            <Spinner size={13} />
          </div>
        )}
      </div>
      <div className="tile__meta">
        <StatusDot status={status} />
        <span className="tile__name">{displayName(conn)}</span>
        {conn.name.trim() && conn.name.trim() !== conn.host && <span className="tile__host mono">{conn.host}</span>}
      </div>
    </div>
  );
}

export const GridTile = memo(GridTileInner);
