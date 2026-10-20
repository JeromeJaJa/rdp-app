import { memo, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { DndContext, DragOverlay, useDroppable, type Modifier } from "@dnd-kit/core";
import { horizontalListSortingStrategy, SortableContext } from "@dnd-kit/sortable";
import { useStore } from "../../state/store";
import type { GroupId } from "../../lib/types";
import { TabItem, TabOverlay } from "./TabItem";
import { containerId, pastTitlebar, stripWheel, useTabsDnd } from "./useTabsDnd";
import "./toptabs.css";

// The tab stays in the strip until it is pulled well away from it (to tear it off).
const lockY: Modifier = ({ transform }) => (Math.abs(transform.y) > 40 ? transform : { ...transform, y: 0 });

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
  // Pulled down out of the title bar: a window of its own.
  const { sections, activeId, sensors, collisionDetection, handlers } = useTabsDnd(pastTitlebar);
  const groups = useStore((s) => s.groups);
  const groupName = useMemo(() => new Map(groups.map((g) => [g.id, g.name])), [groups]);
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
