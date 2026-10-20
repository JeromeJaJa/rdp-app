import { useEffect } from "react";
import { engine } from "../engine";
import {
  openDialog,
  selectIndex,
  selectRelative,
  setFullscreen,
  toggleGrid,
} from "./actions";
import { getState, isLive, setState, useStore } from "./store";

/** App-level keyboard shortcuts. Suspended while a RemoteSurface has focus. */
export function useShortcuts() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const s = getState();
      if (s.remoteFocused) return;
      const mod = e.ctrlKey || e.metaKey;
      const typing = (() => {
        const t = e.target as HTMLElement | null;
        return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable);
      })();

      if (e.key === "Escape" && s.fullscreen && !s.dialog && !s.menu) {
        setFullscreen(false);
        return;
      }
      if (!mod) return;
      const key = e.key.toLowerCase();

      if (key === "n") {
        e.preventDefault();
        openDialog({ kind: "add", mode: e.shiftKey ? "bulk" : "single" });
        return;
      }
      if (s.dialog) return;
      if (key === "k") {
        e.preventDefault();
        setState({ paletteOpen: !s.paletteOpen, menu: null });
        return;
      }
      if (key === "g" && !typing) {
        e.preventDefault();
        toggleGrid();
        return;
      }
      if (key === "tab") {
        e.preventDefault();
        selectRelative(e.shiftKey ? -1 : 1);
        return;
      }
      if (!typing && /^[1-9]$/.test(e.key)) {
        e.preventDefault();
        selectIndex(Number(e.key));
        return;
      }
      if (key === "," && !typing) {
        e.preventDefault();
        openDialog({ kind: "settings" });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Keep the store flag honest if a dialog steals focus from the surface.
  const dialog = useStore((s) => s.dialog);
  useEffect(() => {
    if (dialog && getState().remoteFocused) useStore.setState({ remoteFocused: false });
  }, [dialog]);
}

/** Mirrors what is on screen into the engine so hidden sessions can be paused. */
export function useEngineSync() {
  useEffect(() => {
    let lastKey = "";
    let lastThumbWidth = 0;
    const sync = () => {
      const s = getState();
      const activeStatus = s.selectedId ? s.sessions[s.selectedId]?.status ?? "idle" : "idle";
      const active = s.view === "session" && s.selectedId && isLive(activeStatus) ? s.selectedId : null;
      const thumbnails =
        s.view === "grid"
          ? s.visibleTiles.filter((id) => isLive(s.sessions[id]?.status ?? "idle")).sort()
          : [];
      const key = `${active}|${thumbnails.join(",")}`;
      if (key !== lastKey) {
        lastKey = key;
        engine.setVisibility({ active, thumbnails });
      }
      const tw = Math.round(s.settings.gridTileWidth * (window.devicePixelRatio || 1));
      if (tw !== lastThumbWidth) {
        lastThumbWidth = tw;
        engine.setThumbnailWidth(tw);
      }
    };
    sync();
    return useStore.subscribe(sync);
  }, []);
}

/** Reports the main session area's size to the engine (kept while the grid is shown). */
export function useViewportReporter(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let last = "";
    const report = () => {
      const r = el.getBoundingClientRect();
      if (r.width < 2 || r.height < 2) return;
      const dpr = window.devicePixelRatio || 1;
      const key = `${Math.round(r.width)}x${Math.round(r.height)}@${dpr}`;
      if (key === last) return;
      last = key;
      engine.setViewport(Math.round(r.width), Math.round(r.height), dpr);
      // Grid tiles adopt the session area's aspect so thumbnails fill them.
      document.documentElement.style.setProperty("--session-aspect", `${Math.round(r.width)} / ${Math.round(r.height)}`);
    };
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    // DPR changes (moving between monitors) don't trigger ResizeObserver.
    const mq = () => matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    let media = mq();
    const onDpr = () => {
      report();
      media.removeEventListener("change", onDpr);
      media = mq();
      media.addEventListener("change", onDpr);
    };
    media.addEventListener("change", onDpr);
    return () => {
      ro.disconnect();
      media.removeEventListener("change", onDpr);
    };
  }, [ref]);
}
