import { useRef } from "react";
import { BrandMark } from "./components/titlebar/Titlebar";
import { WindowControls } from "./components/titlebar/WindowControls";
import { WindowTabStrip } from "./components/tabs/WindowTabStrip";
import { SessionArea } from "./components/session/SessionArea";
import { Dialogs } from "./components/dialogs/Dialogs";
import { CertificatePrompts } from "./components/dialogs/CertificatePrompt";
import { ContextMenu } from "./components/menu/ContextMenu";
import { Toasts } from "./components/toast/Toasts";
import { Tooltips } from "./components/ui/Tooltips";
import { VitalsCards } from "./components/vitals/Vitals";
import { useEngineSync, useShortcuts, useTabAreaReporter, useViewportReporter } from "./state/hooks";
import { useStore } from "./state/store";
import "./app.css";

/** A window that tabs were dragged into: its own tab strip over the selected session. */
export function DetachedApp() {
  const fullscreen = useStore((s) => s.fullscreen);
  const mainRef = useRef<HTMLElement>(null);

  useShortcuts();
  useEngineSync();
  useViewportReporter(mainRef);
  useTabAreaReporter();

  return (
    <div className={`app app--top app--detached ${fullscreen ? "is-fullscreen" : ""}`}>
      {!fullscreen && (
        <header className="titlebar titlebar--top" data-tauri-drag-region>
          <div className="titlebar__brand" data-tauri-drag-region>
            <BrandMark />
          </div>
          <WindowTabStrip />
          <div className="titlebar__spacer" data-tauri-drag-region />
          <WindowControls />
        </header>
      )}
      <div className="app__body">
        <main ref={mainRef} className="app__main app__main--session">
          <SessionArea />
        </main>
      </div>
      <Dialogs />
      <CertificatePrompts />
      <ContextMenu />
      <Toasts />
      <Tooltips />
      <VitalsCards />
    </div>
  );
}
