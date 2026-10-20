import { useState } from "react";
import { FolderOpen, Plus, X } from "lucide-react";
import { be } from "../../state/actions";
import { Button, TextInput } from "../ui/ui";

/** Folders (or whole drives) shared with sessions; each shows on the remote under its name. */
export function FolderList({ value, onChange }: { value: string[]; onChange: (paths: string[]) => void }) {
  const [draft, setDraft] = useState("");
  const add = (raw: string) => {
    const p = raw.trim().replace(/\//g, "\\");
    if (!p) return;
    if (!value.some((x) => x.toLowerCase() === p.toLowerCase())) onChange([...value, p]);
    setDraft("");
  };
  const browse = async () => {
    const path = await be().pickFolder();
    if (path) add(path);
  };
  return (
    <div className="paths">
      {value.length > 0 && (
        <ul className="paths__list">
          {value.map((p) => (
            <li key={p} className="paths__item">
              <span className="mono paths__path">{p}</span>
              <button
                type="button"
                className="paths__remove"
                aria-label={`Stop sharing ${p}`}
                onClick={() => onChange(value.filter((x) => x !== p))}
              >
                <X size={12} />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="paths__add">
        <TextInput
          mono
          value={draft}
          placeholder="D:\Work or C:\"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add(draft);
            }
          }}
        />
        {draft.trim() ? (
          <Button size="sm" variant="secondary" icon={<Plus size={12} />} onClick={() => add(draft)}>
            Add
          </Button>
        ) : (
          <Button size="sm" variant="secondary" icon={<FolderOpen size={12} />} onClick={browse}>
            Browse…
          </Button>
        )}
      </div>
      {value.length === 0 && <div className="paths__empty">No folders shared yet.</div>}
    </div>
  );
}
