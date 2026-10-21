import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useGlassUnderlay } from "./useGlassUnderlay";

type Side = "top" | "bottom" | "left" | "right";

interface Tip {
  text: string;
  side: Side;
  anchor: DOMRect;
  /** Over a remote screen (inside the session toolbar): always dark glass. */
  dark: boolean;
}

/** Hover this long (ms) before a tooltip shows. */
const DELAY = 450;
/** Gap between an element and its tooltip, and the margin kept from the window's edges. */
const GAP = 6;
const EDGE = 6;

/**
 * Tooltips for every element with `data-tip` (placed by `data-tip-side`, below by default).
 * One floating element per window, outside every stacking context, so it's real glass (with
 * its underlay in acrylic, like any other glass), never clipped, and kept in the window. An
 * element carrying its own visible label marks it `data-tip-label`: while that label shows,
 * the element gets no tooltip.
 */
export function Tooltips() {
  const [tip, setTip] = useState<Tip | null>(null);

  useEffect(() => {
    let target: Element | null = null;
    let timer = 0;
    const clear = () => {
      clearTimeout(timer);
      target = null;
      setTip(null);
    };
    const over = (e: PointerEvent) => {
      const el = e.target instanceof Element ? e.target.closest("[data-tip]") : null;
      if (el === target) return;
      clearTimeout(timer);
      target = el;
      setTip(null);
      if (!(el instanceof HTMLElement)) return;
      timer = window.setTimeout(() => {
        const text = el.dataset.tip;
        if (target !== el || !el.isConnected || !text) return;
        const label = el.querySelector<HTMLElement>("[data-tip-label]");
        if (label && label.offsetWidth > 0) return;
        setTip({
          text,
          side: (el.dataset.tipSide as Side | undefined) ?? "bottom",
          anchor: el.getBoundingClientRect(),
          dark: el.closest(".glass--dark") !== null,
        });
      }, DELAY);
    };
    // The pointer left the window.
    const out = (e: PointerEvent) => {
      if (!e.relatedTarget) clear();
    };
    document.addEventListener("pointerover", over);
    document.addEventListener("pointerout", out);
    document.addEventListener("pointerdown", clear, true);
    document.addEventListener("keydown", clear, true);
    document.addEventListener("scroll", clear, true);
    window.addEventListener("blur", clear);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("pointerover", over);
      document.removeEventListener("pointerout", out);
      document.removeEventListener("pointerdown", clear, true);
      document.removeEventListener("keydown", clear, true);
      document.removeEventListener("scroll", clear, true);
      window.removeEventListener("blur", clear);
    };
  }, []);

  return tip ? <Tooltip {...tip} /> : null;
}

function Tooltip({ text, side, anchor, dark }: Tip) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  useGlassUnderlay(ref);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let left: number;
    let top: number;
    if (side === "top" || side === "bottom") {
      left = anchor.left + anchor.width / 2 - w / 2;
      top = side === "bottom" ? anchor.bottom + GAP : anchor.top - GAP - h;
    } else {
      top = anchor.top + anchor.height / 2 - h / 2;
      left = side === "right" ? anchor.right + GAP : anchor.left - GAP - w;
    }
    left = Math.min(Math.max(left, EDGE), window.innerWidth - EDGE - w);
    top = Math.min(Math.max(top, EDGE), window.innerHeight - EDGE - h);
    setPos({ left, top });
  }, [side, anchor]);

  return (
    <div
      ref={ref}
      className={`tooltip glass ${dark ? "glass--dark" : ""}`}
      data-side={side}
      aria-hidden
      style={pos ?? { visibility: "hidden" }}
    >
      <span>{text}</span>
    </div>
  );
}
