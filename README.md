<img src="assets/icon.svg" width="64" height="64" alt="">

# RDP App

A fast, minimal Windows Remote Desktop client for managing many connections at once.

- **Built for scale**: 50–100+ simultaneous sessions. Each session has its own thread, and sessions you aren't looking at are paused on the server side, so they cost almost nothing.
- **H.264**: graphics pipeline with AVC420 and AVC444 (full 4:4:4 color, so small colored text stays crisp), decoded on the GPU (Media Foundation / DXVA) or in software. Only the regions that changed are converted, straight into shared memory.
- **Tabs on top or left**, drag to reorder (in the grid too, also across groups), a live **grid view** of every session.
- **Separate windows**, Chrome-style: drag a tab out to give it a window of its own, drag tabs between windows. Sessions stay connected throughout and are sized to the window they're in.
- **Groups**: every connection belongs to a group, the built-in **All** group or one of yours. Each group can connect/disconnect as a whole and have its whole list **replaced** from a pasted list (matching connections keep their live sessions).
- **Bulk add**: paste `host:user:pass` lines (also `host:port:user:pass`, `DOMAIN\user`, `user@domain`, `[ipv6]:port:…`).
- **Import / export** everything or single groups, as JSON (groups, notes) or bulk text. Exports include passwords in plain text, so treat the file like the passwords themselves.
- Connections only connect when you ask: double-click or Enter on a tab, **Connect all** / **Disconnect all**, per group too.
- Audio playback (from the session on screen, or all; each connection can override it or be muted), microphone, drive / printer / smart card redirection, RD Gateway (engine and API only for now; hidden in the interface), clipboard sharing (text, images, and files copied either way: copy in Explorer on one side, paste on the other), dynamic resolution, auto-reconnect.
- **Companion** (experimental, off by default): a small agent inside each remote session, installed over WinRM where
  the host allows it. With it:
  - **Fast file copies**: copy files here, paste there (or the other way), streamed instead of fetched a chunk at a
    time, so a high-latency link no longer caps a copy at a few MB/s.
  - **Drag and drop** files between this PC and a session, both ways, behaving like a normal Windows drag (remote
    folders highlight, the cursor shows what a drop would do).
  - **Send files** to one connection, a group, or a selection: right-click → Send files…, or drop files on a tab, a
    group or a grid tile. Hosts that aren't connected are connected first.
  - **Host stats**: CPU, memory, disk and network load on each tab and tile (every 30 s by default, a setting).
- Server certificates are pinned on first use and you are asked if one ever changes.
- A **local REST API** (with docs) so scripts and AI agents can do everything you can do in the window.
- Passwords are encrypted with Windows DPAPI (tied to your Windows account) and never reach the UI.

## Using it

| Action | How |
| --- | --- |
| Jump to a connection or run a command | `Ctrl+K`, or **Search** in the title bar |
| Add one / many | `Ctrl+N` / `Ctrl+Shift+N`, or **+ New** (top of the sidebar; title bar when tabs are on top) |
| Connect a tab | Double-click it, or select and press `Enter` |
| Group actions (connect, replace list, import/export, rename) | Right-click the group header |
| Grid view | `Ctrl+G` or the grid toggle (bottom of the sidebar; title bar when tabs are on top) — click a tile to open it, drag to reorder |
| Switch within a group | With tabs on the left, click the connection name in the title bar |
| Next / previous tab | `Ctrl+Tab` / `Ctrl+Shift+Tab`, `Ctrl+1…9`, or `Shift`+scroll over the top tabs |
| Open a tab in its own window | Drag it out of the tab strip (or over the session), or right-click → **Open in new window** |
| Move a tab between windows | Drag it onto the other window; its × (or `Ctrl+W`) sends it back to the main window. Closing a window does too |
| Session toolbar (send keys, mute, reconnect, fullscreen) | Rest the pointer on the top edge of a session; its X hides it until you move away |
| Send Win, Win+R, Alt+Tab, Task Manager… | Session toolbar → keyboard button |
| Release keyboard from a session | Click outside it, or `Ctrl+Alt+Home` |

While a session has focus, every key goes to it (app shortcuts resume when it loses focus). Windows itself keeps a few
combinations, like Win, Alt+Tab and Ctrl+Alt+Del. Use the toolbar to send those.

Your library lives in `%APPDATA%\com.jeromejaja.rdpapp\library.json` and pinned certificates in `certs.json` next to it.
Libraries from older versions are upgraded on first start (connections without a group move to **All**).

## Local REST API

Settings → Automation shows the address, the token and the docs. The API listens on `127.0.0.1:47123` only and every
request needs the token:

```sh
curl -H "Authorization: Bearer <token>" http://127.0.0.1:47123/api/v1/state
```

- `http://127.0.0.1:47123/docs`: interactive reference (try requests in the page)
- `http://127.0.0.1:47123/llms.txt`: the same reference as plain text, meant to be handed to an agent
- `http://127.0.0.1:47123/openapi.json`: OpenAPI 3 description

It covers groups and connections (including bulk add and group replace), sessions (connect with `?wait=true`,
disconnect, reconnect), PNG screenshots, keyboard/mouse/text input, certificates, settings, import/export, driving the
window, and a Server-Sent Events stream of status changes.

## Development

Requirements: Rust (MSVC toolchain), [Bun](https://bun.sh), Visual Studio C++ build tools, and NASM (on `PATH`; without
it the H.264 decoder quietly builds without its assembly and decodes much slower).

