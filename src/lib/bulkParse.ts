// Bulk connection parser: one connection per line.
//
// Accepted shapes (password is everything after the user field, so it may contain ':'):
//   10.0.0.5:admin:hunter2                 → host, user, pass
//   10.0.0.5:3390:admin:hunter2            → host, port, user, pass (≥4 fields, field 2 = 1–65535)
//   srv01.corp.local:CORP\jdoe:p@ss:word   → domain "CORP", user "jdoe", pass "p@ss:word"
//   srv01:jdoe@corp.local:secret           → user "jdoe@corp.local" (UPN kept as-is)
//   [fe80::1]:3389:admin:secret            → IPv6 host in brackets
//   10.0.0.5  admin  secret                → whitespace / tab / comma / semicolon separated (no ':')
//   # comment lines and blank lines are skipped
//
// Examples:
//   parseBulk("a:u:p\na:u:p", [])          → [ok, duplicate]
//   parseBulk("a:99999:u:p", [])           → [error "Invalid port 99999"]
//   parseBulk("a", [])                     → [error "Missing username"]

export interface BulkEntry {
  host: string;
  port: number;
  username: string;
  domain: string;
  password: string;
}

export type BulkStatus = "ok" | "duplicate" | "error";

export interface BulkLine {
  /** 1-based line number in the pasted text. */
  line: number;
  raw: string;
  status: BulkStatus;
  error?: string;
  entry?: BulkEntry;
}

export interface ExistingKey {
  host: string;
  port: number;
  username: string;
  domain: string;
}

const DEFAULT_PORT = 3389;
const HOSTNAME_RE = /^[A-Za-z0-9]([A-Za-z0-9\-_.]*[A-Za-z0-9])?$/;
const IPV6_RE = /^[0-9A-Fa-f:.]+(%[\w.]+)?$/;

export function connectionKey(k: ExistingKey): string {
  return `${k.host.toLowerCase()}|${k.port}|${k.domain.toLowerCase()}|${k.username.toLowerCase()}`;
}

function splitUser(user: string): { username: string; domain: string } {
  const slash = user.indexOf("\\");
  if (slash > 0) return { domain: user.slice(0, slash), username: user.slice(slash + 1) };
  return { username: user, domain: "" };
}

function parseLine(text: string): { entry?: BulkEntry; error?: string } {
  let host: string;
  let rest: string[];

  if (text.startsWith("[")) {
    const close = text.indexOf("]");
    if (close < 0) return { error: "Unclosed '[' in IPv6 host" };
    host = text.slice(1, close);
    const after = text.slice(close + 1);
    if (after && !after.startsWith(":")) return { error: "Expected ':' after ']'" };
    rest = after ? after.slice(1).split(":") : [];
    if (!IPV6_RE.test(host)) return { error: "Invalid IPv6 address" };
  } else if (!text.includes(":")) {
    // Fallback for spreadsheet-ish pastes: tab, comma, semicolon or whitespace separated.
    const sep = text.includes("\t") ? /\t+/ : text.includes(",") ? /\s*,\s*/ : text.includes(";") ? /\s*;\s*/ : /\s+/;
    const parts = text.split(sep);
    host = parts[0];
    rest = parts.slice(1);
    // Re-join with ':' semantics: the password is everything after the user.
    if (rest.length > 2 && !/^\d+$/.test(rest[0])) rest = [rest[0], rest.slice(1).join(" ")];
  } else {
    const parts = text.split(":");
    host = parts[0];
    rest = parts.slice(1);
  }

  host = host.trim();
  if (!host) return { error: "Missing host" };
  if (!text.startsWith("[") && !HOSTNAME_RE.test(host)) return { error: "Invalid host" };
  if (rest.length === 0 || (rest.length === 1 && !rest[0].trim())) return { error: "Missing username" };

  let port = DEFAULT_PORT;
  if (rest.length >= 3 && /^\d+$/.test(rest[0].trim())) {
    const p = Number(rest[0].trim());
    if (p < 1 || p > 65535) return { error: `Invalid port ${rest[0].trim()}` };
    port = p;
    rest = rest.slice(1);
  }

  const user = rest[0].trim();
  if (!user) return { error: "Missing username" };
  const password = rest.slice(1).join(":");
  const { username, domain } = splitUser(user);
  if (!username) return { error: "Missing username" };

  return { entry: { host, port, username, domain, password } };
}

export function parseBulk(text: string, existing: ExistingKey[]): BulkLine[] {
  const seen = new Set(existing.map(connectionKey));
  const out: BulkLine[] = [];
  const lines = text.split(/\r?\n/);

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const trimmed = raw.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const { entry, error } = parseLine(trimmed);
    if (!entry) {
      out.push({ line: i + 1, raw, status: "error", error });
      continue;
    }
    const key = connectionKey(entry);
    if (seen.has(key)) {
      out.push({ line: i + 1, raw, status: "duplicate", entry, error: "Already exists" });
      continue;
    }
    seen.add(key);
    out.push({ line: i + 1, raw, status: "ok", entry });
  }
  return out;
}
