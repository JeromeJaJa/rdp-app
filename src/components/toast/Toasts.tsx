import { AlertCircle, CheckCircle2, Info, X } from "lucide-react";
import { dismissToast } from "../../state/actions";
import { useStore } from "../../state/store";
import "./toast.css";

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  if (!toasts.length) return null;
  return (
    <div className="toasts" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast toast--${t.tone}`}>
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
      ))}
    </div>
  );
}
