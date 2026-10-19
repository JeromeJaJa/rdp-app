import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import { closeDialog } from "../../state/actions";
import "./dialogs.css";
import "./extra.css";

interface DialogProps {
  title: ReactNode;
  headerExtra?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
  onClose?: () => void;
  className?: string;
  /** Children render their own `.dialog__body` / `.dialog__footer` (e.g. a <form>). */
  bare?: boolean;
}

export function Dialog({
  title,
  headerExtra,
  children,
  footer,
  width = 480,
  onClose = closeDialog,
  className,
  bare,
}: DialogProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const downOnBackdrop = useRef(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  useEffect(() => {
    // Focus the first field (or the panel) when opening.
    const panel = panelRef.current;
    if (!panel) return;
    const first =
      panel.querySelector<HTMLElement>("[data-autofocus]") ??
      panel.querySelector<HTMLElement>("input:not([type]), input[type=text], input[type=password], textarea");
    (first ?? panel).focus();
  }, []);

  return (
    <div
      className="dialog-backdrop"
      onMouseDown={(e) => (downOnBackdrop.current = e.target === e.currentTarget)}
      onMouseUp={(e) => {
        if (downOnBackdrop.current && e.target === e.currentTarget) onClose();
        downOnBackdrop.current = false;
      }}
    >
      <div
        ref={panelRef}
        className={`dialog ${className ?? ""}`}
        style={{ width }}
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
      >
        <div className="dialog__header">
          <h2 className="dialog__title">{title}</h2>
          {headerExtra}
          <button className="dialog__close" aria-label="Close" onClick={onClose}>
            <X size={15} />
          </button>
        </div>
        {bare ? (
          children
        ) : (
          <>
            <div className="dialog__body">{children}</div>
            {footer && <div className="dialog__footer">{footer}</div>}
          </>
        )}
      </div>
    </div>
  );
}
