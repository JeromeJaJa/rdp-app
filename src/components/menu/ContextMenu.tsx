import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  AppWindow,
  ArrowDownToLine,
  ArrowUpFromLine,
  Check,
  ChevronRight,
  Copy,
  FolderInput,
  FolderPlus,
  ListRestart,
  Network,
  Pencil,
  Plus,
  RotateCw,
  SlidersHorizontal,
  Trash2,
  Undo2,
  Volume2,
  VolumeX,
} from "lucide-react";
import {
  closeMenu,
  confirm,
  connect,
  connectAll,
  createGroup,
  deleteConnections,
  disconnect,
  disconnectAll,
  duplicateConnection,
  groupMembers,
  moveToGroup,
  moveToMainWindow,
  openDialog,
  openInNewWindow,
  reconnect,
  showTabWindow,
  startRenameGroup,
  toggleConnectionMute,
} from "../../state/actions";
import { isMainWindow, isTauri, windowLabel } from "../../lib/env";
import { FEATURES } from "../../lib/features";
import {
  displayName,
  hostOf,
  isBusy,
  isLive,
  type MenuState,
  resolveSetting,
  useBulkActions,
  useStore,
  windowTabs,
} from "../../state/store";
import { ConnectIcon, DisconnectIcon } from "../common/SessionIcons";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";
import "./menu.css";

interface ItemProps {
  icon?: ReactNode;
  label: string;
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
  checked?: boolean;
  onSelect?: () => void;
  submenu?: ReactNode;
}

function Item({ icon, label, hint, danger, disabled, checked, onSelect, submenu }: ItemProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const [flip, setFlip] = useState(false);

  useLayoutEffect(() => {
    if (!open || !ref.current) return;
    const r = ref.current.getBoundingClientRect();
    setFlip(r.right + 200 > window.innerWidth);
  }, [open]);

  return (
    <div
      ref={ref}
      className={`menu__item ${danger ? "is-danger" : ""} ${disabled ? "is-disabled" : ""} ${open ? "is-open" : ""}`}
      role="menuitem"
      aria-disabled={disabled}
      onMouseEnter={() => submenu && setOpen(true)}
      onMouseLeave={() => submenu && setOpen(false)}
      onClick={(e) => {
        e.stopPropagation();
        if (disabled) return;
        if (submenu) {
          setOpen((o) => !o);
          return;
        }
        closeMenu();
        onSelect?.();
      }}
    >
      <span className="menu__icon">{checked ? <Check size={14} /> : icon}</span>
      <span className="menu__label">{label}</span>
      {hint && <span className="menu__hint">{hint}</span>}
      {submenu && <ChevronRight size={13} className="menu__chev" />}
      {submenu && open && <Submenu flip={flip}>{submenu}</Submenu>}
    </div>
  );
}

