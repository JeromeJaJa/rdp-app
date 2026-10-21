import { forwardRef, memo, useEffect, useRef, type CSSProperties, type HTMLAttributes } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { connect, disconnect, dragWindow, openMenu, select } from "../../state/actions";
import { displayName, hostOf, isBusy, isLive, useConnection, useStatus, useStore } from "../../state/store";
import { isMainWindow } from "../../lib/env";
import { AppWindow, X, Zap } from "lucide-react";
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
  /** Shows an × that calls this (closes the tab and disconnects). */
  onClose?: () => void;
}

const TabView = forwardRef<HTMLDivElement, ViewProps>(function TabView(
  { id, variant, dragging, overlay, onClose, style, ...rest },
  ref,
) {
  const conn = useConnection(id);
  const status = useStatus(id);
  const selected = useStore((s) => s.selectedId === id);
  // In the library (sidebar): this connection already has an open tab somewhere.
  const open = useStore((s) => hostOf(s, id) !== null);
  // In the main window, a tab that lives in another window.
  const elsewhere = useStore((s) => isMainWindow() && hostOf(s, id) !== "main");
  // A companion is attached on the remote (deeper integration is live).
  const companion = useStore((s) => !!s.sessions[id]?.companion);
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
      } ${elsewhere ? "is-elsewhere" : ""} ${variant === "side" && open ? "is-open" : ""}`}
      role="tab"
      aria-selected={selected}
      title={variant === "top" ? `${name} — ${conn.host}` : undefined}
      data-tab-id={overlay ? undefined : id}
      {...rest}
    >
      <StatusDot status={status} />
      <span className="tab__name">{name}</span>
      {companion && (
        <span className="tab__companion" title="Companion connected">
          <Zap size={11} />
        </span>
      )}
      {elsewhere && (
        <span className="tab__elsewhere" title="Open in another window">
          <AppWindow size={12} />
        </span>
      )}
      {onClose && !overlay && (
        <button
          className="tab__close"
          aria-label="Close tab and disconnect"
          title="Close tab (disconnects)"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onClose();
          }}
        >
          <X size={12} />
        </button>
      )}
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

function SortableTab({ id, variant, onClose }: { id: ConnectionId; variant: Variant; onClose?: () => void }) {
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
      onClose={onClose}
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

/** A detached window's only tab: dragging it moves the window (Chrome-style). */
export function SoloTab({ id, onClose }: { id: ConnectionId; onClose: () => void }) {
  return (
    <TabView
      id={id}
      variant="top"
      onClose={onClose}
      onPointerDown={(e) => {
        if (e.button === 0) dragWindow();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openMenu({ x: e.clientX, y: e.clientY, target: { kind: "connection", id } });
      }}
    />
  );
}
