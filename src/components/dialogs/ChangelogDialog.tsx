import { useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import changelog from "../../../CHANGELOG.md?raw";
import { closeDialog } from "../../state/actions";
import { useStore } from "../../state/store";
import { updateNow } from "../../state/updates";
import { Markdown } from "../common/Markdown";
import { Button, Spinner } from "../ui/ui";
import { Dialog } from "./Dialog";
import "./changelog.css";

/** What's new: an update that's available (its notes, and restarting into it), then the
 *  changelog of this version and every one before it (shipped with the app). */
export function ChangelogDialog() {
  const version = useStore((s) => s.appVersion);
  const update = useStore(useShallow((s) => s.update));
  // The releases only: not the file's own title and introduction.
  const history = useMemo(() => {
    const first = changelog.search(/^## \[/m);
    return first < 0 ? changelog : changelog.slice(first);
  }, []);
  const offered = !!update.version && ["available", "downloading", "installing"].includes(update.status);
  const busy = update.status === "downloading" || update.status === "installing";

  return (
    <Dialog
      title="What's new"
      width={640}
      className="dialog--changelog"
      footer={
        <>
          <span className="dialog__footer-note">{version ? `You're on ${version}` : ""}</span>
          <div className="dialog__footer-spacer" />
          {offered ? (
            <>
              <Button variant="ghost" onClick={closeDialog} disabled={busy}>
                Later
              </Button>
              <Button variant="primary" data-autofocus onClick={updateNow} disabled={busy}>
                {update.status === "downloading"
                  ? `Downloading ${Math.round((update.progress ?? 0) * 100)}%`
                  : update.status === "installing"
                    ? "Installing…"
                    : "Update and restart"}
              </Button>
            </>
          ) : (
            <Button variant="ghost" onClick={closeDialog}>
              Close
            </Button>
          )}
        </>
      }
    >
      {offered && (
        <section className="changelog__update">
          <div className="changelog__update-head">
            <span className="changelog__update-title">Version {update.version}</span>
            <span className="changelog__update-state">
              {update.status === "downloading" ? (
                <>
                  <Spinner size={11} /> Downloading {Math.round((update.progress ?? 0) * 100)}%
                </>
              ) : update.status === "installing" ? (
                <>
                  <Spinner size={11} /> Installing: the app restarts in a moment
                </>
              ) : (
                "Available"
              )}
            </span>
          </div>
          <Markdown text={update.notes?.trim() || "No notes for this release."} />
        </section>
      )}
      <Markdown text={history} className="changelog__history" />
    </Dialog>
  );
}
