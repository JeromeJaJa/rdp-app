import { closeDialog, confirm, deleteConnections, setConnectionSettings, toast, updateConnection } from "../../state/actions";
import { displayName, useConnection } from "../../state/store";
import { resolveGroup } from "./AddDialog";
import { CONNECTION_FORM_WIDTH, ConnectionForm, type ConnectionFormValues } from "./ConnectionForm";
import { Dialog } from "./Dialog";

export function EditDialog({ id }: { id: string }) {
  const conn = useConnection(id);
  if (!conn) return null;

  const save = async (v: ConnectionFormValues) => {
    const groupId = resolveGroup(v.group, v.newGroupName);
    try {
      await updateConnection(id, {
        name: v.name,
        host: v.host,
        port: v.port,
        username: v.username,
        domain: v.domain,
        password: v.password,
        groupId,
        notes: v.notes,
        gateway: v.gateway,
      });
      if (JSON.stringify(v.settings) !== JSON.stringify(conn.settings ?? {})) await setConnectionSettings(id, v.settings);
      closeDialog();
    } catch (err) {
      toast("error", "Could not save", err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <Dialog title="Edit connection" width={CONNECTION_FORM_WIDTH} bare>
      <ConnectionForm
        initial={conn}
        submitLabel="Save"
        onSubmit={save}
        onCancel={closeDialog}
        onDelete={() =>
          confirm({
            title: `Delete ${displayName(conn)}?`,
            message: "The connection and its saved password will be removed. This can't be undone.",
            confirmLabel: "Delete",
            danger: true,
            onConfirm: () => deleteConnections([id]),
          })
        }
      />
    </Dialog>
  );
}
