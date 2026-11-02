// Shared domain types. This file is the contract between the React UI and the
// backend (Rust via Tauri, or the in-browser mock). Keep it in sync with
// `src-tauri/src/store.rs` (serde uses camelCase).

export type ConnectionId = string;
export type GroupId = string;

/** The built-in group every connection belongs to unless moved to a custom group. */
export const DEFAULT_GROUP_ID: GroupId = "all";

/**
 * RD Gateway (MS-TSGU) settings. On a group they apply to all its connections;
 * on a connection, `null` means "inherit the group's gateway".
 */
export interface GatewaySettings {
  /** false = connect directly (lets one connection opt out of its group's gateway). */
  enabled: boolean;
  /** Gateway address, e.g. "rdg.contoso.com" or "rdg.contoso.com:443". */
  host: string;
  /** Reuse the connection's own username/password/domain for the gateway. */
  sameCredentials: boolean;
  username: string;
  domain: string;
  /** Gateway password stored (never sent to the UI). */
  hasPassword: boolean;
}

/** Gateway settings as written by the UI (password write-only). */
export interface GatewayInput {
  enabled: boolean;
  host: string;
  sameCredentials: boolean;
  username: string;
  domain: string;
  /** undefined = unchanged, "" = clear, otherwise replace. */
  password?: string;
}

export interface Connection {
  id: ConnectionId;
  /** Display name. Empty string means "use host". */
  name: string;
  host: string;
  port: number;
  username: string;
  /** Windows domain, may be empty. */
  domain: string;
  /** Passwords never leave the backend; the UI only knows whether one is stored. */
  hasPassword: boolean;
  /** Always set: DEFAULT_GROUP_ID or a custom group. */
  groupId: GroupId;
  notes: string;
  createdAt: number;
  /** null = use the group's gateway (if any). */
  gateway: GatewaySettings | null;
  /** Connection settings this connection overrides (over its group and the app). */
  settings?: SettingOverrides;
}

/** Settings a group or a connection can override: app → group → connection. */
export const OVERRIDABLE = [
  "audio",
  "microphone",
  "clipboardSync",
  "driveRedirection",
  "drivePaths",
  "printerRedirection",
  "smartcardRedirection",
  "autoReconnect",
  "codec",
  "h264Quality",
  "videoDecoder",
  "remoteWallpaper",
  "companion",
  "hostStatsInterval",
  "directTransfers",
  "remoteCommands",
] as const;
export type OverridableKey = (typeof OVERRIDABLE)[number];
/** Only the overridden settings; anything absent is inherited. */
export type SettingOverrides = Partial<Pick<Settings, OverridableKey>>;

/** Remote audio playback: off, only the session on screen, or always. */
export type AudioMode = "off" | "active" | "all";

export interface NewConnection {
  name?: string;
  host: string;
  port?: number;
  username: string;
  domain?: string;
  password?: string;
  /** Defaults to DEFAULT_GROUP_ID. */
  groupId?: GroupId;
  notes?: string;
  /** Duplicate support: copy the stored password of this existing connection (ignored if `password` is set). */
  copyPasswordFrom?: ConnectionId;
  gateway?: GatewayInput | null;
  settings?: SettingOverrides;
}

export interface ConnectionPatch {
  name?: string;
  host?: string;
  port?: number;
  username?: string;
  domain?: string;
  /** undefined = unchanged, "" = clear stored password, otherwise replace. */
  password?: string;
  groupId?: GroupId;
  notes?: string;
  /** undefined = unchanged, null = inherit from group. */
  gateway?: GatewayInput | null;
  /** undefined = unchanged; otherwise the complete set of overrides. */
  settings?: SettingOverrides;
}

export interface Group {
  id: GroupId;
  name: string;
  collapsed: boolean;
  /** True for the built-in "All" group (cannot be deleted). */
  builtin: boolean;
  /** Gateway used by connections in this group that don't set their own. */
  gateway: GatewaySettings | null;
  /** Connection settings overridden for this group's connections. */
  settings?: SettingOverrides;
}

