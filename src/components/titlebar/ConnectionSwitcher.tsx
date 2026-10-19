import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { Check, ChevronDown, Search } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import type { Connection } from "../../lib/types";
import { DEFAULT_GROUP_ID } from "../../lib/types";
import { select } from "../../state/actions";
import { displayName, useStore } from "../../state/store";
import { StatusDot } from "../common/StatusDot";

/** Show a filter box once a group has more than this many connections. */
const SEARCH_FROM = 9;

function hostLabel(c: Connection) {
  return c.port !== 3389 ? `${c.host}:${c.port}` : c.host;
}

/** Ids of the connections in `groupId`, in list order (unknown groups fall back to All). */
function useGroupMembers(groupId: string) {
  return useStore(
    useShallow((s) => {
      const known = new Set(s.groups.map((g) => g.id));
      return s.order.filter((id) => {
        const c = s.connById[id];
        return !!c && (known.has(c.groupId) ? c.groupId : DEFAULT_GROUP_ID) === groupId;
      });
    }),
  );
}

/** The current connection's name and host; opens a list of the others in its group. */
export function ConnectionSwitcher({ sel }: { sel: Connection }) {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const members = useGroupMembers(sel.groupId);
  const canSwitch = members.length > 1;

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className={`tb-switch ${open ? "is-open" : ""}`}
        disabled={!canSwitch}
        aria-haspopup="menu"
        aria-expanded={open}
        title={canSwitch ? "Switch connection" : undefined}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="tb-title__name">{displayName(sel)}</span>
        <span className="tb-title__meta mono">{hostLabel(sel)}</span>
        {canSwitch && <ChevronDown size={13} className="tb-switch__chevron" />}
      </button>
      {open && (
        <SwitcherPanel anchor={buttonRef.current} currentId={sel.id} members={members} onClose={() => setOpen(false)} />
      )}
    </>
  );
}

function SwitcherPanel({
  anchor,
  currentId,
  members,
  onClose,
}: {
  anchor: HTMLElement | null;
  currentId: string;
  members: string[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const connById = useStore((s) => s.connById);
  const sessions = useStore((s) => s.sessions);
  const [query, setQuery] = useState("");
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  const items = useMemo(() => {
    const q = query.trim().toLowerCase();
    return members
      .map((id) => connById[id])
      .filter((c): c is Connection => !!c)
      .filter((c) => !q || displayName(c).toLowerCase().includes(q) || c.host.toLowerCase().includes(q));
  }, [members, connById, query]);
  const [active, setActive] = useState(() => Math.max(0, members.indexOf(currentId)));
  const activeIndex = Math.min(active, Math.max(0, items.length - 1));

  useLayoutEffect(() => {
    if (!anchor || !ref.current) return;
    const a = anchor.getBoundingClientRect();
    const w = ref.current.offsetWidth;
    setPos({ left: Math.max(8, Math.min(a.left - 6, window.innerWidth - w - 8)), top: a.bottom + 6 });
  }, [anchor]);

  // Keep the highlighted row in view while moving with the keyboard.
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-index="${activeIndex}"]`)?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!ref.current?.contains(t) && !anchor?.contains(t)) onClose();
    };
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [anchor, onClose]);

  const choose = (id: string) => {
    onClose();
    if (id !== currentId) select(id);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      anchor?.focus();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      setActive((activeIndex + step + items.length) % Math.max(1, items.length));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const c = items[activeIndex];
      if (c) choose(c.id);
    }
  };

  const searchable = members.length >= SEARCH_FROM;
  // Focus the panel (or its search box) once it is positioned, so arrow keys work right away.
  const placed = pos !== null;
  useEffect(() => {
    if (!placed) return;
    const target = searchable ? ref.current?.querySelector("input") : ref.current;
    target?.focus({ preventScroll: true });
  }, [placed, searchable]);

  return (
    <div
      ref={ref}
      className="switcher"
      role="menu"
      tabIndex={-1}
      style={pos ? { left: pos.left, top: pos.top } : { visibility: "hidden" }}
      onKeyDown={onKeyDown}
    >
      {searchable && (
        <div className="switcher__search">
          <Search size={13} />
          <input
            value={query}
            placeholder="Filter connections"
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(0);
            }}
          />
        </div>
      )}
      <div ref={listRef} className="switcher__list">
        {items.map((c, i) => {
          const status = sessions[c.id]?.status ?? "idle";
          return (
            <button
              key={c.id}
              type="button"
              role="menuitem"
              data-index={i}
              className={`switcher__item ${i === activeIndex ? "is-active" : ""} ${c.id === currentId ? "is-current" : ""}`}
              onMouseEnter={() => setActive(i)}
              onClick={() => choose(c.id)}
            >
              <StatusDot status={status} />
              <span className="switcher__name">{displayName(c)}</span>
              <span className="switcher__host mono">{hostLabel(c)}</span>
              <span className="switcher__check">{c.id === currentId && <Check size={13} />}</span>
            </button>
          );
        })}
        {items.length === 0 && <div className="switcher__empty">No matches</div>}
      </div>
    </div>
  );
}
