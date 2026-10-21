import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { FilePlus2, FolderPlus, Send, X } from "lucide-react";
import type { ConnectionId } from "../../lib/types";
import { be, closeDialog, sendFiles } from "../../state/actions";
import { displayName, resolveSetting, useStore } from "../../state/store";
import { Dialog } from "../dialogs/Dialog";
import { Button, Segmented, TextInput } from "../ui/ui";
import "./transfers.css";

type Place = "Desktop" | "Downloads" | "Documents" | "other";
const FOLDER_KEY = "rdp-app:send-folder";

/** The destination last used (a known folder, or a path). */
function lastFolder(): { place: Place; path: string } {
  let saved = "";
  try {
    saved = localStorage.getItem(FOLDER_KEY) ?? "";
  } catch {}
  if (saved === "Desktop" || saved === "Downloads" || saved === "Documents") return { place: saved, path: "" };
  return saved ? { place: "other", path: saved } : { place: "Desktop", path: "" };
}

/** The last part of a path ("report.pdf"), and where it is ("C:\Users\me"). */
function splitPath(path: string): [string, string] {
  const at = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return at < 0 ? [path, ""] : [path.slice(at + 1) || path, path.slice(0, at)];
}

/** Send files and folders to one or many connections, through their companions. */
export function SendDialog({ ids, paths: initial }: { ids: ConnectionId[]; paths?: string[] }) {
  const names = useStore(useShallow((s) => ids.map((id) => (s.connById[id] ? displayName(s.connById[id]) : id))));
  const noCompanion = useStore(useShallow((s) => ids.filter((id) => !resolveSetting(s, id, "companion"))));
  const [paths, setPaths] = useState<string[]>(initial ?? []);
  const [{ place, path }, setDest] = useState(lastFolder);
  const [busy, setBusy] = useState(false);
  // Files dropped onto the dialog join the list.
  const dropped = useStore((s) => s.droppedPaths);
  useEffect(() => {
    if (dropped) setPaths((prev) => [...prev, ...dropped.paths.filter((p) => !prev.includes(p))]);
  }, [dropped]);

  const add = async (kind: "files" | "folder") => {
    const picked = await be().pickPaths(kind);
    if (picked) setPaths((prev) => [...prev, ...picked.filter((p) => !prev.includes(p))]);
  };
  const folder = place === "other" ? path.trim() : place;
  const canSend = paths.length > 0 && folder !== "" && noCompanion.length < ids.length && !busy;

  const send = async () => {
    setBusy(true);
    try {
      localStorage.setItem(FOLDER_KEY, folder);
    } catch {}
    const ok = await sendFiles(ids, paths, folder);
    setBusy(false);
    if (ok) closeDialog();
  };

  const target = ids.length === 1 ? names[0] : `${ids.length} connections`;
  return (
    <Dialog
      title={`Send to ${target}`}
      width={540}
      footer={
        <>
          <span className="dialog__footer-note">
            {ids.length > 1 ? "Each one gets a copy. " : ""}Hosts that aren't connected are connected first.
          </span>
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" onClick={closeDialog}>
            Cancel
          </Button>
          <Button variant="primary" icon={<Send size={13} />} onClick={send} disabled={!canSend}>
            Send
          </Button>
        </>
      }
    >
      {ids.length > 1 && (
        <div className="send__targets">
          {names.slice(0, 8).map((n, i) => (
            <span key={ids[i]} className="send__chip">
              {n}
            </span>
          ))}
          {names.length > 8 && <span className="send__chip send__chip--more">+{names.length - 8} more</span>}
        </div>
      )}

      <div className="send__section">
        <div className="send__heading">
          <span>What</span>
          <div className="send__add">
            <Button size="sm" variant="ghost" icon={<FilePlus2 size={13} />} onClick={() => add("files")}>
              Add files…
            </Button>
            <Button size="sm" variant="ghost" icon={<FolderPlus size={13} />} onClick={() => add("folder")}>
              Add folder…
            </Button>
          </div>
        </div>
        {paths.length === 0 ? (
          <button type="button" className="send__empty" onClick={() => add("files")}>
            Choose files or a folder to send
          </button>
        ) : (
          <ul className="send__list">
            {paths.map((p) => {
              const [name, dir] = splitPath(p);
              return (
                <li key={p} className="send__item">
                  <span className="send__name">{name}</span>
                  <span className="send__dir mono">{dir}</span>
                  <button
                    type="button"
                    className="send__remove"
                    aria-label={`Remove ${name}`}
                    onClick={() => setPaths((prev) => prev.filter((x) => x !== p))}
                  >
                    <X size={12} />
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className="send__section">
        <div className="send__heading">
          <span>Where</span>
        </div>
        <Segmented<Place>
          value={place}
          onChange={(next) => setDest({ place: next, path })}
          options={[
            { value: "Desktop", label: "Desktop" },
            { value: "Downloads", label: "Downloads" },
            { value: "Documents", label: "Documents" },
            { value: "other", label: "Other folder" },
          ]}
        />
        {place === "other" ? (
          <TextInput
            mono
            className="send__path"
            placeholder="C:\Temp, %USERPROFILE%\Tools, Desktop\Drop…"
            value={path}
            onChange={(e) => setDest({ place, path: e.target.value })}
          />
        ) : (
          <div className="send__hint">The signed-in user's {place} on each host. Existing names are kept: copies get a number.</div>
        )}
      </div>

      {noCompanion.length > 0 && (
        <div className="send__warn">
          {noCompanion.length === ids.length
            ? "Sending needs the companion, which is off for "
            : "The companion is off for "}
          {noCompanion.length === 1 ? names[ids.indexOf(noCompanion[0])] : `${noCompanion.length} of these`}. Turn it on
          in their settings (Connection → Companion).
        </div>
      )}
    </Dialog>
  );
}
