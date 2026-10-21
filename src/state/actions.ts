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
  type SessionStatus,
  type Settings,
  type SettingOverrides,
  type TabGrab,
  type Theme,
  type CompanionStatus,
  type HostStats,
  type Transfer,
  type UiCommand,
  type WindowMaterial,
  type WindowLayout,
  type WindowTabs,
} from "../lib/types";
import { gatewayFromInput } from "../lib/gateway";
import { isMainWindow, isTauri, windowLabel } from "../lib/env";
import { morphView } from "../lib/morph";
import {
  displayName,
  flatOrder,
  getState,
  hostOf,
  isActive,
  isBusy,
  isLive,
  resolveSetting,
  sessionOf,
  setState,
  useStore,
  windowTabs,
  withDefaultGroup,
  VITALS_HISTORY,
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
  const main = isMainWindow();
  applyLibrary(lib, { preferSelected: main ? selectedId : null });
  const layout = await backend.windowLayout().catch(() => ({ mainTabs: [], windows: [] }));
  let sidebarShown = true;
  try {
    sidebarShown = localStorage.getItem("rdp-app:sidebar") !== "0";
  } catch {
    /* ignore */
  }
  setState({
    ready: true,
    mainTabs: layout.mainTabs,
    windows: layout.windows,
    sidebarShown,
    ...(main ? {} : { view: "session" as const }),
  });
  if (main) restoreOpenTabs(lib, selectedId);
  else keepDetachedSelection([]);
  applyTheme(lib.settings.theme, false);
  applyMaterial(lib.settings.windowMaterial);
  backend.onSessionEvent(onSessionEvent);
  backend.onHostStats(onHostStats);
  void backend.hostStats().then((all) => {
    for (const [id, samples] of Object.entries(all)) onHostStats(id, samples);
  });
  backend.onTransfer(onTransfer);
  void backend.transfers().then((all) => all.forEach(onTransfer));
  backend.onLibraryChanged((next) => {
    applyLibrary(next);
    if (!isMainWindow()) keepDetachedSelection(getState().windows);
  });
  backend.onWindowLayout(onWindowLayout);
  // A tab dragged out of a window re-entered this one while held: it's this window's tab
  // now (selected, like Chrome), and the strip carries on dragging it.
  backend.onContinueDrag((drag) => setState({ selectedId: drag.id, continueDrag: drag }));
  backend.onDragRelease(() => setState({ dragRelease: getState().dragRelease + 1 }));
  if (main) {
    // The REST API drives (and reads) the main window.
    backend.onUiCommand(onUiCommand);
    backend.onCompanionStatus(onCompanionStatus);
    startUiStateReporting();
    // Switched to tabs only for active sessions: the others' tabs close.
    useStore.subscribe((s, prev) => {
      if (s.settings.topTabs !== prev.settings.topTabs && s.settings.topTabs !== "opened") closeInactiveTabs();
    });
  } else {
    backend.onSelectTab((id) => setState({ selectedId: id }));
  }
}

// ---------------------------------------------------------------------------
// Windows: tabs dragged out of the main window
// ---------------------------------------------------------------------------

const TABS_KEY = "rdp-app:open-tabs";

/** Reopens the tabs that were open last time (or, on first run, all connections). */
function restoreOpenTabs(lib: Library, selectedId: ConnectionId | null) {
  if (lib.settings.topTabs !== "opened") {
    // Tabs are only for active sessions: keep whatever the backend has (the live ones, after
    // a reload) and show the last selected connection, tab or not.
    const sel = selectedId && lib.connections.some((c) => c.id === selectedId) ? selectedId : getState().mainTabs[0];
    if (sel) setState({ selectedId: sel, view: "session" });
    return;
  }
  let ids: ConnectionId[];
  try {
    const raw = localStorage.getItem(TABS_KEY);
    ids = raw ? (JSON.parse(raw) as ConnectionId[]) : lib.connections.map((c) => c.id);
  } catch {
    ids = [];
  }
  const existing = ids.filter((id) => lib.connections.some((c) => c.id === id));
  setState({ mainTabs: existing });
  bg(be().setMainTabs(existing), "restore tabs");
  const sel = selectedId && existing.includes(selectedId) ? selectedId : (existing[0] ?? null);
  if (sel) setState({ selectedId: sel, view: "session" });
}

