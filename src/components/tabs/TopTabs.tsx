import { memo, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { DndContext, DragOverlay, useDroppable, type Modifier } from "@dnd-kit/core";
import { horizontalListSortingStrategy, SortableContext } from "@dnd-kit/sortable";
import { selectRelative } from "../../state/actions";
import { useStore } from "../../state/store";
import type { GroupId } from "../../lib/types";
import { TabItem, TabOverlay } from "./TabItem";
import { containerId, useTabsDnd } from "./useTabsDnd";
import "./toptabs.css";

const lockY: Modifier = ({ transform }) => ({ ...transform, y: 0 });

function Segment({ groupId, label, ids }: { groupId: GroupId; label?: string; ids: string[] }) {
  const { setNodeRef, isOver } = useDroppable({ id: containerId(groupId) });
  return (
    <div ref={setNodeRef} className={`top-seg ${isOver ? "is-over" : ""} ${ids.length ? "" : "is-empty"}`}>
      {label && <span className="top-seg__label">{label}</span>}
      <SortableContext items={ids} strategy={horizontalListSortingStrategy}>
        {ids.map((id) => (
          <TabItem key={id} id={id} variant="top" />
        ))}
      </SortableContext>
    </div>
  );
}

function TopTabsInner() {
  const { sections, activeId, sensors, collisionDetection, handlers } = useTabsDnd();
  const groups = useStore((s) => s.groups);
  const groupName = useMemo(() => new Map(groups.map((g) => [g.id, g.name])), [groups]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [fade, setFade] = useState({ left: false, right: false });

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () =>
      setFade({ left: el.scrollLeft > 2, right: el.scrollLeft + el.clientWidth < el.scrollWidth - 2 });
    // Shift+wheel switches to the next / previous tab (one per notch; touchpads
    // accumulate); a plain wheel scrolls the strip.
    let acc = 0;
    let lastWheel = 0;
    const onWheel = (e: WheelEvent) => {
      if (e.shiftKey) {
        e.preventDefault();
        // Windows turns Shift+wheel into horizontal scrolling.
        const delta = e.deltaY || e.deltaX;
        const now = performance.now();
        if (now - lastWheel > 300 || Math.sign(delta) !== Math.sign(acc)) acc = 0;
        lastWheel = now;
        acc += e.deltaMode === 1 ? delta * 40 : delta;
        while (Math.abs(acc) >= 60) {
          selectRelative(acc > 0 ? 1 : -1);
          acc -= Math.sign(acc) * 100;
          if (Math.abs(acc) < 60) acc = 0;
        }
        return;
      }
      if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
        el.scrollLeft += e.deltaY;
        e.preventDefault();
      }
    };
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
  }, [sections.length]);

  const dragging = activeId !== null;
  // With only the built-in "All" group, labels would just be noise.
  const hasGroups = groups.length > 1;

  return (
    <div
      ref={scrollRef}
      className={`top-tabs ${fade.left ? "fade-left" : ""} ${fade.right ? "fade-right" : ""}`}
      role="tablist"
    >
      <div className="top-tabs__track">
        <DndContext sensors={sensors} collisionDetection={collisionDetection} modifiers={[lockY]} {...handlers}>
          {sections.map((sec) => {
            if (!sec.ids.length && !(dragging && hasGroups)) return null;
            const label = hasGroups ? groupName.get(sec.groupId) : undefined;
            return <Segment key={sec.groupId} groupId={sec.groupId} label={label} ids={sec.ids} />;
          })}
          {createPortal(
            <DragOverlay dropAnimation={{ duration: 160, easing: "cubic-bezier(0.16, 1, 0.3, 1)" }}>
              {activeId ? <TabOverlay id={activeId} variant="top" /> : null}
            </DragOverlay>,
            document.body,
          )}
        </DndContext>
      </div>
    </div>
  );
}

export const TopTabs = memo(TopTabsInner);