/** Group fields written by the UI (gateway password write-only). */
export interface GroupInput {
  id: GroupId;
  name: string;
  collapsed: boolean;
  /** undefined = keep the stored gateway, null = remove it. */
  gateway?: GatewayInput | null;
}

export type Theme = "dark" | "light" | "system";
export type WindowMaterial = "solid" | "acrylic";
/** Which connections get a tab: only active ones (queued, connecting, connected,
 *  reconnecting), or any you open (they stay until closed). */
export type TopTabs = "active" | "opened";
export type Codec = "auto" | "h264" | "remotefx";

export interface Settings {
  theme: Theme;
  /** Experimental: the system acrylic backdrop behind the window's surfaces. */
  windowMaterial: WindowMaterial;
  /** Which connections get a tab in the top strip. */
  topTabs: TopTabs;
  /** Left sidebar width in CSS px. */
  sidebarWidth: number;
  /** Grid tile width in CSS px. */
  gridTileWidth: number;
  gridFilter: "all" | "connected";
  /** When true, selecting a tab of an idle connection connects it immediately. */
  connectOnSelect: boolean;
  /** How many connection handshakes may run at once (connect-all queue). */
  maxConcurrentConnects: number;
  /** Ask servers to stop sending frames for sessions that are not on screen. */
  pauseHiddenSessions: boolean;
  /** Preferred graphics codec. "auto" = H.264 when available. */
  codec: Codec;
  /** H.264 colour mode: "balanced" = 4:2:0 (AVC420), "sharp" = 4:4:4 (AVC444) when the host offers it. */
  h264Quality: "balanced" | "sharp";
  /** H.264 decoder: "auto" = GPU for the first sessions, then software. */
  videoDecoder: "auto" | "hardware" | "software";
  /** Show the remote desktop's wallpaper; off gives sessions a plain background (less to send). */
  remoteWallpaper: boolean;
  /** Automatically reconnect after a network drop. */
  autoReconnect: boolean;
  /** Let a companion running on the remote attach for deeper integration (experimental). */
  companion: boolean;
  /** How often a companion reports its host's load, in seconds (0 = never). */
  hostStatsInterval: number;
  /** Companion transfers go over a direct, encrypted link beside the session when one can be made. */
  directTransfers: boolean;
  /** The REST API may run commands on the host through its companion (POST /run). */
  remoteCommands: boolean;
  /** Which of its host's load sidebar rows and grid tiles show ("CPU 12%"), or nothing. */
  tabStats: TabStats;
  /** Share the clipboard (text, images, files) with remote sessions. */
  clipboardSync: boolean;
  /** Remote audio playback: off, only the session on screen, or every session. */
  audio: AudioMode;
  /** Redirect this PC's microphone to the session on screen. */
  microphone: boolean;
  /** Share local drives with sessions. "custom" uses `drivePaths`. */
  driveRedirection: "none" | "all" | "custom";
  /** Folders/drives to share when driveRedirection = "custom", e.g. ["C:\\", "D:\\Work"]. */
  drivePaths: string[];
  /** Share this PC's default printer. */
  printerRedirection: boolean;
  /** Share smart card readers. */
  smartcardRedirection: boolean;
  /**
   * Server certificate checks. "tofu" = trust on first use, block if it changes;
   * "prompt" = ask before trusting an unknown certificate; "ignore" = accept any.
   */
  certificatePolicy: "tofu" | "prompt" | "ignore";
  /** Local REST API for automation/agents (loopback only, token required). */
  apiEnabled: boolean;
  apiPort: number;
}

export const DEFAULT_SETTINGS: Settings = {
  theme: "dark",
  windowMaterial: "solid",
  topTabs: "active",
  sidebarWidth: 248,
  gridTileWidth: 320,
  gridFilter: "all",
  connectOnSelect: false,
  maxConcurrentConnects: 6,
  pauseHiddenSessions: true,
  codec: "auto",
  h264Quality: "sharp",
  videoDecoder: "auto",
  remoteWallpaper: true,
  autoReconnect: true,
  companion: false,
  hostStatsInterval: 30,
  directTransfers: true,
  remoteCommands: false,
  tabStats: "cpu",
  clipboardSync: true,
  audio: "active",
  microphone: false,
  driveRedirection: "none",
  drivePaths: [],
  printerRedirection: false,
  smartcardRedirection: false,
  certificatePolicy: "tofu",
  apiEnabled: true,
  apiPort: 47123,
};