function persistMainTabs(tabs: ConnectionId[]) {
  try {
    localStorage.setItem(TABS_KEY, JSON.stringify(tabs));
  } catch {
    /* ignore */
  }
}

function onWindowLayout(layout: WindowLayout) {
  const prev = getState();
  setState({ mainTabs: layout.mainTabs, windows: layout.windows });
  if (isMainWindow()) {
    persistMainTabs(layout.mainTabs);
    // If the selected tab left the main window (closed or torn off), pick a neighbour. Not
    // when it closed because its session ended (tabs only for active sessions): it stays
    // selected, so its error or idle card stays in view.
    const sel = prev.selectedId;
    const ended = getState().settings.topTabs !== "opened" && sel !== null && !isActive(sessionOf(sel).status);
    if (sel && !ended && !layout.mainTabs.includes(sel) && hostOf(getState(), sel) === null) {
      const idx = Math.max(0, prev.mainTabs.indexOf(sel));
      setState({ selectedId: layout.mainTabs[Math.min(idx, layout.mainTabs.length - 1)] ?? null });
    }
  } else {
    keepDetachedSelection(prev.windows);
  }
}

/** Opens a connection as a tab (in the main window) and selects it. */
export function openTab(id: ConnectionId, opts: { connect?: boolean } = {}) {
  select(id, opts);
}

/** Closes a connection's tab and disconnects its session. */
export function closeTab(id: ConnectionId) {
  const me = windowLabel();
  // Optimistic: drop the tab now (the backend confirms via onWindowLayout).
  if (me === "main") {
    const prev = getState().mainTabs;
    const idx = prev.indexOf(id);
    const tabs = prev.filter((t) => t !== id);
    setState({ mainTabs: tabs });
    persistMainTabs(tabs);
    // Closing the selected tab: fall onto a neighbour (or nothing if empty).
    if (getState().selectedId === id) {
      const next = tabs[Math.min(Math.max(idx, 0), tabs.length - 1)] ?? null;
      setState({ selectedId: next });
      try {
        if (next) localStorage.setItem(SELECTED_KEY, next);
      } catch {
        /* ignore */
      }
    }
  } else {
    setState({
      windows: getState().windows.map((w) => (w.label === me ? { ...w, tabs: w.tabs.filter((t) => t !== id) } : w)),
    });
  }
  bg(be().closeTab(id), "close tab");
  disconnect([id]);
}

/**
 * A detached window shows one of its own tabs: a tab that just arrived, or the
 * selected one if it is still here, or the first.
 */
function keepDetachedSelection(previous: WindowTabs[]) {
  const s = getState();
  const me = windowLabel();
  const tabs = windowTabs(s, me);
  const before = new Set(windowTabs({ windows: previous, mainTabs: [] }, me));
  const arrived = tabs.find((t) => !before.has(t));
  const selectedId = arrived ?? (s.selectedId && tabs.includes(s.selectedId) ? s.selectedId : (tabs[0] ?? null));
  if (selectedId !== s.selectedId) setState({ selectedId, view: "session" });
  const conn = selectedId ? s.connById[selectedId] : undefined;
  if (isTauri() && conn) {
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().setTitle(`${displayName(conn)} — RDP App`))
      .catch(() => {});
  }
}

/** Opens a new window holding the connection's tab. */
export async function openInNewWindow(id: ConnectionId) {
  try {
    await be().detachTab(id);
  } catch (err) {
    toast("error", "Could not open a window", errorText(err));
    return;
  }
  // With tabs only for active sessions, a tab means a session: start it.
  if (getState().settings.topTabs !== "opened" && !isActive(sessionOf(id).status)) connect([id]);
}

