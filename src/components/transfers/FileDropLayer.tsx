import { useEffect, useRef, useState } from "react";
import { BorderBeam } from "border-beam";
import type { ConnectionId } from "../../lib/types";
import { be, groupMembers, openSendDialog } from "../../state/actions";
import { displayName, getState, isLive, setState, useStore } from "../../state/store";
import "./transfers.css";

/** Where dragged-in files would go. */
type Target =
  | { kind: "send"; ids: ConnectionId[]; label: string; el: Element }
  /** The session on screen, whose companion takes the drag: a real drag on the remote. */
  | { kind: "remote"; id: ConnectionId; label: string; el: Element; canvas: HTMLCanvasElement }
  /** The open Send dialog: they join its list. */
  | { kind: "dialog"; el: Element };

function nameOf(id: ConnectionId): string {
  const c = getState().connById[id];
  return c ? displayName(c) : id;
}

/** What's under the pointer that can take files (tabs, sidebar rows, tiles, group headers,
 *  the session on screen, an open Send dialog). */
function targetAt(x: number, y: number): Target | null {
  const el = document.elementFromPoint(x, y);
  if (!el) return null;
  const s = getState();
  if (s.dialog) {
    const dialog = el.closest(".dialog");
    return s.dialog.kind === "send" && dialog ? { kind: "dialog", el: dialog } : null;
  }
  if (s.menu || s.paletteOpen) return null;
  const tab = el.closest("[data-tab-id]");
  const tabId = tab?.getAttribute("data-tab-id");
  if (tab && tabId) return { kind: "send", ids: [tabId], label: nameOf(tabId), el: tab };
  const tile = el.closest(".tile[data-id]");
  const tileId = tile?.getAttribute("data-id");
  if (tile && tileId) return { kind: "send", ids: [tileId], label: nameOf(tileId), el: tile };
  const group = el.closest("[data-group-id]");
  const groupId = group?.getAttribute("data-group-id");
  if (group && groupId) {
    const ids = groupMembers(groupId);
    const name = s.groups.find((g) => g.id === groupId)?.name ?? "group";
    if (ids.length) return { kind: "send", ids, label: `${name} (${ids.length})`, el: group };
  }
  const id = s.selectedId;
  const live = el.closest(".live-session");
  const canvas = live?.querySelector<HTMLCanvasElement>(".rs-surface__canvas");
  const session = id ? s.sessions[id] : undefined;
  if (live && canvas && id && session?.companion && isLive(session.status)) {
    return { kind: "remote", id, label: nameOf(id), el: live, canvas };
  }
  const idle = el.closest(".live-session, .idle");
  if (idle && id) return { kind: "send", ids: [id], label: nameOf(id), el: idle };
  return null;
}

/** A point in the window to remote desktop pixels, as the session's own input does. */
function toRemote(target: Extract<Target, { kind: "remote" }>, x: number, y: number): [number, number] | null {
  const session = getState().sessions[target.id];
  const r = target.canvas.getBoundingClientRect();
  if (!session?.width || !session.height || r.width === 0 || r.height === 0) return null;
  const rx = Math.floor(((x - r.left) / r.width) * session.width);
  const ry = Math.floor(((y - r.top) / r.height) * session.height);
  return [Math.min(session.width - 1, Math.max(0, rx)), Math.min(session.height - 1, Math.max(0, ry))];
}

function count(paths: string[]): string {
  return paths.length === 1 ? (paths[0].split(/[\\/]/).pop() ?? paths[0]) : `${paths.length} items`;
}

/** A drag onto a session in progress. */
interface Remote {
  id: ConnectionId;
  /** The remote drag is under way (the companion took it and the button is down). */
  ready: boolean;
  /** It couldn't start: drops there open Send files instead. */
  failed: boolean;
  /** What a drop where the pointer is would do there (DROPEFFECT bits). */
  effect: number;
  lastMove: number;
}

interface Shown {
  target: Target;
  rect: DOMRect;
  radius: string;
  x: number;
  y: number;
  /** Over a session: whether the remote would take a drop where the pointer is. */
  remoteTakes?: boolean;
}

/**
 * Files dragged in from Explorer: a beam traces whatever they'd land on and a label says what
 * the drop would do. Over a session with a companion, it's a real drag on the remote (its
 * folders and apps react as usual); elsewhere, dropping opens Send files, filled in.
 */
