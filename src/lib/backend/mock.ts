// In-browser backend for UI development (`bun run dev` in a normal browser).
// Persists to localStorage and simulates the connect lifecycle with a queue.

import type { Backend, Unsubscribe } from "./types";
import {
  DEFAULT_GROUP_ID,
  DEFAULT_SETTINGS,
  type ApiInfo,
  type Connection,
  type ConnectionId,
  type ConnectionPatch,
  type ExportOptions,
  type GatewayInput,
  type GatewaySettings,
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
  type SessionStatus,
  type Settings,
  type TrustedCertificate,
  type CompanionStatus,
  type UiCommand,
  type UiState,
  type WindowLayout,
} from "../types";
import { gatewayFromInput } from "../gateway";
import { matchKey } from "../match";
import { parseBulk } from "../bulkParse";

const STORAGE_KEY = "rdp-app:mock-library:v1";
const PASSWORDS_KEY = "rdp-app:mock-passwords:v1";
const CERTS_KEY = "rdp-app:mock-certs:v1";
const TOKEN_KEY = "rdp-app:mock-token:v1";

const ERRORS = [
  "Authentication failed (NLA): the user name or password is incorrect",
  "Host unreachable: connection timed out after 10s",
  "Connection refused by remote host",
];

const CODECS = ["H.264 · AVC444", "H.264 · AVC444", "H.264 · AVC420", "RemoteFX"];

interface Persisted {
  groups: Group[];
  connections: Connection[];
  settings: Settings;
}

function uid(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : Math.random().toString(36).slice(2) + Date.now().toString(36);
}

function defaultGroup(): Group {
  return { id: DEFAULT_GROUP_ID, name: "All", collapsed: false, builtin: true, gateway: null };
}

function seed(): { lib: Persisted; passwords: Record<string, string> } {
  const all = defaultGroup();
  const prod: Group = {
    id: uid(),
    name: "Production",
    collapsed: false,
    builtin: false,
    gateway: { enabled: true, host: "rdg.contoso.com", sameCredentials: true, username: "", domain: "", hasPassword: false },
  };
  const staging: Group = { id: uid(), name: "Staging", collapsed: false, builtin: false, gateway: null };
  const lab: Group = { id: uid(), name: "Lab", collapsed: false, builtin: false, gateway: null };
  const now = Date.now();
  const mk = (name: string, host: string, username: string, groupId: string, domain = ""): Connection => ({
    id: uid(),
    name,
    host,
    port: 3389,
    username,
    domain,
    hasPassword: true,
    groupId,
    notes: "",
    createdAt: now,
    gateway: null,
  });
  const connections: Connection[] = [
    mk("Jump box", "203.0.113.10", "admin", all.id),
    mk("", "198.51.100.24", "Administrator", all.id),
    mk("DC01", "10.20.0.10", "jdoe", prod.id, "CORP"),
    mk("DC02", "10.20.0.11", "jdoe", prod.id, "CORP"),
    mk("SQL primary", "10.20.1.40", "svc-sql", prod.id, "CORP"),
    mk("SQL replica", "10.20.1.41", "svc-sql", prod.id, "CORP"),
    mk("IIS web 1", "10.20.2.21", "Administrator", prod.id),
    mk("IIS web 2", "10.20.2.22", "Administrator", prod.id),
    mk("Staging app", "10.30.0.15", "deploy", staging.id),
    mk("Staging SQL", "10.30.0.16", "deploy", staging.id),
    mk("Build agent", "10.30.4.2", "builder", staging.id),
    mk("Win11 test", "192.168.50.11", "tester", lab.id),
    mk("Win10 legacy", "192.168.50.12", "tester", lab.id),
    mk("Server 2025 eval", "192.168.50.20", "Administrator", lab.id),
  ];
  const passwords: Record<string, string> = {};
  for (const c of connections) passwords[c.id] = "demo";
  return { lib: { groups: [all, prod, staging, lab], connections, settings: { ...DEFAULT_SETTINGS } }, passwords };
}

