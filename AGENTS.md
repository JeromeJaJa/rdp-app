# Working on RDP App

A high-performance, minimal Windows RDP manager built to run 50–100+ sessions at once.
This file is for anyone (human or agent) changing the code. The [README](README.md) covers
what the app does, the file layout, and how to build and run; read it first. This file is the
part that isn't obvious from the code: the invariants, the performance rules, the Windows
gotchas, and the conventions.

## The bar: performance is the point

This app exists to be faster and lighter than every other RDP client while holding dozens of
live sessions. Treat performance as a correctness requirement, not a nice-to-have. Before
adding anything on a hot path, know its cost.

- **Per-frame work is sacred.** Frames go Rust → a WebView2 shared buffer → the page, with no
  per-frame IPC and no extra copies (see the README's "How frames reach the screen"). Only
  changed rectangles are converted and copied. Don't add a per-frame allocation, serialization,
  event, or React render. The page repaints by polling a buffer header, not by state updates.
- **Hidden sessions cost almost nothing.** Sessions you aren't looking at are paused server-side
  (SUPPRESS_OUTPUT). Visibility drives this (`engine.set_visibility`); keep it accurate and cheap.
- **One thread per session, isolated.** Each session runs its own Tokio task and is wrapped in
  `catch_unwind` so one crash can't take down the others. Don't share locks across sessions on
  hot paths; prefer per-session state and message passing (`Command`).
- **Measure, don't guess.** `cargo run -p rdp-engine --bin rdp-probe` connects headlessly and
  prints frame rates and per-session CPU (`RDP_LOG=rdp_engine::session=debug`). The test server
  (`crates/rdp-testserver`) generates load locally. Kill load-test servers and dev instances as
  soon as you're done — this is a shared machine.
- **The UI is a thin, fast shell.** zustand with narrow selectors; `useShallow` for object/array
  selects; memoized components. A store update must not cascade into every tab. The remote
  surface element never re-renders from React state.

If a change trades performance for convenience, it needs an explicit reason and ideally a number.

## Architecture invariants

- **`src-tauri/src/core.rs` is the single chokepoint.** Everything the UI and the REST API can do
  goes through `Core`. New capabilities get a `Core` method, then a thin Tauri command and/or API
  endpoint on top. Don't let the UI reach into the store or engine directly.
- **The backend contract is `src/lib/backend/types.ts`.** `tauri.ts` implements it against Rust;
  `mock.ts` implements it in-browser (fake sessions) so the whole UI runs in a plain browser with
  `bun run dev`. Every backend method must exist in both. Keep the mock honest.
- **The engine (`crates/rdp-engine`) has no UI dependencies.** It talks to the shell through
  `EngineEvent` (out) and `Command`/method calls (in), plus a `SurfaceProvider` for frame memory.
  It must stay reusable and headless (that's what `rdp-probe` relies on).
- **Settings are layered: app → group → connection.** `src-tauri/src/profile.rs` resolves them
  (`resolve`/`clean`); `OVERRIDABLE` lists the keys a group or connection may override. To add a
  layered setting: add it to `default_settings()` (core.rs), to `OVERRIDABLE` (profile.rs) and the
  frontend `OVERRIDABLE` (types.ts), consume it in `connect_params`, and give it a row in
  `OverridesEditor.tsx` and `SettingsDialog.tsx`. Validation is automatic from the default's type.
- **Windows beyond `main`.** Tabs can tear off into their own windows (`src-tauri/src/tearoff.rs`,
  `src/DetachedApp.tsx`). The main window drives the REST API and grid; detached windows show one
  session each. `tearoff` tracks which window hosts each tab and routes that session's full-size
  frames there. See "Tear-off windows" below.

## Working on the code

Build/run/test basics are in the README. Beyond those:

```sh
bunx tsc --noEmit -p tsconfig.json          # typecheck the UI (do this after every TS change)
cargo clippy --workspace --all-targets      # lint Rust (keep it clean; no new warnings)
cargo fmt                                    # rustfmt (rustfmt.toml sets max_width = 120)
cargo test --workspace                       # all Rust tests
```

### Seeing a change actually work (the real app, not just the mock)

The mock backend (`bun run dev`) is great for UI layout, but anything touching the engine, frames,
windows, or the OS needs the real Tauri app. Drive it over the WebView2 DevTools protocol:

```sh
# Launch dev with remote debugging on port 9222 and a separate identity so it can't clobber a
# real install's data, token, or API port:
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222 \
  bun run tauri dev --no-watch --config '{"identifier":"com.jeromejaja.rdpapp.dev"}'
```

Then connect a CDP client (puppeteer-core) to `http://127.0.0.1:9222` and find the page whose URL
contains `localhost:1420`. You can call Tauri commands from the page with
`window.__TAURI_INTERNALS__.invoke("cmd", args)`, read the DOM, and screenshot. A working harness
lives under `scratch/uitest/` (not committed): `drive.mjs` for the main window, `tear.mjs` for
multi-window flows. For windowed/visual things, screenshot each webview separately (iterate
`browser.pages()` filtered to `localhost:1420`).

- The window starts hidden and is revealed after first paint; detached windows are shown by the
  backend once they hold a tab.
- **CDP captures only the webview, not the OS compositor** — DWM acrylic/backdrop and the native
  drag move-loop are invisible to a screenshot. Those need a human to look.
- After dev runs, delete the dev data so it doesn't linger:
  `AppData/{Local,Roaming}/com.jeromejaja.rdpapp.dev`, and any scratch token/host files.

### A note on editing files

The Bash tool mangles backslashes (`\\` collapses) and chokes on big heredocs. For anything with
backslashes (Windows paths in Rust/TS) or multi-line content, use the Write/Edit tools or write a
small Python patch script with the Write tool and run it. Re-read a file after `cargo fmt` before
pattern-matching against it.

## Windows gotchas (hard-won — don't relearn these)

- **Never emit a Tauri event or call back into the app inside a `with_webview` closure.** That
  closure runs on the UI thread holding the webview's lock, and emitting evaluates script in every
  listening webview (including that one) → deadlock. `frames.rs` defers such emits with
  `emit_later` (spawns onto the async runtime). This cost a frozen-on-tab-close bug.
- **Tauri event targeting.** `listen()` in JS receives an event for *any* target. For an event
  meant for one window (`emit_to`), listen with `getCurrentWebviewWindow().listen(...)`, or every
  window reacts. See `onSelectTab` and the `surface-released` listener.
- **Blocking native work goes off the UI thread.** The tab-drag move-loop (`WM_SYSCOMMAND SC_MOVE`)
  blocks until the mouse is released; it runs via `spawn_blocking`, and `ReleaseCapture` is
  marshalled to the main thread with `run_on_main_thread`. Don't block the event loop.
- **Acrylic needs a transparent window.** The backdrop can't show through an opaque webview, so
  windows are created `transparent: true` and solid mode fills an opaque `--bg`. The material is
  applied with `window-vibrancy` (a tauri dependency), not hand-rolled DWM calls (those returned
  E_INVALIDARG). It's experimental and off by default (`material.rs`, `windowMaterial` setting).
- **rustls needs a crypto provider installed.** The dep tree carries both `ring` and `aws-lc-rs`,
  so rustls can't auto-pick. Install one explicitly at startup
  (`rustls::crypto::ring::default_provider().install_default()`) — the engine and test server both
  do. A new binary that does TLS must too.
- **Dynamic resize can be silently dropped.** A monitor-layout PDU sent while the server is still
  applying the previous one is ignored. `session.rs` verifies the size took effect and re-sends a
  few times (`RESIZE_VERIFY`). This is why a session resizes correctly after a window closes.
- **Getting a native stack on Windows.** `lldb` here needs `python310.dll` on `PATH`/`PYTHONHOME`
  (the embeddable Python zip works); its output is buried in debug-info noise — filter for
  `frame #`. This is how the tab-close deadlock was found.

## IronRDP and vendored crates

- IronRDP is pinned to a **git rev** in the root `Cargo.toml` (`[workspace.dependencies]`), not a
  crates.io release — the releases lag important EGFX/AVC420 fixes. Bumping the rev means bumping
  it in every `ironrdp-*` line and in each vendored crate's `Cargo.toml`.
- **Vendored crates live in `third_party/` and are wired via `[patch]`** in the root `Cargo.toml`,
  and excluded from the workspace `members`. We vendor only when a local fix isn't upstream yet:
  - `ironrdp-graphics` — SRL/RemoteFX Progressive decode tweaks for real Windows servers.
  - `ironrdp-rdpdr-native` — lets a redirected drive be rooted at any folder on a local volume,
    not only a volume root (`C:\`). See `third_party/README.md` for the exact diffs.
  To vendor another: copy the crate from the IronRDP checkout, point its deps at the git rev, add a
  `[patch]` entry and a `members`/`exclude` line, and document the change in `third_party/README.md`.
- **Dynamic virtual channels (DVC).** The client registers channels/listeners on
  `DrdynvcClient` in `connect.rs`. A server-initiated channel (audio input, the companion) uses a
  `DvcChannelListener` whose `create()` returns a processor; the processor's `process()` return
  value is sent automatically, and off-thread sends go through `Command::SendDvc`. The microphone
  (`audio.rs`) and companion (`companion.rs`) are the two patterns to copy.
- Don't paste FreeRDP-derived code. Base protocol work on the MS-RDP spec and IronRDP.

## Subsystems, briefly

- **Tear-off windows** (`tearoff.rs`, `DetachedApp.tsx`, `useTabsDnd.ts`, `WindowTabStrip.tsx`):
  Chrome-style. Dragging a tab out hands it to a window that follows the pointer (system move
  loop); a prepared hidden window makes it instant; a window sized so the session keeps its size
  avoids a mid-drag resize (`held`); size changes apply on drop. Drop targets are found z-order
  aware (`WindowFromPoint` walk). Each window reports its tab-strip rects so a drag can target it.
- **Frames** (`frames.rs`): the `SurfaceProvider` for Windows — WebView2 shared buffers, one host
  window per full-size session, re-posted/released as tabs move between windows.
- **Clipboard** (`crates/rdp-clipboard`): CLIPRDR file streaming over OLE `IDataObject`/`IStream`
  on an STA thread, with a direct paste channel. Large transfers are the slow part on a single TCP
  stream — the companion is the real fix.
- **Companion** (`crates/rdp-companion*`, `rdp-engine/src/companion.rs`): base only. A remote-side
  binary opens a DVC back to the client for features pure RDP can't do (parallel-stream transfers,
  native drag). Protocol is in `rdp-companion-proto` (unit-tested framing). **The end-to-end
  channel is unverified against a live Windows host** — that's the next step, plus auto-deploy of
  the binary and the transport itself. Enabled by the layered `companion` setting (off by default).

## Conventions and constraints

- **Commits are conventional and scoped per change.** `feat(...)`, `fix(...)`, `style(...)`, etc.
  Keep pure-formatting changes in their own `style:` commit. Commit per feature/fix, not in one
  lump. Don't push unless asked.
- **This repo will be open-sourced.** Keep secrets, real hostnames, tokens, screen captures of real
  sessions, and personal data out of the code and out of git history. Real test hosts go in
  `test-hosts.txt` (gitignored) and are never printed.
- **Passwords are DPAPI-encrypted** (tied to the Windows account) in the library and never reach
  the UI. Exports intentionally include plain-text passwords (a deliberate user choice) — treat an
  export file like the passwords themselves.
- **The REST API is loopback-only with token auth.** Keep it that way; it's meant for local scripts
  and agents, not the network.
- **Security boundaries.** No system-wide keyboard hook and no OS key-injection. Never log
  keystrokes. Send special keys (Win, Ctrl+Alt+Del, …) through the session, not by faking input.
- **Connecting to the user's own live machine takes over their session.** Only do it when asked,
  keep it brief, tell them, and don't touch their apps.
- **UI direction: dark glass.** New floating UI (menus, popovers, palette, toasts, toolbars) uses
  the dark-glass tokens (`--glass-*`, `.glass`). Mica was rejected; acrylic is an opt-in experiment.

## What still needs a real Windows host to verify

Unit tests and the test server cover a lot, but these can only be confirmed against real hardware:
the companion channel end-to-end, RDPDR drive/folder sharing against a Windows host, AVC444 packed
by a Windows server (vs. the test server), and the native tab-drag move-loop (needs a real mouse).
