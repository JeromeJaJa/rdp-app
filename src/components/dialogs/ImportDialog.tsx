import { useEffect, useMemo, useRef, useState } from "react";
import { AlertCircle, AlertTriangle, ArrowRight, ClipboardPaste, FileText, FolderOpen, X } from "lucide-react";
import type { GroupId, ImportOptions, ImportPreview } from "../../lib/types";
import { applyLibrary, be, closeDialog, errorText, toast } from "../../state/actions";
import { useStore } from "../../state/store";
import { Button, Field, Segmented, Select, Spinner } from "../ui/ui";
import { Dialog } from "./Dialog";

interface Source {
  name?: string;
  text: string;
}

const BY_NAME = "byName";

export function ImportDialog({ groupId }: { groupId?: GroupId }) {
  const groups = useStore((s) => s.groups);
  const groupName = useMemo(() => new Map(groups.map((g) => [g.id, g.name])), [groups]);
  const [source, setSource] = useState<Source | null>(null);
  const [pasting, setPasting] = useState(false);
  const [mode, setMode] = useState<ImportOptions["mode"]>("merge");
  const [target, setTarget] = useState<string>(groupId ?? BY_NAME);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [dragOver, setDragOver] = useState(false);
  const textRef = useRef<HTMLTextAreaElement>(null);

  // Live preview, debounced; stale responses are ignored.
  const seq = useRef(0);
  useEffect(() => {
    const text = source?.text ?? "";
    if (!text.trim()) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    const id = ++seq.current;
    setLoading(true);
    const t = window.setTimeout(async () => {
      try {
        const p = await be().previewImport({ text }, { mode, target });
        if (id === seq.current) {
          setPreview(p);
          setPreviewError(null);
        }
      } catch (err) {
        if (id === seq.current) {
          setPreview(null);
          setPreviewError(errorText(err));
        }
      } finally {
        if (id === seq.current) setLoading(false);
      }
    }, 220);
    return () => window.clearTimeout(t);
  }, [source, mode, target]);

  const chooseFile = async () => {
    try {
      const f = await be().openFile();
      if (f) {
        setSource(f);
        setPasting(false);
      }
    } catch (err) {
      toast("error", "Could not open file", errorText(err));
    }
  };

  const onDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const file = e.dataTransfer.files?.[0];
    if (file) {
      setSource({ name: file.name, text: await file.text() });
      setPasting(false);
      return;
    }
    const text = e.dataTransfer.getData("text");
    if (text) {
      setSource({ text });
      setPasting(true);
    }
  };

  const apply = async () => {
    if (!source) return;
    setBusy(true);
    try {
      const r = await be().applyImport({ text: source.text }, { mode, target });
      applyLibrary(r.library);
      closeDialog();
      const parts = [
        r.added && `${r.added} added`,
        r.updated && `${r.updated} updated`,
        r.removed && `${r.removed} removed`,
        r.groupsCreated && `${r.groupsCreated} new group${r.groupsCreated === 1 ? "" : "s"}`,
      ].filter(Boolean);
      toast("success", "Import complete", parts.join(" · ") || "Nothing changed");
    } catch (err) {
      toast("error", "Import failed", errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const total = preview?.total ?? 0;
  const canApply = !!source && !!preview && total > 0 && !busy && !loading;
  const targetOptions = [
    { value: BY_NAME, label: "Matching groups by name" },
    ...groups.map((g) => ({ value: g.id, label: `Into “${g.name}”` })),
  ];

  return (
    <Dialog
      title="Import connections"
      width={620}
      footer={
        <>
          <span className="dialog__footer-note">
            {preview && total > 0 ? `${total} connection${total === 1 ? "" : "s"} in ${preview.groups.length} group${preview.groups.length === 1 ? "" : "s"}` : "JSON export or bulk text"}
          </span>
          <div className="dialog__footer-spacer" />
          <Button variant="ghost" onClick={closeDialog}>
            Cancel
          </Button>
          <Button variant={mode === "replace" ? "danger" : "primary"} disabled={!canApply} onClick={apply}>
            {mode === "replace" ? "Replace" : "Import"}
            {total > 0 ? ` ${total}` : ""}
          </Button>
        </>
      }
    >
      <div
        className="import"
        onDragOver={(e) => {
          e.preventDefault();
          setDragOver(true);
        }}
        onDragLeave={(e) => {
          if (e.currentTarget === e.target) setDragOver(false);
        }}
        onDrop={onDrop}
      >
        {!source && !pasting ? (
          <div className={`dropzone ${dragOver ? "is-over" : ""}`}>
            <div className="dropzone__icon">
              <FileText size={20} />
            </div>
            <div className="dropzone__title">Drop an export file here</div>
            <div className="dropzone__desc">RDP App JSON export, or a text list in the bulk format</div>
            <div className="dropzone__actions">
              <Button size="sm" variant="secondary" icon={<FolderOpen size={13} />} onClick={chooseFile} data-autofocus>
                Choose file…
              </Button>
              <Button
                size="sm"
                variant="ghost"
                icon={<ClipboardPaste size={13} />}
                onClick={() => {
                  setPasting(true);
                  window.setTimeout(() => textRef.current?.focus(), 0);
                }}
              >
                Paste text
              </Button>
            </div>
          </div>
        ) : source?.name && !pasting ? (
          <div className="file-chip">
            <FileText size={15} className="file-chip__icon" />
            <span className="file-chip__name">{source.name}</span>
            <span className="file-chip__meta">
              {preview ? (preview.format === "json" ? "JSON export" : "Text list") : ""}
              {source.text.length > 0 && ` · ${(source.text.length / 1024).toFixed(source.text.length > 10240 ? 0 : 1)} KB`}
            </span>
            <button className="file-chip__clear" aria-label="Remove file" onClick={() => setSource(null)}>
              <X size={13} />
            </button>
          </div>
        ) : (
          <Field
            label="Paste"
            hint={
              <>
                Bulk lines (<span className="mono">host[:port]:user:pass</span>, <span className="mono"># Group: Name</span> starts a
                group) or an export file's JSON.{" "}
                <button type="button" className="link" onClick={chooseFile}>
                  Choose a file instead
                </button>
              </>
            }
          >
            <textarea
              ref={textRef}
              className={`textarea mono bulk__textarea ${dragOver ? "is-over" : ""}`}
              rows={6}
              spellCheck={false}
              value={source?.text ?? ""}
              placeholder={"# Group: Production\n10.20.0.10:3389:CORP\\jdoe:secret\n10.20.0.11:CORP\\jdoe:secret"}
              onChange={(e) => setSource({ text: e.target.value })}
            />
          </Field>
        )}

        <div className="import__options">
          <Field label="Mode">
            <Segmented
              size="sm"
              value={mode}
              onChange={setMode}
              options={[
                { value: "merge", label: "Merge" },
                { value: "replace", label: "Replace" },
              ]}
            />
          </Field>
          <Field label="Destination" className="grow">
            <Select value={target} onChange={setTarget} options={targetOptions} />
          </Field>
        </div>
        <div className="import__mode-hint">
          {mode === "merge"
            ? "Adds new connections and updates matching ones (same host, port and user). Nothing is removed."
            : "Each destination group ends up containing exactly the imported connections. Others in those groups are removed."}
        </div>

        {previewError && (
          <div className="callout callout--danger">
            <AlertCircle size={14} />
            <span>{previewError}</span>
          </div>
        )}

        {preview && preview.groups.length > 0 && (
          <div className="import__preview">
            <table className="import__table">
              <thead>
                <tr>
                  <th>Group in file</th>
                  <th className="num">Connections</th>
                  <th className="num">Matches</th>
                  <th>Destination</th>
                </tr>
              </thead>
              <tbody>
                {preview.groups.map((g, i) => (
                  <tr key={`${g.name}-${i}`}>
                    <td className="ellipsis">{g.name || "—"}</td>
                    <td className="num mono">{g.connections}</td>
                    <td className="num mono muted">{g.duplicates || "—"}</td>
                    <td className="ellipsis">
                      <span className="import__dest">
                        <ArrowRight size={12} />
                        {g.existingGroupId ? (
                          <span>{groupName.get(g.existingGroupId) ?? "Existing group"}</span>
                        ) : (
                          <span className="import__new">New group</span>
                        )}
                        {mode === "replace" && g.existingGroupId && <span className="import__replace">replaced</span>}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {preview && preview.errors.length > 0 && (
          <div className="callout callout--warn">
            <AlertTriangle size={14} />
            <div>
              <div>
                {preview.errors.length} line{preview.errors.length === 1 ? "" : "s"} skipped
              </div>
              <ul className="import__errors">
                {preview.errors.slice(0, 5).map((e, i) => (
                  <li key={i} className="mono">
                    {e}
                  </li>
                ))}
                {preview.errors.length > 5 && <li>+ {preview.errors.length - 5} more</li>}
              </ul>
            </div>
          </div>
        )}

        {loading && !preview && source?.text.trim() && (
          <div className="import__loading">
            <Spinner /> Reading…
          </div>
        )}
        {preview && preview.groups.length === 0 && !previewError && (
          <div className="import__empty">No connections found.</div>
        )}
      </div>
    </Dialog>
  );
}
