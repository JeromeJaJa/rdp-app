// Updates (main window). Checks only find out whether a newer version is out; nothing is
// downloaded until the user updates, and then the newest one is fetched (several releases may
// have come out since the check). Updating restarts the app, which drops every session, so
// it's never automatic.
import { be, confirm, errorText, openDialog, toast } from "./actions";
import { getState, isLive, setState, type UpdateState } from "./store";

/** The first check waits this long after start (connecting sessions come first). */
const FIRST_CHECK_MS = 15_000;
/** Then the feed is asked again this often. */
const CHECK_EVERY_MS = 6 * 60 * 60 * 1000;
/** The version that last ran, to say "Updated to …" once after an update. */
const LAST_VERSION_KEY = "rdp-app:last-version";

const setUpdate = (patch: Partial<UpdateState>) => setState((s) => ({ update: { ...s.update, ...patch } }));
/** Versions already announced this run (a check every few hours shouldn't repeat itself). */
const announced = new Set<string>();

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
    if (getState().settings.autoCheckUpdates) void checkForUpdates();
  }, FIRST_CHECK_MS);
  window.setInterval(() => {
    if (getState().settings.autoCheckUpdates) void checkForUpdates();
  }, CHECK_EVERY_MS);
}

/** Asks the release feed whether a newer version is out (nothing is downloaded). `manual`: the
 *  user asked (Settings → About), so the outcome is said either way. */
export async function checkForUpdates(opts: { manual?: boolean } = {}) {
  const { status } = getState().update;
  if (status === "checking" || status === "downloading" || status === "installing") return;
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
    if (opts.manual || !announced.has(found.version)) {
      announced.add(found.version);
      toast("info", `RDP App ${found.version} is available`, "Update when it suits you.", {
        label: "What's new",
        run: () => openDialog({ kind: "changelog" }),
      });
    }
  } catch (e) {
    setUpdate({ status: "error", error: errorText(e), checkedAt: Date.now() });
    if (opts.manual) toast("error", "Couldn't check for updates", errorText(e));
  }
}

/** Updates now: asks first if that drops live sessions, then fetches the newest version,
 *  installs it and restarts into it. */
export function updateNow() {
  const s = getState();
  const live = Object.values(s.sessions).filter((x) => isLive(x.status)).length;
  if (!live) {
    void downloadAndInstall();
    return;
  }
  confirm({
    title: `Update to ${s.update.version} and restart?`,
    message: `${live} session${live === 1 ? "" : "s"} will disconnect. The app downloads the update, then restarts on it.`,
    confirmLabel: "Update and restart",
    onConfirm: () => void downloadAndInstall(),
  });
}

async function downloadAndInstall() {
  setUpdate({ status: "downloading", progress: 0, error: undefined });
  try {
    // Ask again first: a newer release may be out since the last check.
    const newest = await be().checkForUpdate();
    if (!newest) {
      setUpdate({ status: "current", checkedAt: Date.now(), version: undefined, notes: undefined });
      toast("success", "You're up to date", `RDP App ${getState().appVersion} is the latest version.`);
      return;
    }
    setUpdate({ version: newest.version, notes: newest.notes, checkedAt: Date.now() });
    await be().downloadUpdate((done, total) => {
      if (total) setUpdate({ progress: Math.min(1, done / total) });
    });
    setUpdate({ status: "installing", progress: 1 });
    await be().installUpdate();
  } catch (e) {
    setUpdate({ status: "error", error: errorText(e) });
    toast("error", "Couldn't update", errorText(e));
  }
}
