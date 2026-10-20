import { flushSync } from "react-dom";

/** The view-transition name shared by a grid tile's screen and the session area. */
const NAME = "session-focus";

type ViewTransitionDocument = Document & {
  startViewTransition?: (update: () => void) => { finished: Promise<void> };
};

function tileScreen(id: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`.tile[data-id="${CSS.escape(id)}"] .tile__screen`);
}

/**
 * Applies a state change that switches between the grid and the session view, morphing
 * the connection's tile into the session area (or back) where the browser supports view
 * transitions. `id` is the connection whose tile takes part.
 */
export function morphView(id: string | null, toGrid: boolean, update: () => void) {
  const doc = document as ViewTransitionDocument;
  if (!doc.startViewTransition || matchMedia("(prefers-reduced-motion: reduce)").matches) {
    update();
    return;
  }
  // Grid → session: the clicked tile is the "before" element.
  const from = !toGrid && id ? tileScreen(id) : null;
  if (from) from.style.viewTransitionName = NAME;
  let to: HTMLElement | null = null;
  const transition = doc.startViewTransition(() => {
    if (from) from.style.viewTransitionName = "";
    flushSync(update);
    // Session → grid: the tile, now rendered, is the "after" element.
    if (toGrid && id) {
      to = tileScreen(id);
      if (to) to.style.viewTransitionName = NAME;
    }
  });
  transition.finished.finally(() => {
    if (to) to.style.viewTransitionName = "";
  });
}
