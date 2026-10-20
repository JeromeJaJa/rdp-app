import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  CornerDownLeft,
  LayoutGrid,
  ListPlus,
  Maximize2,
  Moon,
  PanelLeft,
  Plus,
  Search,
  Settings,
  SquareStack,
} from "lucide-react";
import type { Connection } from "../../lib/types";
import {
  connectAll,
  cycleTheme,
  disconnectAll,
  openDialog,
  select,
  setFullscreen,
  toggleGrid,
  updateSettings,
} from "../../state/actions";
import { displayName, getState, isLive, setState, useStore } from "../../state/store";
import { ConnectIcon, DisconnectIcon } from "../common/SessionIcons";
import { StatusDot } from "../common/StatusDot";
import "./palette.css";

const OFFSET_KEY = "rdp-app:palette-offset";

/** Where the user last dragged the palette, relative to its default spot. */
function loadOffset(): { x: number; y: number } {
  try {
    const v = JSON.parse(localStorage.getItem(OFFSET_KEY) ?? "null") as { x?: unknown; y?: unknown } | null;
    if (v && typeof v.x === "number" && typeof v.y === "number") return { x: v.x, y: v.y };
  } catch {
    /* ignore */
  }
  return { x: 0, y: 0 };
}

/**
 * Drag the palette by its search bar (like Spotlight). A press that doesn't move is an
 * ordinary click, so the text field still works; the position is remembered.
 */
function usePaletteDrag(panelRef: React.RefObject<HTMLDivElement | null>) {
  const [offset, setOffset] = useState(loadOffset);
  const [dragging, setDragging] = useState(false);

  // Keep it on screen (the window may have shrunk since).
  useLayoutEffect(() => {
    const el = panelRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    if (r.left < 0 || r.top < 0 || r.right > innerWidth || r.top + 60 > innerHeight) setOffset({ x: 0, y: 0 });
  }, [panelRef]);

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const el = panelRef.current;
    if (e.button !== 0 || !el) return;
    const bar = e.currentTarget;
    const start = { x: e.clientX, y: e.clientY };
    const base = offset;
    const rect = el.getBoundingClientRect();
    let moved = false;
    let last = base;
    const onMove = (ev: PointerEvent) => {
      const dx = ev.clientX - start.x;
      const dy = ev.clientY - start.y;
      if (!moved) {
        if (Math.hypot(dx, dy) < 4) return;
        moved = true;
        setDragging(true);
        // Take the pointer from the text field so it stops selecting text.
        bar.setPointerCapture(e.pointerId);
        const input = bar.querySelector("input");
        if (input) input.setSelectionRange(input.selectionEnd, input.selectionEnd);
      }
      ev.preventDefault();
      const clampedX = Math.min(Math.max(dx, 8 - rect.left), innerWidth - 8 - rect.right);
      const clampedY = Math.min(Math.max(dy, 8 - rect.top), innerHeight - 60 - rect.top);
      last = { x: base.x + clampedX, y: base.y + clampedY };
      setOffset(last);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
      if (!moved) return;
      setDragging(false);
      try {
        localStorage.setItem(OFFSET_KEY, JSON.stringify(last));
      } catch {
        /* ignore */
      }
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
  };

  return { offset, dragging, onPointerDown };
}

/** Subsequence match: higher is better, null when `query` doesn't match `text`. */
function score(query: string, text: string): number | null {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  const direct = t.indexOf(q);
  if (direct >= 0) return 100 - direct + (direct === 0 || /\W/.test(t[direct - 1] ?? "") ? 40 : 0);
  let ti = 0;
  let total = 0;
  let run = 0;
  for (const ch of q) {
    const found = t.indexOf(ch, ti);
    if (found < 0) return null;
    run = found === ti ? run + 1 : 0;
    total += 1 + run * 2 - Math.min(found - ti, 6) * 0.5;
    ti = found + 1;
  }
  return total;
}

interface Item {
  key: string;
  section: "Connections" | "Actions";
  label: string;
  detail?: string;
  icon: ReactNode;
  hint?: string;
  run: () => void;
}

function useActions(): Item[] {
  const view = useStore((s) => s.view);
  const placement = useStore((s) => s.settings.tabPlacement);
  return useMemo(() => {
    const a = (key: string, label: string, icon: ReactNode, run: () => void, hint?: string): Item => ({
      key: `a:${key}`,
      section: "Actions",
      label,
      icon,
      hint,
      run,
    });
    return [
      a("new", "New connection", <Plus size={15} />, () => openDialog({ kind: "add", mode: "single" }), "Ctrl N"),
      a("bulk", "Add many connections", <ListPlus size={15} />, () => openDialog({ kind: "add", mode: "bulk" }), "Ctrl ⇧ N"),
      a("connect-all", "Connect all", <ConnectIcon size={14} />, () => connectAll()),
      a("disconnect-all", "Disconnect all", <DisconnectIcon size={14} />, () => disconnectAll()),
      a("grid", view === "grid" ? "Session view" : "Grid view", view === "grid" ? <SquareStack size={15} /> : <LayoutGrid size={15} />, toggleGrid, "Ctrl G"),
      a("fullscreen", "Fullscreen", <Maximize2 size={15} />, () => setFullscreen(true)),
      a("theme", "Toggle light / dark", <Moon size={15} />, cycleTheme),
      a(
        "tabs",
        placement === "left" ? "Tabs on top" : "Tabs on the left",
        <PanelLeft size={15} />,
        () => updateSettings({ tabPlacement: placement === "left" ? "top" : "left" }),
      ),
      a("import", "Import connections", <ArrowDownToLine size={15} />, () => openDialog({ kind: "import" })),
      a("export", "Export connections", <ArrowUpFromLine size={15} />, () => openDialog({ kind: "export" })),
      a("settings", "Settings", <Settings size={15} />, () => openDialog({ kind: "settings" }), "Ctrl ,"),
    ];
  }, [view, placement]);
}

