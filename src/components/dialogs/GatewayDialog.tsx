import { useState } from "react";
import { closeDialog, groupMembers, setGroupGateway, toast } from "../../state/actions";
import { useStore } from "../../state/store";
import { Button, Switch } from "../ui/ui";
import { Dialog } from "./Dialog";
import { GATEWAY_HOST_RE, GatewayFields, draftFromGateway, draftToInput, type GatewayDraft } from "./GatewayFields";

export function GatewayDialog({ groupId }: { groupId: string }) {
  const group = useStore((s) => s.groups.find((g) => g.id === groupId));
  const overrides = useStore((s) => groupMembers(groupId).filter((id) => s.connById[id]?.gateway).length);
  const [enabled, setEnabled] = useState(!!group?.gateway?.enabled);
  const [draft, setDraft] = useState<GatewayDraft>(() => draftFromGateway(group?.gateway ?? null));
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  if (!group) return null;

  const host = draft.host.trim();
  const hostError = !enabled ? undefined : !host ? "Gateway server is required" : !GATEWAY_HOST_RE.test(host) ? "Not a valid host name" : undefined;

  const save = async () => {
    setSubmitted(true);
    if (hostError) return;
    setBusy(true);
    try {
      // Turning it off keeps the server details for later; the group just connects directly.
      await setGroupGateway(groupId, host || enabled ? draftToInput(draft, enabled) : null);
      closeDialog();
      toast("success", enabled ? `“${group.name}” now uses ${host}` : `“${group.name}” connects directly`);
    } catch {
      /* toast shown by the action */
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="RD Gateway"
      width={480}
      footer={
        <>
          {group.gateway && (
            <Button
              variant="ghost"
              className="btn--danger-ghost"
              onClick={async () => {
                await setGroupGateway(groupId, null).catch(() => {});
                closeDialog();
              }}
            >
              Remove
            </Button>
          )}
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
      <div className="gw-dialog">
        <div className="gw-dialog__lead">
          <div>
            <div className="gw-dialog__title">Connect “{group.name}” through a gateway</div>
            <div className="gw-dialog__desc">
              Every connection in this group tunnels through the Remote Desktop Gateway unless it sets its own.
              {overrides > 0 && ` ${overrides} connection${overrides === 1 ? " has" : "s have"} its own setting.`}
            </div>
          </div>
          <Switch checked={enabled} onChange={setEnabled} label="Use a gateway" />
        </div>
        <div className={`gw-dialog__fields ${enabled ? "" : "is-disabled"}`} aria-disabled={!enabled}>
          <GatewayFields
            value={draft}
            onChange={setDraft}
            storedPassword={!!group.gateway?.hasPassword}
            hostError={submitted ? hostError : undefined}
          />
        </div>
      </div>
    </Dialog>
  );
}
