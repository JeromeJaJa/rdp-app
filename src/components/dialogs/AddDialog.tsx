import { useState } from "react";
import { addConnections, closeDialog, createGroup, select, toast } from "../../state/actions";
import { getState } from "../../state/store";
import { DEFAULT_GROUP_ID, type GroupId } from "../../lib/types";
import { Segmented } from "../ui/ui";
import { BulkForm, type BulkSubmit } from "./BulkForm";
import { ConnectionForm, type ConnectionFormValues } from "./ConnectionForm";
import { Dialog } from "./Dialog";
import { NEW_GROUP } from "./GroupSelect";

export function resolveGroup(group: GroupId | typeof NEW_GROUP, newName: string): GroupId {
  if (group !== NEW_GROUP) return group;
  const existing = getState().groups.find((g) => g.name.toLowerCase() === newName.toLowerCase());
  return existing ? existing.id : createGroup(newName);
}

export function AddDialog({ mode: initialMode, groupId }: { mode: "single" | "bulk"; groupId?: GroupId }) {
  const [mode, setMode] = useState(initialMode);
  const [bulkText, setBulkText] = useState("");

  const addSingle = async (v: ConnectionFormValues) => {
    const gid = resolveGroup(v.group, v.newGroupName);
    const [created] = await addConnections([
      {
        name: v.name,
        host: v.host,
        port: v.port,
        username: v.username,
        domain: v.domain,
        password: v.password || undefined,
        groupId: gid,
        notes: v.notes,
        gateway: v.gateway,
      },
    ]);
    closeDialog();
    if (created) select(created.id, { connect: false });
  };

  const addBulk = async ({ lines, group, newGroupName }: BulkSubmit) => {
    const ok = lines.filter((l) => l.status === "ok" && l.entry);
    if (!ok.length) return;
    const gid = resolveGroup(group, newGroupName);
    const created = await addConnections(
      ok.map(({ entry }) => ({
        host: entry!.host,
        port: entry!.port,
        username: entry!.username,
        domain: entry!.domain,
        password: entry!.password || undefined,
        groupId: gid,
      })),
    );
    closeDialog();
    const skipped = lines.length - ok.length;
    toast(
      "success",
      `Added ${created.length} connection${created.length === 1 ? "" : "s"}`,
      skipped ? `${skipped} line${skipped === 1 ? "" : "s"} skipped` : undefined,
    );
    if (created[0] && !getState().selectedId) select(created[0].id, { connect: false });
  };

  return (
    <Dialog
      title={mode === "single" ? "New connection" : "Bulk add"}
      width={mode === "single" ? 500 : 680}
      bare
      headerExtra={
        <Segmented
          size="sm"
          className="dialog__mode"
          value={mode}
          onChange={setMode}
          options={[
            { value: "single", label: "Single" },
            { value: "bulk", label: "Bulk" },
          ]}
        />
      }
    >
      {mode === "single" ? (
        <ConnectionForm
          defaultGroup={groupId ?? DEFAULT_GROUP_ID}
          submitLabel="Add connection"
          onSubmit={addSingle}
          onCancel={closeDialog}
          onBulkPaste={(text) => {
            setBulkText(text);
            setMode("bulk");
          }}
        />
      ) : (
        <BulkForm key={bulkText} initialText={bulkText} defaultGroup={groupId ?? DEFAULT_GROUP_ID} onSubmit={addBulk} onCancel={closeDialog} />
      )}
    </Dialog>
  );
}
