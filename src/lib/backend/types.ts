import type {
  ApiInfo,
  Connection,
  ConnectionId,
  ConnectionPatch,
  ExportOptions,
  Group,
  GroupId,
  GroupInput,
  ImportOptions,
  ImportPreview,
  ImportResult,
  ImportSource,
  Library,
  NewConnection,
  ReplaceResult,
  SettingOverrides,
  SessionEvent,
  Settings,
  TrustedCertificate,
  CompanionStatus,
  ContinueDrag,
  HostStats,
  Transfer,
  TabGrab,
  UiCommand,
  UiState,
  WindowLayout,
} from "../types";

export type Unsubscribe = () => void;

/**
 * Persistence + session control. Implemented by `tauri.ts` (real, Rust) and
 * `mock.ts` (in-browser, for UI development with `bun run dev`).
 *
 * Mutations persist immediately on the backend side. The UI applies changes
 * optimistically to its store and calls these in the background. Changes made
 * elsewhere (the REST API) arrive through `onLibraryChanged`.
 */
export interface Backend {
  readonly kind: "tauri" | "mock";

  loadLibrary(): Promise<Library>;

  /** Adds one or many connections (bulk import). Returns them with ids assigned. */
  addConnections(items: NewConnection[]): Promise<Connection[]>;
  updateConnection(id: ConnectionId, patch: ConnectionPatch): Promise<Connection>;
  /**
   * Replaces a connection's setting overrides and applies what can change live. Resolves
   * false when its session has to reconnect before it can be heard (it connected muted).
   */
  setConnectionSettings(id: ConnectionId, settings: SettingOverrides): Promise<boolean>;
  /** Replaces a group's setting overrides for its connections. */
  setGroupSettings(id: GroupId, settings: SettingOverrides): Promise<Group>;
  deleteConnections(ids: ConnectionId[]): Promise<void>;
  /** Full new display order + group membership of every connection. */
  reorderConnections(order: { id: ConnectionId; groupId: GroupId }[]): Promise<void>;
  /**
   * Replaces the whole group list (create / rename / reorder / collapse / gateway).
   * The built-in "All" group is always kept. Connections of groups missing from
   * the list move to "All" (delete them first to remove them instead).
   */
  saveGroups(groups: GroupInput[]): Promise<Library>;
  /**
   * Makes `groupId` contain exactly `items`, in that order. Entries matching an
   * existing connection of the group (same host, port, username — case-insensitive)
   * keep it (and any live session), updating name/password/domain when given;
   * others are added; connections not in the list are disconnected and deleted.
   */
  replaceGroup(groupId: GroupId, items: NewConnection[]): Promise<ReplaceResult>;
  saveSettings(settings: Settings): Promise<void>;

  /** Serialises connections for export (see ExportOptions). */
  exportData(options: ExportOptions): Promise<string>;
  previewImport(source: ImportSource, options: ImportOptions): Promise<ImportPreview>;
  applyImport(source: ImportSource, options: ImportOptions): Promise<ImportResult>;

  /** Opens a save dialog and writes `content`. Resolves false if cancelled. */
  saveFile(content: string, suggestedName: string, kind: "json" | "text"): Promise<boolean>;
  /** Opens a file picker. Resolves null if cancelled. */
  openFile(): Promise<{ name: string; text: string } | null>;
  /** Opens a folder picker. Resolves the chosen folder's path, or null if cancelled. */
  pickFolder(): Promise<string | null>;

  /**
   * Starts connecting. Resolves once the request is queued, not when connected —
   * progress arrives as `status` session events. The remote desktop size comes
   * from the engine's current viewport (see `engine.setViewport`).
   */
  connect(ids: ConnectionId[]): Promise<void>;
  disconnect(ids: ConnectionId[]): Promise<void>;
  /** Connects again; a running session is fully stopped first. */
  reconnect(ids: ConnectionId[]): Promise<void>;

  /** Accepts a server certificate (after a `certificate` session event). */
  trustCertificate(endpoint: string, fingerprint: string): Promise<void>;
  listCertificates(): Promise<TrustedCertificate[]>;
  forgetCertificate(endpoint: string): Promise<void>;

  apiInfo(): Promise<ApiInfo>;
  regenerateApiToken(): Promise<ApiInfo>;

  onSessionEvent(cb: (e: SessionEvent) => void): Unsubscribe;
  /** The library changed outside the UI (REST API, import): replace local state. */
  onLibraryChanged(cb: (library: Library) => void): Unsubscribe;
  /** UI commands from the REST API (select a tab, switch view, …). */
  onUiCommand(cb: (command: UiCommand) => void): Unsubscribe;
  /** Companion auto-install progress for a connection (experimental). */
  onCompanionStatus(cb: (status: CompanionStatus) => void): Unsubscribe;
  /** Adds a host to the WinRM client's TrustedHosts (UAC prompt), then retries install. */
  companionTrustHost(id: ConnectionId, host: string): Promise<string>;

