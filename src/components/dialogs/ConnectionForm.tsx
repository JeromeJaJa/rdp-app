import { useMemo, useState, type ClipboardEvent, type FormEvent } from "react";
import { ChevronRight, Eye, EyeOff, Trash2 } from "lucide-react";
import { parseBulk } from "../../lib/bulkParse";
import { FEATURES } from "../../lib/features";
import { describeGateway } from "../../lib/gateway";
import { DEFAULT_GROUP_ID, type AudioMode, type Connection, type GatewayInput, type GroupId } from "../../lib/types";
import { useStore } from "../../state/store";
import { Button, Field, Segmented, Select, TextInput } from "../ui/ui";
import { GATEWAY_HOST_RE, GatewayFields, draftFromGateway, draftToInput, type GatewayDraft } from "./GatewayFields";
import { GroupSelect, NEW_GROUP } from "./GroupSelect";

type GatewayMode = "group" | "direct" | "custom";

const DIRECT: GatewayInput = { enabled: false, host: "", sameCredentials: true, username: "", domain: "" };

export interface ConnectionFormValues {
  name: string;
  host: string;
  port: number;
  username: string;
  domain: string;
  /** undefined = unchanged (edit mode). */
  password: string | undefined;
  group: GroupId | typeof NEW_GROUP;
  newGroupName: string;
  notes: string;
  /** null = use the group's gateway; undefined = unchanged (edit mode). */
  gateway: GatewayInput | null | undefined;
  /** null = follow Settings → Audio. */
  audio: AudioMode | null;
}

const AUDIO_LABEL: Record<AudioMode, string> = { off: "Off", active: "Only when on screen", all: "Always" };

/** Without a Domain field, the domain is shown and edited as `DOMAIN\user`. */
const joinUser = (domain: string, user: string) => (domain && !FEATURES.domainField ? `${domain}\\${user}` : user);
function splitUser(value: string, domain: string): [domain: string, user: string] {
  const v = value.trim();
  if (FEATURES.domainField) return [domain.trim(), v];
  const slash = v.indexOf("\\");
  return slash > 0 ? [v.slice(0, slash), v.slice(slash + 1)] : ["", v];
}

const HOST_RE = /^(\[[0-9A-Fa-f:.%\w]+\]|[0-9A-Fa-f:]+:[0-9A-Fa-f:.]+|[A-Za-z0-9]([A-Za-z0-9\-_.]*[A-Za-z0-9])?)$/;

