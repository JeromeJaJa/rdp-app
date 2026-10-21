import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  AlertTriangle,
  Bot,
  Check,
  Copy,
  ExternalLink,
  Eye,
  EyeOff,
  Minus,
  Monitor,
  Plug,
  Plus,
  RefreshCw,
  ShieldCheck,
  SlidersHorizontal,
  Speaker,
  Trash2,
} from "lucide-react";
import type { ApiInfo, TrustedCertificate } from "../../lib/types";
import { be, errorText, toast, updateSettings } from "../../state/actions";
import { useStore, type SettingsSection } from "../../state/store";
import { Button, Segmented, Select, Spinner, Switch, TextInput } from "../ui/ui";
import { Dialog } from "./Dialog";
import { FolderList } from "./FolderList";

function Row({ title, desc, children, stack }: { title: string; desc?: ReactNode; children: ReactNode; stack?: boolean }) {
  return (
    <div className={`setting ${stack ? "setting--stack" : ""}`}>
      <div className="setting__text">
        <div className="setting__title">{title}</div>
        {desc && <div className="setting__desc">{desc}</div>}
      </div>
      <div className="setting__control">{children}</div>
    </div>
  );
}

function Stepper({ value, min, max, onChange }: { value: number; min: number; max: number; onChange: (v: number) => void }) {
  return (
    <div className="stepper">
      <button type="button" aria-label="Decrease" disabled={value <= min} onClick={() => onChange(Math.max(min, value - 1))}>
        <Minus size={12} />
      </button>
      <span className="stepper__value mono">{value}</span>
      <button type="button" aria-label="Increase" disabled={value >= max} onClick={() => onChange(Math.min(max, value + 1))}>
        <Plus size={12} />
      </button>
    </div>
  );
}

function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <Button
      size="sm"
      variant="ghost"
      icon={done ? <Check size={12} /> : <Copy size={12} />}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setDone(true);
          window.setTimeout(() => setDone(false), 1400);
        } catch {
          toast("error", "Could not copy");
        }
      }}
    >
      {done ? "Copied" : label}
    </Button>
  );
}

/** Two-step button: the first click arms it, the second confirms (resets after a few seconds). */
function ConfirmButton({ label, confirmLabel, onConfirm }: { label: string; confirmLabel: string; onConfirm: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 4000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <Button
      size="sm"
      variant={armed ? "danger" : "ghost"}
      onClick={() => {
        if (armed) {
          setArmed(false);
          onConfirm();
        } else setArmed(true);
      }}
    >
      {armed ? confirmLabel : label}
    </Button>
  );
}

const NOTE_NEW = "Applies to new connections.";

// ── Sections ────────────────────────────────────────────────────────────────

function General() {
  const s = useStore((st) => st.settings);
  return (
    <>
      <h3 className="settings__heading">Appearance</h3>
      <Row title="Theme">
        <Segmented
          size="sm"
          value={s.theme}
          onChange={(theme) => updateSettings({ theme })}
          options={[
            { value: "dark", label: "Dark" },
            { value: "light", label: "Light" },
            { value: "system", label: "System" },
          ]}
        />
      </Row>
      <Row title="Window material" desc="Acrylic lets a blur of what's behind the window show through (experimental).">
        <Segmented
          size="sm"
          value={s.windowMaterial}
          onChange={(windowMaterial) => updateSettings({ windowMaterial })}
          options={[
            { value: "solid", label: "Solid" },
            { value: "acrylic", label: "Acrylic" },
          ]}
        />
      </Row>
      <h3 className="settings__heading">Tabs</h3>
      <Row
        title="Tabs show"
        desc="Active: only connections that are connecting or connected; a tab closes when its session ends. Opened: any connection you open, until you close its tab."
      >
        <Segmented
          size="sm"
          value={s.topTabs}
          onChange={(topTabs) => updateSettings({ topTabs })}
          options={[
            { value: "active", label: "Active" },
            { value: "opened", label: "Opened" },
          ]}
        />
      </Row>
    </>
  );
}