/** Upgrades data saved by older UI builds: every connection now belongs to a group. */
function migrate(lib: Persisted): Persisted {
  const groups: Group[] = (lib.groups ?? []).map((g) => ({
    ...g,
    builtin: g.id === DEFAULT_GROUP_ID,
    gateway: g.gateway ?? null,
  }));
  if (!groups.some((g) => g.id === DEFAULT_GROUP_ID)) groups.unshift(defaultGroup());
  const known = new Set(groups.map((g) => g.id));
  const connections = (lib.connections ?? []).map((c) => ({
    ...c,
    groupId: c.groupId && known.has(c.groupId) ? c.groupId : DEFAULT_GROUP_ID,
    gateway: c.gateway ?? null,
  }));
  return { groups, connections, settings: { ...DEFAULT_SETTINGS, ...lib.settings } };
}

function load(): { lib: Persisted; passwords: Record<string, string> } {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const lib = migrate(JSON.parse(raw) as Persisted);
      const passwords = JSON.parse(localStorage.getItem(PASSWORDS_KEY) ?? "{}") as Record<string, string>;
      return { lib, passwords };
    }
  } catch {
    /* fall through to seed */
  }
  return seed();
}

// ── Export format (mirrors the Rust backend) ────────────────────────────────

interface ExportGateway {
  enabled: boolean;
  host: string;
  sameCredentials: boolean;
  username: string;
  domain: string;
  password?: string;
}

interface ExportConnection {
  name: string;
  host: string;
  port: number;
  username: string;
  domain: string;
  password?: string;
  notes: string;
  gateway?: ExportGateway | null;
}

interface ExportGroup {
  name: string;
  gateway?: ExportGateway | null;
  connections: ExportConnection[];
}

interface ExportDoc {
  format: "rdp-app/export";
  version: 1;
  exportedAt: string;
  passwords: "none" | "encrypted" | "plain";
  groups: ExportGroup[];
}

interface ParsedImport {
  format: "json" | "text";
  groups: ExportGroup[];
  errors: string[];
}

function parseImport(source: ImportSource): ParsedImport {
  const text = source.text.trim();
  if (!text) return { format: "text", groups: [], errors: [] };
  if (text.startsWith("{")) {
    let doc: ExportDoc;
    try {
      doc = JSON.parse(text) as ExportDoc;
    } catch (e) {
      return { format: "json", groups: [], errors: [`Invalid JSON: ${(e as Error).message}`] };
    }
    if (doc.format !== "rdp-app/export" || !Array.isArray(doc.groups)) {
      return { format: "json", groups: [], errors: ["Not an RDP App export file"] };
    }
    if (doc.passwords === "encrypted") {
      return { format: "json", groups: [], errors: ["This export's passwords are encrypted with a passphrase, which is no longer supported"] };
    }
    return { format: "json", groups: doc.groups, errors: [] };
  }

  // Bulk text, optionally split into groups with "# Group: <name>" lines.
  const groups: ExportGroup[] = [];
  const errors: string[] = [];
  let current: ExportGroup = { name: "Imported", connections: [] };
  let buffer: string[] = [];
  const flush = () => {
    for (const l of parseBulk(buffer.join("\n"), [])) {
      if (l.entry) {
        current.connections.push({ name: "", notes: "", ...l.entry, password: l.entry.password || undefined });
      } else if (l.status === "error") errors.push(`“${l.raw.trim()}”: ${l.error}`);
    }
    if (current.connections.length) groups.push(current);
    buffer = [];
  };
  for (const line of text.split(/\r?\n/)) {
    const m = /^#\s*group:\s*(.+)$/i.exec(line.trim());
    if (m) {
      flush();
      current = { name: m[1].trim(), connections: [] };
    } else buffer.push(line);
  }
  flush();
  return { format: "text", groups, errors };
}

interface SessionSim {
  status: SessionStatus;
  timer?: ReturnType<typeof setTimeout>;
}

export class MockBackend implements Backend {
  readonly kind = "mock" as const;

  private lib: Persisted;
  private passwords: Record<string, string>;
  private gatewayPasswords: Record<string, string> = {};
  private certs: TrustedCertificate[];
  private token: string;
  private listeners = new Set<(e: SessionEvent) => void>();
  private sessions = new Map<ConnectionId, SessionSim>();
  private queue: ConnectionId[] = [];
  private active = 0;

