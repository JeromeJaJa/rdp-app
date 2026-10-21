import { memo } from "react";
import { Moon, PanelLeft, Search, Settings, Sun } from "lucide-react";
import { openPalette } from "../palette/CommandPalette";
import { cycleTheme, openDialog, toggleSidebar } from "../../state/actions";
import { useStore } from "../../state/store";
import { IconButton } from "../ui/ui";
import { WindowTabStrip } from "../tabs/WindowTabStrip";
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

function TitlebarInner() {
  const sidebarWidth = useStore((s) => s.settings.sidebarWidth);
  const sidebarShown = useStore((s) => s.sidebarShown);
  const theme = useStore((s) => s.settings.theme);
  const isDark = theme === "dark" || (theme === "system" && document.documentElement.dataset.theme === "dark");

  return (
    <header className="titlebar titlebar--top" data-tauri-drag-region>
      <div
        className="titlebar__brand"
        data-tauri-drag-region
        style={sidebarShown ? { width: sidebarWidth } : undefined}
      >
        <BrandMark />
        {sidebarShown && (
          <span className="titlebar__wordmark" data-tauri-drag-region>
            RDP App
          </span>
        )}
      </div>

      <WindowTabStrip />

      <div className="titlebar__spacer" data-tauri-drag-region />

      <div className="titlebar__actions">
        <button className="tb-search" onClick={openPalette} aria-label="Search connections and commands (Ctrl+K)">
          <Search size={13} />
          <span className="tb-search__label">Search</span>
          <kbd className="tb-search__kbd">Ctrl K</kbd>
        </button>
        <ViewToggle />
        <span className="titlebar__divider" />
        <BulkSessionButtons />
        <NewConnectionButton />
        <TransferButtons />
        <span className="titlebar__divider" />
        <IconButton label={sidebarShown ? "Hide the sidebar" : "Show the sidebar"} onClick={toggleSidebar}>
          <PanelLeft size={15} />
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