export function FileDropLayer() {
  const [shown, setShown] = useState<Shown | null>(null);
  const paths = useRef<string[]>([]);
  const remote = useRef<Remote | null>(null);
  const want = useRef(false);
  const dark = useStore((s) => s.settings.theme !== "light");

  useEffect(() => {
    const backend = be();
    let last = { x: 0, y: 0 };

    const effect = (copy: boolean) => {
      if (want.current !== copy) {
        want.current = copy;
        backend.setDropEffect(copy);
      }
    };
    const endRemote = () => {
      const r = remote.current;
      if (r) backend.remoteDragCancel(r.id);
      remote.current = null;
    };

    const show = (x: number, y: number) => {
      last = { x, y };
      const target = targetAt(x, y);
      if (remote.current && (target?.kind !== "remote" || target.id !== remote.current.id)) endRemote();
      if (!target) {
        effect(false);
        return setShown(null);
      }
      let remoteTakes: boolean | undefined;
      if (target.kind === "remote") {
        const point = toRemote(target, x, y);
        let r = remote.current;
        if (!r && point) {
          r = remote.current = { id: target.id, ready: false, failed: false, effect: 0, lastMove: 0 };
          const started = r;
          backend
            .remoteDragStart(target.id, paths.current, point[0], point[1])
            .then(() => {
              started.ready = true;
            })
            .catch(() => {
              started.failed = true;
              if (remote.current === started) show(last.x, last.y);
            });
        } else if (r?.ready && point) {
          const now = performance.now();
          if (now - r.lastMove >= 16) {
            r.lastMove = now;
            backend.remoteDragMove(target.id, point[0], point[1]);
          }
        }
        remoteTakes = !!r && (r.failed || (r.ready && r.effect !== 0));
        effect(remoteTakes);
      } else {
        effect(true);
      }
      setShown({
        target,
        rect: target.el.getBoundingClientRect(),
        radius: getComputedStyle(target.el).borderRadius || "8px",
        x,
        y,
        remoteTakes,
      });
    };

    const clear = () => {
      endRemote();
      want.current = false;
      setShown(null);
    };

    const offRemote = backend.onRemoteDrag((drag) => {
      const r = remote.current;
      if (!r || drag.session !== r.id) return;
      if (drag.ended) return;
      r.effect = drag.effect;
      show(last.x, last.y);
    });

    const offDrag = backend.onFileDrag((drag) => {
      if (drag.phase === "enter") {
        paths.current = drag.paths ?? [];
        show(drag.x, drag.y);
      } else if (drag.phase === "over") {
        show(drag.x, drag.y);
      } else if (drag.phase === "leave") {
        clear();
      } else {
        const target = targetAt(drag.x, drag.y);
        const dropped = drag.paths ?? paths.current;
        const r = remote.current;
        if (target?.kind === "remote" && r && r.id === target.id && r.ready && !r.failed) {
          // The remote drop target takes it from here (and reads the files as it copies).
          const point = toRemote(target, drag.x, drag.y);
          if (point) backend.remoteDragDrop(target.id, point[0], point[1]);
          remote.current = null;
          want.current = false;
          setShown(null);
          return;
        }
        clear();
        if (!target || !dropped.length) return;
        if (target.kind === "dialog") {
          setState((s) => ({ droppedPaths: { paths: dropped, seq: (s.droppedPaths?.seq ?? 0) + 1 } }));
        } else if (target.kind === "remote") {
          openSendDialog([target.id], dropped);
        } else {
          openSendDialog(target.ids, dropped);
        }
      }
    });
    return () => {
      offRemote();
      offDrag();
    };
  }, []);

  if (!shown) return null;
  const { target, rect, radius, x, y, remoteTakes } = shown;
  const what = count(paths.current);
  const label =
    target.kind === "dialog"
      ? `Add ${what}`
      : target.kind === "remote"
        ? remoteTakes
          ? `Copy ${what} here`
          : null
        : `Send ${what} to ${target.label}`;
  return (
    <>
      <div
        className="drop-frame"
        style={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}
        aria-hidden
      >
        <BorderBeam size={rect.height < 48 ? "sm" : "md"} colorVariant="colorful" theme={dark ? "dark" : "light"}>
          <div
            className={`drop-frame__box ${target.kind === "remote" ? "is-remote" : ""}`}
            style={{ width: rect.width, height: rect.height, borderRadius: radius }}
          />
        </BorderBeam>
      </div>
      {label && (
        <div className="drop-label glass" style={{ left: x + 16, top: y + 18 }}>
          {label}
        </div>
      )}
    </>
  );
}
