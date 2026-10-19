import type { ReactNode } from "react";
import { AlertCircle, Check, Copy } from "lucide-react";
import type { BulkLine } from "../../lib/bulkParse";

export const BULK_PLACEHOLDER = `10.0.0.21:Administrator:Passw0rd!
10.0.0.22:3390:admin:s3cret:with:colons
srv-app-01.corp.local:CORP\\jdoe:hunter2
# lines starting with # are ignored`;

export function BulkStatusCell({ line }: { line: BulkLine }) {
  if (line.status === "ok")
    return (
      <span className="bulk-status bulk-status--ok">
        <Check size={12} /> Ready
      </span>
    );
  if (line.status === "duplicate")
    return (
      <span className="bulk-status bulk-status--dup" title="Same host, port and user already exists">
        <Copy size={12} /> Duplicate
      </span>
    );
  return (
    <span className="bulk-status bulk-status--err" title={line.error}>
      <AlertCircle size={12} /> {line.error}
    </span>
  );
}

/** Parsed bulk lines as a compact table. */
export function BulkPreviewTable({
  lines,
  status = (l) => <BulkStatusCell line={l} />,
  limit = 500,
}: {
  lines: BulkLine[];
  status?: (line: BulkLine) => ReactNode;
  limit?: number;
}) {
  return (
    <div className="bulk__table-wrap">
      <table className="bulk__table">
        <thead>
          <tr>
            <th className="num">#</th>
            <th>Host</th>
            <th className="num">Port</th>
            <th>User</th>
            <th>Password</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {lines.slice(0, limit).map((l) => (
            <tr key={l.line} className={`is-${l.status}`}>
              <td className="num mono">{l.line}</td>
              {l.entry ? (
                <>
                  <td className="mono ellipsis">{l.entry.host}</td>
                  <td className="num mono">{l.entry.port}</td>
                  <td className="ellipsis">
                    {l.entry.domain ? `${l.entry.domain}\\` : ""}
                    {l.entry.username}
                  </td>
                  <td className="mono muted">{l.entry.password ? "•".repeat(Math.min(10, l.entry.password.length)) : "—"}</td>
                </>
              ) : (
                <td colSpan={4} className="mono muted ellipsis">
                  {l.raw.trim()}
                </td>
              )}
              <td>{status(l)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {lines.length > limit && <div className="bulk__more">+ {lines.length - limit} more lines</div>}
    </div>
  );
}
