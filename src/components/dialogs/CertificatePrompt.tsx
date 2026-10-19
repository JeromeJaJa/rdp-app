import { ShieldAlert, ShieldQuestion } from "lucide-react";
import { dismissCertificate, trustCertificate } from "../../state/actions";
import { displayName, useStore, type CertPrompt } from "../../state/store";
import { Button } from "../ui/ui";
import { Dialog } from "./Dialog";

/** Splits a fingerprint into readable lines of 8 byte pairs. */
function FingerprintBlock({ value, muted }: { value: string; muted?: boolean }) {
  const pairs = value.includes(":") ? value.split(":") : (value.match(/.{1,2}/g) ?? [value]);
  const rows: string[] = [];
  for (let i = 0; i < pairs.length; i += 8) rows.push(pairs.slice(i, i + 8).join(":"));
  return (
    <div className={`fingerprint mono ${muted ? "is-muted" : ""}`}>
      {rows.map((r, i) => (
        <div key={i}>{r}</div>
      ))}
    </div>
  );
}

function PromptDialog({ prompt, queued }: { prompt: CertPrompt; queued: number }) {
  const conn = useStore((s) => s.connById[prompt.id]);
  const changed = prompt.reason === "changed";
  const name = conn ? displayName(conn) : prompt.endpoint;

  return (
    <Dialog
      title={changed ? "Server certificate changed" : "Trust this server?"}
      width={480}
      className={`dialog--cert ${changed ? "is-changed" : ""}`}
      onClose={() => dismissCertificate(prompt)}
      footer={
        <>
          {queued > 0 && <span className="dialog__footer-note">{queued} more waiting</span>}
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" onClick={() => dismissCertificate(prompt)}>
            Cancel
          </Button>
          <Button variant={changed ? "danger" : "primary"} data-autofocus={!changed || undefined} onClick={() => trustCertificate(prompt)}>
            {changed ? "Trust new certificate & connect" : "Trust & connect"}
          </Button>
        </>
      }
    >
      <div className="cert">
        <div className={`cert__banner ${changed ? "is-danger" : ""}`}>
          {changed ? <ShieldAlert size={18} /> : <ShieldQuestion size={18} />}
          <div>
            {changed ? (
              <>
                <b>{name}</b> presented a different certificate than the one you trusted before. This can happen after the server
                was reinstalled or its certificate renewed — or if someone is intercepting the connection.
              </>
            ) : (
              <>
                <b>{name}</b> presented a certificate this PC hasn't seen before. Check the fingerprint with the server's
                administrator if you're unsure.
              </>
            )}
          </div>
        </div>
        <dl className="cert__facts">
          <dt>Server</dt>
          <dd className="mono">{prompt.endpoint}</dd>
          {prompt.subject && (
            <>
              <dt>Subject</dt>
              <dd className="mono">{prompt.subject}</dd>
            </>
          )}
          <dt>{changed ? "New SHA-256" : "SHA-256"}</dt>
          <dd>
            <FingerprintBlock value={prompt.fingerprint} />
          </dd>
          {changed && prompt.previousFingerprint && (
            <>
              <dt>Trusted before</dt>
              <dd>
                <FingerprintBlock value={prompt.previousFingerprint} muted />
              </dd>
            </>
          )}
        </dl>
      </div>
    </Dialog>
  );
}

/** Shows pending certificate decisions one at a time, when no other dialog is open. */
export function CertificatePrompts() {
  const prompts = useStore((s) => s.certPrompts);
  const blocked = useStore((s) => s.dialog !== null);
  if (!prompts.length || blocked) return null;
  return <PromptDialog key={prompts[0].endpoint} prompt={prompts[0]} queued={prompts.length - 1} />;
}
