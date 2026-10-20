export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** This window's label: "main", or "win-…" for a window a tab was dragged into. */
export function windowLabel(): string {
  const label = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: unknown } } } })
    .__TAURI_INTERNALS__?.metadata?.currentWindow?.label;
  return typeof label === "string" ? label : "main";
}

export function isMainWindow(): boolean {
  return windowLabel() === "main";
}
