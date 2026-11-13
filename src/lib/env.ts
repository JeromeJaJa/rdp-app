/** Running on a Mac: Cmd stands in for Ctrl, and windows keep their traffic lights. */
export const isMac: boolean = typeof navigator !== "undefined" && /Mac/.test(navigator.platform || navigator.userAgent);

/** The shortcut modifier as shown here: "⌘" on a Mac, "Ctrl" elsewhere. */
export const MOD = isMac ? "⌘" : "Ctrl";

/** A shortcut written with "Ctrl" (e.g. "Ctrl+K", "Ctrl ⇧ N"), as shown on this platform. */
export function keys(shortcut: string): string {
  return isMac ? shortcut.replace(/Ctrl\+?\s?/g, "⌘").replace(/Shift\+/g, "⇧") : shortcut;
}

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
