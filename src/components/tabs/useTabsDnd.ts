import { useMemo, useState } from "react";
import {
  closestCenter,
  PointerSensor,
  pointerWithin,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import { arrayMove } from "@dnd-kit/sortable";
import { applyOrder } from "../../state/actions";
import { buildSections, useStore, type Section } from "../../state/store";
import type { GroupId } from "../../lib/types";

export const containerId = (groupId: GroupId) => `c:${groupId}`;
const isContainer = (id: string) => id.startsWith("c:");

/** Prefer item hits under the pointer, then container hits, then nearest center. */
const collision: CollisionDetection = (args) => {
  const hits = pointerWithin(args);
  const items = hits.filter((h) => !isContainer(String(h.id)));
  if (items.length) return items;
  if (hits.length) return hits;
  return closestCenter(args);
};

export function useSections(): Section[] {
  const order = useStore((s) => s.order);
  const connById = useStore((s) => s.connById);
  const groups = useStore((s) => s.groups);
  return useMemo(() => buildSections(order, connById, groups), [order, connById, groups]);
}

/**
 * Multi-container sortable state shared by the sidebar and the top tab strip.
 * While dragging we work on a local draft of the sections so cross-group moves
 * render live; the store is updated once on drop.
 */
export function useTabsDnd() {
  const sections = useSections();
  const [draft, setDraft] = useState<Section[] | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  const current = draft ?? sections;

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));

  const findContainer = (secs: Section[], rawId: string): number => {
    const id = rawId.endsWith(":header") ? rawId.slice(0, -":header".length) : rawId;
    if (isContainer(id)) return secs.findIndex((s) => containerId(s.groupId) === id);
    return secs.findIndex((s) => s.ids.includes(id));
  };

  const onDragStart = (e: DragStartEvent) => {
    setActiveId(String(e.active.id));
    setDraft(sections.map((s) => ({ ...s, ids: [...s.ids] })));
  };

  const onDragOver = ({ active, over }: DragOverEvent) => {
    if (!over || !draft) return;
    const a = String(active.id);
    const o = String(over.id);
    const from = findContainer(draft, a);
    const to = findContainer(draft, o);
    if (from < 0 || to < 0 || from === to) return;
    setDraft((prev) => {
      if (!prev) return prev;
      const next = prev.map((s) => ({ ...s, ids: [...s.ids] }));
      next[from].ids = next[from].ids.filter((x) => x !== a);
      const target = next[to].ids;
      const overIdx = isContainer(o) ? target.length : target.indexOf(o);
      target.splice(overIdx < 0 ? target.length : overIdx, 0, a);
      return next;
    });
  };

  const finish = (final: Section[]) => {
    applyOrder(final.flatMap((s) => s.ids.map((id) => ({ id, groupId: s.groupId }))));
  };

  const onDragEnd = ({ active, over }: DragEndEvent) => {
    const secs = draft;
    setActiveId(null);
    setDraft(null);
    if (!secs || !over) return;
    const a = String(active.id);
    const o = String(over.id);
    const c = findContainer(secs, a);
    if (c < 0) return;
    let final = secs;
    if (!isContainer(o) && secs[c].ids.includes(o) && a !== o) {
      final = secs.map((s, i) =>
        i === c ? { ...s, ids: arrayMove(s.ids, s.ids.indexOf(a), s.ids.indexOf(o)) } : s,
      );
    }
    const changed = final.some(
      (s, i) => s.ids.length !== sections[i]?.ids.length || s.ids.some((id, j) => sections[i].ids[j] !== id),
    );
    if (changed) finish(final);
  };

  const onDragCancel = () => {
    setActiveId(null);
    setDraft(null);
  };

  return {
    sections: current,
    activeId,
    sensors,
    collisionDetection: collision,
    handlers: { onDragStart, onDragOver, onDragEnd, onDragCancel },
  };
}
