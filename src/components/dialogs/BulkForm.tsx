import { useDeferredValue, useMemo, useState, type FormEvent } from "react";
import { parseBulk, type BulkLine } from "../../lib/bulkParse";
import type { GroupId } from "../../lib/types";
import { useStore } from "../../state/store";
import { Button, Field } from "../ui/ui";
import { BULK_PLACEHOLDER, BulkPreviewTable } from "./BulkPreview";
import { GroupSelect, NEW_GROUP } from "./GroupSelect";

export interface BulkSubmit {
  lines: BulkLine[];
  group: GroupId | typeof NEW_GROUP;
  newGroupName: string;
}

export function BulkForm({
  initialText = "",
  defaultGroup,
  onSubmit,
  onCancel,
}: {
  initialText?: string;
  defaultGroup: GroupId;
  onSubmit: (v: BulkSubmit) => Promise<void> | void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initialText);
  const [group, setGroup] = useState<GroupId | typeof NEW_GROUP>(defaultGroup);
  const [newGroupName, setNewGroupName] = useState("");
  const [busy, setBusy] = useState(false);
  const connById = useStore((s) => s.connById);
  const deferred = useDeferredValue(text);

  const existing = useMemo(() => Object.values(connById), [connById]);
  const lines = useMemo(() => parseBulk(deferred, existing), [deferred, existing]);
  const ok = lines.filter((l) => l.status === "ok").length;
  const dup = lines.filter((l) => l.status === "duplicate").length;
  const bad = lines.filter((l) => l.status === "error").length;
  const groupInvalid = group === NEW_GROUP && !newGroupName.trim();

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!ok || groupInvalid) return;
    setBusy(true);
    try {
      await onSubmit({ lines: parseBulk(text, existing), group, newGroupName: newGroupName.trim() });
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="dialog__form" onSubmit={submit} noValidate>
      <div className="dialog__body bulk">
        <Field
          label="Connections"
          hint={
            <>
              One per line: <span className="mono">host:user:pass</span> · optional port{" "}
              <span className="mono">host:port:user:pass</span> · <span className="mono">DOMAIN\user</span> works too
            </>
          }
        >
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
            <div className="bulk__summary">
              <span className="bulk-chip bulk-chip--ok">{ok} ready</span>
              {dup > 0 && <span className="bulk-chip bulk-chip--dup">{dup} duplicate{dup === 1 ? "" : "s"} skipped</span>}
              {bad > 0 && <span className="bulk-chip bulk-chip--err">{bad} invalid</span>}
            </div>
            <BulkPreviewTable lines={lines} />
          </div>
        )}

        <Field label="Add to group" hint={groupInvalid ? "Name the new group" : undefined}>
          <GroupSelect value={group} onChange={setGroup} newName={newGroupName} onNewName={setNewGroupName} />
        </Field>
      </div>
      <div className="dialog__footer">
        <span className="dialog__footer-note">Connections are saved, not connected.</span>
        <div className="dialog__footer-spacer" />
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="primary" type="submit" disabled={!ok || groupInvalid || busy}>
          {ok ? `Add ${ok} connection${ok === 1 ? "" : "s"}` : "Add connections"}
        </Button>
      </div>
    </form>
  );
}