function Connections() {
  const s = useStore((st) => st.settings);
  return (
    <>
      <h3 className="settings__heading">Connecting</h3>
      <Row title="Connect when selected" desc="Selecting an idle connection connects it immediately. Otherwise double-click or press Enter.">
        <Switch checked={s.connectOnSelect} onChange={(connectOnSelect) => updateSettings({ connectOnSelect })} />
      </Row>
      <Row title="Parallel connects" desc="How many logins run at once when connecting many sessions.">
        <Stepper value={s.maxConcurrentConnects} min={1} max={32} onChange={(maxConcurrentConnects) => updateSettings({ maxConcurrentConnects })} />
      </Row>
      <Row title="Auto-reconnect" desc="Reconnect automatically after a network drop.">
        <Switch checked={s.autoReconnect} onChange={(autoReconnect) => updateSettings({ autoReconnect })} />
      </Row>
      <Row title="Share clipboard" desc={`Copy and paste between this PC and remote sessions. ${NOTE_NEW}`}>
        <Switch checked={s.clipboardSync} onChange={(clipboardSync) => updateSettings({ clipboardSync })} />
      </Row>
      <Row
        title="Companion"
        desc="Experimental: a small companion inside each remote session copies files much faster (copy here, paste there), receives files you send, and reports the host's load. Installed automatically over WinRM where the host allows it."
      >
        <Switch checked={s.companion} onChange={(companion) => updateSettings({ companion })} />
      </Row>
      <Row
        title="Host stats"
        desc="How often each host's companion reports its CPU, memory, disk and network load (shown on its tab). Sampling is a few system calls; nothing runs in between."
      >
        <Select
          value={String(s.hostStatsInterval)}
          onChange={(v) => updateSettings({ hostStatsInterval: Number(v) })}
          options={[
            { value: "0", label: "Off" },
            { value: "15", label: "Every 15 seconds" },
            { value: "30", label: "Every 30 seconds" },
            { value: "60", label: "Every minute" },
            { value: "300", label: "Every 5 minutes" },
          ]}
        />
      </Row>
    </>
  );
}

function Graphics() {
  const s = useStore((st) => st.settings);
  return (
    <>
      <h3 className="settings__heading">Graphics</h3>
      <Row title="Codec" desc={`H.264 is smoother for motion; RemoteFX is an older, simpler path. ${NOTE_NEW}`}>
        <Select
          value={s.codec}
          onChange={(codec) => updateSettings({ codec })}
          options={[
            { value: "auto", label: "Auto" },
            { value: "h264", label: "H.264" },
            { value: "remotefx", label: "RemoteFX" },
          ]}
        />
      </Row>
      <Row
        title="H.264 quality"
        desc={`Sharp asks for 4:4:4 color, which keeps small colored text crisp, when the host offers it. Balanced (4:2:0) uses less bandwidth and CPU. ${NOTE_NEW}`}
      >
        <Segmented
          size="sm"
          value={s.h264Quality}
          onChange={(h264Quality) => updateSettings({ h264Quality })}
          options={[
            { value: "balanced", label: "Balanced" },
            { value: "sharp", label: "Sharp" },
          ]}
        />
      </Row>
      <Row
        title="Video decoder"
        desc={`Auto decodes on the GPU for the first 8 sessions and in software after that. GPU always uses the GPU when one is available. ${NOTE_NEW}`}
      >
        <Segmented
          size="sm"
          value={s.videoDecoder}
          onChange={(videoDecoder) => updateSettings({ videoDecoder })}
          options={[
            { value: "auto", label: "Auto" },
            { value: "hardware", label: "GPU" },
            { value: "software", label: "Software" },
          ]}
        />
      </Row>
      <h3 className="settings__heading">Performance</h3>
      <Row title="Pause hidden sessions" desc="Servers stop sending frames for sessions that aren't on screen. Saves bandwidth and CPU.">
        <Switch checked={s.pauseHiddenSessions} onChange={(pauseHiddenSessions) => updateSettings({ pauseHiddenSessions })} />
      </Row>
    </>
  );
}

