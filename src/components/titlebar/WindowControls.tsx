import { useEffect, useState } from "react";
import { isTauri } from "../../lib/env";

type Win = Awaited<ReturnType<typeof loadWindow>>;
async function loadWindow() {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow();
}

export function WindowControls() {
  const [win, setWin] = useState<Win | null>(null);
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    loadWindow().then(async (w) => {
      setWin(w);
      setMaximized(await w.isMaximized());
      unlisten = await w.onResized(async () => setMaximized(await w.isMaximized()));
    });
    return () => unlisten?.();
  }, []);

  if (!isTauri()) return null;

  return (
    <div className="win-controls">
      <button className="win-btn" aria-label="Minimize" onClick={() => win?.minimize()}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
          <path d="M0 5.5h10" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
      <button
        className="win-btn"
        aria-label={maximized ? "Restore" : "Maximize"}
        onClick={() => win?.toggleMaximize()}
      >
        {maximized ? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
            <path d="M2.5 2.5V1.5a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1" fill="none" stroke="currentColor" />
            <rect x="0.5" y="2.5" width="7" height="7" rx="1" fill="none" stroke="currentColor" />
          </svg>
        ) : (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
            <rect x="0.5" y="0.5" width="9" height="9" rx="1.2" fill="none" stroke="currentColor" />
          </svg>
        )}
      </button>
      <button className="win-btn win-btn--close" aria-label="Close" onClick={() => win?.close()}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
          <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
    </div>
  );
}
