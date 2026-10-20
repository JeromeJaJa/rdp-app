import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "./styles/tokens.css";
import "./styles/base.css";
import { App } from "./App";
import { DetachedApp } from "./DetachedApp";
import { initApp } from "./state/actions";
import { isMainWindow, isTauri } from "./lib/env";

async function boot() {
  try {
    await initApp();
  } catch (err) {
    console.error("Failed to initialise", err);
  }
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      {isMainWindow() ? <App /> : <DetachedApp />}
    </React.StrictMode>,
  );
  // Detached windows are shown by the backend once they hold a tab (one may be prepared
  // ahead of a tear-off, hidden).
  if (isTauri() && isMainWindow()) {
    // The window starts hidden; reveal it after the first paint to avoid a white flash.
    requestAnimationFrame(() =>
      requestAnimationFrame(async () => {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        await getCurrentWindow().show();
        await getCurrentWindow().setFocus();
      }),
    );
  }
}

boot();
