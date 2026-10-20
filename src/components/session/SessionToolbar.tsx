import { useEffect, useRef, useState } from "react";
import { Keyboard, Maximize2, Minimize2, RotateCw, Volume2, VolumeX, X } from "lucide-react";
import { DisconnectIcon } from "../common/SessionIcons";
import { engine } from "../../engine";
import { disconnect, reconnect, setFullscreen, toggleConnectionMute } from "../../state/actions";
import { useSession, useStore } from "../../state/store";

const WIN = 0xe05b;
const CTRL = 0x1d;
const SHIFT = 0x2a;
const ALT = 0x38;

/** How long the pointer must rest at the top edge before the toolbar appears. */
const DWELL_MS = 400;
/** Grace period before hiding once the pointer has left. */
const HIDE_MS = 250;
/**
 * The trigger strip along the top edge: a few pixels of the session plus the bottom of
 * the title bar above it, so overshooting the edge while moving up still counts.
 */
const TRIGGER_TOP = -10;
const TRIGGER_PX = 20;
/** Height of the area the toolbar lives in; a dismissal lasts while the pointer stays inside it. */
const AREA_PX = 64;

/** Combinations the local OS would otherwise intercept. */
const COMBOS: { label: string; keys: string; codes: number[] }[] = [
  { label: "Start menu", keys: "Win", codes: [WIN] },
  { label: "Run", keys: "Win+R", codes: [WIN, 0x13] },
  { label: "File Explorer", keys: "Win+E", codes: [WIN, 0x12] },
  { label: "Show desktop", keys: "Win+D", codes: [WIN, 0x20] },
  { label: "Switch window", keys: "Alt+Tab", codes: [ALT, 0x0f] },
  { label: "Close window", keys: "Alt+F4", codes: [ALT, 0x3e] },
  { label: "Task Manager", keys: "Ctrl+Shift+Esc", codes: [CTRL, SHIFT, 0x01] },
  { label: "Lock", keys: "Win+L", codes: [WIN, 0x26] },
  { label: "Screenshot", keys: "PrtSc", codes: [0xe037] },
];