function DrivePaths() {
  const paths = useStore((st) => st.settings.drivePaths);
  return <FolderList value={paths} onChange={(drivePaths) => updateSettings({ drivePaths })} />;
}

function Devices() {
  const s = useStore((st) => st.settings);
  return (
    <>
      <h3 className="settings__heading">Audio</h3>
      <Row title="Remote audio" desc="Play sound from remote sessions on this PC. Connections can override this (Mute in the toolbar or menu, or Sound in Edit).">
        <Segmented
          size="sm"
          value={s.audio}
          onChange={(audio) => updateSettings({ audio })}
          options={[
            { value: "off", label: "Off" },
            { value: "active", label: "On screen" },
            { value: "all", label: "All" },
          ]}
        />
      </Row>
      <Row title="Microphone" desc={`Redirect this PC's microphone to the session on screen. ${NOTE_NEW}`}>
        <Switch checked={s.microphone} onChange={(microphone) => updateSettings({ microphone })} />
      </Row>

      <h3 className="settings__heading">Devices</h3>
      <Row
        title="Shared folders"
        desc={`Local folders (or whole drives) appear in the remote session's File Explorer, under their own names. Groups and connections can override this. ${NOTE_NEW}`}
        stack={s.driveRedirection === "custom"}
      >
        <Segmented
          size="sm"
          value={s.driveRedirection}
          onChange={(driveRedirection) => updateSettings({ driveRedirection })}
          options={[
            { value: "none", label: "None" },
            { value: "all", label: "All drives" },
            { value: "custom", label: "Chosen folders" },
          ]}
        />
      </Row>
      {s.driveRedirection === "custom" && <DrivePaths />}
      <Row title="Printer" desc={`Share this PC's default printer. ${NOTE_NEW}`}>
        <Switch checked={s.printerRedirection} onChange={(printerRedirection) => updateSettings({ printerRedirection })} />
      </Row>
      <Row title="Smart cards" desc={`Use local smart card readers for sign-in and in apps. ${NOTE_NEW}`}>
        <Switch checked={s.smartcardRedirection} onChange={(smartcardRedirection) => updateSettings({ smartcardRedirection })} />
      </Row>
    </>
  );
}

function shortFingerprint(f: string): string {
  const parts = f.split(":");
  if (parts.length < 8) return f.length > 24 ? `${f.slice(0, 11)}…${f.slice(-8)}` : f;
  return `${parts.slice(0, 4).join(":")} … ${parts.slice(-2).join(":")}`;
}

