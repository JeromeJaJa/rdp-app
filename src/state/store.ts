import { create } from "zustand";
import {
  DEFAULT_GROUP_ID,
  DEFAULT_SETTINGS,
  type Connection,
  type ConnectionId,
  type DockPreview,
  type Group,
  type GroupId,
  type SessionStatus,
  type OverridableKey,
  type Settings,
  type WindowTabs,
} from "../lib/types";

export interface SessionInfo {
  status: SessionStatus;
  message?: string;
  width?: number;
  height?: number;
  codec?: string;
  /** Set while a companion on the remote is attached (its version and host name). */
  companion?: { version: string; host: string };
}

export type View = "session" | "grid";

export type SettingsSection = "general" | "connections" | "graphics" | "devices" | "security" | "automation";

export type DialogState =
  | { kind: "add"; mode: "single" | "bulk"; groupId?: GroupId }
  | { kind: "edit"; id: ConnectionId }
  | { kind: "settings"; section?: SettingsSection }
  | { kind: "replace"; groupId: GroupId }
  | { kind: "import"; groupId?: GroupId }
  | { kind: "export"; groupIds?: GroupId[] }
  | { kind: "gateway"; groupId: GroupId }
  | { kind: "groupSettings"; groupId: GroupId }
  | { kind: "deleteGroup"; groupId: GroupId }
  | {
      kind: "confirm";
      title: string;
      message: string;
      confirmLabel: string;
      danger?: boolean;
      onConfirm: () => void;
    }
  | null;

export type MenuTarget = { kind: "connection"; id: ConnectionId } | { kind: "group"; id: GroupId };

export interface MenuState {
  x: number;
  y: number;
  target: MenuTarget;
}

/** A server certificate waiting for the user's decision (from a `certificate` session event). */
export interface CertPrompt {
  id: ConnectionId;
  endpoint: string;
  fingerprint: string;
  reason: "unknown" | "changed";
  previousFingerprint?: string;
  subject?: string;
}

export interface Toast {
  id: number;
  tone: "info" | "success" | "error";
  title: string;
  message?: string;
  /** One follow-up button (e.g. "Reconnect"). */
  action?: { label: string; run: () => void };
  /** Stays until acted on or dismissed (for prompts that need a decision). */
  sticky?: boolean;
}

export interface AppState {
  ready: boolean;
  order: ConnectionId[];
  connById: Record<ConnectionId, Connection>;
  groups: Group[];
  settings: Settings;
  sessions: Record<ConnectionId, SessionInfo>;

  selectedId: ConnectionId | null;
  view: View;
  dialog: DialogState;
  menu: MenuState | null;
  /** The command palette (Ctrl+K) is open. */
  paletteOpen: boolean;
  /** The main window's open tabs, in order (connections you've opened). */
  mainTabs: ConnectionId[];
  /** Windows that tabs were dragged out into. */
  windows: WindowTabs[];
  /** The left library sidebar is shown (main window only). */
  sidebarShown: boolean;
  /** A tab is being dragged over this window's strip: which tab, and where it would land. */
  dockPreview: DockPreview | null;
  toasts: Toast[];
  fullscreen: boolean;
  /** Keyboard focus is inside a RemoteSurface: app shortcuts are suspended. */
  remoteFocused: boolean;
  /** Grid tiles currently intersecting the viewport. */
  visibleTiles: ConnectionId[];
  /** Group currently being renamed inline in the sidebar. */
  renamingGroupId: GroupId | null;
  /** Sidebar filter text. */
  filter: string;
  /** Certificates awaiting a trust decision, oldest first (deduped by endpoint). */
  certPrompts: CertPrompt[];
}

export const useStore = create<AppState>(() => ({
  ready: false,
  order: [],
  connById: {},
  groups: [],
  settings: { ...DEFAULT_SETTINGS },
  sessions: {},

  selectedId: null,
  view: "session",
  dialog: null,
  menu: null,
  paletteOpen: false,
  mainTabs: [],
  windows: [],
  sidebarShown: true,
  dockPreview: null,
  toasts: [],
  fullscreen: false,
  remoteFocused: false,
  visibleTiles: [],
  renamingGroupId: null,
  filter: "",
  certPrompts: [],
}));

export const getState = useStore.getState;
export const setState = useStore.setState;

const IDLE: SessionInfo = { status: "idle" };

export function useConnection(id: ConnectionId): Connection | undefined {
  return useStore((s) => s.connById[id]);
}

export function useSession(id: ConnectionId): SessionInfo {
  return useStore((s) => s.sessions[id] ?? IDLE);
}

export function useStatus(id: ConnectionId): SessionStatus {
  return useStore((s) => s.sessions[id]?.status ?? "idle");
}

