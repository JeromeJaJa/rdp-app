import { useRef } from "react";
import { Titlebar } from "./components/titlebar/Titlebar";
import { Sidebar } from "./components/tabs/Sidebar";
import { SessionArea } from "./components/session/SessionArea";
import { GridView } from "./components/grid/GridView";
import { Dialogs } from "./components/dialogs/Dialogs";
import { CertificatePrompts } from "./components/dialogs/CertificatePrompt";
import { ContextMenu } from "./components/menu/ContextMenu";
import { Toasts } from "./components/toast/Toasts";
import { CommandPalette } from "./components/palette/CommandPalette";
import { useEngineSync, useShortcuts, useTabAreaReporter, useViewportReporter } from "./state/hooks";
import { useStore } from "./state/store";
import "./app.css";

export function App() {
  const placement = useStore((s) => s.settings.tabPlacement);
  const view = useStore((s) => s.view);
  const fullscreen = useStore((s) => s.fullscreen);
  const mainRef = useRef<HTMLElement>(null);

  useShortcuts();
  useEngineSync();
  useViewportReporter(mainRef);
  useTabAreaReporter();

  return (
    <div className={`app app--${placement} ${fullscreen ? "is-fullscreen" : ""}`}>
      {!fullscreen && <Titlebar />}
      <div className="app__body">
        {placement === "left" && !fullscreen && <Sidebar />}
        <main ref={mainRef} className={`app__main app__main--${view}`}>
          {view === "grid" ? <GridView /> : <SessionArea />}
        </main>
      </div>
      <Dialogs />
      <CertificatePrompts />
      <ContextMenu />
      <CommandPalette />
      <Toasts />
    </div>
  );
}
