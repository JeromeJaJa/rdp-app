import { useEffect, useRef, useState } from "react";
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
import { windowLabel } from "../../lib/env";
import { moveToMainWindow, reorderWindowTabs } from "../../state/actions";
import { useStore, windowTabs } from "../../state/store";
import { SoloTab, TabItem, TabOverlay } from "./TabItem";
import { dragPointer, pastTitlebar, stripWheel, useTearOff } from "./useTabsDnd";
import "./toptabs.css";

// The tab stays in the strip until it is pulled well away from it (to tear it off).
const lockY: Modifier = ({ transform }) => (Math.abs(transform.y) > 40 ? transform : { ...transform, y: 0 });

/**
 * A detached window's tabs: reorder them, pull one out into a window of its own (or onto
 * another window's tabs), × sends one back. With a single tab, dragging it moves the window.
 */
export function WindowTabStrip() {
  const tabs = useStore(useShallow((s) => windowTabs(s, windowLabel())));
  const [activeId, setActiveId] = useState<string | null>(null);
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));
  const tear = useTearOff(pastTitlebar);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = stripWheel(el);
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const onDragEnd = (e: DragEndEvent) => {
    setActiveId(null);
    const id = String(e.active.id);
    const over = e.over ? String(e.over.id) : null;
    const from = tabs.indexOf(id);
    const to = over ? tabs.indexOf(over) : -1;
    if (from < 0 || to < 0 || from === to) return;
    reorderWindowTabs(arrayMove(tabs, from, to));
  };

  return (
    <div ref={scrollRef} className="top-tabs" role="tablist">
      <div className="top-tabs__track">
        {tabs.length === 1 ? (
          <div className="top-seg">
            <SoloTab id={tabs[0]} onClose={() => moveToMainWindow(tabs[0])} />
          </div>
        ) : (
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            modifiers={[lockY]}
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
                {tabs.map((id) => (
                  <TabItem key={id} id={id} variant="top" onClose={() => moveToMainWindow(id)} />
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
