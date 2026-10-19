import { forwardRef, memo, useEffect, useRef, type CSSProperties, type HTMLAttributes } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { connect, disconnect, openMenu, select } from "../../state/actions";
import { displayName, isBusy, isLive, useConnection, useStatus, useStore } from "../../state/store";
import { StatusDot } from "../common/StatusDot";
import { ConnectIcon, DisconnectIcon } from "../common/SessionIcons";
import type { ConnectionId } from "../../lib/types";
import "./tabs.css";

type Variant = "side" | "top";

interface ViewProps extends HTMLAttributes<HTMLDivElement> {
  id: ConnectionId;
  variant: Variant;
  dragging?: boolean;
  overlay?: boolean;
}

const TabView = forwardRef<HTMLDivElement, ViewProps>(function TabView(
  { id, variant, dragging, overlay, style, ...rest },
  ref,
) {
  const conn = useConnection(id);
  const status = useStatus(id);
  const selected = useStore((s) => s.selectedId === id);
  if (!conn) return null;

  const name = displayName(conn);
  const showHost = conn.name.trim() !== "" && conn.name.trim() !== conn.host;
  const active = isLive(status) || isBusy(status);

  return (
    <div
      ref={ref}
      style={style as CSSProperties}
      className={`tab tab--${variant} ${selected ? "is-selected" : ""} ${dragging ? "is-dragging" : ""} ${
        overlay ? "is-overlay" : ""
      }`}
      role="tab"
      aria-selected={selected}
      title={variant === "top" ? `${name} — ${conn.host}` : undefined}
      {...rest}
    >
      <StatusDot status={status} />
      <span className="tab__name">{name}</span>
      {variant === "side" && (
        <>
          {showHost && <span className="tab__host mono">{conn.host}</span>}
          {!overlay && (
            <span className="tab__actions">
              <button
                className="tab__action"
                aria-label={active ? "Disconnect" : "Connect"}
                title={active ? "Disconnect" : "Connect"}
                onPointerDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  if (active) disconnect([id]);
                  else connect([id]);
                }}
              >
                {active ? <DisconnectIcon size={12} /> : <ConnectIcon size={12} />}
              </button>
            </span>
          )}
        </>
      )}
    </div>
  );
});

function SortableTab({ id, variant }: { id: ConnectionId; variant: Variant }) {
  const { setNodeRef, attributes, listeners, transform, transition, isDragging } = useSortable({ id });
  const selected = useStore((s) => s.selectedId === id);
  const elRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (selected) elRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [selected]);

  return (
    <TabView
      ref={(el) => {
        elRef.current = el;
        setNodeRef(el);
      }}
      id={id}
      variant={variant}
      dragging={isDragging}
      style={{ transform: CSS.Translate.toString(transform), transition }}
      {...attributes}
      {...listeners}
      tabIndex={-1}
      onClick={() => select(id)}
      onDoubleClick={() => select(id, { connect: true })}
      onContextMenu={(e) => {
        e.preventDefault();
        select(id, { connect: false });
        openMenu({ x: e.clientX, y: e.clientY, target: { kind: "connection", id } });
      }}
    />
  );
}

export const TabItem = memo(SortableTab);

export function TabOverlay({ id, variant }: { id: ConnectionId; variant: Variant }) {
  return <TabView id={id} variant={variant} overlay />;
}
