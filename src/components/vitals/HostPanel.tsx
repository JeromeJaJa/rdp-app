import { memo, useState } from "react";
import { ChevronDown } from "lucide-react";
import { displayName, useStore } from "../../state/store";
import { ago } from "./format";
import { HostTiles, useNow } from "./Vitals";
import "./vitals.css";

const KEY = "rdp-app:host-panel";

/** "12s ago", on its own so the ticking doesn't re-render the tiles. */
function Age({ at }: { at: number }) {
  const now = useNow();
  return <span className="hostpanel__age">{ago(at, now)}</span>;
}

/**
 * The bottom of the sidebar: the focused connection's load in more detail (four stat tiles
 * with trend lines, and free space). Collapsible; shows only while that host reports.
 */
function HostPanelInner() {
  const id = useStore((s) => (s.selectedId && s.stats[s.selectedId] ? s.selectedId : null));
  const name = useStore((s) => (id && s.connById[id] ? displayName(s.connById[id]) : ""));
  const at = useStore((s) => (id ? s.stats[id]?.latest.at : undefined));
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(KEY) !== "closed";
    } catch {
      return true;
    }
  });
  if (!id) return null;
  const toggle = () => {
    setOpen((o) => {
      try {
        localStorage.setItem(KEY, o ? "closed" : "open");
      } catch {}
      return !o;
    });
  };
  return (
    <section className={`hostpanel ${open ? "is-open" : ""}`} aria-label={`${name} load`}>
      <button type="button" className="hostpanel__head" onClick={toggle} aria-expanded={open}>
        <span className="hostpanel__title">{name}</span>
        {at !== undefined && <Age at={at} />}
        <ChevronDown size={13} className="hostpanel__chevron" />
      </button>
      {open && <HostTiles id={id} />}
    </section>
  );
}

export const HostPanel = memo(HostPanelInner);
