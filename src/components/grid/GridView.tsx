import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { DndContext, DragOverlay, useDroppable } from "@dnd-kit/core";
import { SortableContext, rectSortingStrategy } from "@dnd-kit/sortable";
import { ChevronRight, MoreHorizontal } from "lucide-react";
import { connectAll, disconnectAll, openMenu, toggleGridGroup, updateSettings } from "../../state/actions";
import { isLive, setState, useBulkActions, useStore } from "../../state/store";
import { containerId, useTabsDnd } from "../tabs/useTabsDnd";
import { Button, Segmented } from "../ui/ui";
import { ConnectIcon, DisconnectIcon } from "../common/SessionIcons";
import type { ConnectionId } from "../../lib/types";
import { GridTile, GridTileOverlay, noteDragEnd } from "./GridTile";
import { TileObserverContext, type TileObserver } from "./tileObserver";
import "./grid.css";


function useTileObserver(root: React.RefObject<HTMLDivElement | null>): TileObserver | null {
  const [obs, setObs] = useState<TileObserver | null>(null);

  useEffect(() => {
    const visible = new Set<string>();
    const idOf = new WeakMap<Element, string>();
    let raf = 0;
    const flush = () => {
      raf = 0;
      setState({ visibleTiles: Array.from(visible) });
    };
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const id = idOf.get(e.target);
          if (!id) continue;
          if (e.isIntersecting) visible.add(id);
          else visible.delete(id);
        }
        if (!raf) raf = requestAnimationFrame(flush);
      },
      { root: root.current, rootMargin: "160px 0px" },
    );
    setObs({
      observe(el, id) {
        idOf.set(el, id);
        io.observe(el);
        return () => {
          io.unobserve(el);
          if (visible.delete(id) && !raf) raf = requestAnimationFrame(flush);
        };
      },
    });
    return () => {
      io.disconnect();
      cancelAnimationFrame(raf);
      setState({ visibleTiles: [] });
    };
  }, [root]);

  return obs;
}

/** A grid group's heading: its name and live/total count (click to fold the group), and the
 *  group's controls (connect all, disconnect all, and the full group menu). */
function GridSectionHeader({
  groupId,
  name,
  ids,
  folded,
}: {
  groupId: string;
  name: string;
  ids: ConnectionId[];
  folded: boolean;
}) {
  const live = useStore((s) => ids.reduce((n, id) => n + (isLive(s.sessions[id]?.status ?? "idle") ? 1 : 0), 0));
  const { canConnect, canDisconnect } = useBulkActions(ids);
  return (
    <div className={`grid-section__head ${folded ? "is-folded" : ""}`}>
      <h2 className="grid-section__title">
        <button className="grid-section__toggle" aria-expanded={!folded} onClick={() => toggleGridGroup(groupId)}>
          <ChevronRight size={12} className="grid-section__chevron" />
          {name}
          <span className="grid-section__count">{live ? `${live}/${ids.length}` : ids.length}</span>
        </button>
      </h2>
      <div className="grid-section__actions">
        {canConnect && (
          <Button size="sm" icon={<ConnectIcon size={12} />} onClick={() => connectAll(ids)}>
            Connect all
          </Button>
        )}
        {canDisconnect && (
          <Button size="sm" icon={<DisconnectIcon size={12} />} onClick={() => disconnectAll(ids)}>
            Disconnect all
          </Button>
        )}
        <Button
          size="sm"
          className="btn--icon"
          aria-label="Group actions"
          data-tip="Group actions"
          icon={<MoreHorizontal size={14} />}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            openMenu({ x: r.left, y: r.bottom + 4, target: { kind: "group", id: groupId } });
          }}
        />
      </div>
    </div>
  );
}

/** A section's grid; also the drop target for tiles dragged in from another group. */
function GridDropZone({ groupId, tileWidth, children }: { groupId: string; tileWidth: number; children: ReactNode }) {
  const { setNodeRef } = useDroppable({ id: containerId(groupId) });
  return (
    <div ref={setNodeRef} className="grid" style={{ "--tile": `${tileWidth}px` } as React.CSSProperties}>
      {children}
    </div>
  );
}