export function ConnectionForm({
  initial,
  defaultGroup = DEFAULT_GROUP_ID,
  submitLabel,
  onSubmit,
  onCancel,
  onDelete,
  onBulkPaste,
}: {
  initial?: Connection;
  defaultGroup?: GroupId;
  submitLabel: string;
  onSubmit: (v: ConnectionFormValues) => Promise<void> | void;
  onCancel: () => void;
  onDelete?: () => void;
  /** Multi-line paste into the host field: hand the text to bulk mode. */
  onBulkPaste?: (text: string) => void;
}) {
  const editing = !!initial;
  const [name, setName] = useState(initial?.name ?? "");
  const [host, setHost] = useState(initial?.host ?? "");
  const [port, setPort] = useState(String(initial?.port ?? 3389));
  const [username, setUsername] = useState(joinUser(initial?.domain ?? "", initial?.username ?? ""));
  const [domain, setDomain] = useState(FEATURES.domainField ? (initial?.domain ?? "") : "");
  const [password, setPassword] = useState("");
  const [passwordTouched, setPasswordTouched] = useState(false);
  const [clearPassword, setClearPassword] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const [group, setGroup] = useState<GroupId | typeof NEW_GROUP>(initial ? initial.groupId : defaultGroup);
  const initialMode: GatewayMode = !initial?.gateway ? "group" : initial.gateway.enabled ? "custom" : "direct";
  const [gwMode, setGwMode] = useState<GatewayMode>(initialMode);
  const [gwOpen, setGwOpen] = useState(initialMode !== "group");
  const [gwDraft, setGwDraft] = useState<GatewayDraft>(() => draftFromGateway(initial?.gateway?.enabled ? initial.gateway : null));
  const groupGateway = useStore((s) => (group === NEW_GROUP ? null : (s.groups.find((g) => g.id === group)?.gateway ?? null)));
  const [newGroupName, setNewGroupName] = useState("");
  const [notes, setNotes] = useState(initial?.notes ?? "");
  const [audio, setAudio] = useState<AudioMode | "default">(initial?.audio ?? "default");
  const globalAudio = useStore((s) => s.settings.audio);
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);

  const errors = useMemo(() => {
    const e: Record<string, string> = {};
    const h = host.trim();
    if (!h) e.host = "Host is required";
    else if (!HOST_RE.test(h)) e.host = "Not a valid host name or IP";
    const p = Number(port);
    if (!Number.isInteger(p) || p < 1 || p > 65535) e.port = "1–65535";
    if (!splitUser(username, domain)[1]) e.username = "Username is required";
    if (group === NEW_GROUP && !newGroupName.trim()) e.group = "Name the new group";
    if (FEATURES.gateway && gwMode === "custom") {
      const gh = gwDraft.host.trim();
      if (!gh) e.gateway = "Gateway server is required";
      else if (!GATEWAY_HOST_RE.test(gh)) e.gateway = "Not a valid host name";
    }
    return e;
  }, [host, port, username, domain, group, newGroupName, gwMode, gwDraft.host]);

  const onHostPaste = (e: ClipboardEvent<HTMLInputElement>) => {
    const text = e.clipboardData.getData("text");
    if (!text) return;
    const lines = text.split(/\r?\n/).filter((l) => l.trim() && !l.trim().startsWith("#"));
    if (lines.length > 1 && onBulkPaste && !editing) {
      e.preventDefault();
      onBulkPaste(text);
      return;
    }
    if (lines.length === 1 && text.includes(":")) {
      const [parsed] = parseBulk(lines[0], []);
      if (parsed?.entry && (parsed.entry.username || parsed.entry.password)) {
        e.preventDefault();
        setHost(parsed.entry.host);
        setPort(String(parsed.entry.port));
        if (FEATURES.domainField) {
          setUsername(parsed.entry.username);
          setDomain(parsed.entry.domain);
        } else setUsername(joinUser(parsed.entry.domain, parsed.entry.username));
        if (parsed.entry.password) {
          setPassword(parsed.entry.password);
          setPasswordTouched(true);
          setClearPassword(false);
        }
      }
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setSubmitted(true);
    if (Object.keys(errors).length) {
      if (errors.gateway) setGwOpen(true);
      return;
    }
    setBusy(true);
    const [submitDomain, submitUser] = splitUser(username, domain);
    try {
      await onSubmit({
        name: name.trim(),
        host: host.trim().replace(/^\[(.*)\]$/, "$1"),
        port: Number(port),
        username: submitUser,
        domain: submitDomain,
        password: editing ? (clearPassword ? "" : passwordTouched && password ? password : undefined) : password,
        group,
        newGroupName: newGroupName.trim(),
        notes,
        audio: audio === "default" ? null : audio,
        gateway: !FEATURES.gateway
          ? editing
            ? undefined
            : null
          : gwMode === "group"
            ? null
            : gwMode === "direct"
              ? DIRECT
              : draftToInput(gwDraft, true),
      });
    } finally {
      setBusy(false);
    }
  };

  const err = (k: string) => (submitted ? errors[k] : undefined);

  return (
    <form className="dialog__form" onSubmit={submit} noValidate>
      <div className="dialog__body form-grid">
        <Field label="Name" hint={!name ? "Optional — defaults to the host" : undefined} className="span-2">
          <TextInput value={name} placeholder={host || "e.g. Web server 1"} onChange={(e) => setName(e.target.value)} />
        </Field>
        <div className="span-2 form-row">
        <Field label="Host" hint={err("host")} className={`grow ${err("host") ? "has-error" : ""}`}>
          <TextInput
            mono
            data-autofocus={!editing || undefined}
            value={host}
            placeholder="10.0.0.12 or server.example.com"
            onChange={(e) => setHost(e.target.value)}
            onPaste={onHostPaste}
            className={err("host") ? "is-invalid" : ""}
          />
        </Field>
        <Field label="Port" hint={err("port")} className={`port ${err("port") ? "has-error" : ""}`}>
          <TextInput
            mono
            inputMode="numeric"
            value={port}
            onChange={(e) => setPort(e.target.value.replace(/[^\d]/g, "").slice(0, 5))}
            className={err("port") ? "is-invalid" : ""}
          />
        </Field>
        </div>
        <Field
          label="Username"
          hint={err("username") ?? (!FEATURES.domainField && !username ? "DOMAIN\\user for domain accounts" : undefined)}
          className={`${FEATURES.domainField ? "" : "span-2"} ${err("username") ? "has-error" : ""}`}
        >
          <TextInput
            value={username}
            placeholder="Administrator"
            onChange={(e) => {
              const v = e.target.value;
              // Typing DOMAIN\user splits into the domain field (when there is one).
              const slash = v.indexOf("\\");
              if (FEATURES.domainField && slash > 0 && !domain) {
                setDomain(v.slice(0, slash));
                setUsername(v.slice(slash + 1));
              } else setUsername(v);
            }}
            className={err("username") ? "is-invalid" : ""}
          />
        </Field>
        {FEATURES.domainField && (
          <Field label="Domain" hint={!domain ? "Optional" : undefined}>
            <TextInput value={domain} placeholder="CORP" onChange={(e) => setDomain(e.target.value)} />
          </Field>
        )}
        <Field
          label="Password"
          className="span-2"
          hint={
            editing && initial?.hasPassword ? (
              clearPassword ? (
                <span>
                  Saved password will be removed.{" "}
                  <button type="button" className="link" onClick={() => setClearPassword(false)}>
                    Undo
                  </button>
                </span>
              ) : (
                <span>
                  Leave empty to keep the saved password.{" "}
                  <button
                    type="button"
                    className="link link--danger"
                    onClick={() => {
                      setClearPassword(true);
                      setPassword("");
                    }}
                  >
                    Clear saved password
                  </button>
                </span>
              )
            ) : (
              "Encrypted with your Windows account. Never shown again."
            )
          }
        >
          <div className="password-input">
            <TextInput
              type={showPassword ? "text" : "password"}
              value={password}
              disabled={clearPassword}
              placeholder={editing && initial?.hasPassword && !clearPassword ? "••••••••••  (unchanged)" : ""}
              onChange={(e) => {
                setPassword(e.target.value);
                setPasswordTouched(true);
              }}
            />
            <button
              type="button"
              className="password-input__toggle"
              aria-label={showPassword ? "Hide password" : "Show password"}
              onClick={() => setShowPassword((v) => !v)}
              tabIndex={-1}
            >
              {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
            </button>
          </div>
        </Field>
        <Field label="Group" hint={err("group")} className="span-2">
          <GroupSelect value={group} onChange={setGroup} newName={newGroupName} onNewName={setNewGroupName} />
        </Field>
        {FEATURES.gateway && (
        <div className="span-2 disclosure">
          <button type="button" className={`disclosure__head ${gwOpen ? "is-open" : ""}`} onClick={() => setGwOpen((o) => !o)}>
            <ChevronRight size={13} className="disclosure__chevron" />
            <span className="disclosure__title">Gateway</span>
            <span className="disclosure__summary">
              {gwMode === "group"
                ? `Group default · ${describeGateway(groupGateway)}`
                : gwMode === "direct"
                  ? "Direct connection"
                  : gwDraft.host.trim()
                    ? `via ${gwDraft.host.trim()}`
                    : "Custom gateway"}
            </span>
          </button>
          {gwOpen && (
            <div className="disclosure__body">
              <Segmented
                size="sm"
                value={gwMode}
                onChange={setGwMode}
                options={[
                  { value: "group", label: "Group default" },
                  { value: "direct", label: "Direct" },
                  { value: "custom", label: "Custom" },
                ]}
              />
              {gwMode === "group" && (
                <p className="disclosure__note">
                  {groupGateway?.enabled && groupGateway.host
                    ? <>Connects through the group's gateway <span className="mono">{groupGateway.host}</span>.</>
                    : "The group has no gateway, so this connects directly."}
                </p>
              )}
              {gwMode === "direct" && <p className="disclosure__note">Always connects directly, even if the group uses a gateway.</p>}
              {gwMode === "custom" && (
                <GatewayFields
                  value={gwDraft}
                  onChange={setGwDraft}
                  storedPassword={!!initial?.gateway?.hasPassword}
                  hostError={err("gateway")}
                />
              )}
            </div>
          )}
        </div>
        )}
        <Field label="Sound" className="span-2">
          <Select<AudioMode | "default">
            value={audio}
            onChange={setAudio}
            options={[
              { value: "default", label: `Default (${AUDIO_LABEL[globalAudio].toLowerCase()})` },
              { value: "off", label: AUDIO_LABEL.off },
              { value: "active", label: AUDIO_LABEL.active },
              { value: "all", label: AUDIO_LABEL.all },
            ]}
          />
        </Field>
        <Field label="Notes" className="span-2">
          <textarea
            className="textarea"
            rows={2}
            value={notes}
            placeholder="Optional"
            onChange={(e) => setNotes(e.target.value)}
          />
        </Field>
      </div>
      <div className="dialog__footer">
        {onDelete && (
          <Button variant="ghost" className="btn--danger-ghost" icon={<Trash2 size={13} />} onClick={onDelete}>
            Delete
          </Button>
        )}
        <div className="dialog__footer-spacer" />
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="primary" type="submit" disabled={busy}>
          {submitLabel}
        </Button>
      </div>
    </form>
  );
}