export function sessionOf(id: ConnectionId): SessionInfo {
  return getState().sessions[id] ?? IDLE;
}

export function isLive(status: SessionStatus): boolean {
  return status === "connected" || status === "reconnecting";
}

export function isBusy(status: SessionStatus): boolean {
  return status === "queued" || status === "connecting" || status === "reconnecting";
}

/** Whether "connect all" / "disconnect all" would do anything for these connections. */
export function useBulkActions(ids: ConnectionId[]): { canConnect: boolean; canDisconnect: boolean } {
  const mask = useStore((s) => {
    let m = 0;
    for (const id of ids) {
      const st = s.sessions[id]?.status ?? "idle";
      if (st === "idle" || st === "error") m |= 1;
      if (isLive(st) || isBusy(st)) m |= 2;
      if (m === 3) break;
    }
    return m;
  });
  return { canConnect: (mask & 1) !== 0, canDisconnect: (mask & 2) !== 0 };
}

/** Name, or the address when unnamed (with the port when it is not the default). */
/** A connection's effective setting: its own override, its group's, or the app's. */
export function resolveSetting<K extends OverridableKey>(s: AppState, id: ConnectionId, key: K): Settings[K] {
  const c = s.connById[id];
  const own = c?.settings?.[key];
  if (own !== undefined) return own as Settings[K];
  const fromGroup = c ? s.groups.find((g) => g.id === c.groupId)?.settings?.[key] : undefined;
  if (fromGroup !== undefined) return fromGroup as Settings[K];
  return s.settings[key];
}

export function displayName(c: Pick<Connection, "name" | "host"> & { port?: number }): string {
  const name = c.name.trim();
  if (name) return name;
  return c.port && c.port !== 3389 ? `${c.host}:${c.port}` : c.host;
}

export function userLabel(c: Pick<Connection, "username" | "domain">): string {
  return c.domain ? `${c.domain}\\${c.username}` : c.username;
}

export interface Section {
  groupId: GroupId;
  ids: ConnectionId[];
}

/** The built-in "All" group, used if a library somehow lacks it. */
export const FALLBACK_DEFAULT_GROUP: Group = {
  id: DEFAULT_GROUP_ID,
  name: "All",
  collapsed: false,
  builtin: true,
  gateway: null,
};

/** Groups in display order, guaranteed to contain the built-in "All" group. */
export function withDefaultGroup(groups: Group[]): Group[] {
  return groups.some((g) => g.id === DEFAULT_GROUP_ID) ? groups : [FALLBACK_DEFAULT_GROUP, ...groups];
}

/** Display sections: one per group, in group order. Unknown group ids fall back to "All". */
export function buildSections(order: ConnectionId[], connById: Record<string, Connection>, groups: Group[]): Section[] {
  const all = withDefaultGroup(groups);
  const buckets = new Map<GroupId, ConnectionId[]>();
  for (const g of all) buckets.set(g.id, []);
  for (const id of order) {
    const c = connById[id];
    if (!c) continue;
    (buckets.get(c.groupId) ?? buckets.get(DEFAULT_GROUP_ID)!).push(id);
  }
  return all.map((g) => ({ groupId: g.id, ids: buckets.get(g.id)! }));
}

/** Effective group of a connection (unknown ids count as "All"). */
export function groupOf(c: Connection, groups: Group[]): GroupId {
  return groups.some((g) => g.id === c.groupId) ? c.groupId : DEFAULT_GROUP_ID;
}

/** Flat display order (what Ctrl+Tab walks and what top tabs show). */
/** The window a connection's tab lives in: "main" or a detached window's label. */
/** The window a connection's tab lives in ("main" or a detached label), or null if the
 *  connection isn't open as a tab anywhere. */
export function hostOf(s: Pick<AppState, "windows" | "mainTabs">, id: ConnectionId): string | null {
  const detached = s.windows.find((w) => w.tabs.includes(id));
  if (detached) return detached.label;
  return s.mainTabs.includes(id) ? "main" : null;
}

/** Whether a connection is open as a tab in any window. */
export function isOpen(s: Pick<AppState, "windows" | "mainTabs">, id: ConnectionId): boolean {
  return hostOf(s, id) !== null;
}

/** A window's open tabs, in order (main window or a detached label). */
export function windowTabs(s: Pick<AppState, "windows" | "mainTabs">, label: string): ConnectionId[] {
  if (label === "main") return s.mainTabs;
  return s.windows.find((w) => w.label === label)?.tabs ?? [];
}

export function flatOrder(s: Pick<AppState, "order" | "connById" | "groups">): ConnectionId[] {
  return buildSections(s.order, s.connById, s.groups).flatMap((x) => x.ids);
}
