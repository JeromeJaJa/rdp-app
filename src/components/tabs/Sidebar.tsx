import { memo, useCallback, useEffect, useMemo, useRef } from "react";
import { createPortal } from "react-dom";
import { DndContext, DragOverlay, useDroppable } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { FolderPlus, Search, X } from "lucide-react";
import { clearPicked, createGroup, updateSettings } from "../../state/actions";
import { displayName, getState, isBusy, isLive, setState, useStore } from "../../state/store";
import { IconButton } from "../ui/ui";
import { BulkSessionButtons, NewConnectionButton, TransferButtons, ViewToggle } from "../common/GlobalActions";
import { GroupHeader } from "./GroupHeader";
import { TabItem, TabOverlay } from "./TabItem";
import { containerId, useTabsDnd } from "./useTabsDnd";
import type { GroupId } from "../../lib/types";
import "./sidebar.css";

function DropZone({ groupId, children, empty }: { groupId: GroupId; children: React.ReactNode; empty?: boolean }) {
  const { setNodeRef, isOver } = useDroppable({ id: containerId(groupId) });
  return (
    <div ref={setNodeRef} className={`side-section__items ${empty ? "is-empty" : ""} ${isOver ? "is-over" : ""}`}>
      {children}
    </div>
  );
}

function FilteredList({ filter }: { filter: string }) {
  const order = useStore((s) => s.order);
  const connById = useStore((s) => s.connById);
  const ids = useMemo(() => {
    const q = filter.toLowerCase();
    return order.filter((id) => {
      const c = connById[id];
      if (!c) return false;
      return (
        displayName(c).toLowerCase().includes(q) ||
        c.host.toLowerCase().includes(q) ||
        c.username.toLowerCase().includes(q)
      );
    });
  }, [order, connById, filter]);

  if (!ids.length) return <div className="side-empty">No matches for “{filter}”</div>;
  return (
    <DndContext>
      <SortableContext items={ids} strategy={verticalListSortingStrategy} disabled>
        <div className="side-section__items">
          {ids.map((id) => (
            <TabItem key={id} id={id} variant="side" />
          ))}
        </div>
      </SortableContext>
    </DndContext>
  );
}

function SidebarList() {
  // The library: click a connection to open its tab. Drag reorders (within/across groups).
  const { sections, activeId, sensors, collisionDetection, handlers } = useTabsDnd();
  const groups = useStore((s) => s.groups);
  const total = useStore((s) => s.order.length);
  const groupById = useMemo(() => new Map(groups.map((g) => [g.id, g])), [groups]);
  const customGroups = groups.length > 1;

  if (!total && !customGroups) return <div className="side-empty">No connections yet</div>;

  return (
    <DndContext sensors={sensors} collisionDetection={collisionDetection} {...handlers}>
      {sections.map((sec) => {
        const group = groupById.get(sec.groupId);
        if (!group) return null;
        return (
          <div key={group.id} className="side-section">
            <GroupHeader group={group} ids={sec.ids} />
            {!group.collapsed && (
              <SortableContext items={sec.ids} strategy={verticalListSortingStrategy}>
                <DropZone groupId={group.id} empty={!sec.ids.length}>
                  {sec.ids.map((id) => (
                    <TabItem key={id} id={id} variant="side" />
                  ))}
                  {!sec.ids.length && (
                    <div className="side-drop-hint" title="Drag connections here">
                      <span>{group.builtin ? "Drag connections here" : "Empty — drag connections here"}</span>
                    </div>
                  )}
                </DropZone>
              </SortableContext>
            )}
          </div>
        );
      })}
      {/* In <body>: the sidebar's container query (layout containment) would offset the copy. */}
      {createPortal(
        <DragOverlay dropAnimation={{ duration: 160, easing: "cubic-bezier(0.16, 1, 0.3, 1)" }}>
          {activeId ? <TabOverlay id={activeId} variant="side" /> : null}
        </DragOverlay>,
        document.body,
      )}
    </DndContext>
  );
}

function Summary() {
  const total = useStore((s) => s.order.length);
  const connected = useStore((s) => {
    let n = 0;
    for (const id in s.sessions) if (isLive(s.sessions[id].status)) n++;
    return n;
  });
  const connecting = useStore((s) => {
    let n = 0;
    for (const id in s.sessions) {
      const st = s.sessions[id].status;
      if (isBusy(st) && !isLive(st)) n++;
    }
    return n;
  });
  return (
    <div className="side-summary">
      <span className={`side-summary__dot ${connected ? "is-on" : ""}`} />
      <span className="side-summary__text">
        <span className="side-summary__live">{connected}</span> connected
        {connecting > 0 && <span className="side-summary__busy"> · {connecting} connecting</span>}
      </span>
      <span className="side-summary__total">{total} total</span>
    </div>
  );
}

/** Esc drops the sidebar's picks (when nothing else, like a menu or dialog, takes it). */
function usePickEscape() {
  const any = useStore((s) => s.picked.length > 0);
  useEffect(() => {
    if (!any) return;
    const onKey = (e: KeyboardEvent) => {
      const s = getState();
      if (e.key === "Escape" && !s.menu && !s.dialog && !s.paletteOpen && !s.remoteFocused) clearPicked();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [any]);
}

function SidebarInner() {
  usePickEscape();
  const width = useStore((s) => s.settings.sidebarWidth);
  const filter = useStore((s) => s.filter);
  const inputRef = useRef<HTMLInputElement>(null);

  const onResizeStart = useCallback((e: React.PointerEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = getState().settings.sidebarWidth;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    document.body.classList.add("is-resizing");
    const move = (ev: PointerEvent) => {
      const w = Math.min(420, Math.max(180, startW + ev.clientX - startX));
      setState({ settings: { ...getState().settings, sidebarWidth: w } });
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      document.body.classList.remove("is-resizing");
      updateSettings({ sidebarWidth: getState().settings.sidebarWidth });
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }, []);

  return (
    <aside className="sidebar" style={{ width }}>
      {/* Actions on every connection (in the title bar when tabs are on top). */}
      <div className="sidebar__actions">
        <BulkSessionButtons />
        <span className="sidebar__actions-spacer" />
        <TransferButtons />
        <NewConnectionButton />
      </div>
      <div className="sidebar__top">
        <div className="side-search">
          <Search size={13} className="side-search__icon" />
          <input
            ref={inputRef}
            className="side-search__input"
            placeholder="Filter…"
            aria-label="Filter connections"
            value={filter}
            spellCheck={false}
            onChange={(e) => setState({ filter: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setState({ filter: "" });
                inputRef.current?.blur();
              }
            }}
          />
          {filter && (
            <button className="side-search__clear" aria-label="Clear filter" onClick={() => setState({ filter: "" })}>
              <X size={12} />
            </button>
          )}
        </div>
        <IconButton label="New group" onClick={() => createGroup("New group", { rename: true })}>
          <FolderPlus size={15} />
        </IconButton>
      </div>
      <div className="sidebar__scroll" role="tablist" aria-orientation="vertical">
        {filter.trim() ? <FilteredList filter={filter.trim()} /> : <SidebarList />}
      </div>
      <div className="sidebar__footer">
        <Summary />
        <ViewToggle className="side-view" labeled />
      </div>
      <div className="sidebar__resize" onPointerDown={onResizeStart} />
    </aside>
  );
}

export const Sidebar = memo(SidebarInner);