export function CommandPalette() {
  const open = useStore((s) => s.paletteOpen);
  if (!open) return null;
  return <Palette />;
}

function Palette() {
  const order = useStore((s) => s.order);
  const connById = useStore((s) => s.connById);
  const groups = useStore((s) => s.groups);
  const sessions = useStore((s) => s.sessions);
  const actions = useActions();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const drag = usePaletteDrag(panelRef);
  const close = () => setState({ paletteOpen: false });

  const items = useMemo(() => {
    const groupById = new Map(groups.map((g) => [g.id, g]));
    const q = query.trim();
    const conns: { item: Item; score: number }[] = [];
    for (const id of order) {
      const c: Connection | undefined = connById[id];
      if (!c) continue;
      const group = groupById.get(c.groupId);
      const live = isLive(sessions[id]?.status ?? "idle");
      const name = displayName(c);
      const best = Math.max(
        score(q, name) ?? -Infinity,
        (score(q, c.host) ?? -Infinity) - 5,
        (score(q, group && !group.builtin ? group.name : "") ?? -Infinity) - 15,
      );
      if (best === -Infinity) continue;
      conns.push({
        score: best + (live ? 3 : 0),
        item: {
          key: `c:${id}`,
          section: "Connections",
          label: name,
          detail: [c.host !== name ? c.host : "", group && !group.builtin ? group.name : ""].filter(Boolean).join("  ·  "),
          icon: <StatusDot status={sessions[id]?.status ?? "idle"} />,
          hint: live ? "Open" : "Connect",
          run: () => select(id, { connect: true }),
        },
      });
    }
    // Without a query: live sessions first, then the list order.
    conns.sort((x, y) => y.score - x.score);
    const acts = actions
      .map((item) => ({ item, score: score(q, item.label) }))
      .filter((x): x is { item: Item; score: number } => x.score !== null)
      .sort((x, y) => y.score - x.score);
    return [...conns.slice(0, q ? 30 : 8).map((x) => x.item), ...acts.slice(0, q ? 6 : 20).map((x) => x.item)];
  }, [query, order, connById, groups, sessions, actions]);

  const activeIndex = Math.min(active, Math.max(0, items.length - 1));
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-index="${activeIndex}"]`)?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  const run = (item: Item | undefined) => {
    if (!item) return;
    close();
    item.run();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp" || (e.key === "Tab" && items.length)) {
      e.preventDefault();
      const step = e.key === "ArrowUp" || (e.key === "Tab" && e.shiftKey) ? -1 : 1;
      setActive((activeIndex + step + items.length) % items.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(items[activeIndex]);
    }
  };

  let lastSection: string | null = null;
  return (
    <div className="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div
        ref={panelRef}
        className={`palette glass ${drag.dragging ? "is-dragging" : ""}`}
        role="dialog"
        aria-label="Command palette"
        style={{ translate: `${drag.offset.x}px ${drag.offset.y}px` }}
        onKeyDown={onKeyDown}
      >
        <div className="palette__search" onPointerDown={drag.onPointerDown}>
          <Search size={16} className="palette__search-icon" />
          <input
            autoFocus
            value={query}
            spellCheck={false}
            placeholder="Jump to a connection or run a command…"
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(0);
            }}
          />
          <kbd className="palette__esc">Esc</kbd>
        </div>
        <div ref={listRef} className="palette__list" role="listbox">
          {items.map((item, i) => {
            const header = item.section !== lastSection ? item.section : null;
            lastSection = item.section;
            return (
              <div key={item.key} role="presentation">
                {header && <div className="palette__section">{header}</div>}
                <button
                  type="button"
                  role="option"
                  aria-selected={i === activeIndex}
                  data-index={i}
                  className={`palette__item ${i === activeIndex ? "is-active" : ""}`}
                  onMouseMove={() => i !== activeIndex && setActive(i)}
                  onClick={() => run(item)}
                >
                  <span className="palette__icon">{item.icon}</span>
                  <span className="palette__label">{item.label}</span>
                  {item.detail && <span className="palette__detail mono">{item.detail}</span>}
                  <span className="palette__hint">
                    {i === activeIndex && item.section === "Connections" ? (
                      <>
                        {item.hint} <CornerDownLeft size={12} />
                      </>
                    ) : (
                      item.hint && item.section === "Actions" && <kbd>{item.hint}</kbd>
                    )}
                  </span>
                </button>
              </div>
            );
          })}
          {items.length === 0 && <div className="palette__empty">Nothing matches “{query}”</div>}
        </div>
      </div>
    </div>
  );
}

/** Opens the palette (Ctrl+K, or the title bar's search button). */
export function openPalette() {
  if (!getState().dialog) setState({ paletteOpen: true, menu: null });
}
