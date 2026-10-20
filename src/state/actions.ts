import { getBackend, type Backend } from "../lib/backend";
import {
  DEFAULT_GROUP_ID,
  type AudioMode,
  type Connection,
  type ConnectionId,
  type ConnectionPatch,
  type GatewayInput,
  type Group,
  type GroupId,
  type GroupInput,
  type Library,
  type NewConnection,
  type ReplaceResult,
  type SessionEvent,
  type Settings,
  type SettingOverrides,
  type Theme,
  type UiCommand,
} from "../lib/types";
import { gatewayFromInput } from "../lib/gateway";
import { isTauri } from "../lib/env";
import { morphView } from "../lib/morph";
import {
  displayName,
  flatOrder,
  getState,
  isBusy,
  isLive,
  resolveSetting,
  sessionOf,
  setState,
  useStore,
  withDefaultGroup,
  type CertPrompt,
  type DialogState,
  type MenuState,
  type Toast,
} from "./store";

let backend: Backend | null = null;
const SELECTED_KEY = "rdp-app:selected";
const THEME_KEY = "rdp-app:theme";

/** The initialised backend (throws before `initApp`). */
export function be(): Backend {
  if (!backend) throw new Error("backend not initialised");
  return backend;
}

export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function bg(p: Promise<unknown>, what: string) {
  p.catch((err: unknown) => {
    console.error(what, err);
    toast("error", `Could not ${what}`, errorText(err));
  });
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

export async function initApp(): Promise<void> {
  backend = await getBackend();
  const lib = await backend.loadLibrary();
  let selectedId: string | null = null;
  try {
    selectedId = localStorage.getItem(SELECTED_KEY);
  } catch {
    /* ignore */
  }
  applyLibrary(lib, { preferSelected: selectedId });
  setState({ ready: true });
  applyTheme(lib.settings.theme, false);
  backend.onSessionEvent(onSessionEvent);
  backend.onLibraryChanged((next) => applyLibrary(next));
  backend.onUiCommand(onUiCommand);
  startUiStateReporting();
}

/**
 * Replaces connections, groups and settings with `lib` (initial load, REST API
 * changes, imports, replaces), keeping UI state. If the selected connection is
 * gone, its nearest neighbour in display order is selected instead.
 */
export function applyLibrary(lib: Library, opts: { preferSelected?: string | null } = {}) {
  const s = getState();
  const prevFlat = flatOrder(s);
  const connById: Record<string, Connection> = {};
  for (const c of lib.connections) connById[c.id] = c;
  const order = lib.connections.map((c) => c.id);
  const groups = withDefaultGroup(lib.groups);

  const sessions = { ...s.sessions };
  for (const id in sessions) if (!connById[id]) delete sessions[id];

  let selectedId = opts.preferSelected !== undefined ? opts.preferSelected : s.selectedId;
  if (!selectedId || !connById[selectedId]) {
    const nextFlat = flatOrder({ order, connById, groups });
    const oldIdx = selectedId ? prevFlat.indexOf(selectedId) : -1;
    selectedId = oldIdx >= 0 ? (nextFlat[Math.min(oldIdx, nextFlat.length - 1)] ?? null) : (nextFlat[0] ?? null);
  }

  const themeChanged = s.ready && lib.settings.theme !== s.settings.theme;
  setState({ order, connById, groups, settings: lib.settings, sessions, selectedId });
  if (themeChanged) applyTheme(lib.settings.theme);
}

// Session events arrive in bursts (connect-all can emit hundreds at once), so
// they are folded into a single store update per ~frame.
let pendingEvents: SessionEvent[] = [];
let flushTimer: ReturnType<typeof setTimeout> | undefined;

function onSessionEvent(e: SessionEvent) {
  pendingEvents.push(e);
  if (flushTimer === undefined) flushTimer = setTimeout(flushSessionEvents, 16);
}

function flushSessionEvents() {
  flushTimer = undefined;
  const events = pendingEvents;
  pendingEvents = [];
  const s = getState();
  const sessions = { ...s.sessions };
  const certPrompts = [...s.certPrompts];
  const certIds = new Set(events.filter((e) => e.type === "certificate").map((e) => e.id));
  for (const e of events) {
    if (!s.connById[e.id]) continue;
    const prev = sessions[e.id] ?? { status: "idle" as const };
    switch (e.type) {
      case "status":
        sessions[e.id] =
          e.status === "idle"
            ? { status: e.status }
            : { ...prev, status: e.status, message: e.status === "error" ? e.message : undefined };
        // A certificate decision gets its own dialog instead of an error toast.
        if (e.status === "error" && !certIds.has(e.id)) queueErrorToast(e.id, e.message);
        break;
      case "resolution":
        sessions[e.id] = { ...prev, width: e.width, height: e.height };
        break;
      case "codec":
        sessions[e.id] = { ...prev, codec: e.codec };
        break;
      case "certificate": {
        const prompt: CertPrompt = {
          id: e.id,
          endpoint: e.endpoint,
          fingerprint: e.fingerprint,
          reason: e.reason,
          previousFingerprint: e.previousFingerprint,
          subject: e.subject,
        };
        const at = certPrompts.findIndex((p) => p.endpoint === e.endpoint);
        if (at >= 0) certPrompts[at] = prompt;
        else certPrompts.push(prompt);
        break;
      }
    }
  }
  setState({ sessions, certPrompts });
}

// Errors from a connect-all burst are collapsed into one toast.
let errorBuffer: { id: ConnectionId; message?: string }[] = [];
let errorTimer: ReturnType<typeof setTimeout> | undefined;
function queueErrorToast(id: ConnectionId, message?: string) {
  errorBuffer.push({ id, message });
  clearTimeout(errorTimer);
  errorTimer = setTimeout(() => {
    const batch = errorBuffer;
    errorBuffer = [];
    const names = batch.map((b) => {
      const c = getState().connById[b.id];
      return c ? displayName(c) : "Connection";
    });
    if (batch.length === 1) toast("error", `${names[0]} failed to connect`, batch[0].message);
    else {
      const shown = names.slice(0, 3).join(", ");
      toast("error", `${batch.length} connections failed`, batch.length > 3 ? `${shown} and ${batch.length - 3} more` : shown);
    }
  }, 700);
}

// ---------------------------------------------------------------------------
// REST API ↔ UI
// ---------------------------------------------------------------------------

function onUiCommand(cmd: UiCommand) {
  switch (cmd.type) {
    case "select":
      if (getState().connById[cmd.id]) select(cmd.id, { connect: false });
      break;
    case "view":
      setView(cmd.view);
      break;
    case "fullscreen":
      setFullscreen(cmd.on);
      break;
    case "focus":
      if (isTauri()) {
        import("@tauri-apps/api/window")
          .then(async ({ getCurrentWindow }) => {
            const w = getCurrentWindow();
            await w.unminimize();
            await w.show();
            await w.setFocus();
          })
          .catch(() => {});
      } else window.focus();
      break;
  }
}

function startUiStateReporting() {
  let last = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  const report = () => {
    const { selectedId, view, fullscreen } = getState();
    const key = `${selectedId}|${view}|${fullscreen}`;
    if (key === last) return;
    last = key;
    be().reportUiState({ selectedId, view, fullscreen });
  };
  report();
  useStore.subscribe((s, prev) => {
    if (s.selectedId === prev.selectedId && s.view === prev.view && s.fullscreen === prev.fullscreen) return;
    clearTimeout(timer);
    timer = setTimeout(report, 100);
  });
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

let systemMql: MediaQueryList | null = null;
function resolveTheme(theme: Theme): "dark" | "light" {
  if (theme !== "system") return theme;
  return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function applyTheme(theme: Theme, animate = true) {
  const root = document.documentElement;
  const resolved = resolveTheme(theme);
  if (animate && root.dataset.theme !== resolved) {
    root.classList.add("theme-transition");
    window.setTimeout(() => root.classList.remove("theme-transition"), 220);
  }
  root.dataset.theme = resolved;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    /* ignore */
  }
  if (isTauri()) {
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().setTheme(theme === "system" ? null : resolved))
      .catch(() => {});
  }
  if (theme === "system" && !systemMql) {
    systemMql = matchMedia("(prefers-color-scheme: dark)");
    systemMql.addEventListener("change", () => {
      if (getState().settings.theme === "system") applyTheme("system");
    });
  }
}

export function cycleTheme() {
  const current = resolveTheme(getState().settings.theme);
  updateSettings({ theme: current === "dark" ? "light" : "dark" });
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

let settingsTimer: ReturnType<typeof setTimeout> | undefined;
export function updateSettings(patch: Partial<Settings>) {
  const settings = { ...getState().settings, ...patch };
  setState({ settings });
  if (patch.theme) applyTheme(patch.theme);
  clearTimeout(settingsTimer);
  settingsTimer = setTimeout(() => bg(be().saveSettings(getState().settings), "save settings"), 250);
}

// ---------------------------------------------------------------------------
// UI state
// ---------------------------------------------------------------------------

export function select(id: ConnectionId | null, opts: { connect?: boolean } = {}) {
  const apply = () => setState({ selectedId: id, view: "session", menu: null });
  if (getState().view === "grid") morphView(id, false, apply);
  else apply();
  try {
    if (id) localStorage.setItem(SELECTED_KEY, id);
  } catch {
    /* ignore */
  }
  if (!id) return;
  const st = sessionOf(id).status;
  const wantConnect = opts.connect ?? getState().settings.connectOnSelect;
  if (wantConnect && (st === "idle" || st === "error")) connect([id]);
}

export function selectRelative(delta: number) {
  const ids = flatOrder(getState());
  if (!ids.length) return;
  const cur = getState().selectedId;
  const idx = cur ? ids.indexOf(cur) : -1;
  const next = ids[(idx + delta + ids.length) % ids.length];
  select(next);
}

export function selectIndex(n: number) {
  const ids = flatOrder(getState());
  const id = n === 9 ? ids[ids.length - 1] : ids[n - 1];
  if (id) select(id);
}

export function setView(view: "session" | "grid") {
  const s = getState();
  if (view === s.view) return setState({ menu: null });
  morphView(s.selectedId, view === "grid", () => setState({ view, menu: null }));
}

export function toggleGrid() {
  setView(getState().view === "grid" ? "session" : "grid");
}

export function openDialog(dialog: DialogState) {
  setState({ dialog, menu: null });
}

export function closeDialog() {
  setState({ dialog: null });
}

export function openMenu(menu: MenuState) {
  setState({ menu });
}

export function closeMenu() {
  if (getState().menu) setState({ menu: null });
}

export function confirm(opts: Omit<Extract<DialogState, { kind: "confirm" }>, "kind">) {
  openDialog({ kind: "confirm", ...opts });
}

let toastSeq = 0;
export function toast(tone: Toast["tone"], title: string, message?: string, action?: Toast["action"]) {
  const id = ++toastSeq;
  const toasts = [...getState().toasts, { id, tone, title, message, action }].slice(-4);
  setState({ toasts });
  window.setTimeout(() => dismissToast(id), tone === "error" || action ? 6000 : 3500);
}

export function dismissToast(id: number) {
  setState({ toasts: getState().toasts.filter((t) => t.id !== id) });
}

export async function setFullscreen(on: boolean) {
  setState({ fullscreen: on });
  try {
    if (isTauri()) {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().setFullscreen(on);
    } else if (on && !document.fullscreenElement) {
      await document.documentElement.requestFullscreen();
    } else if (!on && document.fullscreenElement) {
      await document.exitFullscreen();
    }
  } catch {
    /* fullscreen not permitted (e.g. browser without gesture) — chrome is still hidden */
  }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export function connect(ids: ConnectionId[]) {
  const todo = ids.filter((id) => {
    const st = sessionOf(id).status;
    return st === "idle" || st === "error";
  });
  if (todo.length) bg(be().connect(todo), "connect");
}

export function disconnect(ids: ConnectionId[]) {
  const todo = ids.filter((id) => sessionOf(id).status !== "idle");
  if (todo.length) bg(be().disconnect(todo), "disconnect");
}

export async function reconnect(id: ConnectionId) {
  try {
    await be().reconnect([id]);
  } catch (err) {
    toast("error", "Could not reconnect", errorText(err));
  }
}

export function connectAll(ids?: ConnectionId[]) {
  connect(ids ?? flatOrder(getState()));
}

export function disconnectAll(ids?: ConnectionId[]) {
  const all = ids ?? flatOrder(getState());
  disconnect(
    all.filter((id) => {
      const st = sessionOf(id).status;
      return isLive(st) || isBusy(st) || st === "error";
    }),
  );
}

// ---------------------------------------------------------------------------
// Certificates
// ---------------------------------------------------------------------------

function dropPrompt(endpoint: string) {
  setState({ certPrompts: getState().certPrompts.filter((p) => p.endpoint !== endpoint) });
}

export async function trustCertificate(prompt: CertPrompt) {
  dropPrompt(prompt.endpoint);
  try {
    await be().trustCertificate(prompt.endpoint, prompt.fingerprint);
    // Retry the connection that was waiting on the decision.
    await be().connect([prompt.id]);
  } catch (err) {
    toast("error", "Could not trust certificate", errorText(err));
  }
}

export function dismissCertificate(prompt: CertPrompt) {
  dropPrompt(prompt.endpoint);
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

export async function addConnections(items: NewConnection[]): Promise<Connection[]> {
  const created = await be().addConnections(items.map((it) => ({ ...it, groupId: it.groupId ?? DEFAULT_GROUP_ID })));
  const s = getState();
  const connById = { ...s.connById };
  for (const c of created) connById[c.id] = c;
  setState({ connById, order: [...s.order, ...created.map((c) => c.id)] });
  return created;
}

/** The sound a connection plays with (its own choice, its group's, or the app's). */
export function effectiveAudio(id: ConnectionId): AudioMode {
  return resolveSetting(getState(), id, "audio");
}

/** Replaces a connection's setting overrides; sound changes apply live where possible. */
export async function setConnectionSettings(id: ConnectionId, settings: SettingOverrides): Promise<void> {
  const s = getState();
  const prev = s.connById[id];
  if (!prev) return;
  const wasAudible = effectiveAudio(id) !== "off";
  setState({ connById: { ...s.connById, [id]: { ...prev, settings } } });
  try {
    const live = await be().setConnectionSettings(id, settings);
    if (!live && !wasAudible && effectiveAudio(id) !== "off" && isLive(sessionOf(id).status)) {
      toast("info", "Reconnect to hear this session", "It connected with sound off.", {
        label: "Reconnect",
        run: () => reconnect(id),
      });
    }
  } catch (err) {
    setState({ connById: { ...getState().connById, [id]: prev } });
    toast("error", "Could not save the connection's settings", errorText(err));
  }
}

/** Replaces a group's setting overrides for its connections. */
export async function setGroupSettings(id: GroupId, settings: SettingOverrides): Promise<void> {
  const prev = getState().groups;
  setState({ groups: prev.map((g) => (g.id === id ? { ...g, settings } : g)) });
  try {
    const saved = await be().setGroupSettings(id, settings);
    setState({ groups: getState().groups.map((g) => (g.id === id ? saved : g)) });
  } catch (err) {
    setState({ groups: prev });
    toast("error", "Could not save the group's settings", errorText(err));
  }
}

/** Mutes the connection, or unmutes it (back to what it inherits when that plays sound). */
export function toggleConnectionMute(id: ConnectionId) {
  const s = getState();
  const conn = s.connById[id];
  if (!conn) return;
  const { audio: _own, ...rest } = conn.settings ?? {};
  const inherited = s.groups.find((g) => g.id === conn.groupId)?.settings?.audio ?? s.settings.audio;
  const muted = effectiveAudio(id) === "off";
  void setConnectionSettings(id, muted ? (inherited === "off" ? { ...rest, audio: "active" } : rest) : { ...rest, audio: "off" });
}

export async function updateConnection(id: ConnectionId, patch: ConnectionPatch): Promise<void> {
  const s = getState();
  const prev = s.connById[id];
  if (!prev) return;
  const { password, gateway, ...rest } = patch;
  const optimistic: Connection = {
    ...prev,
    ...rest,
    hasPassword: password === undefined ? prev.hasPassword : password !== "",
    gateway: gateway === undefined ? prev.gateway : gatewayFromInput(gateway, prev.gateway),
  };
  setState({ connById: { ...s.connById, [id]: optimistic } });
  try {
    const saved = await be().updateConnection(id, patch);
    setState({ connById: { ...getState().connById, [id]: saved } });
  } catch (err) {
    setState({ connById: { ...getState().connById, [id]: prev } });
    throw err;
  }
}

export async function duplicateConnection(id: ConnectionId) {
  const c = getState().connById[id];
  if (!c) return;
  const [copy] = await addConnections([
    {
      name: `${displayName(c)} copy`,
      host: c.host,
      port: c.port,
      username: c.username,
      domain: c.domain,
      groupId: c.groupId,
      notes: c.notes,
      copyPasswordFrom: c.hasPassword ? c.id : undefined,
      settings: c.settings,
    },
  ]);
  if (copy) {
    // Place the copy right after the original.
    const order = getState().order.filter((x) => x !== copy.id);
    order.splice(order.indexOf(id) + 1, 0, copy.id);
    setState({ order });
    persistOrder();
    select(copy.id, { connect: false });
  }
}

export function deleteConnections(ids: ConnectionId[]): Promise<void> {
  const s = getState();
  const set = new Set(ids);
  const flat = flatOrder(s);
  let selectedId = s.selectedId;
  if (selectedId && set.has(selectedId)) {
    const idx = flat.indexOf(selectedId);
    const remaining = flat.filter((x) => !set.has(x));
    selectedId = remaining[Math.min(idx, remaining.length - 1)] ?? null;
  }
  const connById = { ...s.connById };
  const sessions = { ...s.sessions };
  for (const id of ids) {
    delete connById[id];
    delete sessions[id];
  }
  setState({ connById, sessions, order: s.order.filter((x) => !set.has(x)), selectedId });
  const p = be().deleteConnections(ids);
  bg(p, "delete connections");
  return p.catch(() => {});
}

function persistOrder() {
  const s = getState();
  const order = s.order.filter((id) => s.connById[id]).map((id) => ({ id, groupId: s.connById[id].groupId }));
  bg(be().reorderConnections(order), "save order");
}

/** Applies a new full order with group membership (from drag & drop). */
export function applyOrder(next: { id: ConnectionId; groupId: GroupId }[]) {
  const s = getState();
  const connById = { ...s.connById };
  for (const { id, groupId } of next) {
    const c = connById[id];
    if (c && c.groupId !== groupId) connById[id] = { ...c, groupId };
  }
  setState({ connById, order: next.map((x) => x.id) });
  bg(be().reorderConnections(next), "save order");
}

export function moveToGroup(ids: ConnectionId[], groupId: GroupId) {
  const s = getState();
  const connById = { ...s.connById };
  for (const id of ids) if (connById[id]) connById[id] = { ...connById[id], groupId };
  setState({ connById });
  persistOrder();
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

function toInput(g: Group): GroupInput {
  return { id: g.id, name: g.name, collapsed: g.collapsed };
}

// Only the response to the latest save may overwrite local groups, so a burst of
// collapse toggles can't be undone by an older response arriving late.
let groupSaveSeq = 0;
function saveGroups(groups: Group[], gatewayFor?: { id: GroupId; gateway: GatewayInput | null }) {
  setState({ groups });
  const seq = ++groupSaveSeq;
  const inputs = groups.map((g) => (gatewayFor && g.id === gatewayFor.id ? { ...toInput(g), gateway: gatewayFor.gateway } : toInput(g)));
  const p = be().saveGroups(inputs).then((lib) => {
    if (seq !== groupSaveSeq) return;
    const s = getState();
    // The backend may have normalised groups (e.g. re-added "All") or moved
    // connections out of a deleted group.
    const connById = { ...s.connById };
    for (const c of lib.connections) if (connById[c.id]) connById[c.id] = c;
    setState({ groups: withDefaultGroup(lib.groups), connById });
  });
  bg(p, "save groups");
  return p;
}

function newGroupId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : Math.random().toString(36).slice(2) + Date.now().toString(36);
}

export function createGroup(name = "New group", opts: { rename?: boolean } = {}): GroupId {
  const g: Group = { id: newGroupId(), name, collapsed: false, builtin: false, gateway: null };
  saveGroups([...getState().groups, g]);
  if (opts.rename) setState({ renamingGroupId: g.id });
  return g.id;
}

export function startRenameGroup(id: GroupId) {
  const g = getState().groups.find((x) => x.id === id);
  if (g && !g.builtin) setState({ renamingGroupId: id, menu: null });
}

export function renameGroup(id: GroupId, name: string) {
  const trimmed = name.trim();
  setState({ renamingGroupId: null });
  const g = getState().groups.find((x) => x.id === id);
  if (!trimmed || !g || g.builtin || g.name === trimmed) return;
  saveGroups(getState().groups.map((x) => (x.id === id ? { ...x, name: trimmed } : x)));
}

export function toggleGroupCollapsed(id: GroupId) {
  saveGroups(getState().groups.map((g) => (g.id === id ? { ...g, collapsed: !g.collapsed } : g)));
}

/** Sets (or removes, with null) the RD Gateway of a group. */
export function setGroupGateway(id: GroupId, gateway: GatewayInput | null) {
  const groups = getState().groups.map((g) => (g.id === id ? { ...g, gateway: gatewayFromInput(gateway, g.gateway) } : g));
  return saveGroups(groups, { id, gateway });
}

/** Deletes a custom group, moving its connections to "All" or deleting them too. */
export async function deleteGroup(id: GroupId, mode: "move" | "delete" = "move") {
  const s = getState();
  const g = s.groups.find((x) => x.id === id);
  if (!g || g.builtin) return;
  const members = groupMembers(id);
  if (mode === "delete" && members.length) await deleteConnections(members);
  const after = getState();
  const connById = { ...after.connById };
  for (const cid of members) if (connById[cid]) connById[cid] = { ...connById[cid], groupId: DEFAULT_GROUP_ID };
  setState({ connById });
  saveGroups(after.groups.filter((x) => x.id !== id));
}

export function groupMembers(id: GroupId): ConnectionId[] {
  const s = getState();
  const known = new Set(s.groups.map((g) => g.id));
  return s.order.filter((cid) => {
    const c = s.connById[cid];
    if (!c) return false;
    const gid = known.has(c.groupId) ? c.groupId : DEFAULT_GROUP_ID;
    return gid === id;
  });
}

/** Replaces a group's connections with `items` (see Backend.replaceGroup). */
export async function replaceGroup(groupId: GroupId, items: NewConnection[]): Promise<ReplaceResult> {
  const result = await be().replaceGroup(groupId, items.map((it) => ({ ...it, groupId })));
  applyLibrary(result.library);
  return result;
}
