// Identity used when replacing or importing into a group: the same host, port
// and username (case-insensitive) mean "the same connection". Domain and
// password may differ and are updated in place.

export interface MatchFields {
  host: string;
  port?: number;
  username: string;
}

export function matchKey(c: MatchFields): string {
  return `${c.host.trim().toLowerCase()}|${c.port ?? 3389}|${c.username.trim().toLowerCase()}`;
}
