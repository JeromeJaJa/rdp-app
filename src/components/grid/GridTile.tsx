import { memo, useCallback, useContext, useEffect, useRef, type CSSProperties } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { AlertCircle } from "lucide-react";
import { ConnectIcon } from "../common/SessionIcons";
import { RemoteThumbnail } from "../../engine";
import { connect, openMenu, select } from "../../state/actions";
import { displayName, isLive, useConnection, useStatus, useStore } from "../../state/store";
import { StatusDot, statusLabel } from "../common/StatusDot";
import { Spinner } from "../ui/ui";
import { TileObserverContext } from "./tileObserver";

/** The click that ends a drag must not also select (and open) the tile. */
let dragEndedAt = 0;
export function noteDragEnd() {
  dragEndedAt = performance.now();
}

/** A tile in the grid; drag to reorder (also across groups). */
function GridTileInner({ id }: { id: string }) {
  const visible = useStore((s) => s.visibleTiles.includes(id));
  const observer = useContext(TileObserverContext);
  const ref = useRef<HTMLDivElement | null>(null);
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id });
  const setRefs = useCallback(
    (el: HTMLDivElement | null) => {
      ref.current = el;
      setNodeRef(el);
    },
    [setNodeRef],
  );

  useEffect(() => {
    if (!observer || !ref.current) return;
    return observer.observe(ref.current, id);
  }, [observer, id]);

  const style: CSSProperties = { transform: CSS.Translate.toString(transform), transition };
  return (
    <TileCard
      id={id}
      showThumb={visible}
      nodeRef={setRefs}
      style={style}
      className={isDragging ? "is-dragging" : ""}
      {...attributes}
      {...listeners}
    />
  );
}

/** The floating copy that follows the pointer while a tile is dragged. */
export function GridTileOverlay({ id }: { id: string }) {
  return <TileCard id={id} showThumb className="is-overlay" />;
}

type TileCardProps = {
  id: string;
  showThumb: boolean;
  nodeRef?: (el: HTMLDivElement | null) => void;
  style?: CSSProperties;
  className?: string;
} & Omit<React.HTMLAttributes<HTMLDivElement>, "id" | "style" | "className">;

function TileCard({ id, showThumb, nodeRef, style, className, ...rest }: TileCardProps) {
  const conn = useConnection(id);
  const status = useStatus(id);
  const message = useStore((s) => (s.sessions[id]?.status === "error" ? s.sessions[id]?.message : undefined));
  const selected = useStore((s) => s.selectedId === id);

  if (!conn) return null;
  const live = isLive(status);
  const busy = status === "queued" || status === "connecting";
  const visible = showThumb;

  return (
    <div
      ref={nodeRef}
      style={style}
      {...rest}
      className={`tile ${live ? "is-live" : ""} ${selected ? "is-selected" : ""} ${className ?? ""}`}
      onClick={() => {
        if (performance.now() - dragEndedAt > 250) select(id, { connect: false });
      }}
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
