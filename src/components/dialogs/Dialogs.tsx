import { closeDialog } from "../../state/actions";
import { useStore } from "../../state/store";
import { Button } from "../ui/ui";
import { AddDialog } from "./AddDialog";
import { DeleteGroupDialog } from "./DeleteGroupDialog";
import { Dialog } from "./Dialog";
import { EditDialog } from "./EditDialog";
import { ExportDialog } from "./ExportDialog";
import { GatewayDialog } from "./GatewayDialog";
import { GroupSettingsDialog } from "./GroupSettingsDialog";
import { ImportDialog } from "./ImportDialog";
import { ReplaceDialog } from "./ReplaceDialog";
import { SettingsDialog } from "./SettingsDialog";
import { SendDialog } from "../transfers/SendDialog";

export function Dialogs() {
  const dialog = useStore((s) => s.dialog);
  if (!dialog) return null;
  switch (dialog.kind) {
    case "add":
      return <AddDialog mode={dialog.mode} groupId={dialog.groupId} />;
    case "edit":
      return <EditDialog key={dialog.id} id={dialog.id} />;
    case "settings":
      return <SettingsDialog section={dialog.section} />;
    case "replace":
      return <ReplaceDialog key={dialog.groupId} groupId={dialog.groupId} />;
    case "import":
      return <ImportDialog groupId={dialog.groupId} />;
    case "export":
      return <ExportDialog groupIds={dialog.groupIds} />;
    case "gateway":
      return <GatewayDialog key={dialog.groupId} groupId={dialog.groupId} />;
    case "groupSettings":
      return <GroupSettingsDialog key={dialog.groupId} groupId={dialog.groupId} />;
    case "deleteGroup":
      return <DeleteGroupDialog groupId={dialog.groupId} />;
    case "send":
      return <SendDialog ids={dialog.ids} paths={dialog.paths} />;
    case "confirm":
      return (
        <Dialog
          title={dialog.title}
          width={400}
          className="dialog--confirm"
          footer={
            <>
              <div className="dialog__footer-spacer" />
              <Button variant="ghost" onClick={closeDialog}>
                Cancel
              </Button>
              <Button
                variant={dialog.danger ? "danger" : "primary"}
                data-autofocus
                onClick={() => {
                  closeDialog();
                  dialog.onConfirm();
                }}
              >
                {dialog.confirmLabel}
              </Button>
            </>
          }
        >
          <p className="dialog__message">{dialog.message}</p>
        </Dialog>
      );
  }
}
