# Changelog

What changed in each release, newest first. From 0.3.1 on, each release's list is generated from its
commits (`bun scripts/release-notes.mjs`); versions follow [Semantic Versioning](https://semver.org/)
(pre-1.0, so a minor version can change behavior).

## [Unreleased]

### Fixed

- **The app no longer freezes with many sessions open.** Pasting a copy made on a remote waited for that remote on
  the app's own window, so a slow or dead one (often set off by Windows clipboard history, which reads every copy)
  froze everything for seconds at a time, sometimes 30 s or more. Each session's clipboard now waits on a thread of
  its own.
- **Copies no longer ripple through every session.** A copy is offered at once only to the sessions on screen; the
  others get the latest copy when you switch to them. With dozens of sessions, every copy used to reach every host.
- A paste of a copy that's no longer on its remote returns at once instead of waiting, and a paste over the
  clipboard channel gives up after 10 s instead of 45.
- Redirected drives keep working after reading a file over 1600 bytes, and RemoteFX images from Windows servers
  decode without errors (IronRDP update).

## [0.3.0] - 2026-10-27

### Added

- **Group controls in the grid.** Each group's heading in the grid has Connect all, Disconnect all and the group menu,
  like the sidebar.
- **Close tabs to the right.** Right-click a tab in a window's tab strip to close every tab after it.
- **Remote wallpaper setting** (Settings → Graphics, overridable per group and connection). Turn it off to give
  sessions a plain background, which is less for each host to send. It's on by default, and the host's own wallpaper
  setting is never changed. Applies from the next connect.
- A README with a screenshot, a [how it works](https://github.com/JeromeJaJa/rdp-app/blob/main/docs/ARCHITECTURE.md) guide, and an MIT license.

### Changed

- **Releases ship only the installer.** The bare exe and the portable zip are gone: the installer is the one download
  that always puts the companion next to the app. A bare exe ran, but could never install the companion.

## [0.2.0] - 2026-10-21

### Added

- **The companion** (experimental, off by default): a small helper in each remote session, installed over WinRM.
  - Fast copies: copy files, text or anything else on one side and paste on the other, streamed and compressed.
  - A direct, encrypted link beside the session for its traffic (set up like Tailscale, no open ports), since the
    remote desktop connection caps uploads near 1 MB/s on a distant host. Falls back to RDP if it drops.
  - Drag and drop files between this PC and a session, both ways.
  - Send files to one connection, a group or a selection.
  - Host stats: one labelled stat on sidebar rows and grid tiles, and a card with recent history when you hover a
    tab.
  - Program controls: launch, restart or close a program on one or many hosts.
  - Commands across many hosts show as one row that fills in, not a toast per host.
- Ctrl/Shift+click to pick several connections in the sidebar, then right-click to act on all of them.
- The connection form puts its settings beside the fields, with the group first.
- The window that files are dropped on comes to the front.

### Fixed

- A drag out of a session always ends, so other apps can drag again without restarting the app.
- An ended session's clipboard no longer freezes the window.
- Sessions resize when a window drag is let go, not mid-drag.
- Submenus stay open while the pointer moves to them.
- Tab names, the address and the stat line up at every size.
- The Windows key stays local outside fullscreen.
- A session moved back to the main window keeps painting.

## [0.1.0] - 2026-10-21

The first release.

- Many sessions at once: each on its own thread, with sessions you aren't looking at paused on the server.
- H.264 (AVC420 and AVC444) decoded on the GPU or in software, plus RemoteFX, ClearCodec and Planar.
- Tabs on top or on the left, a live grid of every session, and tabs torn off into windows of their own.
- Groups, bulk add from a pasted list, and import / export.
- Clipboard (text, images and files), audio, microphone, drive / printer / smart card redirection, dynamic
  resolution and auto-reconnect.
- Server certificates pinned on first use.
- A local REST API for scripts and agents.
- Passwords encrypted with Windows DPAPI.

[0.3.0]: https://github.com/JeromeJaJa/rdp-app/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/JeromeJaJa/rdp-app/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/JeromeJaJa/rdp-app/releases/tag/v0.1.0
