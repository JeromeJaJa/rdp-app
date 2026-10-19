import { memo, useEffect, useMemo, useRef, useState } from "react";
import { connectAll, updateSettings } from "../../state/actions";
import { isLive, setState, useStore } from "../../state/store";
import { useSections } from "../tabs/useTabsDnd";
import { Button, Segmented } from "../ui/ui";
import { ConnectIcon } from "../common/SessionIcons";
import { GridTile } from "./GridTile";
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

function GridViewInner() {
  const sections = useSections();
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
        <TileObserverContext.Provider value={observer}>
          {visibleSections.length === 0 ? (
            <div className="grid-empty">
              {filter === "connected" ? "No connected sessions" : "No connections"}
            </div>
          ) : (
            visibleSections.map((sec) => (
              <section key={sec.groupId} className="grid-section">
                {showHeaders && (
                  <h2 className="grid-section__title">
                    {groupName.get(sec.groupId)}
                    <span className="grid-section__count">{sec.ids.length}</span>
                  </h2>
                )}
                <div className="grid" style={{ "--tile": `${tileWidth}px` } as React.CSSProperties}>
                  {sec.ids.map((id) => (
                    <GridTile key={id} id={id} />
                  ))}
                </div>
              </section>
            ))
          )}
        </TileObserverContext.Provider>
      </div>
    </div>
  );
}

export const GridView = memo(GridViewInner);