  // ── Companion: host stats and file sends ───────────────────────────────
  /** Latest host stats of each session whose companion reports them. */
  hostStats(): Promise<Record<ConnectionId, HostStats>>;
  /** A session's companion reported its host's load (null: it stopped reporting). */
  onHostStats(cb: (id: ConnectionId, stats: HostStats | null) => void): Unsubscribe;
  /**
   * Sends files and folders to connections, into `folder` on each remote ("Desktop",
   * "Downloads", "Documents", one of those with a subfolder, or a full path). Connections
   * that aren't connected are connected first. Resolves with the transfer ids; progress
   * arrives through `onTransfer`.
   */
  sendFiles(ids: ConnectionId[], paths: string[], folder: string): Promise<number[]>;
  cancelTransfer(id: number): Promise<void>;
  /** Forgets finished transfers. */
  clearTransfers(): Promise<void>;
  /** Transfers in progress or recently finished. */
  transfers(): Promise<Transfer[]>;
  onTransfer(cb: (transfer: Transfer) => void): Unsubscribe;
  /** Opens a picker for files (several) or a folder to send. Resolves null if cancelled. */
  pickPaths(kind: "files" | "folder"): Promise<string[] | null>;
  /** Tells the backend what the UI shows (answers GET /api/v1/ui). */
  reportUiState(state: UiState): void;
  /** Opens an http(s) link in the default browser. */
  openExternal(url: string): Promise<void>;

  // ── Windows and tabs ────────────────────────────────────────────────────
  /** The main window's open tabs, and which tabs live in which detached window. */
  windowLayout(): Promise<WindowLayout>;
  onWindowLayout(cb: (layout: WindowLayout) => void): Unsubscribe;
  /** Another window asked this one to show a tab. */
  onSelectTab(cb: (id: ConnectionId) => void): Unsubscribe;
  /** A tab dragged out of a window re-entered this one while held: carry the drag on here. */
  onContinueDrag(cb: (drag: ContinueDrag) => void): Unsubscribe;
  /** The button went up after a drag was handed to this window (it may not see the release). */
  onDragRelease(cb: () => void): Unsubscribe;
  /** Opens a connection as a tab in the main window (or focuses it if already open). */
  openTab(id: ConnectionId, select: boolean): Promise<void>;
  /** Closes a connection's tab (the session is disconnected separately). */
  closeTab(id: ConnectionId): Promise<void>;
  /** Opens main-window tabs for those not open anywhere yet, in one go (sessions that became active). */
  openTabs(ids: ConnectionId[]): Promise<void>;
  /** Closes several tabs in one go. */
  closeTabs(ids: ConnectionId[]): Promise<void>;
  /** Replaces the main window's open tabs (restoring them on launch). */
  setMainTabs(ids: ConnectionId[]): Promise<void>;
  /** Moves a tab into "main" or a detached window (at `index`, or the end). */
  moveTab(id: ConnectionId, target: string, index?: number): Promise<void>;
  /** A tab is being dragged out of its strip: get a window ready in case it's torn off. */
  prepareWindow(): Promise<void>;
  /**
   * Tears the tab, held at `grab`, out of this window mid-drag: its new window follows the
   * pointer until the button is released (and its tab joins any window whose strip it
   * reaches). Resolves once the window is open, not when the drag ends.
   */
  tearTab(id: ConnectionId, grab: TabGrab): Promise<void>;
  /** Drags this (detached) window by its only tab, held at `grab`, the same way. */
  dragWindow(grab: TabGrab): Promise<void>;
  /** This window's material: solid, or the acrylic backdrop (experimental). `dark` tints it. */
  setWindowMaterial(acrylic: boolean, dark: boolean): Promise<void>;
  /**
   * Where this window shows its tabs, as [x, y, width, height] rects in CSS px: `areas` are
   * the drop targets (title bar, sidebar), `slots` the individual tabs in order.
   */
  setTabArea(areas: number[][], slots: number[][]): Promise<void>;
  /** Opens a new window holding the tab, next to this one. */
  detachTab(id: ConnectionId): Promise<string>;
  /** New tab order for this (detached) window. */
  reorderWindowTabs(tabs: ConnectionId[]): Promise<void>;
  /** Focuses the window holding the tab and selects it there. */
  showTab(id: ConnectionId): Promise<void>;
}
