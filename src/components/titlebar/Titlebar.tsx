import { memo } from "react";
import { Moon, PanelLeft, PanelTop, Settings, Sun } from "lucide-react";
import { cycleTheme, openDialog, updateSettings } from "../../state/actions";
import { displayName, useStore } from "../../state/store";
import { IconButton } from "../ui/ui";
import { TopTabs } from "../tabs/TopTabs";
import { StatusDot, statusLabel } from "../common/StatusDot";
import { BulkSessionButtons, NewConnectionButton, TransferButtons, ViewToggle } from "../common/GlobalActions";
import { WindowControls } from "./WindowControls";
import "./titlebar.css";

export function BrandMark({ size = 18 }: { size?: number }) {
  return (
    <svg className="brand-mark" width={size} height={size} viewBox="0 0 20 20" aria-hidden>
      <rect x="1.5" y="3" width="13" height="10" rx="2.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <rect x="6" y="7" width="12.5" height="10" rx="2.5" fill="currentColor" />
    </svg>
  );
}

function CurrentTitle() {
  const sel = useStore((s) => (s.selectedId ? s.connById[s.selectedId] : undefined));
  const status = useStore((s) => (s.selectedId ? s.sessions[s.selectedId]?.status ?? "idle" : "idle"));
  // The built-in "All" group adds nothing to the breadcrumb.
  const group = useStore((s) => (sel ? s.groups.find((g) => g.id === sel.groupId && !g.builtin) : undefined));
  const view = useStore((s) => s.view);
  if (view === "grid") {
    return (
      <div className="tb-title" data-tauri-drag-region>
        <span className="tb-title__name" data-tauri-drag-region>
          All sessions
        </span>
      </div>
    );
  }
  if (!sel) return <div className="tb-title" data-tauri-drag-region />;
  return (
    <div className="tb-title" data-tauri-drag-region>
      {group && (
        <>
          <span className="tb-title__crumb" data-tauri-drag-region>
            {group.name}
          </span>
          <span className="tb-title__slash" data-tauri-drag-region>
            /
          </span>
        </>
      )}
      <span className="tb-title__name" data-tauri-drag-region>
        {displayName(sel)}
      </span>
      <span className="tb-title__meta mono" data-tauri-drag-region>
        {sel.host}
        {sel.port !== 3389 ? `:${sel.port}` : ""}
      </span>
      <span className={`tb-title__status tb-title__status--${status}`} data-tauri-drag-region>
        <StatusDot status={status} />
        {statusLabel(status)}
      </span>
    </div>
  );
}

function TitlebarInner() {
  const placement = useStore((s) => s.settings.tabPlacement);
  const sidebarWidth = useStore((s) => s.settings.sidebarWidth);
  const theme = useStore((s) => s.settings.theme);
  const isDark = theme === "dark" || (theme === "system" && document.documentElement.dataset.theme === "dark");

  return (
    <header className={`titlebar titlebar--${placement}`} data-tauri-drag-region>
      <div
        className="titlebar__brand"
        data-tauri-drag-region
        style={placement === "left" ? { width: sidebarWidth } : undefined}
      >
        <BrandMark />
        {placement === "left" && (
          <span className="titlebar__wordmark" data-tauri-drag-region>
            RDP App
          </span>
        )}
      </div>

      {placement === "top" ? <TopTabs /> : <CurrentTitle />}

      <div className="titlebar__spacer" data-tauri-drag-region />

      <div className="titlebar__actions">
        {/* With tabs on the left these live in the sidebar, next to the list they act on. */}
        {placement === "top" && (
          <>
            <ViewToggle />
            <span className="titlebar__divider" />
            <BulkSessionButtons />
            <NewConnectionButton />
            <TransferButtons />
            <span className="titlebar__divider" />
          </>
        )}
        <IconButton
          label={placement === "left" ? "Tabs on top" : "Tabs on left"}
          onClick={() => updateSettings({ tabPlacement: placement === "left" ? "top" : "left" })}
        >
          {placement === "left" ? <PanelTop size={15} /> : <PanelLeft size={15} />}
        </IconButton>
        <IconButton label={isDark ? "Light theme" : "Dark theme"} onClick={cycleTheme}>
          {isDark ? <Sun size={15} /> : <Moon size={15} />}
        </IconButton>
        <IconButton label="Settings (Ctrl+,)" onClick={() => openDialog({ kind: "settings" })}>
          <Settings size={16} />
        </IconButton>
      </div>
      <WindowControls />
    </header>
  );
}

export const Titlebar = memo(TitlebarInner);
