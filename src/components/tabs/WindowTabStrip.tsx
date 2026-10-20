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
import { dropTabOutside, moveToMainWindow, reorderWindowTabs } from "../../state/actions";
import { useStore, windowTabs } from "../../state/store";
import { TabItem, TabOverlay } from "./TabItem";
import { dragPointer, stripWheel, wantsDetach } from "./useTabsDnd";
import "./toptabs.css";

// The tab stays in the strip until it is pulled well away from it (to tear it off).
const lockY: Modifier = ({ transform }) => (Math.abs(transform.y) > 40 ? transform : { ...transform, y: 0 });

// Dropped well below the strip or outside the window: into another window, or a new one.
const away = (_x: number, y: number) => {
  const r = document.querySelector(".titlebar")?.getBoundingClientRect();
  return !!r && y > r.bottom + 48;
};

/** A detached window's tabs: reorder, drag out into another window, × sends one back. */
export function WindowTabStrip() {
  const tabs = useStore(useShallow((s) => windowTabs(s, windowLabel())));
  const [activeId, setActiveId] = useState<string | null>(null);
  const [detaching, setDetaching] = useState(false);
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = stripWheel(el);
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const onDragMove = (e: DragMoveEvent) => {
    const next = wantsDetach(dragPointer(e), away);
    if (next !== detaching) setDetaching(next);
  };

  const onDragEnd = (e: DragEndEvent) => {
    setActiveId(null);
    setDetaching(false);
    const id = String(e.active.id);
    if (wantsDetach(dragPointer(e), away)) {
      dropTabOutside(id);
      return;
    }
    const over = e.over ? String(e.over.id) : null;
    const from = tabs.indexOf(id);
    const to = over ? tabs.indexOf(over) : -1;
    if (from < 0 || to < 0 || from === to) return;
    reorderWindowTabs(arrayMove(tabs, from, to));
  };

  const reset = () => {
    setActiveId(null);
    setDetaching(false);
  };

  return (
    <div ref={scrollRef} className="top-tabs" role="tablist">
      <div className="top-tabs__track">
        <DndContext
          sensors={sensors}
          collisionDetection={closestCenter}
          modifiers={[lockY]}
          onDragStart={(e: DragStartEvent) => setActiveId(String(e.active.id))}
          onDragMove={onDragMove}
          onDragEnd={onDragEnd}
          onDragCancel={reset}
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
              {activeId ? <TabOverlay id={activeId} variant="top" detaching={detaching} /> : null}
            </DragOverlay>,
            document.body,
          )}
        </DndContext>
      </div>
    </div>
  );
}
