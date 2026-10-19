import { useDeferredValue, useMemo, useState } from "react";
import { AlertCircle, AlertTriangle, Check, Copy, Plus, RefreshCw } from "lucide-react";
import { parseBulk, type BulkLine } from "../../lib/bulkParse";
import { matchKey } from "../../lib/match";
import { closeDialog, errorText, groupMembers, replaceGroup, toast } from "../../state/actions";
import { displayName, isBusy, isLive, useStore } from "../../state/store";
import { Button, Field } from "../ui/ui";
import { BULK_PLACEHOLDER, BulkPreviewTable } from "./BulkPreview";
import { Dialog } from "./Dialog";

export function ReplaceDialog({ groupId }: { groupId: string }) {
  const group = useStore((s) => s.groups.find((g) => g.id === groupId));
  const connById = useStore((s) => s.connById);
  const sessions = useStore((s) => s.sessions);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const deferred = useDeferredValue(text);

  const members = useMemo(() => groupMembers(groupId).map((id) => connById[id]).filter(Boolean), [groupId, connById]);
  // Duplicates are only checked within the pasted list: the group is being replaced.
  const lines = useMemo(() => parseBulk(deferred, []), [deferred]);

  const diff = useMemo(() => {
    const byKey = new Map(members.map((c) => [matchKey(c), c]));
    const keepKeys = new Set<string>();
    let add = 0;
    let keep = 0;
    const lineState = new Map<number, "keep" | "add">();
    for (const l of lines) {
      if (l.status !== "ok" || !l.entry) continue;
      const k = matchKey(l.entry);
      if (byKey.has(k) && !keepKeys.has(k)) {
        keepKeys.add(k);
        keep++;
        lineState.set(l.line, "keep");
      } else {
        add++;
        lineState.set(l.line, "add");
      }
    }
    const removed = members.filter((c) => !keepKeys.has(matchKey(c)));
    const removedLive = removed.filter((c) => {
      const st = sessions[c.id]?.status ?? "idle";
      return isLive(st) || isBusy(st);
    }).length;
    return { keep, add, removed, removedLive, lineState };
  }, [lines, members, sessions]);

  if (!group) return null;
  const valid = diff.keep + diff.add;
  const bad = lines.filter((l) => l.status === "error").length;
  const dup = lines.filter((l) => l.status === "duplicate").length;

  const status = (l: BulkLine) => {
    const st = diff.lineState.get(l.line);
    if (st === "keep")
      return (
        <span className="bulk-status bulk-status--keep" title="Matches an existing connection (same host, port, user); it stays and is updated">
          <RefreshCw size={12} /> Keep
        </span>
      );
    if (st === "add")
      return (
        <span className="bulk-status bulk-status--ok">
          <Plus size={12} /> Add
        </span>
      );
    if (l.status === "duplicate")
      return (
        <span className="bulk-status bulk-status--dup" title="Listed twice">
          <Copy size={12} /> Duplicate
        </span>
      );
    return (
      <span className="bulk-status bulk-status--err" title={l.error}>
        <AlertCircle size={12} /> {l.error}
      </span>
    );
  };

  const apply = async () => {
    const entries = lines.filter((l) => l.status === "ok" && l.entry).map((l) => l.entry!);
    setBusy(true);
    try {
      const r = await replaceGroup(
        groupId,
        entries.map((e) => ({
          host: e.host,
          port: e.port,
          username: e.username,
          domain: e.domain,
          password: e.password || undefined,
        })),
      );
      closeDialog();
      const parts = [`${r.kept} kept`, `${r.added} added`, `${r.removed} removed`];
      toast("success", `Replaced connections in “${group.name}”`, parts.join(" · "));
    } catch (err) {
      toast("error", "Could not replace connections", errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={
        <>
          Replace connections in <span className="dialog__title-em">“{group.name}”</span>
        </>
      }
      width={700}
      footer={
        <>
          <span className="dialog__footer-note">
            {members.length} now → {valid} after
          </span>
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" onClick={closeDialog}>
            Cancel
          </Button>
          <Button variant={diff.removed.length ? "danger" : "primary"} disabled={!valid || busy} onClick={apply}>
            {valid ? `Replace with ${valid} connection${valid === 1 ? "" : "s"}` : "Replace"}
          </Button>
        </>
      }
    >
      <div className="bulk">
        <p className="dialog__lede">
          The group will contain exactly this list. Connections with the same host, port and user are kept (with their live
          sessions) and updated; everything else in the group is removed.
        </p>
        <Field label="New list" hint={<>Same format as bulk add: <span className="mono">host[:port]:user:pass</span></>}>
          <textarea
            className="textarea mono bulk__textarea"
            rows={7}
            value={text}
            spellCheck={false}
            placeholder={BULK_PLACEHOLDER}
            data-autofocus
            onChange={(e) => setText(e.target.value)}
          />
        </Field>

        {lines.length > 0 && (
          <div className="bulk__preview">
            <div className="replace-diff">
              <span className="replace-diff__item">
                <Check size={12} /> Keep <b>{diff.keep}</b>
              </span>
              <span className="replace-diff__sep">·</span>
              <span className="replace-diff__item replace-diff__item--add">
                <Plus size={12} /> Add <b>{diff.add}</b>
              </span>
              <span className="replace-diff__sep">·</span>
              <span className={`replace-diff__item ${diff.removed.length ? "replace-diff__item--remove" : ""}`}>
                Remove <b>{diff.removed.length}</b>
              </span>
              {(bad > 0 || dup > 0) && <span className="replace-diff__sep">·</span>}
              {dup > 0 && <span className="bulk-chip bulk-chip--dup">{dup} repeated</span>}
              {bad > 0 && <span className="bulk-chip bulk-chip--err">{bad} invalid</span>}
            </div>
            <BulkPreviewTable lines={lines} status={status} />
          </div>
        )}

        {lines.length > 0 && diff.removed.length > 0 && (
          <div className="replace-removed">
            <div className="replace-removed__head">
              <AlertTriangle size={13} />
              <span>
                {diff.removed.length} connection{diff.removed.length === 1 ? "" : "s"} will be removed
                {diff.removedLive > 0 &&
                  ` · ${diff.removedLive} live session${diff.removedLive === 1 ? "" : "s"} will be disconnected`}
              </span>
            </div>
            <ul className="replace-removed__list">
              {diff.removed.slice(0, 60).map((c) => {
                const st = sessions[c.id]?.status ?? "idle";
                const live = isLive(st) || isBusy(st);
                return (
                  <li key={c.id}>
                    <span className="replace-removed__name">{displayName(c)}</span>
                    <span className="mono replace-removed__host">
                      {c.username}@{c.host}
                      {c.port !== 3389 ? `:${c.port}` : ""}
                    </span>
                    {live && <span className="replace-removed__live">live</span>}
                  </li>
                );
              })}
              {diff.removed.length > 60 && <li className="replace-removed__more">+ {diff.removed.length - 60} more</li>}
            </ul>
          </div>
        )}
      </div>
    </Dialog>
  );
}