function Submenu({ flip, children }: { flip: boolean; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  useGlassUnderlay(ref);
  return (
    <div ref={ref} className={`menu menu--sub glass ${flip ? "is-flipped" : ""}`}>
      {children}
    </div>
  );
}

const Sep = () => <div className="menu__sep" />;

/** Where the tab lives: its own window, back to the main one, or to the front. */
function WindowItems({ id }: { id: string }) {
  const host = useStore((s) => hostOf(s, id));
  // A detached window's only tab already has a window of its own.
  const alone = useStore((s) => !isMainWindow() && windowTabs(s, windowLabel()).length <= 1);
  if (!isTauri()) return null;
  const here = host === windowLabel();
  return (
    <>
      {here && !alone && (
        <Item icon={<AppWindow size={14} />} label="Open in new window" onSelect={() => openInNewWindow(id)} />
      )}
      {!here && <Item icon={<AppWindow size={14} />} label="Show window" onSelect={() => showTabWindow(id)} />}
      {host !== "main" && (
        <Item icon={<Undo2 size={14} />} label="Move to main window" onSelect={() => moveToMainWindow(id)} />
      )}
      <Sep />
    </>
  );
}

function ConnectionMenu({ id }: { id: string }) {
  const conn = useStore((s) => s.connById[id]);
  const status = useStore((s) => s.sessions[id]?.status ?? "idle");
  const groups = useStore((s) => s.groups);
  const muted = useStore((s) => resolveSetting(s, id, "audio") === "off");
  if (!conn) return null;
  const active = isLive(status) || isBusy(status);

  return (
    <>
      {active ? (
        <Item icon={<DisconnectIcon size={13} />} label="Disconnect" onSelect={() => disconnect([id])} />
      ) : (
        <Item icon={<ConnectIcon size={13} />} label="Connect" hint="↵" onSelect={() => connect([id])} />
      )}
      {(active || status === "error") && (
        <Item icon={<RotateCw size={14} />} label="Reconnect" onSelect={() => reconnect(id)} />
      )}
      <Sep />
      <WindowItems id={id} />
      <Item icon={<Pencil size={14} />} label="Edit…" onSelect={() => openDialog({ kind: "edit", id })} />
      <Item icon={<Copy size={14} />} label="Duplicate" onSelect={() => duplicateConnection(id)} />
      <Item
        icon={muted ? <Volume2 size={14} /> : <VolumeX size={14} />}
        label={muted ? "Unmute" : "Mute"}
        onSelect={() => toggleConnectionMute(id)}
      />
      <Item
        icon={<FolderInput size={14} />}
        label="Move to group"
        submenu={
          <>
            {groups.map((g) => (
              <Item key={g.id} label={g.name} checked={conn.groupId === g.id} onSelect={() => moveToGroup([id], g.id)} />
            ))}
            <Sep />
            <Item
              icon={<FolderPlus size={14} />}
              label="New group…"
              onSelect={() => {
                const gid = createGroup("New group", { rename: true });
                moveToGroup([id], gid);
              }}
            />
          </>
        }
      />
      <Sep />
      <Item
        icon={<Trash2 size={14} />}
        label="Delete"
        danger
        onSelect={() =>
          confirm({
            title: `Delete ${displayName(conn)}?`,
            message: "The connection and its saved password will be removed. This can't be undone.",
            confirmLabel: "Delete",
            danger: true,
            onConfirm: () => deleteConnections([id]),
          })
        }
      />
    </>
  );
}

function GroupMenu({ id }: { id: string }) {
  const group = useStore((s) => s.groups.find((g) => g.id === id));
  // The menu is short-lived: membership as of opening it is fine.
  const members = useMemo(() => groupMembers(id), [id]);
  const { canConnect, canDisconnect } = useBulkActions(members);
  if (!group) return null;
  return (
    <>
      {canConnect && (
        <Item icon={<ConnectIcon size={13} />} label="Connect all in group" onSelect={() => connectAll(groupMembers(id))} />
      )}
      {canDisconnect && (
        <Item
          icon={<DisconnectIcon size={13} />}
          label="Disconnect all in group"
          onSelect={() => disconnectAll(groupMembers(id))}
        />
      )}
      {(canConnect || canDisconnect) && <Sep />}
      <Item icon={<Plus size={14} />} label="Add connections…" onSelect={() => openDialog({ kind: "add", mode: "bulk", groupId: id })} />
      <Item icon={<ListRestart size={14} />} label="Replace connections…" onSelect={() => openDialog({ kind: "replace", groupId: id })} />
      <Item icon={<ArrowDownToLine size={14} />} label="Import into group…" onSelect={() => openDialog({ kind: "import", groupId: id })} />
      <Item icon={<ArrowUpFromLine size={14} />} label="Export group…" onSelect={() => openDialog({ kind: "export", groupIds: [id] })} />
      <Sep />
      <Item
        icon={<SlidersHorizontal size={14} />}
        label="Group settings…"
        hint={Object.keys(group.settings ?? {}).filter((k) => k !== "drivePaths").length ? "Changed" : undefined}
        onSelect={() => openDialog({ kind: "groupSettings", groupId: id })}
      />
      {FEATURES.gateway && (
        <Item
          icon={<Network size={14} />}
          label="Gateway…"
          hint={group.gateway?.enabled && group.gateway.host ? "On" : undefined}
          onSelect={() => openDialog({ kind: "gateway", groupId: id })}
        />
      )}
      {!group.builtin && <Item icon={<Pencil size={14} />} label="Rename" onSelect={() => startRenameGroup(id)} />}
      {!group.builtin && (
        <>
          <Sep />
          <Item
            icon={<Trash2 size={14} />}
            label="Delete group…"
            danger
            onSelect={() => openDialog({ kind: "deleteGroup", groupId: id })}
          />
        </>
      )}
    </>
  );
}

function MenuPanel({ menu }: { menu: MenuState }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x: menu.x, y: menu.y, ready: false });
  useGlassUnderlay(ref);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const x = Math.min(menu.x, window.innerWidth - r.width - 8);
    const y = menu.y + r.height > window.innerHeight - 8 ? Math.max(8, menu.y - r.height) : menu.y;
    setPos({ x, y, ready: true });
  }, [menu.x, menu.y]);

  useEffect(() => {
    const close = () => closeMenu();
    const onWheel = (e: WheelEvent) => {
      if (!ref.current?.contains(e.target as Node)) closeMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        closeMenu();
      }
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("wheel", onWheel, { passive: true });
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("wheel", onWheel);
    };
  }, []);

  return (
    <div
      ref={ref}
      className="menu glass"
      role="menu"
      style={{ left: pos.x, top: pos.y, visibility: pos.ready ? "visible" : "hidden" }}
      onMouseDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {menu.target.kind === "connection" ? <ConnectionMenu id={menu.target.id} /> : <GroupMenu id={menu.target.id} />}
    </div>
  );
}

export function ContextMenu() {
  const menu = useStore((s) => s.menu);
  if (!menu) return null;
  return <MenuPanel key={`${menu.x},${menu.y},${menu.target.id}`} menu={menu} />;
}