```sh
bun install
bun run tauri dev      # full app
bun run dev            # UI only, in a browser, with a mock backend and fake sessions
bun run build:release  # release build + installer (with the companion) in target/release/bundle/nsis/
```

### CI and releases

Every push to `main` and every pull request is checked on Windows (`.github/workflows/ci.yml`): UI typecheck and
build, `cargo fmt`, clippy with warnings as errors, and the tests. To publish a release, bump `version` in
`src-tauri/tauri.conf.json`, then push a matching tag (`git tag v0.1.0 && git push origin v0.1.0`). The release workflow
builds the installer, a portable zip, and the bare exe, and attaches them to a GitHub release. Running the workflow by
hand builds the same files as an artifact without publishing.

### Layout

```
src/                     React UI (Vite, TypeScript, zustand, dnd-kit)
  engine/tauri/          session view, input translation, shared-memory frame painter
  engine/mock/           fake sessions for UI work in a browser
  lib/backend/           Backend contract (tauri.ts = Rust, mock.ts = in-browser)
src-tauri/               Tauri shell
  src/core.rs            everything the UI and the API can do (both go through here)
  src/store/             encrypted library, groups, import/export
  src/api/               REST API server, endpoint table, generated docs
  src/certs.rs           pinned server certificates
  src/frames.rs          WebView2 shared buffers
  src/tearoff.rs         tabs torn off into windows of their own
  src/companion_link.rs  attached companions: host stats, sends, fast copies, drags
  src/file_drop.rs       files dropped in from other apps (the window's own OLE drop target)
crates/rdp-engine/       the RDP engine (no UI dependencies)
  src/session.rs         per-session thread: connect, reconnect, input, pause/resume, resize
  src/connect.rs         transport (direct or RD Gateway), TLS, channels
  src/gfx/               graphics pipeline client (surfaces, cache, region-limited color conversion)
    avc444.rs            rebuilds 4:4:4 chroma from the two AVC444 / AVC444v2 streams
    mf.rs                GPU H.264 decoder (Media Foundation + Direct3D 11)
  src/audio.rs           playback and microphone
  src/devices.rs         drive / printer / smart card redirection
  src/surface.rs         shared-memory frame format polled by the renderer
crates/rdp-testserver/   local RDP servers with animated content (RemoteFX, or H.264 with --h264 / --avc444)
crates/rdp-companion/    the companion (runs in the remote session): stats, clipboard, drags, received files
crates/rdp-companion-proto/  its wire protocol and the streamed file transfers both ends run
crates/rdp-vfiles/       files from the other end offered to Windows apps as an OLE data object (paste, drag)
crates/rdp-clipboard/    clipboard redirection (CLIPRDR), copying files through the companion when attached
```

**How frames reach the screen:** each session decodes into a framebuffer in Rust. Changed rectangles are copied into a
WebView2 shared buffer that the page maps as an `ArrayBuffer`, and a ring of rectangles is published in the buffer header.
The page polls that header once per display frame and repaints only what changed. No per-frame IPC messages, no extra
copies. Grid tiles get separate downscaled thumbnail buffers, refreshed at most every 150 ms.

### Testing without real hosts

```sh
cargo run -p rdp-testserver -- --count 50                  # 50 servers on 127.0.0.1:33890+ (user test / pass test)
cargo run -p rdp-testserver -- --count 1 --h264 --fps 30   # H.264 over the graphics pipeline
cargo run -p rdp-testserver -- --count 1 --mix             # H.264 mixed with ClearCodec / Planar regions
cargo run -p rdp-testserver -- --count 1 --avc444          # AVC444 (--avc444v2 for the v2 layout), with a 1-px color test block
cargo run -p rdp-engine --bin rdp-probe -- --file hosts.txt --count 50 --seconds 20
```

`rdp-probe` connects headlessly, prints events and frame rates, and saves a PNG of the first session. Put real hosts in
`test-hosts.txt` (gitignored, one `host[:port]:user:pass` per line) to probe them. Set
`RDP_LOG=rdp_engine::session=debug` to get per-session CPU accounting.

## Current limitations

- Image quality is the host's choice. Windows sends photos/gradients as RemoteFX Progressive at its configured
  quality (default "Medium", visibly soft). For sharper output set the host's Group Policy *Remote Desktop Session Host →
  Remote Session Environment → "Configure image quality for RemoteFX Adaptive Graphics"* to High or Lossless, or enable
  *"Prioritize H.264/AVC 444 graphics mode"*.
- Windows hosts only send AVC444 when configured to (Group Policy "Prioritize H.264/AVC 444 graphics mode"); otherwise
  they use AVC420 for video and lossless codecs for text, which is also handled. AVC444 has been verified against the
  local test server, which packs frames by the MS-RDPEGFX equations, not yet against a Windows host.
- With the decoder on Auto, the first 8 H.264 streams decode on the GPU and the rest in software (each GPU decoder
  holds its own video memory pool).
- Audio, microphone, drive/printer/smart card redirection and RD Gateway are built on IronRDP's implementations but
  have not been tested against real Windows hosts and gateways yet (the local test server doesn't exercise them).

Built on [IronRDP](https://github.com/Devolutions/IronRDP) (MIT/Apache-2.0) and [Tauri](https://tauri.app).
