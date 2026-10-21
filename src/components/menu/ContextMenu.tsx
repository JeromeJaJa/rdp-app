import {
  createContext,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type ReactNode,
  type SetStateAction,
} from "react";
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
  Play,
  Plus,
  Power,
  RotateCw,
  Send,
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
  controlApp,
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
  openSendDialog,
  reconnect,
  showTabWindow,
  startRenameGroup,
  toggleConnectionMute,
} from "../../state/actions";
import { isMainWindow, isTauri, windowLabel } from "../../lib/env";
import { REMOTE_APPS } from "../../lib/remoteApps";
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

/** Which item of a menu (or submenu) has its submenu open: one at a time per level. */
const Level = createContext<[string | null, Dispatch<SetStateAction<string | null>>] | null>(null);

function LevelScope({ children }: { children: ReactNode }) {
  const state = useState<string | null>(null);
  return <Level.Provider value={state}>{children}</Level.Provider>;
}

/** How long a submenu stays after the pointer leaves its item: a slow or diagonal move
 *  towards it (over a gap, or a neighbouring item) still gets there. */
const CLOSE_DELAY = 300;

function Item({ icon, label, hint, danger, disabled, checked, onSelect, submenu }: ItemProps) {
  const key = useId();
  const level = useContext(Level);
  const [mine, setMine] = useState<string | null>(null);
  const [openKey, setOpenKey] = level ?? [mine, setMine];
  const open = openKey === key;
  const closing = useRef(0);
  const ref = useRef<HTMLDivElement>(null);
  const [flip, setFlip] = useState(false);

  useLayoutEffect(() => {
    if (!open || !ref.current) return;
    const r = ref.current.getBoundingClientRect();
    setFlip(r.right + 200 > window.innerWidth);
  }, [open]);
  useEffect(() => () => window.clearTimeout(closing.current), []);

  const show = () => {
    window.clearTimeout(closing.current);
    setOpenKey(key);
  };
  const hide = () => setOpenKey((k) => (k === key ? null : k));

  return (
    <div
      ref={ref}
      className={`menu__item ${danger ? "is-danger" : ""} ${disabled ? "is-disabled" : ""} ${open ? "is-open" : ""} ${
        submenu ? "has-sub" : ""
      } ${flip ? "is-flip" : ""}`}
      role="menuitem"
      aria-disabled={disabled}
      onMouseEnter={() => submenu && show()}
      onMouseLeave={() => {
        if (!submenu) return;
        window.clearTimeout(closing.current);
        closing.current = window.setTimeout(hide, CLOSE_DELAY);
      }}
      onClick={(e) => {
        e.stopPropagation();
        if (disabled) return;
        if (submenu) {
          if (open) hide();
          else show();
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
      <LevelScope>{children}</LevelScope>
    </div>
  );
}

const Sep = () => <div className="menu__sep" />;

/** Each remote program (Valor…), with Launch / Restart / Close for `ids`' hosts. */
function AppItems({ ids }: { ids: string[] }) {
  if (!ids.length || !REMOTE_APPS.length) return null;
  return (
    <>
      {REMOTE_APPS.map((app) => (
        <Item
          key={app.id}
          icon={<img src={app.icon} alt="" className="menu__app-icon" />}
          label={app.name}
          submenu={
            <>
              <Item icon={<Play size={14} />} label="Launch" onSelect={() => controlApp(ids, app, "launch")} />
              <Item icon={<RotateCw size={14} />} label="Restart" onSelect={() => controlApp(ids, app, "restart")} />
              <Item icon={<Power size={14} />} label="Close" onSelect={() => controlApp(ids, app, "close")} />
            </>
          }
        />
      ))}
      <Sep />
    </>
  );
}

/** Where the tab lives: its own window, back to the main one, or to the front. */
function WindowItems({ id }: { id: string }) {
  const host = useStore((s) => hostOf(s, id));
  // A detached window's only tab already has a window of its own.
  const alone = useStore((s) => !isMainWindow() && windowTabs(s, windowLabel()).length <= 1);
  if (!isTauri()) return null;
  const here = host === windowLabel();
  // No tab anywhere yet (tabs only for active connections): it can still get a window.
  const none = host === null;
  return (
    <>
      {(here || none) && !alone && (
        <Item icon={<AppWindow size={14} />} label="Open in new window" onSelect={() => openInNewWindow(id)} />
      )}
      {!here && !none && <Item icon={<AppWindow size={14} />} label="Show window" onSelect={() => showTabWindow(id)} />}
      {!none && host !== "main" && (
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
      <AppItems ids={[id]} />
      <Item icon={<Send size={14} />} label="Send files…" onSelect={() => openSendDialog([id])} />
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
      <AppItems ids={members} />
      {members.length > 0 && (
        <Item icon={<Send size={14} />} label="Send files to group…" onSelect={() => openSendDialog(groupMembers(id))} />
      )}
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

/** Several connections picked in the sidebar: what can be done to all of them at once. */
function SelectionMenu({ ids: all }: { ids: string[] }) {
  // Picks of connections deleted meanwhile drop out.
  const known = useStore((s) => s.connById);
  const ids = useMemo(() => all.filter((id) => known[id]), [all, known]);
  const groups = useStore((s) => s.groups);
  const { canConnect, canDisconnect } = useBulkActions(ids);
  const count = `${ids.length} connection${ids.length === 1 ? "" : "s"}`;
  return (
    <>
      <div className="menu__title">{count}</div>
      {canConnect && <Item icon={<ConnectIcon size={13} />} label="Connect" onSelect={() => connectAll(ids)} />}
      {canDisconnect && (
        <Item icon={<DisconnectIcon size={13} />} label="Disconnect" onSelect={() => disconnectAll(ids)} />
      )}
      {(canConnect || canDisconnect) && <Sep />}
      <AppItems ids={ids} />
      <Item icon={<Send size={14} />} label={`Send files to ${count}…`} onSelect={() => openSendDialog(ids)} />
      <Item
        icon={<FolderInput size={14} />}
        label="Move to group"
        submenu={
          <>
            {groups.map((g) => (
              <Item key={g.id} label={g.name} onSelect={() => moveToGroup(ids, g.id)} />
            ))}
            <Sep />
            <Item
              icon={<FolderPlus size={14} />}
              label="New group…"
              onSelect={() => {
                const gid = createGroup("New group", { rename: true });
                moveToGroup(ids, gid);
              }}
            />
          </>
        }
      />
      <Sep />
      <Item
        icon={<Trash2 size={14} />}
        label={`Delete ${count}…`}
        danger
        onSelect={() =>
          confirm({
            title: `Delete ${count}?`,
            message: "They and their saved passwords will be removed. This can't be undone.",
            confirmLabel: "Delete",
            danger: true,
            onConfirm: () => deleteConnections(ids),
          })
        }
      />
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
      <LevelScope>
        {menu.target.kind === "connection" ? (
          <ConnectionMenu id={menu.target.id} />
        ) : menu.target.kind === "group" ? (
          <GroupMenu id={menu.target.id} />
        ) : (
          <SelectionMenu ids={menu.target.ids} />
        )}
      </LevelScope>
    </div>
  );
}

export function ContextMenu() {
  const menu = useStore((s) => s.menu);
  if (!menu) return null;
  const target = menu.target.kind === "connections" ? menu.target.ids.join(",") : menu.target.id;
  return <MenuPanel key={`${menu.x},${menu.y},${target}`} menu={menu} />;
}