/**
 * Main window: tabs follow the sessions. A session that becomes active always gets a tab
 * (so one started from the library or by "Connect all" shows up in the strip). With tabs
 * only for active sessions (the default), a tab also closes when its session ends; the
 * connection stays selected, so an error stays in view. Batched: one layout update each way.
 */
function followSessions(changed: Map<ConnectionId, SessionStatus>) {
  const s = getState();
  const activeOnly = s.settings.topTabs !== "opened";
  const open: ConnectionId[] = [];
  const close: ConnectionId[] = [];
  for (const [id, status] of changed) {
    const host = hostOf(s, id);
    if (isActive(status)) {
      if (host === null) open.push(id);
    } else if (activeOnly && host !== null) {
      close.push(id);
    }
  }
  if (open.length) {
    const mainTabs = [...s.mainTabs, ...open];
    setState({ mainTabs });
    persistMainTabs(mainTabs);
    bg(be().openTabs(open), "open tabs");
  }
  if (close.length) dropTabs(close);
}

/** Switched to tabs only for active sessions: close the tabs of the ones that aren't. */
function closeInactiveTabs() {
  const s = getState();
  const open = [...s.mainTabs, ...s.windows.flatMap((w) => w.tabs)];
  const ended = open.filter((id) => !isActive(sessionOf(id).status));
  if (ended.length) dropTabs(ended);
}

/** Closes tabs without touching their sessions or the selection (optimistic, then backend). */
function dropTabs(ids: ConnectionId[]) {
  const gone = new Set(ids);
  const s = getState();
  const mainTabs = s.mainTabs.filter((t) => !gone.has(t));
  setState({ mainTabs, windows: s.windows.map((w) => ({ ...w, tabs: w.tabs.filter((t) => !gone.has(t)) })) });
  persistMainTabs(mainTabs);
  bg(be().closeTabs(ids), "close tabs");
}

/** Sends a tab back to the main window. */
export function moveToMainWindow(id: ConnectionId) {
  bg(be().moveTab(id, "main"), "move tab");
}

/** Brings the window holding the tab to the front and selects the tab there. */
export function showTabWindow(id: ConnectionId) {
  bg(be().showTab(id), "show tab");
}

/** New tab order for this window's strip (main or detached). */
export function reorderWindowTabs(tabs: ConnectionId[]) {
  const me = windowLabel();
  if (me === "main") setState({ mainTabs: tabs });
  else setState({ windows: getState().windows.map((w) => (w.label === me ? { ...w, tabs } : w)) });
  bg(be().reorderWindowTabs(tabs), "reorder tabs");
}

/** Shows or hides the main window's library sidebar. */
export function toggleSidebar() {
  const sidebarShown = !getState().sidebarShown;
  setState({ sidebarShown });
  try {
    localStorage.setItem("rdp-app:sidebar", sidebarShown ? "1" : "0");
  } catch {
    /* ignore */
  }
}

/** The tabs this window can switch between, in order. */
function windowOrder(): ConnectionId[] {
  return windowTabs(getState(), windowLabel());
}

/** A tab drag left its strip: a window gets ready in case the tab is torn off. */
export function prepareTearOff() {
  be()
    .prepareWindow()
    .catch(() => {});
}

/**
 * Tears a tab, held at `grab`, out of this window mid-drag (Chrome-style): this window's
 * drag ends and the tab's new window follows the pointer, still held at the same spot.
 */
export function tearOff(id: ConnectionId, grab: TabGrab) {
  // dnd-kit cancels on Escape; the backend's drag loop takes the pointer from here.
  document.dispatchEvent(new KeyboardEvent("keydown", { code: "Escape", key: "Escape" }));
  bg(be().tearTab(id, grab), "open a window");
}

