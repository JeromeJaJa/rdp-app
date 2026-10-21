import { useRef } from "react";
import { AlertCircle, CheckCircle2, Info, X } from "lucide-react";
import { dismissToast } from "../../state/actions";
import { useStore, type Toast } from "../../state/store";
import { useGlassUnderlay } from "../ui/useGlassUnderlay";
import { isMainWindow } from "../../lib/env";
import { TransfersPanel } from "../transfers/TransfersPanel";
import "./toast.css";

function ToastItem({ t }: { t: Toast }) {
  const ref = useRef<HTMLDivElement>(null);
  useGlassUnderlay(ref);
  return (
    <div ref={ref} className={`toast glass toast--${t.tone}`}>
      <span className="toast__icon">
        {t.tone === "error" ? <AlertCircle size={15} /> : t.tone === "success" ? <CheckCircle2 size={15} /> : <Info size={15} />}
      </span>
      <div className="toast__body">
        <div className="toast__title">{t.title}</div>
        {t.message && <div className="toast__message">{t.message}</div>}
      </div>
      {t.action && (
        <button
          className="toast__action"
          onClick={() => {
            t.action?.run();
            dismissToast(t.id);
          }}
        >
          {t.action.label}
        </button>
      )}
      <button className="toast__close" aria-label="Dismiss" onClick={() => dismissToast(t.id)}>
        <X size={13} />
      </button>
    </div>
  );
}

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  // Files being sent show in the main window, under its toasts.
  const transfers = useStore((s) => isMainWindow() && Object.keys(s.transfers).length > 0);
  if (!toasts.length && !transfers) return null;
  return (
    <div className="toasts" aria-live="polite">
      {toasts.map((t) => (
        <ToastItem key={t.id} t={t} />
      ))}
      {transfers && <TransfersPanel />}
    </div>
  );
}
