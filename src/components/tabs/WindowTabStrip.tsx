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
import { useStore, windowTabs } from "../../state/store";
import { DockGhost, SoloTab, TabItem, TabOverlay } from "./TabItem";
import { pastStrip, stripWheel, useTearOff } from "./useTabsDnd";
import "./toptabs.css";

// Chrome forces the drag horizontal while the tab is still in the strip; it only gains
// vertical freedom once it tears off into a window of its own (handled natively).
const horizontalOnly: Modifier = ({ transform }) => ({ ...transform, y: 0 });

/**
 * A window's tab strip (the main window and detached windows alike): a flat row of the
 * window's open tabs. Reorder them, pull one out into a window of its own (or onto another
 * window's strip), × closes and disconnects. A detached window with a single tab lets you
 * drag it to move the window.
 */
function WindowTabStripInner() {
  const label = windowLabel();
  const main = isMainWindow();
  const tabs = useStore(useShallow((s) => windowTabs(s, label)));
  // A tab from another window is being dragged over this strip: show it landing where it'd go.
  const dock = useStore((s) => s.dockPreview);
  const ghost = dock !== null && !tabs.includes(dock.id);
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

  // While a tab from another window is dragged over this strip, its ghost follows the real
  // cursor (not the insertion slot), so it stays under the pointer like an ordinary drag.
  const ghostRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!ghost) return;
    let raf = 0;
    let at = 0;
    const apply = () => {
      raf = 0;
      const g = ghostRef.current;
      if (!g) return;
      // Measure the natural centre (transform cleared) in viewport space, then shift it
      // onto the cursor. Both are viewport coords, so scroll/offset don't matter.
      g.style.transform = "none";
      const r = g.getBoundingClientRect();
      g.style.transform = `translateX(${Math.round(at - (r.left + r.width / 2))}px)`;
    };
    const follow = (e: PointerEvent) => {
      at = e.clientX;
      if (!raf) raf = requestAnimationFrame(apply);
    };
    window.addEventListener("pointermove", follow);
    return () => {
      window.removeEventListener("pointermove", follow);
      if (raf) cancelAnimationFrame(raf);
    };
  }, [ghost]);

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

  const onDragEnd = (e: DragEndEvent) => {
    setActiveId(null);
    const id = String(e.active.id);
    const over = e.over ? String(e.over.id) : null;
    const from = tabs.indexOf(id);
    const to = over ? tabs.indexOf(over) : -1;
    if (from < 0 || to < 0 || from === to) return;
    reorderWindowTabs(arrayMove(tabs, from, to));
  };

  // A detached window's single tab is a handle to move the window — unless a tab from
  // another window is being dragged in, when the strip must show it landing.
  const soloWindowDrag = !main && tabs.length === 1 && !ghost;

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
              const ev = e.activatorEvent as PointerEvent | null;
              if (ev) pointer.current = { x: ev.clientX, y: ev.clientY };
              window.addEventListener("pointermove", trackPointer);
              setActiveId(String(e.active.id));
            }}
            onDragMove={(e: DragMoveEvent) => tear.onMove(String(e.active.id), pointer.current)}
            onDragEnd={(e: DragEndEvent) => {
              window.removeEventListener("pointermove", trackPointer);
              onDragEnd(e);
            }}
            onDragCancel={() => {
              window.removeEventListener("pointermove", trackPointer);
              setActiveId(null);
            }}
          >
            <div className="top-seg">
              <SortableContext items={tabs} strategy={horizontalListSortingStrategy}>
                {tabs.flatMap((id, i) => {
                  const tab = <TabItem key={id} id={id} variant="top" onClose={() => closeTab(id)} />;
                  // Stable key so React moves the ghost between slots (no re-grow) as the index changes.
                  return ghost && dock!.index === i
                    ? [<DockGhost key="dock-ghost" ref={ghostRef} id={dock!.id} />, tab]
                    : [tab];
                })}
                {ghost && dock!.index >= tabs.length && <DockGhost key="dock-ghost" ref={ghostRef} id={dock!.id} />}
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
