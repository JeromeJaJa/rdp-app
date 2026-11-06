import { useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ArrowUpCircle } from "lucide-react";
import { openDialog } from "../../state/actions";
import { useStore } from "../../state/store";
import { Markdown } from "../common/Markdown";
import { Spinner } from "../ui/ui";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";

/** Hover this long (ms) before the notes show. */
const DELAY = 260;
const GAP = 8;
const EDGE = 8;

/** A newer version is out, or being installed (main window). Hovering shows what changed in
 *  it; clicking opens What's new, to update. */
export function UpdatePill() {
  const status = useStore((s) => s.update.status);
  const progress = useStore((s) => s.update.progress ?? 0);
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const timer = useRef(0);
  if (status !== "available" && status !== "downloading" && status !== "installing") return null;
  const label =
    status === "available" ? "Update" : status === "downloading" ? `${Math.round(progress * 100)}%` : "Installing";
  const hide = () => {
    window.clearTimeout(timer.current);
    setAnchor(null);
  };
  return (
    <>
      <button
        className={`tb-update ${status === "available" ? "is-ready" : ""}`}
        aria-label="What's new in the update"
        onPointerEnter={(e) => {
          const el = e.currentTarget;
          window.clearTimeout(timer.current);
          if (e.buttons !== 0) return;
          timer.current = window.setTimeout(() => setAnchor(el.getBoundingClientRect()), DELAY);
        }}
        onPointerLeave={hide}
        onClick={() => {
          hide();
          openDialog({ kind: "changelog" });
        }}
      >
        <ArrowUpCircle size={13} />
        <span>{label}</span>
      </button>
      {anchor && createPortal(<UpdateCard anchor={anchor} />, document.body)}
    </>
  );
}

/** The update's version, where it stands, and its notes, under the pill. */
function UpdateCard({ anchor }: { anchor: DOMRect }) {
  const update = useStore((s) => s.update);
  const ref = useRef<HTMLDivElement>(null);
  const notesRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [clipped, setClipped] = useState(false);
  useGlassUnderlay(ref);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    // Under the pill, its right edge lined up with the pill's, kept in the window.
    const left = Math.min(Math.max(anchor.right - w, EDGE), window.innerWidth - EDGE - w);
    setPos({ left, top: anchor.bottom + GAP });
    const notes = notesRef.current;
    setClipped(!!notes && notes.scrollHeight > notes.clientHeight + 1);
  }, [anchor, update.notes]);

  const state =
    update.status === "downloading" ? (
      <>
        <Spinner size={10} /> Downloading {Math.round((update.progress ?? 0) * 100)}%
      </>
    ) : update.status === "installing" ? (
      <>
        <Spinner size={10} /> Installing
      </>
    ) : (
      "Available"
    );
  return (
    <div ref={ref} className="ucard glass" aria-hidden style={pos ?? { visibility: "hidden" }}>
      <div className="ucard__head">
        <span className="ucard__title">Version {update.version}</span>
        <span className="ucard__state">{state}</span>
      </div>
      <div ref={notesRef} className={`ucard__notes ${clipped ? "is-clipped" : ""}`}>
        <Markdown text={update.notes?.trim() || "No notes for this release."} />
      </div>
      <div className="ucard__foot">
        {clipped ? "Click for everything that changed" : "Click to see what's new"}
        {update.status === "available" && " and update"}
      </div>
    </div>
  );
}
