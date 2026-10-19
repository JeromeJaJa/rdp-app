import { ListPlus, Plus } from "lucide-react";
import { openDialog } from "../../state/actions";
import { Button, Kbd } from "../ui/ui";
import { BrandMark } from "../titlebar/Titlebar";

export function EmptyState() {
  return (
    <div className="empty">
      <div className="empty__inner">
        <div className="empty__mark">
          <BrandMark size={28} />
        </div>
        <h1 className="empty__title">No connections yet</h1>
        <p className="empty__text">
          Add a single host, or paste a whole list — one <span className="mono">host:user:pass</span> per line.
          Nothing connects until you ask it to.
        </p>
        <div className="empty__actions">
          <Button variant="primary" size="lg" icon={<Plus size={15} />} onClick={() => openDialog({ kind: "add", mode: "single" })}>
            Add connection
          </Button>
          <Button size="lg" icon={<ListPlus size={15} />} onClick={() => openDialog({ kind: "add", mode: "bulk" })}>
            Bulk add
          </Button>
        </div>
        <div className="empty__keys">
          <span>
            <Kbd>Ctrl</Kbd>
            <Kbd>N</Kbd> new
          </span>
          <span>
            <Kbd>Ctrl</Kbd>
            <Kbd>Shift</Kbd>
            <Kbd>N</Kbd> bulk
          </span>
          <span>
            <Kbd>Ctrl</Kbd>
            <Kbd>G</Kbd> grid
          </span>
        </div>
      </div>
    </div>
  );
}
