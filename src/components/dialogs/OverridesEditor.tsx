import type { ReactNode } from "react";
import type { OverridableKey, Settings, SettingOverrides } from "../../lib/types";
import { Select } from "../ui/ui";
import { FolderList } from "./FolderList";

const INHERIT = "__inherit";

type Choice<T> = { value: T; label: string };

const ON_OFF: Choice<boolean>[] = [
  { value: true, label: "On" },
  { value: false, label: "Off" },
];

const CHOICES: { [K in OverridableKey]?: Choice<Settings[K]>[] } = {
  audio: [
    { value: "off", label: "Off" },
    { value: "active", label: "Only when on screen" },
    { value: "all", label: "Always" },
  ],
  microphone: ON_OFF,
  clipboardSync: ON_OFF,
  companion: ON_OFF,
  directTransfers: ON_OFF,
  hostStatsInterval: [
    { value: 0, label: "Off" },
    { value: 15, label: "Every 15 seconds" },
    { value: 30, label: "Every 30 seconds" },
    { value: 60, label: "Every minute" },
    { value: 300, label: "Every 5 minutes" },
  ],
  driveRedirection: [
    { value: "none", label: "None" },
    { value: "all", label: "All drives" },
    { value: "custom", label: "Chosen folders" },
  ],
  printerRedirection: ON_OFF,
  smartcardRedirection: ON_OFF,
  autoReconnect: ON_OFF,
  remoteWallpaper: ON_OFF,
  codec: [
    { value: "auto", label: "Auto" },
    { value: "h264", label: "H.264" },
    { value: "remotefx", label: "RemoteFX" },
  ],
  h264Quality: [
    { value: "balanced", label: "Balanced (4:2:0)" },
    { value: "sharp", label: "Sharp text (4:4:4)" },
  ],
  videoDecoder: [
    { value: "auto", label: "Auto" },
    { value: "hardware", label: "GPU" },
    { value: "software", label: "Software" },
  ],
};

const SECTIONS: { title: string; rows: { key: OverridableKey; label: string }[] }[] = [
  {
    title: "Devices",
    rows: [
      { key: "audio", label: "Sound" },
      { key: "microphone", label: "Microphone" },
      { key: "clipboardSync", label: "Clipboard" },
      { key: "driveRedirection", label: "Shared folders" },
      { key: "printerRedirection", label: "Printer" },
      { key: "smartcardRedirection", label: "Smart cards" },
    ],
  },
  {
    title: "Connection",
    rows: [
      { key: "autoReconnect", label: "Auto-reconnect" },
      { key: "companion", label: "Companion (experimental)" },
      { key: "directTransfers", label: "Direct transfers" },
      { key: "hostStatsInterval", label: "Host stats" },
      { key: "codec", label: "Graphics codec" },
      { key: "h264Quality", label: "H.264 colour" },
      { key: "videoDecoder", label: "Video decoder" },
      { key: "remoteWallpaper", label: "Remote wallpaper" },
    ],
  },
];

function labelOf<K extends OverridableKey>(key: K, value: Settings[K]): string {
  return (CHOICES[key] as Choice<Settings[K]>[] | undefined)?.find((c) => c.value === value)?.label ?? String(value);
}

function encode(value: unknown): string {
  return JSON.stringify(value);
}

/**
 * Edits a group's or connection's setting overrides. Every row starts at "Inherit",
 * showing what it would get from the level above (`inherited`).
 */
export function OverridesEditor({
  value,
  onChange,
  inherited,
  inheritedFrom,
}: {
  value: SettingOverrides;
  onChange: (next: SettingOverrides) => void;
  /** The settings this level inherits (app, plus the group's overrides for a connection). */
  inherited: Settings;
  /** "app settings" or "group" — for the Inherit label. */
  inheritedFrom: string;
}) {
  const set = <K extends OverridableKey>(key: K, next: Settings[K] | undefined) => {
    const out: SettingOverrides = { ...value };
    if (next === undefined) delete out[key];
    else (out as Record<string, unknown>)[key] = next;
    // Choosing folders at this level starts from the inherited list.
    if (key === "driveRedirection") {
      if (next === "custom") out.drivePaths = value.drivePaths ?? [...inherited.drivePaths];
      else delete out.drivePaths;
    }
    onChange(out);
  };

  const row = <K extends OverridableKey>(key: K, label: string): ReactNode => {
    const choices = CHOICES[key] as Choice<Settings[K]>[];
    const current = value[key] as Settings[K] | undefined;
    const effective = (current ?? inherited[key]) as Settings[K];
    return (
      <div key={key} className={`override ${current !== undefined ? "is-set" : ""}`}>
        <span className="override__label">{label}</span>
        <Select<string>
          value={current === undefined ? INHERIT : encode(current)}
          onChange={(v) => set(key, v === INHERIT ? undefined : (JSON.parse(v) as Settings[K]))}
          options={[
            { value: INHERIT, label: `Inherit (${labelOf(key, inherited[key])})` },
            ...choices.map((c) => ({ value: encode(c.value), label: c.label })),
          ]}
        />
        {key === "driveRedirection" && effective === "custom" && (
          <div className="override__extra">
            {current === "custom" ? (
              <FolderList value={value.drivePaths ?? []} onChange={(drivePaths) => onChange({ ...value, drivePaths })} />
            ) : (
              <div className="override__inherited mono">
                {inherited.drivePaths.length ? inherited.drivePaths.join(", ") : "No folders"} · from {inheritedFrom}
              </div>
            )}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="overrides">
      {SECTIONS.map((section) => (
        <section key={section.title} className="overrides__section">
          <div className="overrides__title">{section.title}</div>
          {section.rows.map((r) => row(r.key, r.label))}
        </section>
      ))}
    </div>
  );
}

/** How many settings a level overrides (drivePaths travels with driveRedirection). */
export function overrideCount(value: SettingOverrides | undefined): number {
  return Object.keys(value ?? {}).filter((k) => k !== "drivePaths").length;
}
