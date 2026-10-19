import { useState } from "react";
import { closeDialog, deleteGroup, groupMembers } from "../../state/actions";
import { isBusy, isLive, useStore } from "../../state/store";
import { Button } from "../ui/ui";
import { Choice } from "./Choice";
import { Dialog } from "./Dialog";

export function DeleteGroupDialog({ groupId }: { groupId: string }) {
  const group = useStore((s) => s.groups.find((g) => g.id === groupId));
  const members = groupMembers(groupId);
  const live = useStore((s) =>
    members.reduce((n, id) => {
      const st = s.sessions[id]?.status ?? "idle";
      return n + (isLive(st) || isBusy(st) ? 1 : 0);
    }, 0),
  );
  const [mode, setMode] = useState<"move" | "delete">("move");
  if (!group || group.builtin) return null;
  const n = members.length;
  const plural = n === 1 ? "" : "s";

  return (
    <Dialog
      title={`Delete group “${group.name}”?`}
      width={440}
      className="dialog--confirm"
      footer={
        <>
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" onClick={closeDialog}>
            Cancel
          </Button>
          <Button
            variant="danger"
            data-autofocus
            onClick={() => {
              closeDialog();
              deleteGroup(groupId, mode);
            }}
          >
            {mode === "delete" && n ? `Delete group and ${n} connection${plural}` : "Delete group"}
          </Button>
        </>
      }
    >
      {n === 0 ? (
        <p className="dialog__message">The group is empty.</p>
      ) : (
        <div className="choices">
          <Choice
            checked={mode === "move"}
            onSelect={() => setMode("move")}
            title={`Move its ${n} connection${plural} to All`}
            desc="Connections, passwords and live sessions are kept."
          />
          <Choice
            checked={mode === "delete"}
            onSelect={() => setMode("delete")}
            danger
            title={`Delete the ${n} connection${plural} too`}
            desc={
              live
                ? `${live} live session${live === 1 ? " is" : "s are"} disconnected. Saved passwords are removed. This can't be undone.`
                : "Saved passwords are removed. This can't be undone."
            }
          />
        </div>
      )}
    </Dialog>
  );
}
