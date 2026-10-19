import type { ComponentType } from "react";
import type { ConnectionId } from "../lib/types";

/**
 * The engine owns everything pixel- and input-related for live sessions:
 * rendering frames, translating keyboard/mouse to RDP, remote cursors,
 * dynamic resolution, and pausing sessions that are not on screen.
 *
 * The UI treats it as a black box: it reports layout/visibility facts and
 * mounts the two components below.
 */
export interface Engine {
  readonly kind: "tauri" | "mock";

  /**
   * Size of the main session area in CSS px plus devicePixelRatio. The UI calls
   * this from a ResizeObserver on the session area (even while the grid is
   * shown, keep reporting the last known size). New connections use it as the
   * remote desktop size, and connected sessions are resized to it (debounced).
   */
  setViewport(width: number, height: number, dpr: number): void;

  /**
   * What is currently on screen: the session shown in the main area (if any)
   * and the sessions whose grid tiles are visible. Everything else may be
   * paused (server stops sending frames) to save bandwidth and CPU.
   */
  setVisibility(v: { active: ConnectionId | null; thumbnails: ConnectionId[] }): void;

  /** Grid tile width in device pixels; thumbnails are rendered at this width. */
  setThumbnailWidth(px: number): void;

  /** Sends Ctrl+Alt+Del to the remote session. */
  sendCtrlAltDel(id: ConnectionId): void;

  /**
   * Sends a key combination the local OS would otherwise keep (Win, Alt+Tab, …):
   * scancodes are pressed in order and released in reverse. Extended keys use
   * the 0xE000 prefix (e.g. 0xE05B = left Windows key).
   */
  sendKeys(id: ConnectionId, scancodes: number[]): void;
}

export interface RemoteSurfaceProps {
  id: ConnectionId;
  className?: string;
  /** Called when the surface takes/loses keyboard focus (input capture). */
  onFocusChange?: (focused: boolean) => void;
}

export interface RemoteThumbnailProps {
  id: ConnectionId;
  className?: string;
}

export interface EngineModule {
  engine: Engine;
  /**
   * Live, interactive session view. Fills its container (position it with CSS);
   * the remote image is letterboxed to keep aspect ratio. Clicking it captures
   * keyboard + mouse; all keys go to the remote while focused.
   */
  RemoteSurface: ComponentType<RemoteSurfaceProps>;
  /** View-only live miniature for grid tiles. Fills its container (contain-fit). */
  RemoteThumbnail: ComponentType<RemoteThumbnailProps>;
}