  constructor() {
    const { lib, passwords } = load();
    this.lib = lib;
    this.passwords = passwords;
    this.certs = JSON.parse(localStorage.getItem(CERTS_KEY) ?? "[]") as TrustedCertificate[];
    this.token = localStorage.getItem(TOKEN_KEY) ?? this.newToken();
    this.persist();
  }

  private newToken(): string {
    const t = Array.from(crypto.getRandomValues(new Uint8Array(24)), (b) => b.toString(16).padStart(2, "0")).join("");
    localStorage.setItem(TOKEN_KEY, t);
    return t;
  }

  private persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(this.lib));
      localStorage.setItem(PASSWORDS_KEY, JSON.stringify(this.passwords));
      localStorage.setItem(CERTS_KEY, JSON.stringify(this.certs));
    } catch {
      /* storage full / disabled: keep in memory */
    }
  }

  private emit(e: SessionEvent) {
    // Deliver asynchronously, like real IPC.
    queueMicrotask(() => this.listeners.forEach((cb) => cb(e)));
  }

  private setStatus(id: ConnectionId, status: SessionStatus, message?: string) {
    const s = this.sessions.get(id) ?? { status: "idle" };
    s.status = status;
    this.sessions.set(id, s);
    this.emit({ type: "status", id, status, message });
  }

  private snapshot(): Library {
    return structuredClone(this.lib);
  }

  private storeGateway(key: string, input: GatewayInput | null | undefined, prev: GatewaySettings | null): GatewaySettings | null {
    if (input === undefined) return prev;
    if (input === null) {
      delete this.gatewayPasswords[key];
      return null;
    }
    if (input.password !== undefined) {
      if (input.password) this.gatewayPasswords[key] = input.password;
      else delete this.gatewayPasswords[key];
    }
    return gatewayFromInput(input, prev);
  }

  async loadLibrary(): Promise<Library> {
    return this.snapshot();
  }

  private makeConnection(it: NewConnection, now = Date.now()): Connection {
    const id = uid();
    const password = it.password || (it.copyPasswordFrom ? this.passwords[it.copyPasswordFrom] : undefined);
    if (password) this.passwords[id] = password;
    const known = this.lib.groups.some((g) => g.id === it.groupId);
    return {
      id,
      name: it.name ?? "",
      host: it.host,
      port: it.port ?? 3389,
      username: it.username,
      domain: it.domain ?? "",
      hasPassword: !!password,
      groupId: it.groupId && known ? it.groupId : DEFAULT_GROUP_ID,
      notes: it.notes ?? "",
      createdAt: now,
      gateway: this.storeGateway(`c:${id}`, it.gateway, null),
    };
  }

  async addConnections(items: NewConnection[]): Promise<Connection[]> {
    const now = Date.now();
    const created = items.map((it) => this.makeConnection(it, now));
    this.lib.connections.push(...created);
    this.persist();
    return structuredClone(created);
  }

  async updateConnection(id: ConnectionId, patch: ConnectionPatch): Promise<Connection> {
    const c = this.lib.connections.find((x) => x.id === id);
    if (!c) throw new Error("Connection not found");
    const { password, gateway, ...rest } = patch;
    Object.assign(c, rest);
    if (password !== undefined) {
      if (password === "") delete this.passwords[id];
      else this.passwords[id] = password;
      c.hasPassword = password !== "";
    }
    c.gateway = this.storeGateway(`c:${id}`, gateway, c.gateway);
    this.persist();
    return structuredClone(c);
  }

  async setConnectionSettings(id: ConnectionId, settings: SettingOverrides): Promise<boolean> {
    const c = this.lib.connections.find((x) => x.id === id);
    if (!c) throw new Error("Connection not found");
    c.settings = { ...settings };
    this.persist();
    return true;
  }

  async setGroupSettings(id: GroupId, settings: SettingOverrides): Promise<Group> {
    const g = this.lib.groups.find((x) => x.id === id);
    if (!g) throw new Error("Group not found");
    g.settings = { ...settings };
    this.persist();
    return structuredClone(g);
  }

  async deleteConnections(ids: ConnectionId[]): Promise<void> {
    await this.disconnect(ids);
    const set = new Set(ids);
    this.lib.connections = this.lib.connections.filter((c) => !set.has(c.id));
    for (const id of ids) delete this.passwords[id];
    this.persist();
  }

  async reorderConnections(order: { id: ConnectionId; groupId: GroupId }[]): Promise<void> {
    const byId = new Map(this.lib.connections.map((c) => [c.id, c]));
    const next: Connection[] = [];
    for (const o of order) {
      const c = byId.get(o.id);
      if (!c) continue;
      c.groupId = o.groupId;
      next.push(c);
      byId.delete(o.id);
    }
    next.push(...byId.values());
    this.lib.connections = next;
    this.persist();
  }

  async saveGroups(inputs: GroupInput[]): Promise<Library> {
    const prev = new Map(this.lib.groups.map((g) => [g.id, g]));
    const groups: Group[] = inputs.map((inp) => {
      const old = prev.get(inp.id);
      const builtin = inp.id === DEFAULT_GROUP_ID;
      return {
        id: inp.id,
        name: builtin ? "All" : inp.name,
        collapsed: inp.collapsed,
        builtin,
        gateway: this.storeGateway(`g:${inp.id}`, inp.gateway, old?.gateway ?? null),
      };
    });
    if (!groups.some((g) => g.id === DEFAULT_GROUP_ID)) groups.unshift(prev.get(DEFAULT_GROUP_ID) ?? defaultGroup());
    const ids = new Set(groups.map((g) => g.id));
    for (const c of this.lib.connections) if (!ids.has(c.groupId)) c.groupId = DEFAULT_GROUP_ID;
    this.lib.groups = groups;
    this.persist();
    return this.snapshot();
  }

  /**
   * Makes `groupId` contain exactly `items` (in order). Matching connections
   * (host, port, username) are kept and updated; the rest are removed.
   */
  private replaceInto(groupId: GroupId, items: NewConnection[]): { kept: number; added: number; removed: number; updated: number } {
    const members = this.lib.connections.filter((c) => c.groupId === groupId);
    const byKey = new Map<string, Connection>();
    for (const c of members) if (!byKey.has(matchKey(c))) byKey.set(matchKey(c), c);
    const keep: Connection[] = [];
    let added = 0;
    let updated = 0;
    const now = Date.now();
    for (const it of items) {
      const existing = byKey.get(matchKey(it));
      if (existing) {
        byKey.delete(matchKey(it));
        if (it.name) existing.name = it.name;
        if (it.domain !== undefined) existing.domain = it.domain;
        if (it.notes) existing.notes = it.notes;
        if (it.password) {
          this.passwords[existing.id] = it.password;
          existing.hasPassword = true;
        }
        if (it.gateway !== undefined) existing.gateway = this.storeGateway(`c:${existing.id}`, it.gateway, existing.gateway);
        keep.push(existing);
        updated++;
      } else {
        keep.push(this.makeConnection({ ...it, groupId }, now));
        added++;
      }
    }
    const removedIds = [...byKey.values()].map((c) => c.id);
    void this.disconnect(removedIds);
    for (const id of removedIds) delete this.passwords[id];
    const removedSet = new Set(removedIds);
    // Keep the group's slot in the global order: place the new list where the group's first member was.
    const firstIdx = this.lib.connections.findIndex((c) => c.groupId === groupId);
    const others = this.lib.connections.filter((c) => c.groupId !== groupId && !removedSet.has(c.id));
    const at = firstIdx < 0 ? others.length : this.lib.connections.slice(0, firstIdx).filter((c) => c.groupId !== groupId).length;
    others.splice(at, 0, ...keep);
    this.lib.connections = others;
    return { kept: updated, added, removed: removedIds.length, updated };
  }

  async replaceGroup(groupId: GroupId, items: NewConnection[]): Promise<ReplaceResult> {
    if (!this.lib.groups.some((g) => g.id === groupId)) throw new Error("Group not found");
    const r = this.replaceInto(groupId, items);
    this.persist();
    return { kept: r.kept, added: r.added, removed: r.removed, library: this.snapshot() };
  }

  async saveSettings(settings: Settings): Promise<void> {
    this.lib.settings = { ...settings };
    this.persist();
    this.pump();
  }

  // ── Export / import ──────────────────────────────────────────────────────

  async exportData(options: ExportOptions): Promise<string> {
    const { format } = options;
    const wanted = options.groupIds?.length ? new Set(options.groupIds) : null;
    const groups = this.lib.groups.filter((g) => !wanted || wanted.has(g.id));
    const pw = (raw: string | undefined): string | undefined => raw || undefined;
    const gw = (key: string, g: GatewaySettings | null): ExportGateway | null =>
      g ? { ...g, password: pw(this.gatewayPasswords[key]) } : null;

    if (format === "text") {
      const out: string[] = [];
      for (const g of groups) {
        const members = this.lib.connections.filter((c) => c.groupId === g.id);
        if (!members.length) continue;
        out.push(`# Group: ${g.name}`);
        for (const c of members) {
          const user = c.domain ? `${c.domain}\\${c.username}` : c.username;
          const p = this.passwords[c.id];
          out.push(p ? `${c.host}:${c.port}:${user}:${p}` : `${c.host}:${c.port}:${user}`);
        }
        out.push("");
      }
      return out.join("\n").trimEnd() + "\n";
    }

    const doc: ExportDoc = {
      format: "rdp-app/export",
      version: 1,
      exportedAt: new Date().toISOString(),
      passwords: "plain",
      groups: groups.map((g) => ({
        name: g.name,
        gateway: gw(`g:${g.id}`, g.gateway),
        connections: this.lib.connections
          .filter((c) => c.groupId === g.id)
          .map((c) => ({
            name: c.name,
            host: c.host,
            port: c.port,
            username: c.username,
            domain: c.domain,
            password: pw(this.passwords[c.id]),
            notes: c.notes,
            gateway: gw(`c:${c.id}`, c.gateway),
          })),
      })),
    };
    return JSON.stringify(doc, null, 2);
  }

  private destination(name: string, options: ImportOptions): GroupId | null {
    if (options.target !== "byName") return options.target;
    const lower = name.trim().toLowerCase();
    const g = this.lib.groups.find((x) => x.name.trim().toLowerCase() === lower || (x.builtin && lower === "all"));
    return g?.id ?? null;
  }

  async previewImport(source: ImportSource, options: ImportOptions): Promise<ImportPreview> {
    const parsed = parseImport(source);
    const groups = parsed.groups.map((g) => {
      const dest = this.destination(g.name, options);
      const existing = new Set(this.lib.connections.filter((c) => c.groupId === dest).map(matchKey));
      return {
        name: g.name,
        connections: g.connections.length,
        existingGroupId: dest,
        duplicates: g.connections.filter((c) => existing.has(matchKey(c))).length,
      };
    });
    return {
      format: parsed.format,
      groups,
      total: groups.reduce((n, g) => n + g.connections, 0),
      errors: parsed.errors,
    };
  }

  async applyImport(source: ImportSource, options: ImportOptions): Promise<ImportResult> {
    const parsed = parseImport(source);
    let added = 0;
    let updated = 0;
    let removed = 0;
    let groupsCreated = 0;
    const toItem = (c: ExportConnection, groupId: GroupId): NewConnection => ({
      name: c.name,
      host: c.host,
      port: c.port,
      username: c.username,
      domain: c.domain,
      password: c.password,
      notes: c.notes,
      groupId,
      gateway: c.gateway ? { ...c.gateway } : null,
    });

    // Group the imported connections by destination first, so "replace" with a
    // single target collects everything into one list.
    const byDest = new Map<GroupId, NewConnection[]>();
    for (const g of parsed.groups) {
      let dest = this.destination(g.name, options);
      if (!dest) {
        dest = uid();
        this.lib.groups.push({
          id: dest,
          name: g.name || "Imported",
          collapsed: false,
          builtin: false,
          gateway: this.storeGateway(`g:${dest}`, g.gateway ? { ...g.gateway } : null, null),
        });
        groupsCreated++;
      }
      const list = byDest.get(dest) ?? [];
      list.push(...g.connections.map((c) => toItem(c, dest!)));
      byDest.set(dest, list);
    }

    for (const [dest, items] of byDest) {
      if (options.mode === "replace") {
        const r = this.replaceInto(dest, items);
        added += r.added;
        updated += r.updated;
        removed += r.removed;
      } else {
        const existing = new Map(this.lib.connections.filter((c) => c.groupId === dest).map((c) => [matchKey(c), c]));
        for (const it of items) {
          const match = existing.get(matchKey(it));
          if (match) {
            if (it.name) match.name = it.name;
            if (it.domain) match.domain = it.domain;
            if (it.password) {
              this.passwords[match.id] = it.password;
              match.hasPassword = true;
            }
            updated++;
          } else {
            const c = this.makeConnection(it);
            this.lib.connections.push(c);
            existing.set(matchKey(c), c);
            added++;
          }
        }
      }
    }
    this.persist();
    return { added, updated, removed, groupsCreated, library: this.snapshot() };
  }

  async saveFile(content: string, suggestedName: string): Promise<boolean> {
    const blob = new Blob([content], { type: suggestedName.endsWith(".json") ? "application/json" : "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = suggestedName;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return true;
  }

  async pickFolder(): Promise<string | null> {
    const path = window.prompt("Folder to share (browser preview)", "D:\Shared");
    return path?.trim() || null;
  }

  openFile(): Promise<{ name: string; text: string } | null> {
    return new Promise((resolve) => {
      const input = document.createElement("input");
      input.type = "file";
      input.accept = ".json,.txt,.csv,text/plain,application/json";
      input.style.display = "none";
      input.addEventListener("change", async () => {
        const file = input.files?.[0];
        input.remove();
        resolve(file ? { name: file.name, text: await file.text() } : null);
      });
      input.addEventListener("cancel", () => {
        input.remove();
        resolve(null);
      });
      document.body.appendChild(input);
      input.click();
    });
  }

  // ── Sessions ─────────────────────────────────────────────────────────────

  async connect(ids: ConnectionId[]): Promise<void> {
    for (const id of ids) {
      const s = this.sessions.get(id);
      if (s && s.status !== "idle" && s.status !== "error") continue;
      if (!this.lib.connections.some((c) => c.id === id)) continue;
      this.queue.push(id);
      this.setStatus(id, "queued");
    }
    this.pump();
  }

  private pump() {
    const max = Math.max(1, this.lib.settings.maxConcurrentConnects);
    while (this.active < max && this.queue.length) {
      const id = this.queue.shift()!;
      this.active++;
      this.setStatus(id, "connecting");
      const s = this.sessions.get(id)!;
      s.timer = setTimeout(() => {
        this.active--;
        s.timer = undefined;
        const conn = this.lib.connections.find((c) => c.id === id);
        const endpoint = conn ? `${conn.host}:${conn.port}` : "";
        const trusted = this.certs.find((c) => c.endpoint === endpoint);
        const policy = this.lib.settings.certificatePolicy;
        const roll = Math.random();
        if (conn && policy !== "ignore") {
          // Trust on first use records new certificates silently; "prompt" asks.
          // ~5% of reconnects to a known host simulate a changed certificate.
          let reason: "unknown" | "changed" | null = null;
          if (!trusted && policy === "prompt") reason = "unknown";
          else if (!trusted) void this.trustCertificate(endpoint, fakeFingerprint(endpoint));
          else if (roll < 0.05) reason = "changed";
          if (reason) {
            this.emit({
              type: "certificate",
              id,
              endpoint,
              fingerprint: fakeFingerprint(endpoint + (reason === "changed" ? Date.now() : "")),
              reason,
              previousFingerprint: reason === "changed" ? trusted!.fingerprint : undefined,
              subject: `CN=${conn.host}`,
            });
            this.setStatus(id, "error", reason === "changed" ? "Server certificate changed" : "Server certificate is not trusted yet");
            this.pump();
            return;
          }
        }
        if (roll > 0.92) {
          this.setStatus(id, "error", ERRORS[Math.floor(Math.random() * ERRORS.length)]);
        } else {
          this.setStatus(id, "connected");
          this.emit({ type: "resolution", id, width: 1920, height: 1080 });
          this.emit({ type: "codec", id, codec: CODECS[Math.floor(Math.random() * CODECS.length)] });
        }
        this.pump();
      }, 600 + Math.random() * 1400);
    }
  }

  async reconnect(ids: ConnectionId[]): Promise<void> {
    await this.disconnect(ids);
    await this.connect(ids);
  }

  async disconnect(ids: ConnectionId[]): Promise<void> {
    for (const id of ids) {
      const s = this.sessions.get(id);
      if (!s || s.status === "idle") continue;
      this.queue = this.queue.filter((q) => q !== id);
      if (s.timer) {
        clearTimeout(s.timer);
        s.timer = undefined;
        this.active--;
      }
      this.setStatus(id, "idle");
    }
    this.pump();
  }

  // ── Certificates & API ───────────────────────────────────────────────────

  async trustCertificate(endpoint: string, fingerprint: string): Promise<void> {
    this.certs = this.certs.filter((c) => c.endpoint !== endpoint);
    this.certs.push({ endpoint, fingerprint, subject: `CN=${endpoint.split(":")[0]}`, firstSeen: Date.now() });
    this.persist();
  }

  async listCertificates(): Promise<TrustedCertificate[]> {
    return structuredClone(this.certs);
  }

  async forgetCertificate(endpoint: string): Promise<void> {
    this.certs = this.certs.filter((c) => c.endpoint !== endpoint);
    this.persist();
  }

  async apiInfo(): Promise<ApiInfo> {
    const { apiEnabled, apiPort } = this.lib.settings;
    const url = `http://127.0.0.1:${apiPort}`;
    return {
      enabled: apiEnabled,
      url,
      token: this.token,
      docsUrl: `${url}/docs`,
      llmsUrl: `${url}/llms.txt`,
      running: apiEnabled,
      error: apiEnabled && (apiPort < 1024 || apiPort > 65535) ? `Port ${apiPort} is not available` : undefined,
    };
  }

  async regenerateApiToken(): Promise<ApiInfo> {
    this.token = this.newToken();
    return this.apiInfo();
  }

  onSessionEvent(cb: (e: SessionEvent) => void): Unsubscribe {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  onLibraryChanged(_cb: (library: Library) => void): Unsubscribe {
    return () => {};
  }

  onUiCommand(_cb: (command: UiCommand) => void): Unsubscribe {
    return () => {};
  }

  onCompanionStatus(_cb: (status: CompanionStatus) => void): Unsubscribe {
    return () => {};
  }

  async companionTrustHost(): Promise<string> {
    return "WinrmUnavailable";
  }

  reportUiState(state: UiState): void {
    if (import.meta.env.DEV) (window as unknown as { __uiState: UiState }).__uiState = state;
  }

  async openExternal(url: string): Promise<void> {
    window.open(url, "_blank", "noopener");
  }

  // The browser preview has a single window.
  async windowLayout(): Promise<WindowLayout> {
    return { mainTabs: [], windows: [] };
  }

  onWindowLayout(_cb: (layout: WindowLayout) => void): Unsubscribe {
    return () => {};
  }

  async openTab(): Promise<void> {}

  async closeTab(): Promise<void> {}

  async setMainTabs(): Promise<void> {}

  onSelectTab(_cb: (id: ConnectionId) => void): Unsubscribe {
    return () => {};
  }

  async moveTab(): Promise<void> {}

  async prepareWindow(): Promise<void> {}

  async tearTab(): Promise<string> {
    return "main";
  }

  async dragWindow(): Promise<string> {
    return "main";
  }

  async setTabArea(): Promise<void> {}

  async setWindowMaterial(): Promise<void> {}

  async detachTab(): Promise<string> {
    throw new Error("Separate windows need the desktop app");
  }

  async reorderWindowTabs(): Promise<void> {}

  async showTab(): Promise<void> {}
}

function fakeFingerprint(seed: string): string {
  const bytes: string[] = [];
  let h = 0x811c9dc5;
  for (let i = 0; i < 32; i++) {
    for (let j = 0; j < seed.length; j++) h = Math.imul(h ^ (seed.charCodeAt(j) + i), 16777619);
    bytes.push(((h >>> 8) & 0xff).toString(16).padStart(2, "0").toUpperCase());
  }
  return bytes.join(":");
}
