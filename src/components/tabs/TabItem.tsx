import { forwardRef, memo, useEffect, useRef, type CSSProperties, type HTMLAttributes } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { clearPicked, connect, disconnect, dragWindow, openMenu, pick, select } from "../../state/actions";
import {
  displayName,
  getState,
  hostOf,
  inDetachedWindow,
  isBusy,
  isLive,
  useConnection,
  useStatus,
  useStore,
} from "../../state/store";
import { isMainWindow } from "../../lib/env";
import { AppWindow, X, Zap } from "lucide-react";
import { StatusDot } from "../common/StatusDot";
import { ConnectIcon, DisconnectIcon } from "../common/SessionIcons";
import { TabStat } from "../vitals/Vitals";
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

/** Middle-click closes a tab, browser-style (the press is swallowed so it can't autoscroll). */
function middleClickCloses(onClose: (() => void) | undefined): HTMLAttributes<HTMLDivElement> {
  if (!onClose) return {};
  return {
    onMouseDown: (e) => {
      if (e.button === 1) e.preventDefault();
    },
    onAuxClick: (e) => {
      if (e.button !== 1) return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    },
  };
}

const TabView = forwardRef<HTMLDivElement, ViewProps>(function TabView(
  { id, variant, dragging, overlay, onClose, style, ...rest },
  ref,
) {
  const conn = useConnection(id);
  const status = useStatus(id);
  const selected = useStore((s) => s.selectedId === id);
  // Picked in the sidebar for an action on several connections (Ctrl/Shift+click).
  const picked = useStore((s) => variant === "side" && s.picked.includes(id));
  // In the library (sidebar): this connection already has an open tab somewhere.
  const open = useStore((s) => hostOf(s, id) !== null);
  // In the main window, a tab that lives in another window.
  const elsewhere = useStore((s) => isMainWindow() && inDetachedWindow(s, id));
  // A companion is attached on the remote (deeper integration is live).
  const companion = useStore((s) => !!s.sessions[id]?.companion);
  // The host reports its load: top tabs open the card on hover; sidebar rows show a stat
  // (unless stats are off) in place of the companion's mark.
  const stats = useStore((s) => !!s.stats[id]);
  const statShown = useStore((s) => variant === "side" && s.settings.tabStats !== "off");
  const vitals = stats && (variant === "top" || statShown);
  const card = stats && variant === "top" && !overlay;
  if (!conn) return null;

  const name = displayName(conn);
  const active = isLive(status) || isBusy(status);

  return (
    <div
      ref={ref}
      style={style as CSSProperties}
      className={`tab tab--${variant} ${selected ? "is-selected" : ""} ${picked ? "is-picked" : ""} ${dragging ? "is-dragging" : ""} ${
        overlay ? "is-overlay" : ""
      } ${elsewhere ? "is-elsewhere" : ""} ${variant === "side" && open ? "is-open" : ""}`}
      role="tab"
      aria-selected={selected}
      // The card names the host too, so the plain tooltip only shows without it.
      title={variant === "top" && !card ? `${name} — ${conn.host}` : undefined}
      data-tab-id={overlay ? undefined : id}
      data-vitals={card ? id : undefined}
      data-vitals-side={card ? "bottom" : undefined}
      data-vitals-delay={card ? 600 : undefined}
      {...rest}
    >
      <StatusDot status={status} />
      <span className="tab__name">{name}</span>
      {companion && !vitals && (
        <span className="tab__companion" title="Companion connected">
          <Zap size={11} />
        </span>
      )}
      {vitals && variant === "side" && !overlay && <TabStat id={id} variant="side" />}
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
      {...middleClickCloses(onClose)}
      tabIndex={-1}
      onClick={(e) => {
        // In the sidebar, Ctrl/Shift+click picks connections (as in Explorer) without
        // opening them; a plain click opens one and drops the picks.
        if (variant === "side" && (e.ctrlKey || e.metaKey || e.shiftKey)) {
          pick(id, { toggle: e.ctrlKey || e.metaKey, range: e.shiftKey });
          return;
        }
        clearPicked(id);
        select(id);
      }}
      onDoubleClick={(e) => {
        if (e.ctrlKey || e.metaKey || e.shiftKey) return;
        select(id, { connect: true });
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        const picks = getState().picked;
        // A right click on one of several picks acts on all of them.
        if (variant === "side" && picks.length > 1 && picks.includes(id)) {
          openMenu({ x: e.clientX, y: e.clientY, target: { kind: "connections", ids: picks } });
          return;
        }
        clearPicked(id);
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
      {...middleClickCloses(onClose)}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        const r = e.currentTarget.getBoundingClientRect();
        dragWindow({ x: e.clientX - r.left, w: r.width });
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openMenu({ x: e.clientX, y: e.clientY, target: { kind: "connection", id } });
      }}
    />
  );
}
