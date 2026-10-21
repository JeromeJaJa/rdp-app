import { useEffect, type RefObject } from "react";
import { useStore } from "../../state/store";

/**
 * Acrylic material only: makes a glass surface's blur work over the window's see-through
 * surfaces.
 *
 * On screen, WebView2 leaves translucent page content unblurred under a backdrop blur (the
 * acrylic sidebar's text reads crisply through a menu), but blurs opaque content fine. So
 * beneath the glass we lay an opaque patch at the very bottom of the page: the surfaces in
 * between turn opaque right there, the glass blurs them as in solid mode, and the glass
 * itself covers the patch, so the rest of the window keeps its acrylic. The patch follows
 * the surface while it's shown (it may animate, be dragged, or stack).
 */
export function useGlassUnderlay(ref: RefObject<HTMLElement | null>) {
  const acrylic = useStore((s) => s.settings.windowMaterial === "acrylic");
  useEffect(() => {
    const el = ref.current;
    if (!acrylic || !el) return;
    const patch = document.createElement("div");
    patch.className = "glass-underlay";
    patch.style.borderRadius = getComputedStyle(el).borderRadius;
    document.body.prepend(patch);
    let last = "";
    let frame = 0;
    const follow = () => {
      const r = el.getBoundingClientRect();
      const key = `${r.left},${r.top},${r.width},${r.height}`;
      if (key !== last) {
        last = key;
        patch.style.transform = `translate(${r.left}px, ${r.top}px)`;
        patch.style.width = `${r.width}px`;
        patch.style.height = `${r.height}px`;
      }
      frame = requestAnimationFrame(follow);
    };
    follow();
    return () => {
      cancelAnimationFrame(frame);
      patch.remove();
    };
  }, [ref, acrylic]);
}
