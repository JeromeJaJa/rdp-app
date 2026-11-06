// Updates (main window): checks the release feed, downloads in the background, and applies an
// update when the user restarts into it. A restart drops every session, so it's never automatic.
import { be, confirm, errorText, openDialog, toast } from "./actions";
import { getState, isLive, setState, type UpdateState } from "./store";

/** The first check waits this long after start (connecting sessions come first). */
const FIRST_CHECK_MS = 15_000;
/** Then the feed is asked again this often. */
const CHECK_EVERY_MS = 6 * 60 * 60 * 1000;
/** The version that last ran, to say "Updated to …" once after an update. */
const LAST_VERSION_KEY = "rdp-app:last-version";

const setUpdate = (patch: Partial<UpdateState>) => setState((s) => ({ update: { ...s.update, ...patch } }));

/** At start: learns this version, mentions an update just applied, and schedules checks. */
export async function initUpdates() {
  let version = "";
  try {
    version = await be().appVersion();
  } catch {
    return;
  }
  setState({ appVersion: version });

  let last: string | null = null;
  try {
    last = localStorage.getItem(LAST_VERSION_KEY);
    localStorage.setItem(LAST_VERSION_KEY, version);
  } catch {
    /* storage unavailable: no "updated" notice */
  }
  if (last && last !== version) {
    toast("success", `Updated to ${version}`, undefined, {
      label: "What's new",
      run: () => openDialog({ kind: "changelog" }),
    });
  }

  window.setTimeout(() => {
    if (getState().settings.autoUpdate) void checkForUpdates();
  }, FIRST_CHECK_MS);
  window.setInterval(() => {
    if (getState().settings.autoUpdate) void checkForUpdates();
  }, CHECK_EVERY_MS);
}

/** Asks the release feed for a newer version and, if there is one, downloads it. `manual`:
 *  the user asked (Settings → About), so the outcome is said either way. */
export async function checkForUpdates(opts: { manual?: boolean } = {}) {
  const { status } = getState().update;
  // Already have one (downloading, or ready to apply): nothing to ask.
  if (status === "checking" || status === "downloading" || status === "ready") return;
  setUpdate({ status: "checking", error: undefined });
  try {
    const found = await be().checkForUpdate();
    const checkedAt = Date.now();
    if (!found) {
      setUpdate({ status: "current", checkedAt, version: undefined, notes: undefined });
      if (opts.manual) toast("success", "You're up to date", `RDP App ${getState().appVersion} is the latest version.`);
      return;
    }
    setUpdate({ status: "available", checkedAt, version: found.version, notes: found.notes });
    await downloadUpdate();
  } catch (e) {
    setUpdate({ status: "error", error: errorText(e), checkedAt: Date.now() });
    if (opts.manual) toast("error", "Couldn't check for updates", errorText(e));
  }
}

/** Downloads the update found by the last check; says when it's ready to apply. */
export async function downloadUpdate() {
  const { version } = getState().update;
  setUpdate({ status: "downloading", progress: 0 });
  try {
    await be().downloadUpdate((done, total) => {
      if (total) setUpdate({ progress: Math.min(1, done / total) });
    });
    setUpdate({ status: "ready", progress: 1 });
    toast("info", `RDP App ${version} is ready`, "Restart to update when it suits you.", {
      label: "What's new",
      run: () => openDialog({ kind: "changelog" }),
    });
  } catch (e) {
    setUpdate({ status: "error", error: errorText(e) });
  }
}

/** Restarts into the downloaded update, asking first if that drops live sessions. */
export function restartToUpdate() {
  const s = getState();
  const live = Object.values(s.sessions).filter((x) => isLive(x.status)).length;
  const install = () =>
    be()
      .installUpdate()
      .catch((e) => toast("error", "Couldn't install the update", errorText(e)));
  if (!live) {
    void install();
    return;
  }
  confirm({
    title: `Restart to update to ${s.update.version}?`,
    message: `${live} session${live === 1 ? "" : "s"} will disconnect. The app restarts on the new version.`,
    confirmLabel: "Restart and update",
    onConfirm: () => void install(),
  });
}
