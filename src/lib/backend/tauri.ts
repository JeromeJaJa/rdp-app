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
  type GroupId,
  type GroupInput,
  type ImportOptions,
  type ImportPreview,
  type ImportResult,
  type ImportSource,
  type Library,
  type NewConnection,
  type ReplaceResult,
  type SessionEvent,
  type Settings,
  type TrustedCertificate,
  type UiCommand,
  type UiState,
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

  openExternal(url: string): Promise<void> {
    return invoke("open_url", { url });
  }

  reportUiState(state: UiState): void {
    invoke("report_ui_state", { state }).catch(() => {});
  }
}
