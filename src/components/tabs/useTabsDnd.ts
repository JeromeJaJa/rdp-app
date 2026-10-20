import { useMemo, useState } from "react";
import {
  closestCenter,
  PointerSensor,
  pointerWithin,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragMoveEvent,
  type DragOverEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import { arrayMove } from "@dnd-kit/sortable";
import { applyOrder, dropTabOutside, selectRelative } from "../../state/actions";
import { isTauri } from "../../lib/env";
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
 * Wheel over a horizontal tab strip: Shift+wheel switches to the next / previous tab
 * (one per notch; touchpads accumulate), a plain wheel scrolls the strip.
 */
export function stripWheel(el: HTMLElement): (e: WheelEvent) => void {
  let acc = 0;
  let lastWheel = 0;
  return (e) => {
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
}

/** Where the pointer is during a drag (start position plus how far it moved). */
export function dragPointer(e: { activatorEvent: Event | null; delta: { x: number; y: number } }) {
  const start = e.activatorEvent as PointerEvent | null;
  return { x: (start?.clientX ?? 0) + e.delta.x, y: (start?.clientY ?? 0) + e.delta.y };
}

/**
 * Whether releasing a tab here should take it out of this window: the pointer is
 * outside the window, or `away` (e.g. well below a tab strip) says so.
 */
export function wantsDetach(p: { x: number; y: number }, away?: (x: number, y: number) => boolean): boolean {
  if (!isTauri()) return false;
  const outside = p.x < 0 || p.y < 0 || p.x >= window.innerWidth || p.y >= window.innerHeight;
  return outside || !!away?.(p.x, p.y);
}

/**
 * Multi-container sortable state shared by the sidebar and the top tab strip.
 * While dragging we work on a local draft of the sections so cross-group moves
 * render live; the store is updated once on drop.
 */
export function useTabsDnd(opts: { away?: (x: number, y: number) => boolean } = {}) {
  const sections = useSections();
  const [draft, setDraft] = useState<Section[] | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  // Released now, the tab would leave this window (Chrome-style tear-off).
  const [detaching, setDetaching] = useState(false);
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

  const onDragMove = (e: DragMoveEvent) => {
    const next = wantsDetach(dragPointer(e), opts.away);
    if (next !== detaching) setDetaching(next);
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

  const onDragEnd = (e: DragEndEvent) => {
    const { active, over } = e;
    const secs = draft;
    setActiveId(null);
    setDraft(null);
    setDetaching(false);
    if (wantsDetach(dragPointer(e), opts.away)) {
      dropTabOutside(String(active.id));
      return;
    }
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
    setDetaching(false);
  };

  return {
    sections: current,
    activeId,
    sensors,
    collisionDetection: collision,
    detaching,
    handlers: { onDragStart, onDragMove, onDragOver, onDragEnd, onDragCancel },
  };
}
