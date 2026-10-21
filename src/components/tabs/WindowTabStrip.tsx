import { Fragment, memo, useEffect, useRef, useState } from "react";
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
} from "@dnd-kit/core";
import { arrayMove, horizontalListSortingStrategy, SortableContext } from "@dnd-kit/sortable";
import { useShallow } from "zustand/react/shallow";
import { isMainWindow, windowLabel } from "../../lib/env";
import { closeTab, reorderWindowTabs } from "../../state/actions";
import { useStore, windowTabs } from "../../state/store";
import { SoloTab, TabItem, TabOverlay } from "./TabItem";
import { dragPointer, pastTitlebar, stripWheel, useTearOff } from "./useTabsDnd";
import "./toptabs.css";

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
  // A tab from another window is being dragged over this strip: open a gap where it'd land.
  const dock = useStore((s) => s.dockPreview);
  const [activeId, setActiveId] = useState<string | null>(null);
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));
  const tear = useTearOff(pastTitlebar);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [fade, setFade] = useState({ left: false, right: false });

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

  // A detached window's single tab is a handle to move the window.
  const soloWindowDrag = !main && tabs.length === 1;

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
            onDragStart={(e: DragStartEvent) => {
              tear.onStart();
              setActiveId(String(e.active.id));
            }}
            onDragMove={(e: DragMoveEvent) => tear.onMove(String(e.active.id), dragPointer(e))}
            onDragEnd={onDragEnd}
            onDragCancel={() => setActiveId(null)}
          >
            <div className="top-seg">
              <SortableContext items={tabs} strategy={horizontalListSortingStrategy}>
                {tabs.map((id, i) => (
                  <Fragment key={id}>
                    {dock === i && <span className="top-dock-gap" aria-hidden />}
                    <TabItem id={id} variant="top" onClose={() => closeTab(id)} />
                  </Fragment>
                ))}
                {dock !== null && dock >= tabs.length && <span className="top-dock-gap" aria-hidden />}
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
