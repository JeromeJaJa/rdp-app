import { useMemo, useState } from "react";
import { AlertTriangle, Check, Copy, Download, Lock } from "lucide-react";
import type { ExportOptions, GroupId, PasswordExport } from "../../lib/types";
import { be, closeDialog, errorText, groupMembers, toast } from "../../state/actions";
import { useStore } from "../../state/store";
import { Button, Field, Segmented, TextInput } from "../ui/ui";
import { Choice } from "./Choice";
import { Dialog } from "./Dialog";

function slug(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "group";
}

export function ExportDialog({ groupIds }: { groupIds?: GroupId[] }) {
  const groups = useStore((s) => s.groups);
  const order = useStore((s) => s.order);
  const counts = useMemo(() => new Map(groups.map((g) => [g.id, groupMembers(g.id).length])), [groups, order]);
  const [scope, setScope] = useState<"all" | "some">(groupIds?.length ? "some" : "all");
  const [picked, setPicked] = useState<Set<GroupId>>(() => new Set(groupIds ?? []));
  const [format, setFormat] = useState<"json" | "text">("json");
  const [passwords, setPasswords] = useState<PasswordExport>("none");
  const [passphrase, setPassphrase] = useState("");
  const [confirmPass, setConfirmPass] = useState("");
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  const effectivePasswords: PasswordExport = format === "text" && passwords === "encrypted" ? "none" : passwords;
  const selected = scope === "all" ? groups.map((g) => g.id) : groups.filter((g) => picked.has(g.id)).map((g) => g.id);
  const total = selected.reduce((n, id) => n + (counts.get(id) ?? 0), 0);
  const passError =
    effectivePasswords !== "encrypted"
      ? undefined
      : passphrase.length < 8
        ? "At least 8 characters"
        : passphrase !== confirmPass
          ? "Passphrases don't match"
          : undefined;
  const canExport = selected.length > 0 && !passError && !busy;

  const options = (): ExportOptions => ({
    groupIds: scope === "all" ? null : selected,
    format,
    passwords: effectivePasswords,
    passphrase: effectivePasswords === "encrypted" ? passphrase : undefined,
  });

  const fileName = () => {
    const date = new Date().toISOString().slice(0, 10);
    const one = scope === "some" && selected.length === 1 ? groups.find((g) => g.id === selected[0]) : undefined;
    const base = one ? `rdp-${slug(one.name)}` : "rdp-connections";
    return `${base}-${date}.${format === "json" ? "json" : "txt"}`;
  };

  const save = async () => {
    setBusy(true);
    try {
      const content = await be().exportData(options());
      if (await be().saveFile(content, fileName(), format)) {
        closeDialog();
        toast("success", `Exported ${total} connection${total === 1 ? "" : "s"}`, fileName());
      }
    } catch (err) {
      toast("error", "Export failed", errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const copy = async () => {
    setBusy(true);
    try {
      const content = await be().exportData(options());
      await navigator.clipboard.writeText(content);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch (err) {
      toast("error", "Copy failed", errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const toggle = (id: GroupId) =>
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <Dialog
      title="Export connections"
      width={540}
      footer={
        <>
          <span className="dialog__footer-note">
            {total} connection{total === 1 ? "" : "s"} · {selected.length} group{selected.length === 1 ? "" : "s"}
          </span>
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" icon={copied ? <Check size={13} /> : <Copy size={13} />} disabled={!canExport} onClick={copy}>
            {copied ? "Copied" : "Copy"}
          </Button>
          <Button variant="primary" icon={<Download size={13} />} disabled={!canExport} onClick={save}>
            Save file…
          </Button>
        </>
      }
    >
      <div className="export">
        <section className="export__section">
          <div className="export__label">Groups</div>
          <div className="choices">
            <Choice checked={scope === "all"} onSelect={() => setScope("all")} title="Everything" desc={`All ${groups.length} groups`} />
            <Choice checked={scope === "some"} onSelect={() => setScope("some")} title="Selected groups">
              <div className="group-picks">
                {groups.map((g) => (
                  <label key={g.id} className={`group-pick ${picked.has(g.id) ? "is-on" : ""}`}>
                    <input type="checkbox" checked={picked.has(g.id)} onChange={() => toggle(g.id)} />
                    <span className="group-pick__box" aria-hidden>
                      <Check size={11} />
                    </span>
                    <span className="group-pick__name">{g.name}</span>
                    <span className="group-pick__count">{counts.get(g.id) ?? 0}</span>
                  </label>
                ))}
              </div>
            </Choice>
          </div>
        </section>

        <section className="export__section">
          <div className="export__row">
            <div>
              <div className="export__label">Format</div>
              <div className="export__hint">
                {format === "json" ? "Groups, gateways and notes. Re-importable." : "One host:port:user:pass per line."}
              </div>
            </div>
            <Segmented
              size="sm"
              value={format}
              onChange={setFormat}
              options={[
                { value: "json", label: "JSON" },
                { value: "text", label: "Text" },
              ]}
            />
          </div>
        </section>

        <section className="export__section">
          <div className="export__row">
            <div className="export__label">Passwords</div>
            <Segmented
              size="sm"
              value={effectivePasswords}
              onChange={setPasswords}
              options={
                format === "json"
                  ? [
                      { value: "none", label: "Don't include" },
                      { value: "encrypted", label: "Encrypted" },
                      { value: "plain", label: "Plain text" },
                    ]
                  : [
                      { value: "none", label: "Don't include" },
                      { value: "plain", label: "Plain text" },
                    ]
              }
            />
          </div>
          {effectivePasswords === "encrypted" && (
            <div className="export__pass">
              <Field label="Passphrase" hint={passError && passphrase ? passError : "Needed to import the file again."} className={passError && passphrase ? "has-error" : ""}>
                <TextInput type="password" value={passphrase} onChange={(e) => setPassphrase(e.target.value)} placeholder="At least 8 characters" />
              </Field>
              <Field label="Confirm">
                <TextInput type="password" value={confirmPass} onChange={(e) => setConfirmPass(e.target.value)} />
              </Field>
              <div className="export__note">
                <Lock size={12} /> Passwords are encrypted with this passphrase. It can't be recovered if lost.
              </div>
            </div>
          )}
          {effectivePasswords === "plain" && (
            <div className="callout callout--warn">
              <AlertTriangle size={14} />
              <span>Anyone with the file can read every password. Store it somewhere safe and delete it when done.</span>
            </div>
          )}
          {effectivePasswords === "none" && <div className="export__hint">Connections are exported without credentials.</div>}
        </section>
      </div>
    </Dialog>
  );
}