/**
 * Everything persisted. `connections` is in display order; within a group,
 * connections are shown in this relative order. `groups` is in display order
 * and always contains the built-in DEFAULT_GROUP_ID ("All") group.
 */
export interface Library {
  groups: Group[];
  connections: Connection[];
  settings: Settings;
}

export type SessionStatus =
  | "idle" // not connected
  | "queued" // waiting for a connect slot
  | "connecting"
  | "connected"
  | "reconnecting"
  | "error"; // last attempt failed; `message` explains

export type SessionEvent =
  | { type: "status"; id: ConnectionId; status: SessionStatus; message?: string }
  /** Remote desktop size changed (after connect or dynamic resize). */
  | { type: "resolution"; id: ConnectionId; width: number; height: number }
  /** Codec actually negotiated, e.g. "H.264 · AVC444", "RemoteFX". */
  | { type: "codec"; id: ConnectionId; codec: string }
  /** A companion running on the remote attached to this session. */
  | { type: "companionConnected"; id: ConnectionId; version: string; host: string }
  | { type: "companionDisconnected"; id: ConnectionId }
  | { type: "companionRefused"; id: ConnectionId; protocol: number }
  /**
   * The server presented a certificate that needs a decision: never seen before
   * (policy "prompt") or different from the one trusted earlier. The connection
   * attempt failed; trusting it (backend.trustCertificate) and reconnecting proceeds.
   */
  | {
      type: "certificate";
      id: ConnectionId;
      endpoint: string;
      fingerprint: string;
      reason: "unknown" | "changed";
      previousFingerprint?: string;
      subject?: string;
    };

// ── Replace / import / export ───────────────────────────────────────────────

/** Outcome of replacing a group's connections with a new list. */
export interface ReplaceResult {
  /** Existing connections that matched an entry (same host, port, user) and were kept/updated. */
  kept: number;
  added: number;
  removed: number;
  library: Library;
}

/** Exports always include passwords, in plain text. */
export interface ExportOptions {
  /** null/undefined = everything; otherwise only these groups (with their connections). */
  groupIds?: GroupId[] | null;
  /** "json" = full fidelity (groups, gateways, notes); "text" = bulk lines host:port:user:pass. */
  format: "json" | "text";
}

export interface ImportSource {
  /** File contents (JSON export or bulk text). */
  text: string;
}

export interface ImportPreviewGroup {
  name: string;
  connections: number;
  /** An existing group with the same name (case-insensitive), if any. */
  existingGroupId: GroupId | null;
  /** Entries that already exist in the target (same host, port, user). */
  duplicates: number;
}

export interface ImportPreview {
  format: "json" | "text";
  groups: ImportPreviewGroup[];
  total: number;
  errors: string[];
}

export interface ImportOptions {
  /**
   * "merge" = add new connections, update matching ones (same host, port, user).
   * "replace" = make each target group contain exactly the imported connections.
   */
  mode: "merge" | "replace";
  /**
   * Where groups go. "byName" = into the existing group with the same name, else a
   * new group. A group id = everything into that one group (text imports always
   * use this, defaulting to DEFAULT_GROUP_ID).
   */
  target: "byName" | GroupId;
}

export interface ImportResult {
  added: number;
  updated: number;
  removed: number;
  groupsCreated: number;
  library: Library;
}

// ── Certificates & API ──────────────────────────────────────────────────────

export interface TrustedCertificate {
  endpoint: string;
  fingerprint: string;
  subject: string;
  firstSeen: number;
}

export interface ApiInfo {
  enabled: boolean;
  /** e.g. "http://127.0.0.1:47123" */
  url: string;
  /** Bearer token required by every /api request. */
  token: string;
  /** Human docs (HTML). */
  docsUrl: string;
  /** Plain-text reference for agents. */
  llmsUrl: string;
  running: boolean;
  error?: string;
}