function GridViewInner() {
  const { sections, activeId, sensors, collisionDetection, handlers } = useTabsDnd();
  const groups = useStore((s) => s.groups);
  const filter = useStore((s) => s.settings.gridFilter);
  const tileWidth = useStore((s) => s.settings.gridTileWidth);
  const sessions = useStore((s) => (filter === "connected" ? s.sessions : null));
  const counts = useStore((s) => {
    let live = 0;
    for (const id of s.order) if (isLive(s.sessions[id]?.status ?? "idle")) live++;
    return `${live}/${s.order.length}`;
  });
  const [liveCount, total] = counts.split("/").map(Number);
  const scrollRef = useRef<HTMLDivElement>(null);
  const observer = useTileObserver(scrollRef);
  const groupName = useMemo(() => new Map(groups.map((g) => [g.id, g.name])), [groups]);
  const foldedIds = useStore((s) => s.settings.gridCollapsed);
  const folded = useMemo(() => new Set(foldedIds), [foldedIds]);

  const visibleSections = useMemo(() => {
    return sections
      .map((sec) => ({
        ...sec,
        ids: sessions ? sec.ids.filter((id) => isLive(sessions[id]?.status ?? "idle")) : sec.ids,
      }))
      .filter((sec) => sec.ids.length > 0);
  }, [sections, sessions]);

  const showHeaders = groups.length > 1;

  return (
    <div className="grid-view">
      <div className="grid-toolbar">
        <Segmented
          size="sm"
          value={filter}
          onChange={(v) => updateSettings({ gridFilter: v })}
          options={[
            { value: "all", label: <>All <span className="grid-toolbar__count">{total}</span></> },
            { value: "connected", label: <>Connected <span className="grid-toolbar__count">{liveCount}</span></> },
          ]}
        />
        <div className="grid-toolbar__spacer" />
        <label className="grid-size" title="Tile size">
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden>
            <rect x="1" y="1" width="4" height="4" rx="1" fill="currentColor" />
            <rect x="7" y="1" width="4" height="4" rx="1" fill="currentColor" />
            <rect x="1" y="7" width="4" height="4" rx="1" fill="currentColor" />
            <rect x="7" y="7" width="4" height="4" rx="1" fill="currentColor" />
          </svg>
          <input
            type="range"
            min={160}
            max={640}
            step={8}
            value={tileWidth}
            onChange={(e) => updateSettings({ gridTileWidth: Number(e.target.value) })}
            style={{ "--fill": `${((tileWidth - 160) / 480) * 100}%` } as React.CSSProperties}
          />
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden>
            <rect x="1" y="1" width="10" height="10" rx="2" fill="currentColor" />
          </svg>
        </label>
        <Button size="sm" icon={<ConnectIcon size={12} />} onClick={() => connectAll()} disabled={liveCount === total}>
          Connect all
        </Button>
      </div>

      <div className="grid-scroll" ref={scrollRef}>
        <DndContext
          sensors={sensors}
          collisionDetection={collisionDetection}
          {...handlers}
          onDragEnd={(e) => {
            noteDragEnd();
            handlers.onDragEnd(e);
          }}
          onDragCancel={() => {
            noteDragEnd();
            handlers.onDragCancel();
          }}
        >
          <TileObserverContext.Provider value={observer}>
            {visibleSections.length === 0 ? (
              <div className="grid-empty">
                {filter === "connected" ? "No connected sessions" : "No connections"}
              </div>
            ) : (
              visibleSections.map((sec) => (
                <section key={sec.groupId} className="grid-section">
                  {showHeaders && (
                    <GridSectionHeader
                      groupId={sec.groupId}
                      name={groupName.get(sec.groupId) ?? ""}
                      ids={sec.ids}
                      folded={folded.has(sec.groupId)}
                    />
                  )}
                  {/* A folded group's tiles aren't rendered, so their sessions get no thumbnails
                      (and are paused on the server like any session off screen). */}
                  {!(showHeaders && folded.has(sec.groupId)) && (
                    <SortableContext items={sec.ids} strategy={rectSortingStrategy}>
                      <GridDropZone groupId={sec.groupId} tileWidth={tileWidth}>
                        {sec.ids.map((id) => (
                          <GridTile key={id} id={id} />
                        ))}
                      </GridDropZone>
                    </SortableContext>
                  )}
                </section>
              ))
            )}
          </TileObserverContext.Provider>
          {/* In <body>: the main area's CSS containment would otherwise offset the fixed-position copy. */}
          {createPortal(
            <DragOverlay dropAnimation={{ duration: 180, easing: "cubic-bezier(0.16, 1, 0.3, 1)" }}>
              {activeId ? <GridTileOverlay id={activeId} /> : null}
            </DragOverlay>,
            document.body,
          )}
        </DndContext>
      </div>
    </div>
  );
}

export const GridView = memo(GridViewInner);
