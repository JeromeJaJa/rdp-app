// Controls that act on every connection, shown in the library sidebar next to the list
// they act on (and in the command palette).

import type { ReactNode } from "react";
import { Download, LayoutGrid, Monitor, Plus, Upload } from "lucide-react";
import { connectAll, disconnectAll, openDialog, setView } from "../../state/actions";
import { isBusy, isLive, useStore } from "../../state/store";
import { IconButton, Segmented } from "../ui/ui";
import { ConnectIcon, DisconnectIcon } from "./SessionIcons";

type TipSide = "top" | "bottom" | "left" | "right";

export function BulkSessionButtons({ tipSide }: { tipSide?: TipSide }) {
  const anyIdle = useStore((s) =>
    s.order.some((id) => {
      const st = s.sessions[id]?.status ?? "idle";
      return st === "idle" || st === "error";
    }),
  );
  const anyActive = useStore((s) =>
    s.order.some((id) => {
      const st = s.sessions[id]?.status ?? "idle";
      return isLive(st) || isBusy(st);
    }),
  );
  return (
    <>
      <IconButton label="Connect all" tipSide={tipSide} onClick={() => connectAll()} disabled={!anyIdle}>
        <ConnectIcon size={15} />
      </IconButton>
      <IconButton label="Disconnect all" tipSide={tipSide} onClick={() => disconnectAll()} disabled={!anyActive}>
        <DisconnectIcon size={15} />
      </IconButton>
    </>
  );
}

export function TransferButtons({ tipSide }: { tipSide?: TipSide }) {
  return (
    <>
      <IconButton label="Import connections" tipSide={tipSide} onClick={() => openDialog({ kind: "import" })}>
        <Download size={15} />
      </IconButton>
      <IconButton label="Export connections" tipSide={tipSide} onClick={() => openDialog({ kind: "export" })}>
        <Upload size={15} />
      </IconButton>
    </>
  );
}

/** "+ New"; the label collapses to a tooltip when space runs out (CSS hides the label). */
export function NewConnectionButton({ tipSide = "bottom" }: { tipSide?: TipSide }) {
  return (
    <button
      type="button"
      className="tb-btn tb-btn--accent"
      aria-label="New connection (Ctrl+N)"
      data-tip="New connection (Ctrl+N)"
      data-tip-side={tipSide}
      onClick={() => openDialog({ kind: "add", mode: "single" })}
    >
      <Plus size={15} />
      <span className="tb-btn__label" data-tip-label>
        New
      </span>
    </button>
  );
}

/** Session view vs grid. `labeled` adds text next to the icons (CSS may hide it when narrow). */
export function ViewToggle({ className, labeled }: { className?: string; labeled?: boolean }) {
  const view = useStore((s) => s.view);
  const label = (icon: ReactNode, text: string) =>
    labeled ? (
      <>
        {icon}
        <span className="view-toggle__text">{text}</span>
      </>
    ) : (
      icon
    );
  return (
    <Segmented
      size="sm"
      className={`tb-view ${className ?? ""}`}
      value={view}
      onChange={setView}
      options={[
        { value: "session", label: label(<Monitor size={14} />, "Session"), title: "Session view" },
        { value: "grid", label: label(<LayoutGrid size={14} />, "Grid"), title: "Grid view (Ctrl+G)" },
      ]}
    />
  );
}