function formatDate(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function Security() {
  const policy = useStore((st) => st.settings.certificatePolicy);
  const [certs, setCerts] = useState<TrustedCertificate[] | null>(null);
  const refresh = useCallback(() => {
    be()
      .listCertificates()
      .then(setCerts)
      .catch(() => setCerts([]));
  }, []);
  useEffect(refresh, [refresh]);

  return (
    <>
      <h3 className="settings__heading">Server certificates</h3>
      <Row title="Certificate checks" desc="Most RDP servers use self-signed certificates, so trust is pinned per server.">
        <Select
          value={policy}
          onChange={(certificatePolicy) => updateSettings({ certificatePolicy })}
          options={[
            { value: "tofu", label: "Trust on first use" },
            { value: "prompt", label: "Ask first" },
            { value: "ignore", label: "Don't check" },
          ]}
        />
      </Row>
      <div className="setting-note">
        {policy === "tofu" && "A new server's certificate is remembered on first connect. If it ever changes, you're asked before connecting."}
        {policy === "prompt" && "You're asked to confirm every server's certificate the first time, and whenever it changes."}
        {policy === "ignore" && (
          <span className="setting-note--warn">
            <AlertTriangle size={12} /> Any certificate is accepted. Connections can be intercepted without warning.
          </span>
        )}
      </div>

      <h3 className="settings__heading">
        Trusted servers{certs && certs.length > 0 && <span className="settings__heading-count">{certs.length}</span>}
      </h3>
      {certs === null ? (
        <div className="settings__loading">
          <Spinner />
        </div>
      ) : certs.length === 0 ? (
        <div className="settings__empty">No certificates trusted yet.</div>
      ) : (
        <ul className="certs">
          {certs.map((c) => (
            <li key={c.endpoint} className="certs__item">
              <div className="certs__main">
                <span className="mono certs__endpoint">{c.endpoint}</span>
                <span className="mono certs__fp" title={c.fingerprint}>
                  {shortFingerprint(c.fingerprint)}
                </span>
              </div>
              <span className="certs__date">{formatDate(c.firstSeen)}</span>
              <button
                className="certs__forget"
                aria-label={`Forget ${c.endpoint}`}
                title="Forget"
                onClick={async () => {
                  try {
                    await be().forgetCertificate(c.endpoint);
                    setCerts((prev) => prev?.filter((x) => x.endpoint !== c.endpoint) ?? null);
                  } catch (err) {
                    toast("error", "Could not forget certificate", errorText(err));
                  }
                }}
              >
                <Trash2 size={13} />
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

function Automation() {
  const enabled = useStore((st) => st.settings.apiEnabled);
  const port = useStore((st) => st.settings.apiPort);
  const [info, setInfo] = useState<ApiInfo | null>(null);
  const [reveal, setReveal] = useState(false);
  const [portDraft, setPortDraft] = useState(String(port));
  const [refreshing, setRefreshing] = useState(false);

  const refresh = useCallback(() => {
    be()
      .apiInfo()
      .then(setInfo)
      .catch((err) => setInfo((prev) => (prev ? { ...prev, running: false, error: errorText(err) } : null)));
  }, []);
  useEffect(refresh, [refresh]);
  // Settings save is debounced and the server restarts on change; re-check shortly after.
  useEffect(() => {
    const t = window.setTimeout(refresh, 800);
    return () => window.clearTimeout(t);
  }, [enabled, port, refresh]);
  useEffect(() => setPortDraft(String(port)), [port]);

  const portNum = Number(portDraft);
  const portValid = Number.isInteger(portNum) && portNum >= 1024 && portNum <= 65535;
  const commitPort = () => {
    if (portValid && portNum !== port) updateSettings({ apiPort: portNum });
    else setPortDraft(String(port));
  };

  const token = info?.token ?? "";
  const masked = token ? "•".repeat(Math.min(32, token.length)) : "";
  const example = info
    ? `curl -H "Authorization: Bearer ${reveal ? token : "<token>"}" \\\n  ${info.url}/api/v1/connections`
    : "";

  return (
    <>
      <h3 className="settings__heading">Local REST API</h3>
      <Row
        title="Enable API"
        desc="Lets scripts and AI agents on this PC do everything you can in the app: manage connections and groups, connect, take screenshots, send input. Loopback only; every request needs the token."
      >
        <Switch checked={enabled} onChange={(apiEnabled) => updateSettings({ apiEnabled })} />
      </Row>
      <Row title="Port" desc={!portValid ? "Use a port between 1024 and 65535." : "127.0.0.1 only."}>
        <TextInput
          mono
          className={`port-input ${!portValid ? "is-invalid" : ""}`}
          inputMode="numeric"
          value={portDraft}
          onChange={(e) => setPortDraft(e.target.value.replace(/[^\d]/g, "").slice(0, 5))}
          onBlur={commitPort}
          onKeyDown={(e) => e.key === "Enter" && (e.currentTarget as HTMLInputElement).blur()}
        />
      </Row>

      <div className="api-status">
        {!info ? (
          <span className="api-status__line">
            <Spinner /> Checking…
          </span>
        ) : info.error ? (
          <span className="api-status__line api-status__line--error">
            <span className="api-dot api-dot--error" /> {info.error}
          </span>
        ) : info.running ? (
          <span className="api-status__line">
            <span className="api-dot api-dot--on" /> Running at <span className="mono">{info.url}</span>
          </span>
        ) : (
          <span className="api-status__line api-status__line--off">
            <span className="api-dot" /> {enabled ? "Starting…" : "Off"}
          </span>
        )}
        <button
          className="api-status__refresh"
          aria-label="Refresh status"
          onClick={() => {
            setRefreshing(true);
            refresh();
            window.setTimeout(() => setRefreshing(false), 500);
          }}
        >
          <RefreshCw size={12} className={refreshing ? "is-spinning" : ""} />
        </button>
      </div>

      {info && (
        <div className={`api-details ${enabled ? "" : "is-dim"}`}>
          <div className="api-field">
            <span className="api-field__label">Token</span>
            <span className="api-field__value mono">{reveal ? token : masked}</span>
            <button className="api-field__icon" aria-label={reveal ? "Hide token" : "Show token"} onClick={() => setReveal((r) => !r)}>
              {reveal ? <EyeOff size={13} /> : <Eye size={13} />}
            </button>
            <CopyButton text={token} />
            <ConfirmButton
              label="Regenerate"
              confirmLabel="Regenerate token?"
              onConfirm={async () => {
                try {
                  setInfo(await be().regenerateApiToken());
                  setReveal(true);
                  toast("success", "New API token generated", "Agents using the old token need the new one.");
                } catch (err) {
                  toast("error", "Could not regenerate token", errorText(err));
                }
              }}
            />
          </div>
          <div className="api-field">
            <span className="api-field__label">Docs</span>
            <span className="api-field__value mono">{info.docsUrl}</span>
            <CopyButton text={info.docsUrl} />
            <Button size="sm" variant="ghost" icon={<ExternalLink size={12} />} onClick={() => be().openExternal(info.docsUrl).catch((e) => toast("error", "Could not open the docs", errorText(e)))}>
              Open
            </Button>
          </div>
          <div className="api-field">
            <span className="api-field__label">For agents</span>
            <span className="api-field__value mono">{info.llmsUrl}</span>
            <CopyButton text={info.llmsUrl} />
          </div>
          <div className="api-example">
            <pre className="mono">{example}</pre>
            <CopyButton text={example.replace("<token>", token)} />
          </div>
        </div>
      )}
    </>
  );
}

const SECTIONS: { id: SettingsSection; label: string; icon: ReactNode; render: () => ReactNode }[] = [
  { id: "general", label: "General", icon: <SlidersHorizontal size={14} />, render: () => <General /> },
  { id: "connections", label: "Connections", icon: <Plug size={14} />, render: () => <Connections /> },
  { id: "graphics", label: "Graphics", icon: <Monitor size={14} />, render: () => <Graphics /> },
  { id: "devices", label: "Audio & devices", icon: <Speaker size={14} />, render: () => <Devices /> },
  { id: "security", label: "Security", icon: <ShieldCheck size={14} />, render: () => <Security /> },
  { id: "automation", label: "Automation", icon: <Bot size={14} />, render: () => <Automation /> },
];

export function SettingsDialog({ section: initial = "general" }: { section?: SettingsSection }) {
  const [section, setSection] = useState<SettingsSection>(initial);
  const current = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0];

  return (
    <Dialog title="Settings" width={780} className="dialog--settings" bare>
      <div className="settings-layout">
        <nav className="settings-nav" aria-label="Settings sections">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              className={`settings-nav__item ${s.id === section ? "is-active" : ""}`}
              aria-current={s.id === section ? "page" : undefined}
              onClick={() => setSection(s.id)}
            >
              {s.icon}
              <span>{s.label}</span>
            </button>
          ))}
          <div className="settings-nav__spacer" />
          <div className="settings-nav__foot">
            <Check size={12} /> Saved automatically
          </div>
        </nav>
        <div className="settings-pane" key={section}>
          <div className="settings">{current.render()}</div>
        </div>
      </div>
    </Dialog>
  );
}