/** Drags this window by its only tab, held at `grab`. */
export function dragWindow(grab: TabGrab) {
  bg(be().dragWindow(grab), "move the window");
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
  if (s.ready && lib.settings.windowMaterial !== s.settings.windowMaterial) applyMaterial(lib.settings.windowMaterial);
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
  // Prompts and error toasts belong to the window that holds the tab, or the main window if
  // none does (a session can fail before its tab opens, or its tab may only be for active
  // sessions).
  const me = windowLabel();
  // With tabs only for active sessions, a failed session's tab closes (maybe taking its
  // window with it), so failures are told in the main window.
  const activeOnly = s.settings.topTabs !== "opened";
  const statuses = new Map<ConnectionId, SessionStatus>();
  for (const e of events) {
    if (!s.connById[e.id]) continue;
    const host = hostOf(s, e.id);
    const mine = host === me || (host === null && me === "main");
    const prev = sessions[e.id] ?? { status: "idle" as const };
    switch (e.type) {
      case "status":
        statuses.set(e.id, e.status);
        sessions[e.id] =
          e.status === "idle"
            ? { status: e.status }
            : { ...prev, status: e.status, message: e.status === "error" ? e.message : undefined };
        // A certificate decision gets its own dialog instead of an error toast.
        if (e.status === "error" && !certIds.has(e.id) && (activeOnly ? me === "main" : mine)) {
          queueErrorToast(e.id, e.message);
        }
        break;
      case "resolution":
        sessions[e.id] = { ...prev, width: e.width, height: e.height };
        break;
      case "codec":
        sessions[e.id] = { ...prev, codec: e.codec };
        break;
      case "companionConnected":
        sessions[e.id] = { ...prev, companion: { version: e.version, host: e.host } };
        break;
      case "companionDisconnected":
        sessions[e.id] = { ...prev, companion: undefined };
        break;
      case "certificate": {
        if (!mine) break;
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
  if (statuses.size && me === "main") followSessions(statuses);
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

/** Solid, or the system acrylic backdrop showing through translucent surfaces (experimental). */
export function applyMaterial(material: WindowMaterial) {
  document.documentElement.dataset.material = material;
  if (isTauri()) {
    const dark = document.documentElement.dataset.theme !== "light";
    be()
      .setWindowMaterial(material === "acrylic", dark)
      .catch(() => {});
  }
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
  // Acrylic's tint follows the theme, so re-apply it when the theme changes.
  if (getState().settings.windowMaterial === "acrylic") applyMaterial("acrylic");
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
  if (patch.windowMaterial) applyMaterial(patch.windowMaterial);
  clearTimeout(settingsTimer);
  settingsTimer = setTimeout(() => bg(be().saveSettings(getState().settings), "save settings"), 250);
}

// ---------------------------------------------------------------------------
// UI state
// ---------------------------------------------------------------------------

export function select(id: ConnectionId | null, opts: { connect?: boolean } = {}) {
  if (id && isMainWindow()) {
    const host = hostOf(getState(), id);
    // Open in another window: show it there.
    if (host && host !== "main") {
      setState({ selectedId: id, menu: null });
      showTabWindow(id);
      return;
    }
    // Not open yet (opened from the library): open it as a main tab. With tabs only for
    // active sessions, an idle one is just selected; connecting it opens its tab.
    const tabbable = getState().settings.topTabs === "opened" || isActive(sessionOf(id).status);
    if (!host && tabbable) {
      setState({ mainTabs: [...getState().mainTabs, id] });
      persistMainTabs(getState().mainTabs);
      bg(be().openTab(id, false), "open tab");
    }
  }
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
  const ids = windowOrder();
  if (!ids.length) return;
  const cur = getState().selectedId;
  const idx = cur ? ids.indexOf(cur) : -1;
  const next = ids[(idx + delta + ids.length) % ids.length];
  select(next);
}

export function selectIndex(n: number) {
  const ids = windowOrder();
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

// ---------------------------------------------------------------------------
// Companion: host stats and file sends
// ---------------------------------------------------------------------------

function onHostStats(id: ConnectionId, samples: HostStats[] | null) {
  setState((s) => {
    const next = { ...s.stats };
    if (!samples) {
      if (!(id in next)) return {};
      delete next[id];
      return { stats: next };
    }
    if (!samples.length) return {};
    const prev = s.stats[id];
    const add = (history: number[] | undefined, values: number[]) =>
      [...(history ?? []), ...values].slice(-VITALS_HISTORY);
    next[id] = {
      latest: samples[samples.length - 1],
      cpu: add(prev?.cpu, samples.map((x) => x.cpu)),
      mem: add(prev?.mem, samples.map((x) => (x.memTotal ? (x.memUsed / x.memTotal) * 100 : 0))),
      disk: add(prev?.disk, samples.map((x) => x.diskBusy ?? 0)),
      net: add(prev?.net, samples.map((x) => x.netRx + x.netTx)),
    };
    return { stats: next };
  });
}

function onTransfer(t: Transfer) {
  setState((s) => ({ transfers: { ...s.transfers, [t.id]: t } }));
}

/** Opens "Send files to…" for these connections (with `paths` already chosen, if any). */
export function openSendDialog(ids: ConnectionId[], paths?: string[]) {
  if (ids.length) openDialog({ kind: "send", ids, paths });
}

/** Sends files and folders to connections, into `folder` on each remote. */
export async function sendFiles(ids: ConnectionId[], paths: string[], folder: string): Promise<boolean> {
  try {
    await be().sendFiles(ids, paths, folder);
    return true;
  } catch (e) {
    toast("error", "Couldn't send the files", errorText(e));
    return false;
  }
}

export function cancelTransfer(id: number) {
  void be().cancelTransfer(id);
}

/** Forgets finished transfers (the panel's "Clear"). */
export function clearTransfers() {
  setState((s) => ({
    transfers: Object.fromEntries(
      Object.entries(s.transfers).filter(([, t]) => t.state === "waiting" || t.state === "sending"),
    ),
  }));
  void be().clearTransfers();
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
// Hosts we've already offered the trust prompt for, so we don't nag on every reconnect.
const trustPrompted = new Set<string>();

/** Companion auto-install progress (main window). Only the trust prompt is interactive. */
function onCompanionStatus(s: CompanionStatus) {
  const name = (s.id && getState().connById[s.id] && displayName(getState().connById[s.id])) || "the host";
  switch (s.status) {
    case "installed":
      toast("success", "Companion installed", `${name} is set up — it'll start automatically from now on.`);
      break;
    case "needs-trust":
      if (!s.host || trustPrompted.has(s.host)) break;
      trustPrompted.add(s.host);
      toast(
        "info",
        "Set up the companion?",
        `To install it on ${name}, this PC needs to trust the host once (asks for admin).`,
        { label: "Enable", run: () => enableCompanionTrust(s.id, s.host!) },
        true,
      );
      break;
    // winrm-unavailable and failed stay quiet — nothing the user can act on yet.
    default:
      break;
  }
}

function enableCompanionTrust(id: ConnectionId, host: string) {
  toast("info", "Setting up…", "Approve the admin prompt to continue.");
  be()
    .companionTrustHost(id, host)
    .then((outcome) => {
      if (outcome === "Installed") toast("success", "Companion installed", "It'll start automatically from now on.");
      else toast("info", "Couldn't reach the host", "WinRM wasn't available on this host.");
    })
    .catch((e) => toast("error", "Setup didn't complete", errorText(e)));
}

export function toast(
  tone: Toast["tone"],
  title: string,
  message?: string,
  action?: Toast["action"],
  sticky?: boolean,
) {
  const id = ++toastSeq;
  const toasts = [...getState().toasts, { id, tone, title, message, action, sticky }].slice(-4);
  setState({ toasts });
  if (!sticky) window.setTimeout(() => dismissToast(id), tone === "error" || action ? 6000 : 3500);
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