export function SessionToolbar({ id }: { id: string }) {
  const { codec, width, height } = useSession(id);
  const fullscreen = useStore((s) => s.fullscreen);
  const muted = useStore((s) => (s.connById[id]?.audio ?? s.settings.audio) === "off");
  const [keysOpen, setKeysOpen] = useState(false);
  const [visible, setVisible] = useState(false);
  const zoneRef = useRef<HTMLDivElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const visibleRef = useRef(false);
  const keysOpenRef = useRef(false);
  const dismissedRef = useRef(false);
  keysOpenRef.current = keysOpen;

  const show = (on: boolean) => {
    visibleRef.current = on;
    setVisible(on);
  };
  const dismiss = () => {
    dismissedRef.current = true;
    setKeysOpen(false);
    show(false);
  };

  // Revealed by resting the pointer on the top edge, not by any hover over the area, and
  // without an invisible element in front of the remote screen that would swallow clicks.
  useEffect(() => {
    let showTimer: number | undefined;
    let hideTimer: number | undefined;
    const clear = (t: number | undefined) => t !== undefined && window.clearTimeout(t);

    const onMove = (e: PointerEvent) => {
      const session = zoneRef.current?.parentElement;
      if (!session) return;
      const r = session.getBoundingClientRect();
      const band = Math.min(560, r.width * 0.7);
      const x = e.clientX - r.left;
      const y = e.clientY - r.top;
      const centered = Math.abs(x - r.width / 2) <= band / 2;
      const inArea = centered && y >= TRIGGER_TOP && y < AREA_PX;
      const onTrigger = inArea && y < TRIGGER_PX && e.buttons === 0;
      const bar = barRef.current?.getBoundingClientRect();
      const overBar =
        !!bar && e.clientX >= bar.left - 12 && e.clientX <= bar.right + 12 && e.clientY <= bar.bottom + 12;

      if (dismissedRef.current) {
        if (!inArea && !overBar) dismissedRef.current = false;
        clear(showTimer);
        showTimer = undefined;
        return;
      }
      if (visibleRef.current) {
        if (overBar || onTrigger || keysOpenRef.current) {
          clear(hideTimer);
          hideTimer = undefined;
        } else if (hideTimer === undefined) {
          hideTimer = window.setTimeout(() => {
            hideTimer = undefined;
            if (!keysOpenRef.current) show(false);
          }, HIDE_MS);
        }
        return;
      }
      if (onTrigger) {
        if (showTimer === undefined) {
          showTimer = window.setTimeout(() => {
            showTimer = undefined;
            show(true);
          }, DWELL_MS);
        }
      } else {
        clear(showTimer);
        showTimer = undefined;
      }
    };
    const onLeaveWindow = (e: PointerEvent) => {
      if (e.relatedTarget === null) {
        clear(showTimer);
        showTimer = undefined;
      }
    };
    window.addEventListener("pointermove", onMove, { capture: true, passive: true });
    document.addEventListener("pointerout", onLeaveWindow, true);
    return () => {
      window.removeEventListener("pointermove", onMove, { capture: true });
      document.removeEventListener("pointerout", onLeaveWindow, true);
      clear(showTimer);
      clear(hideTimer);
    };
  }, []);

  useEffect(() => {
    if (!keysOpen) return;
    const close = (e: PointerEvent) => {
      if (!zoneRef.current?.contains(e.target as Node)) setKeysOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setKeysOpen(false);
    window.addEventListener("pointerdown", close, true);
    window.addEventListener("keydown", esc);
    return () => {
      window.removeEventListener("pointerdown", close, true);
      window.removeEventListener("keydown", esc);
    };
  }, [keysOpen]);

  return (
    <div ref={zoneRef} className={`session-toolbar-zone ${keysOpen ? "is-open" : ""}`}>
      <div
        ref={barRef}
        className={`session-toolbar glass glass--dark ${visible || keysOpen ? "is-visible" : ""}`}
        onMouseDown={(e) => e.preventDefault()}
      >
        {(codec || width) && (
          <span className="session-toolbar__info mono">
            {width && height ? `${width}×${height}` : ""}
            {codec && width ? " · " : ""}
            {codec}
          </span>
        )}
        <span className="session-toolbar__sep" />
        <button
          className="session-toolbar__btn session-toolbar__btn--text"
          data-tip="Send Ctrl+Alt+Del"
          data-tip-side="bottom"
          onClick={() => engine.sendCtrlAltDel(id)}
        >
          Ctrl+Alt+Del
        </button>
        <button
          className={`session-toolbar__btn ${keysOpen ? "is-active" : ""}`}
          aria-label="Send keys"
          aria-expanded={keysOpen}
          data-tip={keysOpen ? undefined : "Send keys"}
          data-tip-side="bottom"
          onClick={() => setKeysOpen((o) => !o)}
        >
          <Keyboard size={14} />
        </button>
        <button
          className={`session-toolbar__btn ${muted ? "is-muted" : ""}`}
          aria-label={muted ? "Unmute" : "Mute"}
          aria-pressed={muted}
          data-tip={muted ? "Unmute" : "Mute"}
          data-tip-side="bottom"
          onClick={() => toggleConnectionMute(id)}
        >
          {muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
        <button
          className="session-toolbar__btn"
          aria-label="Reconnect"
          data-tip="Reconnect"
          data-tip-side="bottom"
          onClick={() => reconnect(id)}
        >
          <RotateCw size={14} />
        </button>
        <button
          className="session-toolbar__btn"
          aria-label={fullscreen ? "Exit fullscreen" : "Fullscreen"}
          data-tip={fullscreen ? "Exit fullscreen" : "Fullscreen"}
          data-tip-side="bottom"
          onClick={() => setFullscreen(!fullscreen)}
        >
          {fullscreen ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
        </button>
        <button
          className="session-toolbar__btn session-toolbar__btn--danger"
          aria-label="Disconnect"
          data-tip="Disconnect"
          data-tip-side="bottom"
          onClick={() => disconnect([id])}
        >
          <DisconnectIcon size={14} />
        </button>
        <span className="session-toolbar__sep" />
        <button
          className="session-toolbar__btn session-toolbar__btn--close"
          aria-label="Hide toolbar"
          data-tip="Hide until you move away"
          data-tip-side="bottom"
          onClick={dismiss}
        >
          <X size={13} />
        </button>
      </div>

      {keysOpen && (
        <div className="session-keys glass glass--dark" role="menu" onMouseDown={(e) => e.preventDefault()}>
          {COMBOS.map((c) => (
            <button
              key={c.keys}
              role="menuitem"
              className="session-keys__item"
              onClick={() => {
                engine.sendKeys(id, c.codes);
                setKeysOpen(false);
              }}
            >
              <span>{c.label}</span>
              <kbd className="session-keys__kbd mono">{c.keys}</kbd>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
