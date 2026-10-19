import { useEffect, useRef, useState } from "react";
import { Keyboard, Maximize2, Minimize2, RotateCw } from "lucide-react";
import { DisconnectIcon } from "../common/SessionIcons";
import { engine } from "../../engine";
import { disconnect, reconnect, setFullscreen } from "../../state/actions";
import { useSession, useStore } from "../../state/store";

const WIN = 0xe05b;
const CTRL = 0x1d;
const SHIFT = 0x2a;
const ALT = 0x38;

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
  const [keysOpen, setKeysOpen] = useState(false);
  const zoneRef = useRef<HTMLDivElement>(null);

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
      <div className="session-toolbar" onMouseDown={(e) => e.preventDefault()}>
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
      </div>

      {keysOpen && (
        <div className="session-keys" role="menu" onMouseDown={(e) => e.preventDefault()}>
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
