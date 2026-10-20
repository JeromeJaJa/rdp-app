import { useState } from "react";
import type { SettingOverrides } from "../../lib/types";
import { closeDialog, setGroupSettings } from "../../state/actions";
import { useStore } from "../../state/store";
import { Button } from "../ui/ui";
import { Dialog } from "./Dialog";
import { OverridesEditor, overrideCount } from "./OverridesEditor";

/** A group's setting overrides for its connections (over the app settings). */
export function GroupSettingsDialog({ groupId }: { groupId: string }) {
  const group = useStore((s) => s.groups.find((g) => g.id === groupId));
  const appSettings = useStore((s) => s.settings);
  const [value, setValue] = useState<SettingOverrides>(group?.settings ?? {});
  const [busy, setBusy] = useState(false);
  if (!group) return null;

  const save = async () => {
    setBusy(true);
    await setGroupSettings(groupId, value);
    setBusy(false);
    closeDialog();
  };

  return (
    <Dialog
      title={`${group.name} settings`}
      width={520}
      footer={
        <>
          <span className="dialog__footer-note">
            {overrideCount(value) ? `${overrideCount(value)} changed for this group` : "Same as the app"} · connections can
            still override
          </span>
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" onClick={closeDialog}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="dialog__body">
        <OverridesEditor value={value} onChange={setValue} inherited={appSettings} inheritedFrom="the app settings" />
      </div>
    </Dialog>
  );
}