/** UI-level commands sent by the REST API (agents) to the window. */
export type UiCommand =
  | { type: "select"; id: ConnectionId }
  | { type: "view"; view: "session" | "grid" }
  | { type: "fullscreen"; on: boolean }
  | { type: "focus" };

/** What the UI reports back so the API can answer GET /ui. */
export interface UiState {
  selectedId: ConnectionId | null;
  view: "session" | "grid";
  fullscreen: boolean;
}

/** A window that tabs were dragged into, with its tabs in order. */
export interface WindowTabs {
  label: string;
  tabs: ConnectionId[];
}

/** The full window/tab layout: the main window's open tabs plus any detached windows. */
export interface WindowLayout {
  mainTabs: ConnectionId[];
  windows: WindowTabs[];
}

/**
 * Where a dragged tab is held: `x` CSS px from its left edge, and its width `w`. Carried
 * through a drag across windows, so the tab stays under the pointer at that same spot.
 */
export interface TabGrab {
  x: number;
  w: number;
}

/**
 * A tab dragged out of a window just re-entered this one while still held: carry on
 * dragging it here, from the pointer (CSS px in this window's client area), held at `grabX`.
 */
export interface ContinueDrag {
  id: ConnectionId;
  x: number;
  y: number;
  grabX: number;
}

/** Which of its host's load sidebar rows and grid tiles show, labelled, or nothing. */
export type TabStats = "cpu" | "memory" | "network" | "disk" | "off";

/** A host's load, reported by its session's companion. Rates average over `span` seconds. */
export interface HostStats {
  /** When it arrived (ms since the Unix epoch). */
  at: number;
  span: number;
  /** CPU use, 0–100. */
  cpu: number;
  memUsed: number;
  memTotal: number;
  /** Network bytes per second. */
  netRx: number;
  netTx: number;
  /** Disk bytes per second and the busiest disk's active time (0–100); absent when the host
   *  doesn't let the companion read its disks. */
  diskRead?: number;
  diskWrite?: number;
  diskBusy?: number;
  /** Fixed volumes. */
  volumes: { name: string; free: number; total: number }[];
}

export type TransferState = "waiting" | "sending" | "done" | "failed" | "cancelled";

/** A drag of local files onto a session, as its companion reports it. */
export interface RemoteDrag {
  session: ConnectionId;
  id: number;
  /** What dropping where the pointer is would do on the remote (DROPEFFECT bits; 0 = nothing). */
  effect: number;
  /** The remote drag is over (dropped or cancelled). */
  ended: boolean;
}

/** Files dragged over this window from another app: where (CSS px), and their paths on
 *  enter and drop. A drop only arrives where the page said it would take one. */
export interface FileDrag {
  phase: "enter" | "over" | "leave" | "drop";
  x: number;
  y: number;
  paths?: string[];
}

/** Files being sent to one connection ("Send files to…"). */
export interface Transfer {
  /** The connection it goes to. */
  session: ConnectionId;
  id: number;
  /** "report.pdf", or "3 items". */
  label: string;
  /** Where it's saved on the remote, as asked ("Desktop", a path…). */
  folder: string;
  files: number;
  total: number;
  sent: number;
  state: TransferState;
  error?: string;
}

/** Progress of the companion auto-install for a connection (experimental). */
export interface CompanionStatus {
  id: ConnectionId;
  /** installing → pushing over WinRM; needs-trust → this PC must trust the host first;
   *  winrm-unavailable → no WinRM (needs the in-session fallback); failed → unexpected. */
  status: "installing" | "installed" | "needs-trust" | "winrm-unavailable" | "failed";
  /** The host, when the UI needs it (e.g. to add it to TrustedHosts). */
  host: string | null;
}

/** What to do with a program on a remote host (see `REMOTE_APPS`). */
export type AppAction = "launch" | "restart" | "close";

/** How an app control went on one connection. */
export interface AppOutcome {
  id: ConnectionId;
  error?: string | null;
  /** The program was running before. */
  wasRunning: boolean;
}
