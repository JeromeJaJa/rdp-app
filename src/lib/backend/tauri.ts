import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { Backend, Unsubscribe } from "./types";
import {
  DEFAULT_SETTINGS,
  type ApiInfo,
  type Connection,
  type ConnectionId,
  type ConnectionPatch,
  type ExportOptions,
  type Group,
  type GroupId,
  type GroupInput,
  type ImportOptions,
  type ImportPreview,
  type ImportResult,
  type ImportSource,
  type Library,
  type NewConnection,
  type ReplaceResult,
  type SettingOverrides,
  type SessionEvent,
  type Settings,
  type TrustedCertificate,
  type CompanionStatus,
  type UiCommand,
  type UiState,
  type WindowTabs,
} from "../types";

/** Library as sent by Rust: settings are null until first saved. */
interface RawLibrary extends Omit<Library, "settings"> {
  settings: Partial<Settings> | null;
}

/** Engine events as emitted by Rust (`rdp_engine::EngineEvent`). */
type RawEvent = SessionEvent | { type: "cursor"; id: ConnectionId; cursor: string };

function normalize(raw: RawLibrary): Library {
  return { ...raw, settings: { ...DEFAULT_SETTINGS, ...(raw.settings ?? {}) } };
}

/** Subscribes to a Tauri event, handling unsubscribe-before-ready. */
function subscribe<T>(event: string, cb: (payload: T) => void, onReady?: () => void): Unsubscribe {
  let disposed = false;
  let unlisten: UnlistenFn | null = null;
  listen<T>(event, (e) => cb(e.payload)).then((fn) => {
    if (disposed) fn();
    else {
      unlisten = fn;
      onReady?.();
    }
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

export class TauriBackend implements Backend {
  readonly kind = "tauri" as const;

  async loadLibrary(): Promise<Library> {
    return normalize(await invoke<RawLibrary>("load_library"));
  }

  addConnections(items: NewConnection[]): Promise<Connection[]> {
    return invoke("add_connections", { items });
  }

  updateConnection(id: ConnectionId, patch: ConnectionPatch): Promise<Connection> {
    return invoke("update_connection", { id, patch });
  }

  setConnectionSettings(id: ConnectionId, settings: SettingOverrides): Promise<boolean> {
    return invoke("set_connection_settings", { id, settings });
  }

  setGroupSettings(id: GroupId, settings: SettingOverrides): Promise<Group> {
    return invoke("set_group_settings", { id, settings });
  }

  deleteConnections(ids: ConnectionId[]): Promise<void> {
    return invoke("delete_connections", { ids });
  }

  reorderConnections(order: { id: ConnectionId; groupId: GroupId }[]): Promise<void> {
    return invoke("reorder_connections", { order });
  }

  async saveGroups(groups: GroupInput[]): Promise<Library> {
    return normalize(await invoke<RawLibrary>("save_groups", { groups }));
  }

  async replaceGroup(groupId: GroupId, items: NewConnection[]): Promise<ReplaceResult> {
    const r = await invoke<Omit<ReplaceResult, "library"> & { library: RawLibrary }>("replace_group", {
      groupId,
      items,
    });
    return { ...r, library: normalize(r.library) };
  }

  saveSettings(settings: Settings): Promise<void> {
    return invoke("save_settings", { settings });
  }

  exportData(options: ExportOptions): Promise<string> {
    return invoke("export_data", { options });
  }

  previewImport(source: ImportSource, options: ImportOptions): Promise<ImportPreview> {
    return invoke("preview_import", { source, options });
  }

  async applyImport(source: ImportSource, options: ImportOptions): Promise<ImportResult> {
    const r = await invoke<Omit<ImportResult, "library"> & { library: RawLibrary }>("apply_import", {
      source,
      options,
    });
    return { ...r, library: normalize(r.library) };
  }

  async saveFile(content: string, suggestedName: string, kind: "json" | "text"): Promise<boolean> {
    const path = await save({
      defaultPath: suggestedName,
      filters:
        kind === "json"
          ? [{ name: "RDP App export", extensions: ["json"] }]
          : [{ name: "Text", extensions: ["txt"] }],
    });
    if (!path) return false;
    await invoke("write_text_file", { path, content });
    return true;
  }

  async pickFolder(): Promise<string | null> {
    const path = await open({ multiple: false, directory: true });
    return typeof path === "string" ? path : null;
  }

  async openFile(): Promise<{ name: string; text: string } | null> {
    const path = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Connections", extensions: ["json", "txt", "csv"] }],
    });
    if (!path || Array.isArray(path)) return null;
    const text = await invoke<string>("read_text_file", { path });
    const name = path.split(/[\\/]/).pop() ?? path;
    return { name, text };
  }

  connect(ids: ConnectionId[]): Promise<void> {
    return invoke("connect", { ids });
  }

  reconnect(ids: ConnectionId[]): Promise<void> {
    return invoke("reconnect", { ids });
  }

  disconnect(ids: ConnectionId[]): Promise<void> {
    return invoke("disconnect", { ids });
  }

  trustCertificate(endpoint: string, fingerprint: string): Promise<void> {
    return invoke("trust_certificate", { endpoint, fingerprint });
  }

  listCertificates(): Promise<TrustedCertificate[]> {
    return invoke("list_certificates");
  }

  forgetCertificate(endpoint: string): Promise<void> {
    return invoke("forget_certificate", { endpoint });
  }

  apiInfo(): Promise<ApiInfo> {
    return invoke("api_info");
  }

  regenerateApiToken(): Promise<ApiInfo> {
    return invoke("regenerate_api_token");
  }

  onSessionEvent(cb: (e: SessionEvent) => void): Unsubscribe {
    let disposed = false;
    const unsub = subscribe<RawEvent>(
      "session-event",
      (e) => {
        // Cursor updates are consumed by the engine module, not the app state.
        if (e.type !== "cursor") cb(e);
      },
      async () => {
        // Sessions may already be running (e.g. after a reload): catch up.
        const snapshot = await invoke<RawEvent[]>("session_snapshot").catch(() => []);
        for (const e of snapshot) if (!disposed && e.type !== "cursor") cb(e);
      },
    );
    return () => {
      disposed = true;
      unsub();
    };
  }

  onLibraryChanged(cb: (library: Library) => void): Unsubscribe {
    return subscribe<RawLibrary>("library-changed", (raw) => cb(normalize(raw)));
  }

  onUiCommand(cb: (command: UiCommand) => void): Unsubscribe {
    return subscribe<UiCommand>("ui-command", cb);
  }

  onCompanionStatus(cb: (status: CompanionStatus) => void): Unsubscribe {
    return subscribe<CompanionStatus>("companion-status", cb);
  }

  companionTrustHost(id: ConnectionId, host: string): Promise<string> {
    return invoke("companion_trust_host", { id, host });
  }

  windowLayout(): Promise<WindowTabs[]> {
    return invoke<{ windows: WindowTabs[] }>("window_layout").then((l) => l.windows);
  }

  onWindowLayout(cb: (windows: WindowTabs[]) => void): Unsubscribe {
    return subscribe<{ windows: WindowTabs[] }>("window-layout", (l) => cb(l.windows));
  }

  onSelectTab(cb: (id: ConnectionId) => void): Unsubscribe {
    // Sent to one window only: listen on this window, not on every target.
    let disposed = false;
    let unlisten: (() => void) | null = null;
    import("@tauri-apps/api/webviewWindow").then(({ getCurrentWebviewWindow }) =>
      getCurrentWebviewWindow()
        .listen<ConnectionId>("select-tab", (e) => cb(e.payload))
        .then((fn) => {
          if (disposed) fn();
          else unlisten = fn;
        }),
    );
    return () => {
      disposed = true;
      unlisten?.();
    };
  }

  moveTab(id: ConnectionId, target: string, index?: number): Promise<void> {
    return invoke("window_move_tab", { id, target, index: index ?? null });
  }

  prepareWindow(): Promise<void> {
    return invoke("window_prepare");
  }

  tearTab(id: ConnectionId): Promise<string> {
    return invoke("window_tear_tab", { id });
  }

  dragWindow(): Promise<string> {
    return invoke("window_drag");
  }

  setWindowMaterial(acrylic: boolean, dark: boolean): Promise<void> {
    return invoke("window_set_material", { acrylic, dark });
  }

  setTabArea(rects: number[][]): Promise<void> {
    return invoke("window_set_tab_area", { rects });
  }

  detachTab(id: ConnectionId): Promise<string> {
    return invoke("window_detach_tab", { id });
  }

  reorderWindowTabs(tabs: ConnectionId[]): Promise<void> {
    return invoke("window_reorder_tabs", { tabs });
  }

  showTab(id: ConnectionId): Promise<void> {
    return invoke("window_show_tab", { id });
  }

  openExternal(url: string): Promise<void> {
    return invoke("open_url", { url });
  }

  reportUiState(state: UiState): void {
    invoke("report_ui_state", { state }).catch(() => {});
  }
}
