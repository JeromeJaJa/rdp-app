import { memo, useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  closestCenter,
  DndContext,
  DragOverlay,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragMoveEvent,
  type DragStartEvent,
  type Modifier,
} from "@dnd-kit/core";
import { arrayMove, horizontalListSortingStrategy, SortableContext } from "@dnd-kit/sortable";
import { useShallow } from "zustand/react/shallow";
import { isMainWindow, windowLabel } from "../../lib/env";
import { closeTab, reorderWindowTabs } from "../../state/actions";
import { getState, setState, useStore, windowTabs } from "../../state/store";
import { SoloTab, TabItem, TabOverlay } from "./TabItem";
import { pastStrip, stripWheel, useTearOff } from "./useTabsDnd";
import "./toptabs.css";

// Chrome forces the drag horizontal while the tab is still in the strip; it only gains
// vertical freedom once it tears off into a window of its own (handled natively).
const horizontalOnly: Modifier = ({ transform }) => ({ ...transform, y: 0 });

/** A primary-button pointer event, for a drag whose real press happened in another window. */
function firePointer(type: "pointerdown" | "pointerup", target: EventTarget, x: number, y: number) {
  target.dispatchEvent(
    new PointerEvent(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      clientX: x,
      clientY: y,
      button: 0,
      buttons: type === "pointerdown" ? 1 : 0,
      pointerId: 1,
      pointerType: "mouse",
      isPrimary: true,
    }),
  );
}

/**
 * A window's tab strip (the main window and detached windows alike): a flat row of the
 * window's open tabs. Reorder them, pull one out into a window of its own (or onto another
 * window's strip), × or middle-click closes and disconnects. A detached window with a single
 * tab lets you drag it to move the window.
 */
function WindowTabStripInner() {
  const label = windowLabel();
  const main = isMainWindow();
  const tabs = useStore(useShallow((s) => windowTabs(s, label)));
  const continueDrag = useStore((s) => s.continueDrag);
  const dragRelease = useStore((s) => s.dragRelease);
  const [activeId, setActiveId] = useState<string | null>(null);
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));
  const tear = useTearOff(pastStrip);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [fade, setFade] = useState({ left: false, right: false });

  // The real pointer, tracked live: tear-off keys off the true cursor, not dnd-kit's
  // delta (which the horizontal-axis lock has zeroed on Y).
  const pointer = useRef({ x: 0, y: 0 });
  const trackPointer = useCallback((e: PointerEvent) => {
    pointer.current = { x: e.clientX, y: e.clientY };
  }, []);

  // A detached window's single tab is a handle to move the window.
  const soloWindowDrag = !main && tabs.length === 1;

  // A tab dragged out of a window came back into this one while still held. It's simply one
  // of this window's tabs now, being dragged: press it where the pointer is, and the held
  // mouse carries on an ordinary drag from there (reorder, tear out again, drop).
  const handedOver = useRef(false);
  useEffect(() => {
    if (!continueDrag || soloWindowDrag || !tabs.includes(continueDrag.id)) return;
    setState({ continueDrag: null });
    const el = scrollRef.current?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(continueDrag.id)}"]`);
    if (!el) return;
    const r = el.getBoundingClientRect();
    // On the tab (under the pointer if it's over it), so the tab lands under the cursor.
    const x = Math.min(Math.max(continueDrag.x, r.left + 8), r.right - 8);
    pointer.current = { x: continueDrag.x, y: continueDrag.y };
    handedOver.current = true;
    firePointer("pointerdown", el, x, r.top + r.height / 2);
  }, [continueDrag, tabs, soloWindowDrag]);

  // The button went up after a drag was handed to this window. The press began elsewhere,
  // so the page may never see the release: end the drag here so it can't stay stuck.
  useEffect(() => {
    if (!dragRelease) return;
    if (getState().continueDrag) setState({ continueDrag: null });
    if (!handedOver.current) return;
    handedOver.current = false;
    firePointer("pointerup", document, pointer.current.x, pointer.current.y);
  }, [dragRelease]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () =>
      setFade({ left: el.scrollLeft > 2, right: el.scrollLeft + el.clientWidth < el.scrollWidth - 2 });
    const onWheel = stripWheel(el);
    update();
    el.addEventListener("scroll", update, { passive: true });
    el.addEventListener("wheel", onWheel, { passive: false });
    const ro = new ResizeObserver(update);
    ro.observe(el);
    for (const child of Array.from(el.children)) ro.observe(child);
    return () => {
      el.removeEventListener("scroll", update);
      el.removeEventListener("wheel", onWheel);
      ro.disconnect();
    };
  }, [tabs.length]);

  const endDrag = () => {
    window.removeEventListener("pointermove", trackPointer);
    handedOver.current = false;
    setActiveId(null);
  };

  const onDragEnd = (e: DragEndEvent) => {
    endDrag();
    const id = String(e.active.id);
    const over = e.over ? String(e.over.id) : null;
    const from = tabs.indexOf(id);
    const to = over ? tabs.indexOf(over) : -1;
    if (from < 0 || to < 0 || from === to) return;
    reorderWindowTabs(arrayMove(tabs, from, to));
  };

  return (
    <div
      ref={scrollRef}
      className={`top-tabs ${fade.left ? "fade-left" : ""} ${fade.right ? "fade-right" : ""}`}
      role="tablist"
    >
      <div className="top-tabs__track">
        {soloWindowDrag ? (
          <div className="top-seg">
            <SoloTab id={tabs[0]} onClose={() => closeTab(tabs[0])} />
          </div>
        ) : (
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            modifiers={[horizontalOnly]}
            onDragStart={(e: DragStartEvent) => {
              tear.onStart();
              if (!handedOver.current) {
                const ev = e.activatorEvent as PointerEvent | null;
                if (ev) pointer.current = { x: ev.clientX, y: ev.clientY };
              }
              window.addEventListener("pointermove", trackPointer);
              setActiveId(String(e.active.id));
            }}
            onDragMove={(e: DragMoveEvent) => tear.onMove(String(e.active.id), pointer.current)}
            onDragEnd={onDragEnd}
            onDragCancel={endDrag}
          >
            <div className="top-seg">
              <SortableContext items={tabs} strategy={horizontalListSortingStrategy}>
                {tabs.map((id) => (
                  <TabItem key={id} id={id} variant="top" onClose={() => closeTab(id)} />
                ))}
              </SortableContext>
            </div>
            {createPortal(
              <DragOverlay dropAnimation={{ duration: 160, easing: "cubic-bezier(0.16, 1, 0.3, 1)" }}>
                {activeId ? <TabOverlay id={activeId} variant="top" /> : null}
              </DragOverlay>,
              document.body,
            )}
          </DndContext>
        )}
      </div>
    </div>
  );
}

export const WindowTabStrip = memo(WindowTabStripInner);
