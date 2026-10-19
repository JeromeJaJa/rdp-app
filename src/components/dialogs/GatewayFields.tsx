import { useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import type { GatewayInput, GatewaySettings } from "../../lib/types";
import { Field, Switch, TextInput } from "../ui/ui";

/** Editable gateway state (password write-only, like connection passwords). */
export interface GatewayDraft {
  host: string;
  sameCredentials: boolean;
  username: string;
  domain: string;
  password: string;
  /** The user typed a new password. */
  passwordTouched: boolean;
  /** The user asked to remove the stored password. */
  clearPassword: boolean;
}

export function draftFromGateway(g: GatewaySettings | null): GatewayDraft {
  return {
    host: g?.host ?? "",
    sameCredentials: g?.sameCredentials ?? true,
    username: g?.username ?? "",
    domain: g?.domain ?? "",
    password: "",
    passwordTouched: false,
    clearPassword: false,
  };
}

export function draftToInput(d: GatewayDraft, enabled = true): GatewayInput {
  const password = d.clearPassword ? "" : d.passwordTouched && d.password ? d.password : undefined;
  return {
    enabled,
    host: d.host.trim(),
    sameCredentials: d.sameCredentials,
    username: d.sameCredentials ? "" : d.username.trim(),
    domain: d.sameCredentials ? "" : d.domain.trim(),
    password: d.sameCredentials ? (d.clearPassword ? "" : undefined) : password,
  };
}

export const GATEWAY_HOST_RE = /^(\[[0-9A-Fa-f:.%\w]+\]|[A-Za-z0-9]([A-Za-z0-9\-_.]*[A-Za-z0-9])?)(:\d{1,5})?$/;

export function GatewayFields({
  value,
  onChange,
  storedPassword,
  hostError,
}: {
  value: GatewayDraft;
  onChange: (d: GatewayDraft) => void;
  /** A gateway password is already saved. */
  storedPassword: boolean;
  hostError?: string;
}) {
  const [show, setShow] = useState(false);
  const set = (patch: Partial<GatewayDraft>) => onChange({ ...value, ...patch });

  return (
    <div className="gw-fields">
      <Field label="Gateway server" hint={hostError ?? "Host name, optionally with :port (443 by default)"} className={hostError ? "has-error" : ""}>
        <TextInput
          mono
          value={value.host}
          placeholder="rdg.contoso.com"
          onChange={(e) => set({ host: e.target.value })}
          className={hostError ? "is-invalid" : ""}
        />
      </Field>
      <div className="gw-fields__toggle">
        <div>
          <div className="gw-fields__toggle-title">Use the connection's credentials</div>
          <div className="gw-fields__toggle-desc">Sign in to the gateway with the same user and password as the remote PC.</div>
        </div>
        <Switch checked={value.sameCredentials} onChange={(sameCredentials) => set({ sameCredentials })} label="Use the connection's credentials" />
      </div>
      {!value.sameCredentials && (
        <div className="form-grid gw-fields__creds">
          <Field label="Gateway username">
            <TextInput
              value={value.username}
              placeholder="jdoe"
              onChange={(e) => {
                const v = e.target.value;
                const slash = v.indexOf("\\");
                if (slash > 0 && !value.domain) set({ domain: v.slice(0, slash), username: v.slice(slash + 1) });
                else set({ username: v });
              }}
            />
          </Field>
          <Field label="Domain" hint={!value.domain ? "Optional" : undefined}>
            <TextInput value={value.domain} placeholder="CORP" onChange={(e) => set({ domain: e.target.value })} />
          </Field>
          <Field
            label="Gateway password"
            className="span-2"
            hint={
              storedPassword ? (
                value.clearPassword ? (
                  <span>
                    Saved password will be removed.{" "}
                    <button type="button" className="link" onClick={() => set({ clearPassword: false })}>
                      Undo
                    </button>
                  </span>
                ) : (
                  <span>
                    Leave empty to keep the saved password.{" "}
                    <button type="button" className="link link--danger" onClick={() => set({ clearPassword: true, password: "" })}>
                      Clear
                    </button>
                  </span>
                )
              ) : undefined
            }
          >
            <div className="password-input">
              <TextInput
                type={show ? "text" : "password"}
                value={value.password}
                disabled={value.clearPassword}
                placeholder={storedPassword && !value.clearPassword ? "••••••••••  (unchanged)" : ""}
                onChange={(e) => set({ password: e.target.value, passwordTouched: true })}
              />
              <button
                type="button"
                className="password-input__toggle"
                aria-label={show ? "Hide password" : "Show password"}
                onClick={() => setShow((v) => !v)}
                tabIndex={-1}
              >
                {show ? <EyeOff size={14} /> : <Eye size={14} />}
              </button>
            </div>
          </Field>
        </div>
      )}
    </div>
  );
}
